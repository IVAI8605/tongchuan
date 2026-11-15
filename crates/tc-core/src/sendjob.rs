//! 管理页的「发送」任务。
//!
//! 页面（浏览器）既拿不到服务端目录，也调不了本地程序，所以发送必须由 daemon 代跑：
//! POST /api/send 起一个后台线程，用与 CLI 完全相同的会话代码发出去，
//! 进度写进 `DaemonState.send_job`，页面每秒取一次。
//!
//! 一次只允许一个任务：两个任务并行会让进度条不知道该跟谁，家用场景也没有并发发送的必要。

use crate::daemon::{DaemonState, SendJob};
use crate::history;
use crate::queue::now_secs;
use crate::session::{run_sender_with_device_control_opts, SendProgress};
use crate::tls::{client_config_with_identity, Identity};
use crate::transport::TlsOpener;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::thread;

/// 管理页提交的发送请求。`fingerprint` 为空时按目标地址去发现列表里找。
pub struct SendRequest {
    pub to: String,
    pub fingerprint: Option<String>,
    pub path: PathBuf,
    pub pair_code: Option<String>,
    /// v0.12：忽略接收端已有文件，本次全部重传
    pub force: bool,
}

/// 启动一次发送。立刻返回：真正的传输在后台线程里跑，页面看 `/api/send/status`。
pub fn start(state: Arc<DaemonState>, req: SendRequest) -> io::Result<()> {
    if req.to.trim().is_empty() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "目标设备为空"));
    }
    if !req.path.exists() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("路径不存在：{}", req.path.display()),
        ));
    }

    // 同一时间只跑一个：上一场没结束时直接拒绝，避免两条进度互相覆盖
    let mut slot = state.send_job.lock().expect("send_job poisoned");
    if let Some(job) = slot.as_ref() {
        if job.running() {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                format!("已有发送任务在进行中（{}）", job.target),
            ));
        }
    }

    // 指纹必须拿到：接收端带身份后强制 TLS，明文连接只会得到一句 EOF
    let fingerprint = req.fingerprint.clone().or_else(|| {
        state
            .peers
            .lock()
            .expect("peers poisoned")
            .iter()
            .find(|p| p.addr == req.to || host_of(&p.addr) == host_of(&req.to))
            .map(|p| p.fingerprint.clone())
    });
    let fingerprint = match fingerprint {
        Some(fp) if !fp.trim().is_empty() => fp,
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "没有这台设备的指纹：先用「刷新设备」重新发现，或在命令行用 tc discover 查",
            ))
        }
    };

    let job = SendJob {
        target: req.to.clone(),
        path: req.path.display().to_string(),
        phase: "scanning".into(),
        percent: 0,
        files: 0,
        bytes_sent: 0,
        total_bytes: 0,
        blocks_sent: 0,
        total_blocks: 0,
        current: String::new(),
        speed_bps: 0,
        files_skipped: 0,
        ok: false,
        reason: None,
        started_at: now_secs(),
        finished_at: None,
    };
    *slot = Some(job);
    drop(slot);

    let path = req.path.clone();
    thread::spawn(move || run(state, req, path, fingerprint));
    Ok(())
}

fn run(state: Arc<DaemonState>, req: SendRequest, path: PathBuf, fingerprint: String) {
    let identity_dir = {
        let cfg = state.config.lock().expect("config poisoned");
        cfg.identity_dir.clone()
    };
    let result = (|| -> io::Result<crate::SendReport> {
        let ident = Identity::load_or_create(&identity_dir)?;
        let expected = Identity::parse_fingerprint(&fingerprint)?;
        let opener = TlsOpener {
            addr: req.to.clone(),
            config: client_config_with_identity(expected, &ident)?,
        };
        let mut last_percent = 0u32;
        let mut progress = |p: SendProgress| {
            if let Ok(mut slot) = state.send_job.lock() {
                if let Some(job) = slot.as_mut() {
                    job.phase = if p.scanning { "scanning" } else { "sending" }.into();
                    job.files = p.files;
                    job.bytes_sent = p.bytes_sent;
                    job.total_bytes = p.total_bytes;
                    job.blocks_sent = p.blocks_sent;
                    job.total_blocks = p.total_blocks;
                    job.current = p.current;
                    job.speed_bps = p.speed_bps;
                    // 扫描阶段没有总字节，百分比保持 0；传输阶段按字节算，
                    // 单调不回退：进度回调可能带上一块的旧值，来回跳看着像卡死
                    if !p.scanning && p.total_bytes > 0 {
                        let pct = ((p.bytes_sent as f64 / p.total_bytes as f64) * 100.0) as u32;
                        last_percent = last_percent.max(pct.min(100));
                        job.percent = last_percent;
                    }
                }
            }
        };
        run_sender_with_device_control_opts(
            &opener,
            &[path.clone()],
            req.pair_code.as_deref(),
            crate::manifest::local_device(),
            Some(&mut progress),
            None,
            // v0.16：管理页本版不提供落点选择，仍发默认落点
            crate::SendOptions {
                force: req.force,
                dest_id: None,
            },
        )
    })();

    let mut slot = state.send_job.lock().expect("send_job poisoned");
    if let Some(job) = slot.as_mut() {
        job.finished_at = Some(now_secs());
        match &result {
            Ok(r) => {
                job.phase = "done".into();
                job.ok = r.verdict == "OK";
                job.percent = if job.ok { 100 } else { job.percent };
                job.files_skipped = r.files_skipped;
                job.reason = if job.ok { None } else { Some(r.verdict.clone()) };
            }
            Err(e) => {
                job.phase = "failed".into();
                job.ok = false;
                job.reason = Some(e.to_string());
            }
        }
    }
    drop(slot);

    // 与命令行共用同一份发送历史：页面发出去的东西，之后也能用右键「再次发送上一批」重发
    record_history(&req, &result);
    match &result {
        Ok(r) => state.log(
            "info",
            &format!(
                "管理页发送完成 目标={} 路径={} verdict={} files={} blocks={} 增量跳过={}",
                req.to,
                path.display(),
                r.verdict,
                r.files,
                r.blocks_sent,
                r.files_skipped
            ),
        ),
        Err(e) => state.log(
            "warn",
            &format!(
                "管理页发送失败 目标={} 路径={}: {e}",
                req.to,
                path.display()
            ),
        ),
    }
}

fn record_history(req: &SendRequest, result: &io::Result<crate::SendReport>) {
    let rec = match result {
        Ok(r) => history::SendRecord {
            id: history::new_id(),
            ts: now_secs(),
            to: req.to.clone(),
            paths: vec![req.path.clone()],
            peer_fp: req.fingerprint.clone(),
            files: r.files,
            bytes: r.bytes,
            blocks_sent: r.blocks_sent,
            verdict: r.verdict.clone(),
            ok: r.verdict == "OK",
            reason: None,
            retry_of: None,
        },
        Err(e) => history::SendRecord {
            id: history::new_id(),
            ts: now_secs(),
            to: req.to.clone(),
            paths: vec![req.path.clone()],
            peer_fp: req.fingerprint.clone(),
            files: 0,
            bytes: 0,
            blocks_sent: 0,
            verdict: "ERROR".into(),
            ok: false,
            reason: Some(e.to_string()),
            retry_of: None,
        },
    };
    // 历史写失败不该影响已经结束的传输，只留一行提示
    if let Err(e) = history::record(&history::default_dir(), &rec) {
        eprintln!("WARN: 发送历史写入失败: {e}");
    }
}

fn host_of(addr: &str) -> &str {
    addr.rsplit_once(':').map(|(h, _)| h).unwrap_or(addr)
}
