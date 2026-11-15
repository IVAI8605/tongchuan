use std::collections::HashMap;
use std::env;
use std::io;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::thread;
use std::time::Duration;
use tc_core::daemon::{Config, DaemonState, HistoryEntry, PeerInfo};
use tc_core::discover;
use tc_core::history;
use tc_core::manifest::local_device;
use tc_core::queue::{self, QueuedTask, DEFAULT_WINDOW_MINUTES};
use tc_core::tls::{client_config, client_config_with_identity, server_config, Identity};
use tc_core::transport::{Opener, PlainAccepter, PlainOpener, TlsAccepter, TlsOpener};
use tc_core::trust::{self, TrustStore};
use tc_core::web;
use tc_core::{
    run_receiver_with_gate_streams, run_receiver_with_streams, HubReport, ReceiverHub, TrustGate,
    TrustMode,
};

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    if let Err(e) = run(&args) {
        eprintln!("ERROR: {e}");
        std::process::exit(1);
    }
}

fn bad(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, msg)
}

fn value(args: &[String], key: &str) -> io::Result<String> {
    args.iter()
        .position(|a| a == key)
        .and_then(|i| args.get(i + 1))
        .cloned()
        .ok_or_else(|| bad(&format!("missing {key}")))
}

fn opt(args: &[String], key: &str) -> Option<String> {
    args.iter()
        .position(|a| a == key)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

fn values(args: &[String], key: &str) -> Vec<String> {
    args.iter()
        .enumerate()
        .filter_map(|(i, a)| {
            if a == key {
                args.get(i + 1).cloned()
            } else {
                None
            }
        })
        .collect()
}

/// v0.19：哈希缓存固定放在配置文件同目录，避免受启动时工作目录影响。
fn cache_file_path(cfg_path: &Path) -> PathBuf {
    cfg_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("hash-cache.jsonl")
}

fn parse_root_map(args: &[String]) -> HashMap<String, String> {
    // 形如 --root-map r0=自定义名字；值仍然要走逐段清洗，避免接收端自己写出越界路径
    values(args, "--root-map")
        .iter()
        .filter_map(|s| {
            let (k, v) = s.split_once('=')?;
            Some((k.trim().to_string(), v.trim().to_string()))
        })
        .collect()
}

/// 信任库子命令。输出统一以 `TRUST ` 开头，方便脚本解析；
/// 只追加事件，不删行，撤销后列表里消失但文件里的 add/revoke 都还在。
fn trust_cmd(args: &[String]) -> io::Result<()> {
    // 不给 --identity 时跟着 daemon 的配置文件走，保证 CLI 与管理页操作同一份 trust.jsonl
    let dir = trust::resolve_identity_dir(
        opt(args, "--identity").map(PathBuf::from).as_deref(),
        opt(args, "--config").map(PathBuf::from).as_deref(),
    )?;
    let store = TrustStore::new(&dir);
    let action = args.get(1).map(String::as_str).unwrap_or("list");
    // 指纹支持位置参数（tc trust approve FP）与 --fp 两种写法，脚本与手敲都顺手
    let fp = args
        .get(2)
        .filter(|s| !s.starts_with("--"))
        .cloned()
        .or_else(|| opt(args, "--fp"))
        .unwrap_or_default();

    match action {
        "list" => {
            let peers = store.list()?;
            println!(
                "TRUST count={} file={}",
                peers.len(),
                store.path().display()
            );
            for p in peers {
                let alias = if p.alias.is_empty() { "-" } else { p.alias.as_str() };
                println!(
                    "TRUST fp={} alias={} by={} first={} last_seen={} name={} platform={}",
                    p.fp,
                    alias,
                    if p.added_by.is_empty() { "-" } else { p.added_by.as_str() },
                    p.first_seen,
                    p.last_ts,
                    if p.name.is_empty() { "-" } else { p.name.as_str() },
                    if p.platform.is_empty() { "-" } else { p.platform.as_str() }
                );
            }
        }
        "pending" => {
            let list = store.pending()?;
            println!("TRUST pending={} dir={}", list.len(), dir.display());
            for e in list {
                println!(
                    "PENDING fp={} name={} platform={} ts={} reason={}",
                    e.fp,
                    if e.name.is_empty() { "-" } else { e.name.as_str() },
                    if e.platform.is_empty() { "-" } else { e.platform.as_str() },
                    e.ts,
                    if e.note.is_empty() { "-" } else { e.note.as_str() }
                );
            }
        }
        "approve" => {
            let alias = opt(args, "--alias").unwrap_or_default();
            let p = store.approve(&fp, &alias, "", "", "manual")?;
            println!(
                "TRUST approve ok=true fp={} alias={} by={} dir={}",
                p.fp,
                if p.alias.is_empty() { "-" } else { p.alias.as_str() },
                if p.added_by.is_empty() { "manual" } else { p.added_by.as_str() },
                dir.display()
            );
        }
        "revoke" => {
            let ok = store.revoke(&fp, "manual", "CLI 撤销")?;
            println!("TRUST revoke ok={ok} fp={fp} dir={}", dir.display());
        }
        "rename" => {
            let alias = value(args, "--alias")?;
            let ok = store.rename(&fp, &alias)?;
            println!(
                "TRUST rename ok={ok} fp={fp} alias={alias} dir={}",
                dir.display()
            );
        }
        other => return Err(bad(&format!("unknown trust action {other}"))),
    }
    if let Ok(n) = store.bad_lines() {
        if n > 0 {
            eprintln!("WARN: 信任表有 {n} 行无法解析，已跳过（不影响其余记录）");
        }
    }
    Ok(())
}

fn port_of(addr: &str) -> u16 {
    addr.rsplit(':').next().and_then(|s| s.parse().ok()).unwrap_or(53318)
}

/// 没给 --peer-fp 时听一轮发现报文，取同一地址的公告指纹。
/// 为什么需要：接收端带身份后强制 TLS，明文连接会被直接掐断，客户端只看到
/// "failed to fill whole buffer"，用户拿到的信息等于没信息（2026-11-13 发版冒烟实测）。
/// 听不到就返回 None，仍走明文老路径（对端是 `tc recv` 明文接收时还能用）。
fn discover_fp_for(to: &str) -> Option<(String, String)> {
    let peers = discover::listen(Duration::from_secs(2), "").ok()?;
    peers
        .into_iter()
        .find(|p| p.addr == to || endpoint_host(&p.addr) == endpoint_host(to))
        .map(|p| (p.fingerprint, p.name))
}

/// 只比主机部分：对端公告用的端口与用户手填的端口可能不一致（同一台机器的不同入口）。
fn endpoint_host(addr: &str) -> &str {
    addr.rsplit_once(':').map(|(h, _)| h).unwrap_or(addr)
}

/// 有 --peer-fp 就走 TLS 并按指纹固定，否则明文。
/// 明文是显式降级，只用于本机回环和排查，正式互传必须带指纹。
/// TLS 时带上本机证书：接收端 closed 模式要靠它识别来源，不带等于自报「未信任」。
fn make_opener(
    to: &str,
    peer_fp: Option<&str>,
    identity: Option<&Identity>,
) -> io::Result<Box<dyn Opener>> {
    match peer_fp {
        Some(fp) => {
            let expected = Identity::parse_fingerprint(fp)?;
            let config = match identity {
                Some(id) => client_config_with_identity(expected, id)?,
                None => client_config(expected)?,
            };
            Ok(Box::new(TlsOpener {
                addr: to.into(),
                config,
            }))
        }
        None => Ok(Box::new(PlainOpener { addr: to.into() })),
    }
}

/// TLS 发送用的本机身份目录：与 `tc trust` 同一套解析规则，
/// 否则会出现「命令能发、管理页却看不到这台设备」的错位。
fn send_identity(args: &[String]) -> io::Result<(PathBuf, Identity)> {
    let dir = trust::resolve_identity_dir(
        opt(args, "--identity").map(PathBuf::from).as_deref(),
        opt(args, "--config").map(PathBuf::from).as_deref(),
    )?;
    let id = Identity::load_or_create(&dir)?;
    Ok((dir, id))
}

fn print_send(r: &tc_core::SendReport) {
    // dest=：接收端回报的落点根目录名（v0.11），命令行也能看出「传到哪个文件夹」
    // packs=：v0.14 收到的批量请求数，0 表示走的是逐块兼容路径
    // streams=：v0.15 实际开了几条数据连接，1 表示单流
    println!(
        "SEND txn={} files={} bytes={} blocks={} packs={} streams={} skipped={} dest={} verdict={}",
        r.txn_id,
        r.files,
        r.bytes,
        r.blocks_sent,
        r.packs,
        r.streams,
        r.files_skipped,
        if r.dest_dirs.is_empty() {
            "-".to_string()
        } else {
            r.dest_dirs.join(",")
        },
        r.verdict
    );
    // v0.17：失败清单单独一行，避免把 SEND 主行撑长；旧脚本匹配 verdict= 不受影响
    for f in &r.failed {
        println!(
            "FAILED path={} reason={}",
            f.path.as_deref().unwrap_or(&f.rel_path),
            f.reason
        );
    }
}

fn print_recv(r: &tc_core::ReceiverReport) {
    println!(
        "RECV txn={} files={} bytes={} written={} resumed={} streams={} verified={} interrupted={} skipped={}",
        r.txn_id,
        r.files,
        r.bytes,
        r.blocks_written,
        r.resumed_blocks,
        r.streams_used,
        r.verified,
        r.interrupted,
        r.skipped_files
    );
}

/// 历史落点：命令行显式给就听命令行的，否则走 TC_HOME / 平台默认目录。
fn history_dir_of(args: &[String]) -> PathBuf {
    opt(args, "--history-dir")
        .map(PathBuf::from)
        .unwrap_or_else(history::default_dir)
}

fn record_send(
    dir: &Path,
    id: &str,
    to: &str,
    paths: &[PathBuf],
    peer_fp: Option<&str>,
    files: u64,
    bytes: u64,
    blocks_sent: u64,
    verdict: String,
    reason: Option<String>,
    retry_of: Option<&str>,
    failed: Vec<history::FailedFile>,
) {
    let rec = history::SendRecord {
        id: id.into(),
        ts: queue::now_secs(),
        to: to.into(),
        paths: paths.to_vec(),
        peer_fp: peer_fp.map(str::to_string),
        files,
        bytes,
        blocks_sent,
        ok: verdict == "OK",
        verdict,
        reason,
        retry_of: retry_of.map(str::to_string),
        failed,
    };
    if let Err(e) = history::record(dir, &rec) {
        // 历史写失败不该把已经完成的传输改判成失败，只提示一句
        eprintln!("WARN: 传输历史写入失败: {e}");
    }
}

/// 发送一次并落历史。成功与失败都记一条，失败原因要能事后查，不能只留在终端里。
/// 返回 (结果, 本次历史记录 id)：自动重试要靠这个 id 把每次尝试串成链。
fn send_attempt(
    to: &str,
    paths: &[PathBuf],
    peer_fp: Option<&str>,
    pair_code: Option<&str>,
    identity: Option<&Identity>,
    history_dir: &Path,
    retry_of: Option<&str>,
    force: bool,
    dest_id: Option<&str>,
) -> (io::Result<tc_core::SendReport>, String) {
    let id = history::new_id();
    // 连握手阶段的错误也要落历史，所以把「建连接」和「跑会话」一起纳入同一条记录路径。
    // v0.12：force 只在发送端带标记，接收端据此不做增量跳过、并让同名旧文件改名让位。
    // v0.16：dest_id 是接收端公布的落点标识，空表示用它的默认落点。
    let options = tc_core::SendOptions {
        force,
        dest_id: dest_id.map(str::to_string),
    };
    let result = make_opener(to, peer_fp, identity)
        .and_then(|opener| tc_core::run_sender_with_opts(opener.as_ref(), paths, pair_code, options));
    match &result {
        Ok(r) => record_send(
            history_dir,
            &id,
            to,
            paths,
            peer_fp,
            r.files,
            r.bytes,
            r.blocks_sent,
            r.verdict.clone(),
            None,
            retry_of,
            r.failed.clone(),
        ),
        Err(e) => record_send(
            history_dir,
            &id,
            to,
            paths,
            peer_fp,
            0,
            0,
            0,
            "ERROR".into(),
            Some(e.to_string()),
            retry_of,
            Vec::new(),
        ),
    }
    (result, id)
}

/// 只关心结果、不需要重试链 id 的调用点（队列补发是它自己的重试机制）。
fn send_and_record(
    to: &str,
    paths: &[PathBuf],
    peer_fp: Option<&str>,
    pair_code: Option<&str>,
    identity: Option<&Identity>,
    history_dir: &Path,
    retry_of: Option<&str>,
    force: bool,
    dest_id: Option<&str>,
) -> io::Result<tc_core::SendReport> {
    send_attempt(
        to,
        paths,
        peer_fp,
        pair_code,
        identity,
        history_dir,
        retry_of,
        force,
        dest_id,
    )
    .0
}

/// 发送，失败时按 30s/2m/10m 自动补试（最多 3 次）。
/// `retry=false` 时行为与旧版完全一致——不加 flag 的命令脚本不该因为一次失败干等十几分钟。
/// 每次尝试都单独落一条历史，`retryOf` 指向上一次尝试，事后能看出「重试了几次、哪次成的」。
fn send_with_retry(
    to: &str,
    paths: &[PathBuf],
    peer_fp: Option<&str>,
    pair_code: Option<&str>,
    identity: Option<&Identity>,
    history_dir: &Path,
    retry: bool,
    retry_of: Option<&str>,
    force: bool,
    dest_id: Option<&str>,
) -> io::Result<tc_core::SendReport> {
    let delays = if retry { tc_core::retry::delays() } else { Vec::new() };
    let mut chain = retry_of.map(str::to_string);
    let mut attempt = 0usize;
    loop {
        let (result, rec_id) = send_attempt(
            to,
            paths,
            peer_fp,
            pair_code,
            identity,
            history_dir,
            chain.as_deref(),
            force,
            dest_id,
        );
        match result {
            Ok(r) => return Ok(r),
            Err(e) => {
                // 不可重试的错误（配对码、未信任、版本）立刻返回；次数用完也返回，但要留一句结论
                if attempt >= delays.len() || !tc_core::retry::is_retryable(&e) {
                    if retry && attempt > 0 {
                        println!(
                            "RETRY gave_up attempts={} class={} reason={}",
                            attempt + 1,
                            tc_core::retry::classify(&e).label_zh(),
                            e
                        );
                    }
                    return Err(e);
                }
                let wait = delays[attempt];
                println!(
                    "RETRY attempt={}/{} wait={}s reason={}",
                    attempt + 1,
                    delays.len(),
                    wait.as_secs(),
                    e
                );
                std::thread::sleep(wait);
                chain = Some(rec_id);
                attempt += 1;
            }
        }
    }
}


fn run(args: &[String]) -> io::Result<()> {
    match args.first().map(String::as_str).unwrap_or("help") {
        "version" => {
            println!("{}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        "identity" => {
            let dir = value(args, "--dir")?;
            let id = Identity::load_or_create(Path::new(&dir))?;
            println!("IDENTITY fingerprint={} dir={}", id.fingerprint_hex(), dir);
            Ok(())
        }
        "send" => {
            let to = value(args, "--to")?;
            let dirs = values(args, "--dir");
            if dirs.is_empty() {
                return Err(bad("send needs at least one --dir"));
            }
            let paths: Vec<PathBuf> = dirs.into_iter().map(PathBuf::from).collect();
            // 没给指纹先听一轮公告：接收端强制 TLS 时，明文连接只会得到一句 EOF 报错
            let peer_fp = match opt(args, "--peer-fp") {
                Some(f) => Some(f),
                None => match discover_fp_for(&to) {
                    Some((fp, name)) => {
                        // 明确告诉用户用的是哪个指纹，避免「悄悄换了连接方式」的观感
                        let short: String = fp.chars().take(16).collect();
                        eprintln!("提示: 未给 --peer-fp，改用发现到的「{name}」指纹 {short}… 走 TLS");
                        Some(fp)
                    }
                    None => None,
                },
            };
            let pair_code = opt(args, "--pair-code");
            let queue_dir = opt(args, "--queue-dir");
            let history_dir = history_dir_of(args);
            // 只有走 TLS 才需要本机证书；明文发送不该顺带生成身份文件
            let ident = if peer_fp.is_some() {
                Some(send_identity(args)?)
            } else {
                None
            };

            match send_with_retry(
                &to,
                &paths,
                peer_fp.as_deref(),
                pair_code.as_deref(),
                ident.as_ref().map(|(_, id)| id),
                &history_dir,
                args.iter().any(|a| a == "--retry"),
                None,
                args.iter().any(|a| a == "--force"),
                opt(args, "--dest-id").as_deref(),
            ) {
                Ok(r) => print_send(&r),
                Err(e) => {
                    // 对端只接受 TLS 时，明文连接会被掐断，这里只体现为 EOF；给一句能执行的话
                    if e.kind() == io::ErrorKind::UnexpectedEof && peer_fp.is_none() {
                        eprintln!(
                            "提示: 对端没有应答，可能只接受 TLS。先跑 tc discover 查对端指纹，再加 --peer-fp 重试"
                        );
                    }
                    if let Some(q) = queue_dir {
                        // 队列表示「这次没发出去、稍后补发」，历史里那条 ERROR 记录如实反映直接发送失败
                        let task = QueuedTask {
                            id: format!("{}-{}", queue::now_secs(), std::process::id()),
                            to: to.clone(),
                            paths,
                            created_at: queue::now_secs(),
                            fingerprint: peer_fp,
                            // 记下本次用的身份目录：补发时要用同一张证书，否则接收端会当成另一台设备
                            identity_dir: ident.as_ref().map(|(d, _)| d.clone()),
                            pair_code,
                            dest_id: opt(args, "--dest-id"),
                        };
                        let p = queue::save(Path::new(&q), &task)?;
                        println!("SEND QUEUED path={} reason={}", p.display(), e);
                    } else {
                        return Err(e);
                    }
                }
            }
            Ok(())
        }
        "dests" => {
            // v0.16：只查对端可选落点，不传文件。握手与正式发送一致，因此同样受信任门禁约束。
            let to = value(args, "--to")?;
            let peer_fp = match opt(args, "--peer-fp") {
                Some(f) => Some(f),
                None => discover_fp_for(&to).map(|(fp, _)| fp),
            };
            let pair_code = opt(args, "--pair-code");
            let ident = if peer_fp.is_some() {
                Some(send_identity(args)?)
            } else {
                None
            };
            let opener = make_opener(
                &to,
                peer_fp.as_deref(),
                ident.as_ref().map(|(_, id)| id),
            )?;
            let list = tc_core::list_destinations_for(opener.as_ref(), pair_code.as_deref())?;
            println!("DESTS count={}", list.len());
            for d in list {
                println!("DEST id={} name={}", d.id, d.name);
            }
            Ok(())
        }
        "history" => {
            let dir = history_dir_of(args);
            let limit = opt(args, "--limit")
                .and_then(|s| s.parse::<usize>().ok())
                .unwrap_or(history::DEFAULT_LIMIT);
            let list = history::list(&dir, limit)?;
            println!("HISTORY count={} dir={}", list.len(), dir.display());
            for r in list {
                let paths = r
                    .paths
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect::<Vec<_>>()
                    .join(";");
                println!(
                    "REC id={} ts={} ok={} verdict={} to={} peerFp={} files={} bytes={} retryOf={} paths={} reason={}",
                    r.id,
                    r.ts,
                    r.ok,
                    r.verdict,
                    r.to,
                    // 右键菜单靠它还原当时的 TLS 连接方式，不用再重新发现设备
                    r.peer_fp.as_deref().unwrap_or("-"),
                    r.files,
                    r.bytes,
                    r.retry_of.unwrap_or_else(|| "-".into()),
                    paths,
                    r.reason.unwrap_or_else(|| "-".into())
                );
            }
            Ok(())
        }
        "diag" => {
            // 配置 / 日志 / 历史一律「命令行 > 配置文件 > 平台默认」：
            // 同一条命令在 NAS、本机、安装目录下都能找到该导的东西
            let cfg_path = opt(args, "--config")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("config.toml"));
            let cfg = Config::load(&cfg_path)?;
            if cfg.is_none() {
                eprintln!("提示: 未读到 {}，只导出日志与历史", cfg_path.display());
            }
            let identity_dir = opt(args, "--identity")
                .map(PathBuf::from)
                .or_else(|| cfg.as_ref().map(|c| c.identity_dir.clone()))
                .or_else(|| trust::resolve_identity_dir(None, None).ok());
            let log_file = opt(args, "--log")
                .map(PathBuf::from)
                .or_else(|| cfg.as_ref().map(|c| c.log_dir.join("daemon.log")));
            let send_history = Some(
                opt(args, "--history-dir")
                    .map(PathBuf::from)
                    .unwrap_or_else(history::default_dir)
                    // --history-dir 是目录，导出要的是具体的 JSONL 文件
                    .join("send-history.jsonl"),
            );
            let recv_history = cfg.as_ref().map(|c| c.log_dir.join("history.jsonl"));
            let queue_dir = cfg.as_ref().map(|c| c.queue_dir.clone());
            let out_dir = opt(args, "--out")
                .map(PathBuf::from)
                .unwrap_or_else(tc_core::diag::default_out_dir);
            let opts = tc_core::diag::DiagOptions {
                out_dir,
                config: cfg,
                config_path: cfg_path.exists().then_some(cfg_path),
                log_file,
                send_history,
                recv_history,
                identity_dir,
                queue_dir,
            };
            // 只打印目录：内容里有路径与设备名，终端回显没有必要，用户直接把这个文件夹发出去
            println!("DIAG dir={}", tc_core::diag::run(&opts)?.display());
            Ok(())
        }
        "resend" => {
            let dir = history_dir_of(args);
            let key = opt(args, "--id").unwrap_or_else(|| "last".into());
            let rec = history::find(&dir, &key)?;
            // v0.17：--failed 只重发这条记录里判定失败的文件，不重跑整批
            let only_failed = args.iter().any(|a| a == "--failed");
            let resend_paths: Vec<PathBuf> = if only_failed {
                let paths: Vec<PathBuf> = rec
                    .failed
                    .iter()
                    .filter_map(|f| f.path.as_ref().map(PathBuf::from))
                    .collect();
                if paths.is_empty() {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        "这条历史没有可重发的失败文件（旧接收端可能未返回清单）",
                    ));
                }
                paths
            } else {
                rec.paths.clone()
            };
            // 历史不存配对码，对端仍要求配对时必须在这里现场给
            let pair_code = opt(args, "--pair-code");
            // 老记录可能没存指纹（当时走的是明文），重发时同样先听一轮公告补上，
            // 否则会对着强制 TLS 的对端重复踩同一个 EOF
            let peer_fp = match rec.peer_fp.clone() {
                Some(f) => Some(f),
                None => discover_fp_for(&rec.to).map(|(fp, _)| fp),
            };
            // 重发同样要带本机证书，否则 closed 模式的接收端会把重发当成陌生设备
            let ident = if peer_fp.is_some() {
                Some(send_identity(args)?)
            } else {
                None
            };
            let r = send_with_retry(
                &rec.to,
                &resend_paths,
                peer_fp.as_deref(),
                pair_code.as_deref(),
                ident.as_ref().map(|(_, id)| id),
                &dir,
                args.iter().any(|a| a == "--retry"),
                Some(&rec.id),
                args.iter().any(|a| a == "--force"),
                opt(args, "--dest-id").as_deref(),
            )?;
            print_send(&r);
            Ok(())
        }
        "recv" => {
            let listen = value(args, "--listen")?;
            let dest = value(args, "--dest")?;
            let pair_code = opt(args, "--pair-code");
            let root_map = parse_root_map(args);
            // TC_MAX_BLOCKS 只给端到端测试用：收够 N 块就停，模拟传一半断线
            let max = env::var("TC_MAX_BLOCKS")
                .ok()
                .and_then(|s| s.parse::<usize>().ok());
            // v0.15：并行流数。默认 4；测速对比时可 --streams 1 退回单流
            let streams = opt(args, "--streams")
                .and_then(|s| s.parse::<u8>().ok())
                .unwrap_or(tc_core::DEFAULT_STREAMS);
            let listener = TcpListener::bind(&listen)?;
            let port = listener.local_addr()?.port();

            // 身份只加载一次：公告线程和服务端证书共用，避免重复读盘/重复生成
            let identity_dir = opt(args, "--identity").map(PathBuf::from);
            let identity = match &identity_dir {
                Some(d) => Some(Identity::load_or_create(d)?),
                None => None,
            };

            if args.iter().any(|a| a == "--announce") {
                if let Some(id) = &identity {
                    let ann = discover::announcement_for(
                        local_device(),
                        id.fingerprint_hex(),
                        port,
                        env!("CARGO_PKG_VERSION"),
                    );
                    std::thread::spawn(move || {
                        discover::announce_forever(ann, Duration::from_secs(5))
                    });
                }
            }

            let r = match identity {
                Some(id) => {
                    let acc = TlsAccepter {
                        listener,
                        config: server_config(&id)?,
                    };
                    // TLS 接收必须过信任门禁；默认 closed，open 仅用于排障
                    let dir = identity_dir.expect("identity 已加载，目录必然存在");
                    let mode = TrustMode::parse(
                        &opt(args, "--trust-mode").unwrap_or_else(|| "closed".into()),
                    );
                    // 非法取值会被 parse 归到 closed，这里打印实际生效值，避免用户以为 open 生效了
                    eprintln!(
                        "TRUST mode={} dir={}",
                        mode.as_str(),
                        dir.display()
                    );
                    let gate = TrustGate::new(
                        Arc::new(TrustStore::new(&dir)),
                        mode,
                        pair_code.clone(),
                    );
                    run_receiver_with_gate_streams(
                        &acc,
                        Path::new(&dest),
                        max,
                        None,
                        &root_map,
                        Some(&gate),
                        streams,
                    )?
                }
                None => {
                    let acc = PlainAccepter { listener };
                    // 明文是排障入口，没有证书可判定；配对码仍按旧语义生效
                    run_receiver_with_streams(
                        &acc,
                        Path::new(&dest),
                        max,
                        pair_code.as_deref(),
                        &root_map,
                        streams,
                    )?
                }
            };
            print_recv(&r);
            Ok(())
        }
        "daemon" => {
            let cfg_path = PathBuf::from(value(args, "--config")?);
            let cfg = Config::load_or_create(&cfg_path)?;
            // v0.19：缓存文件位置要跟着配置文件走，先算好，后面 cfg_path 会交给 DaemonState
            let cache_path = cache_file_path(&cfg_path);
            // DaemonState::new 内部会校验白名单，这里不重复校验
            let state = Arc::new(DaemonState::new(cfg.clone(), cfg_path)?);
            // 调试开关（规范 #9/#10）：环境变量优先于配置，进程重启即生效，现场排障不必改文件。
            // 取值不会静默回落——写错时留一条 warn，否则用户会以为 debug 已经开了。
            if let Ok(v) = env::var("TC_LOG_LEVEL") {
                if state.set_log_level(&v) {
                    state.log("info", &format!("日志级别由 TC_LOG_LEVEL={} 覆盖", v.trim()));
                } else {
                    state.log(
                        "warn",
                        &format!(
                            "TC_LOG_LEVEL={} 非法，仍按 {} 记录（可选 error / warn / info / debug）",
                            v.trim(),
                            state.log_level()
                        ),
                    );
                }
            }
            // 崩溃现场三要素：时间 + 退出码 + 最后 20 行日志（规范 #13）
            tc_core::daemon::install_panic_hook(Arc::clone(&state));
            state.log(
                "debug",
                &format!(
                    "日志级别={}（配置 log_level={}，环境变量优先）",
                    state.log_level(),
                    cfg.log_level
                ),
            );
            let id = Identity::load_or_create(&cfg.identity_dir)?;
            let fp = id.fingerprint_hex();
            // 管理页令牌与身份同目录：文件不存在时生成一次，之后每次启动沿用同一令牌
            let manage_token = Arc::new(web::load_or_create_token(&cfg.identity_dir)?);

            // 管理页、发现广播、发现监听各占一个线程，主线程专心做 TLS 常驻接收
            {
                let st = state.clone();
                let addr = cfg.manage.clone();
                let f = fp.clone();
                let tk = manage_token.clone();
                thread::spawn(move || {
                    if let Err(e) = web::serve(st.clone(), &addr, f, tk) {
                        st.log("error", &format!("管理页退出: {e}"));
                    }
                });
            }
            {
                let ann = discover::announcement_for(
                    local_device(),
                    fp.clone(),
                    port_of(&cfg.listen),
                    env!("CARGO_PKG_VERSION"),
                );
                // 回执用同一份公告：收到别人的广播后单播回一份，解决多网卡/VPN 下广播只走默认路由的问题
                let ann_reply = ann.clone();
                let st = state.clone();
                let self_fp = fp.clone();
                thread::spawn(move || discover::announce_forever(ann, Duration::from_secs(5)));
                thread::spawn(move || {
                    // 每个来源 IP 2 秒内最多回执一次，只用来吸收重复广播
                    let mut replied: std::collections::HashMap<String, std::time::Instant> =
                        std::collections::HashMap::new();
                    loop {
                        // v0.13.1：收到即写进设备列表。旧实现要等 30 秒窗口结束才批量更新，
                        // 管理页打开时常常是空的，看起来像「发现不到设备」。
                        let r = discover::listen_each(Duration::from_secs(30), &self_fp, |p, from| {
                            // 只记设备名与地址；指纹属设备唯一标识，按规范 #14 不落原文
                            st.log("debug", &format!("发现设备 {} @ {}", p.name, p.addr));
                            st.upsert_peer(PeerInfo {
                                addr: p.addr.clone(),
                                name: p.name.clone(),
                                fingerprint: p.fingerprint.clone(),
                                last_seen: queue::now_secs(),
                            });
                            // v0.13.3：单播回执。广播能到我，说明这一跳可达；
                            // 我的广播未必能到它（多网卡/VPN 时只从默认路由出去）。
                            // 只对「广播」回执，对「回执」不再回，避免两台设备互相乒乓。
                            if p.reply {
                                return;
                            }
                            let key = from.ip().to_string();
                            let now = std::time::Instant::now();
                            let due = replied
                                .get(&key)
                                .map(|t| now.duration_since(*t).as_secs() >= 2)
                                .unwrap_or(true);
                            if due {
                                replied.insert(key, now);
                                discover::reply_to(from, &ann_reply);
                            }
                        });
                        if let Err(e) = r {
                            // 端口被占用这类错误会立即返回；不睡就变成每秒上万条 warn 的死循环。
                            // 2026-11-13 实测：同端口再起一个实例时，3 秒写出 1700+ 份轮转日志。
                            st.log("warn", &format!("发现监听失败: {e}"));
                            thread::sleep(Duration::from_secs(5));
                        }
                    }
                });
            }

            state.log(
                "info",
                &format!(
                    "守护进程启动 v{}，落点 {}",
                    env!("CARGO_PKG_VERSION"),
                    cfg.destination.display()
                ),
            );
            // v0.6 剪贴板中继：配置了 clip_listen 才起，缺省关闭（剪贴板里可能有密码）。
            // 与文件接收共用同一份身份与信任表，撤销立即生效；端口独立，不受文件会话并发数影响。
            if !cfg.clip_listen.trim().is_empty() {
                match TcpListener::bind(cfg.clip_listen.trim()) {
                    Ok(l) => {
                        let srv = tc_core::clip::ClipServer::new(
                            cfg.identity_dir.join("clip-history.jsonl"),
                            Some(Arc::clone(&state.trust)),
                        );
                        srv.warm_up(64);
                        let acc: Arc<dyn tc_core::transport::Accepter> = Arc::new(TlsAccepter {
                            listener: l,
                            config: server_config(&id)?,
                        });
                        let st2 = state.clone();
                        let addr = cfg.clip_listen.clone();
                        thread::spawn(move || {
                            st2.log("info", &format!("剪贴板中继已启动 {addr}"));
                            let stop = Arc::new(AtomicBool::new(false));
                            if let Err(e) = srv.serve(acc, stop) {
                                st2.log("error", &format!("剪贴板中继退出: {e}"));
                            }
                        });
                    }
                    Err(e) => state.log("error", &format!("剪贴板中继监听失败: {e}")),
                }
            }
            // 只记录令牌文件位置，不记录令牌本身，避免日志泄露凭据
            state.log(
                "info",
                &format!(
                    "管理页需要令牌，文件 {}；首次访问用 http://{}/?t=<令牌> 打开",
                    cfg.identity_dir.join("manage.token").display(),
                    cfg.manage
                ),
            );
            // 信任策略要让用户一眼看到现状：open 属于临时放开，必须 warn；
            // closed 且表为空时提前提示三种放行方式，免得 P2 上线后「谁都发不进来」却查不到原因。
            let trust_count = state.trust.list().map(|v| v.len()).unwrap_or(0);
            let trust_mode = TrustMode::parse(&cfg.trust_mode);
            if !["closed", "open"].contains(&cfg.trust_mode.as_str()) {
                // 配错不致命，但必须留痕：实际按 closed 生效，别让用户以为已经放开
                state.log(
                    "warn",
                    &format!(
                        "trust_mode={} 非法，已按 closed 运行（只允许 closed / open）",
                        cfg.trust_mode
                    ),
                );
            }
            if trust_mode == TrustMode::Open {
                state.log(
                    "warn",
                    "信任模式 open：未信任设备也可发送，仅用于排障/老设备过渡，用完尽快切回 closed",
                );
            } else if trust_count == 0 {
                state.log(
                    "warn",
                    "信任模式 closed 且信任表为空：需先用 tc trust approve、管理页批准，或配置非空 pair_code",
                );
            }
            let gate = Arc::new(
                TrustGate::new(
                    Arc::clone(&state.trust),
                    trust_mode,
                    cfg.pair_code.clone(),
                )
                // v0.20：管理页生成的二维码令牌与门禁共用同一份，作废后立刻失效
                .with_pair_tokens(Arc::clone(&state.pair)),
            );
            let listener = TcpListener::bind(&cfg.listen)?;
            let acc = TlsAccepter {
                listener,
                config: server_config(&id)?,
            };
            // 并发接多台设备：每场会话独立线程，数据连接靠 txnId 找回自己的会话，
            // 不再按 accept 顺序关联（那会让第二台设备的控制连接被当成第一台的数据连接）
            // v0.19：哈希缓存默认关；--verify 是本次运行的强制重算开关，优先于配置，
            // 出问题时用它启动一次就能分清「跳过」来自缓存还是来自重新比对。
            let force_rehash = args.iter().any(|a| a == "--verify");
            let hash_cache = if cfg.hash_cache && !force_rehash {
                let cache = Arc::new(tc_core::HashCache::load(&cache_path));
                state.log(
                    "info",
                    &format!(
                        "哈希缓存已启用（{} 条）：{}",
                        cache.len(),
                        cache_path.display()
                    ),
                );
                Some(cache)
            } else {
                if cfg.hash_cache {
                    state.log("info", "--verify：本次运行忽略哈希缓存，已有文件的指纹全部重算");
                }
                None
            };
            let hub = Arc::new(
                ReceiverHub::new(
                    Arc::new(acc),
                    cfg.destination.clone(),
                    cfg.pair_code.clone(),
                    HashMap::new(),
                    cfg.max_sessions,
                )
                // 与 CLI / 管理页共用同一个 TrustStore 实例，撤销后下一场会话立即生效
                .with_gate(gate)
                // v0.15：并行流数来自配置，1 = 关闭
                .with_streams(cfg.streams as u8)
                // v0.19：None 时与旧版行为一致
                .with_hash_cache(hash_cache)
                // v0.16：可被发送方选择的落点；空表示只公布默认落点
                .with_destinations(
                    cfg.destinations
                        .iter()
                        .map(|d| tc_core::Destination {
                            id: d.id.clone(),
                            name: d.name.clone(),
                            path: d.path.clone(),
                        })
                        .collect(),
                ),
            );
            let st = state.clone();
            let on_report: Arc<dyn Fn(HubReport) + Send + Sync> = Arc::new(move |report| {
                match report.result {
                    Ok(r) => {
                        // v0.16：只查落点的会话不是传输，不写历史也不刷日志
                        if r.query_only {
                            return;
                        }
                        // 来源设备与落点一并落历史：事后要能回答「谁发来的、存哪了」
                        let dev = r.device.clone();
                        st.add_history(HistoryEntry {
                            ts: queue::now_secs(),
                            txn_id: r.txn_id.clone(),
                            files: r.files,
                            bytes: r.bytes,
                            verified: r.verified,
                            detail: if r.interrupted { "中断待续" } else { "完成" }.into(),
                            peer_name: dev.as_ref().map(|d| d.name.clone()).unwrap_or_default(),
                            peer_id: dev.as_ref().map(|d| d.id.clone()).unwrap_or_default(),
                            peer_platform: dev
                                .as_ref()
                                .map(|d| d.platform.clone())
                                .unwrap_or_default(),
                            peer_addr: r.peer_addr.clone().unwrap_or_default(),
                            roots: r.roots.clone(),
                        });
                        st.log(
                            "info",
                            &format!(
                                "会话 {} 结束 verified={} interrupted={} 来源={} 落点={} 块={} 已续传={} 增量跳过={} 等数据={}ms 写盘={}ms 发请求={}ms 校验={}ms",
                                r.txn_id,
                                r.verified,
                                r.interrupted,
                                dev.as_ref().map(|d| d.name.as_str()).unwrap_or("-"),
                                r.roots.join(","),
                                r.blocks_written,
                                // 续传块单列出来：块数与清单对不上时，只有这个字段能说明差额来自哪
                                r.resumed_blocks,
                                // 增量同步省掉的文件数：日志里要能回答「这次为什么很快就传完了」
                                r.skipped_files,
                                // P0 打点：各段时间换算成毫秒，便于直接对比每块耗时的来源
                                r.wait_us / 1000,
                                r.write_us / 1000,
                                r.req_us / 1000,
                                r.verify_us / 1000
                            ),
                        );
                    }
                    // 并发已满、落点冲突这类拒绝也留痕，用户能看出「有人被挡了」而不是凭空失败
                    Err(e) => st.log(
                        "warn",
                        &format!(
                            "会话 {} 异常（来源 {}）: {e}",
                            report.txn_id,
                            report.peer_addr.as_deref().unwrap_or("-")
                        ),
                    ),
                }
            });
            let mut consecutive_errors = 0u32;
            loop {
                // serve 只在 accept 失败时返回，属于端口/证书级故障，连续多次才停服
                match hub.serve(on_report.clone()) {
                    Ok(()) => break,
                    Err(e) => {
                        consecutive_errors += 1;
                        state.log("warn", &format!("接收入口异常: {e}"));
                        if consecutive_errors >= 20 {
                            state.log("error", "连续异常过多，停止接收");
                            break;
                        }
                        thread::sleep(Duration::from_millis(200));
                    }
                }
            }
            Ok(())
        }
        "discover" => {
            let timeout = opt(args, "--timeout")
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(3);
            let self_fp = match opt(args, "--identity") {
                Some(d) => Identity::load_or_create(Path::new(&d))?.fingerprint_hex(),
                None => String::new(),
            };
            let peers = discover::listen(Duration::from_secs(timeout), &self_fp)?;
            println!("DISCOVER count={}", peers.len());
            for p in peers {
                println!(
                    "PEER addr={} fp={} name={} platform={}",
                    p.addr, p.fingerprint, p.name, p.platform
                );
            }
            Ok(())
        }
        "queue" => {
            let dir = value(args, "--dir")?;
            let window = opt(args, "--window")
                .and_then(|s| s.parse::<i64>().ok())
                .unwrap_or(DEFAULT_WINDOW_MINUTES);
            let list = queue::pending(Path::new(&dir), window)?;
            println!("QUEUE count={} window={}m", list.len(), window);
            for (p, t) in list {
                println!(
                    "TASK file={} to={} age={}s",
                    p.display(),
                    t.to,
                    queue::now_secs() - t.created_at
                );
            }
            Ok(())
        }
        "flush" => {
            let dir = value(args, "--dir")?;
            let window = opt(args, "--window")
                .and_then(|s| s.parse::<i64>().ok())
                .unwrap_or(DEFAULT_WINDOW_MINUTES);
            let history_dir = history_dir_of(args);
            let mut done = 0usize;
            let mut skipped = 0usize;
            for (p, t) in queue::pending(Path::new(&dir), window)? {
                // 补发沿用入队时的身份；旧队列没有记目录时退回当前解析结果
                let ident = match (&t.identity_dir, t.fingerprint.is_some()) {
                    (Some(d), _) => Some(Identity::load_or_create(d)?),
                    (None, true) => Some(send_identity(args)?.1),
                    _ => None,
                };
                // 队列补发也进历史：用户查「昨天那条到底补发成功没有」不能只靠终端
                match send_and_record(
                    &t.to,
                    &t.paths,
                    t.fingerprint.as_deref(),
                    t.pair_code.as_deref(),
                    ident.as_ref(),
                    &history_dir,
                    None,
                    // 队列补发是「补上没发出去的」，不带强制重传：否则每次补发都另存一份
                    false,
                    // 入队时选的落点要一起补发，否则补发会落到默认目录
                    t.dest_id.as_deref(),
                ) {
                    Ok(r) => {
                        queue::mark_done(&p)?;
                        done += 1;
                        println!("FLUSH OK txn={} verdict={}", r.txn_id, r.verdict);
                    }
                    Err(e) => {
                        skipped += 1;
                        println!("FLUSH SKIP reason={}", e);
                    }
                }
            }
            println!("FLUSH done={} skipped={}", done, skipped);
            Ok(())
        }
        "trust" => {
            trust_cmd(args)?;
            Ok(())
        }
        "clip" => {
            clip_cmd(args)?;
            Ok(())
        }
        _ => {
            println!("usage:");
            println!("  tc version");
            println!("  tc identity --dir DIR");
            println!("  tc send --to IP:PORT --dir PATH [--dir PATH...] [--peer-fp HEX] [--pair-code CODE] [--identity DIR] [--queue-dir DIR] [--history-dir DIR] [--retry] [--force] [--dest-id ID]");
            println!("  tc dests --to IP:PORT [--peer-fp HEX] [--pair-code CODE] [--identity DIR]");
            println!("  tc history [--dir DIR] [--limit N]");
            println!("  tc resend [--id ID|last] [--failed] [--history-dir DIR] [--pair-code CODE] [--identity DIR] [--retry] [--force] [--dest-id ID]");
            println!("  tc diag [--config FILE] [--identity DIR] [--out DIR] [--log FILE] [--history-dir DIR]");
            println!("  tc recv --listen IP:PORT --dest PATH [--identity DIR] [--pair-code CODE] [--trust-mode closed|open] [--root-map r0=NAME] [--streams N] [--announce]");
            println!("  tc daemon --config FILE [--verify]");
            println!("  tc discover [--timeout SEC] [--identity DIR]");
            println!("  tc queue --dir DIR [--window MIN]");
            println!("  tc flush --dir DIR [--window MIN] [--history-dir DIR]");
            println!("  tc trust list|pending|approve FP|revoke FP|rename FP --alias NAME [--identity DIR | --config FILE]");
            println!("  tc clip serve --listen IP:PORT [--identity DIR] [--history FILE] [--open]");
            println!("  tc clip push --to IP:PORT (--text TEXT | --stdin) [--peer-fp HEX] [--identity DIR] [--sensitive]");
            println!("  tc clip status --to IP:PORT [--peer-fp HEX] [--identity DIR]");
            println!("  tc clip watch --to IP:PORT [--peer-fp HEX] [--identity DIR] [--apply] [--interval MS]");
            println!("  tc clip pause --minutes N | tc clip resume | tc clip history [--limit N]");
            Ok(())
        }
    }
}

/// 剪贴板内核的命令入口。子命令与 `功能展望-版本路线.md` §3 一一对应：
/// serve（星型主设备）、push（推一条）、watch（订阅并可选写回剪贴板）、status、pause/resume。
fn clip_cmd(args: &[String]) -> io::Result<()> {
    let sub = args.get(1).map(|s| s.as_str()).unwrap_or("");
    match sub {
        "serve" => {
            let listen = opt(args, "--listen")
                .unwrap_or_else(|| format!("0.0.0.0:{}", tc_core::clip::DEFAULT_CLIP_PORT));
            let (dir, id) = send_identity(args)?;
            let history = opt(args, "--history")
                .map(PathBuf::from)
                .unwrap_or_else(|| dir.join("clip-history.jsonl"));
            let listener = TcpListener::bind(&listen)?;
            // --open 只用于本机回环与自动化：此时既不开 TLS 也不查信任表，
            // 否则明文客户端会把 TLS 握手字节当成帧长度，报「message too large」。
            let open = args.iter().any(|a| a == "--open");
            let trust = if open {
                None
            } else {
                Some(Arc::new(TrustStore::new(&dir)))
            };
            let server = tc_core::clip::ClipServer::new(history.clone(), trust);
            server.warm_up(64);
            let acc: Arc<dyn tc_core::transport::Accepter> = if open {
                Arc::new(PlainAccepter { listener })
            } else {
                Arc::new(TlsAccepter {
                    listener,
                    config: server_config(&id)?,
                })
            };
            println!(
                "CLIP serve listen={} history={} trust={}",
                listen,
                history.display(),
                if open { "open" } else { "closed" }
            );
            let stop = Arc::new(AtomicBool::new(false));
            server.serve(acc, stop)
        }
        "push" => {
            let to = opt(args, "--to")
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "缺少 --to"))?;
            let (_, id) = send_identity(args)?;
            let opener = make_opener(&to, opt(args, "--peer-fp").as_deref(), Some(&id))?;
            let text = match opt(args, "--text") {
                Some(t) => t,
                None => {
                    let mut s = String::new();
                    io::Read::read_to_string(&mut io::stdin(), &mut s)?;
                    s
                }
            };
            if text.is_empty() {
                return Err(io::Error::new(io::ErrorKind::InvalidInput, "内容为空"));
            }
            let ev = tc_core::clip::push_once(
                opener.as_ref(),
                local_device(),
                &origin_of(args),
                &text,
                args.iter().any(|a| a == "--sensitive"),
            )?;
            println!(
                "CLIP push id={} size={} hash={}",
                ev.id,
                ev.size,
                &ev.hash[..16.min(ev.hash.len())]
            );
            Ok(())
        }
        "status" => {
            let to = opt(args, "--to")
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "缺少 --to"))?;
            let (_, id) = send_identity(args)?;
            let opener = make_opener(&to, opt(args, "--peer-fp").as_deref(), Some(&id))?;
            let mut conn =
                tc_core::clip::ClipClient::connect(opener.as_ref(), local_device(), &origin_of(args))?;
            conn.send(&tc_core::clip::ClipMsg::Status)?;
            match conn.recv()? {
                tc_core::clip::ClipMsg::StatusReply {
                    clients,
                    lamport,
                    latest,
                } => {
                    let desc = latest
                        .map(|e| format!("{} {} size={}", e.id, e.preview, e.size))
                        .unwrap_or_else(|| "-".into());
                    println!("CLIP status clients={clients} lamport={lamport} latest={desc}");
                    Ok(())
                }
                tc_core::clip::ClipMsg::Deny { reason } => Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    reason,
                )),
                other => Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("unexpected status reply: {other:?}"),
                )),
            }
        }
        "watch" => {
            let to = opt(args, "--to")
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "缺少 --to"))?;
            let (_, id) = send_identity(args)?;
            let opener = make_opener(&to, opt(args, "--peer-fp").as_deref(), Some(&id))?;
            let origin = origin_of(args);
            let apply = args.iter().any(|a| a == "--apply");
            let interval = opt(args, "--interval")
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(700);
            let pause_path = tc_core::clip::default_pause_path();
            let mut conn =
                tc_core::clip::ClipClient::connect(opener.as_ref(), local_device(), &origin)?;
            println!("CLIP watch origin={origin} apply={apply} loop=1");
            let mut since = 0u64;
            loop {
                if let Some(until) = tc_core::clip::paused_until(&pause_path) {
                    // 暂停期间不取事件，但保持连接：恢复后按 lamport 继续
                    println!("CLIP paused until {until}");
                    thread::sleep(Duration::from_millis(interval.max(1000)));
                    continue;
                }
                let (ev, text, lamport) = tc_core::clip::pull(&mut conn, since)?;
                if let Some(ev) = ev {
                    since = since.max(ev.lamport);
                    println!(
                        "CLIP event id={} from={} size={} sensitive={} preview={}",
                        ev.id, ev.origin, ev.size, ev.sensitive, ev.preview
                    );
                    if let Some(t) = text {
                        // 自己发的、带敏感标记的一律不自动写回，避免把密码贴到别人剪贴板
                        if apply && !ev.sensitive && ev.origin != origin {
                            if let Err(e) = tc_core::clip::apply_to_clipboard(&t) {
                                eprintln!("CLIP apply failed: {e}");
                            }
                        }
                    }
                } else if lamport > since {
                    since = lamport;
                }
                thread::sleep(Duration::from_millis(interval));
            }
        }
        "pause" => {
            let minutes = opt(args, "--minutes")
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(15);
            let path = tc_core::clip::default_pause_path();
            let until = tc_core::clip::set_pause(&path, minutes)?;
            println!("CLIP pause until={until} file={}", path.display());
            Ok(())
        }
        "resume" => {
            let path = tc_core::clip::default_pause_path();
            tc_core::clip::clear_pause(&path)?;
            println!("CLIP resume file={}", path.display());
            Ok(())
        }
        "history" => {
            let (dir, _) = send_identity(args)?;
            let file = opt(args, "--history")
                .map(PathBuf::from)
                .unwrap_or_else(|| dir.join("clip-history.jsonl"));
            let limit = opt(args, "--limit")
                .and_then(|s| s.parse::<usize>().ok())
                .unwrap_or(10);
            let events = tc_core::clip::read_history(&file, limit)?;
            println!("CLIP history count={} file={}", events.len(), file.display());
            for e in events {
                println!(
                    "  {} {} lamport={} size={} sensitive={} {}",
                    e.id, e.origin, e.lamport, e.size, e.sensitive, e.preview
                );
            }
            Ok(())
        }
        _ => {
            println!("usage: tc clip serve|push|status|watch|pause|resume|history ...");
            Ok(())
        }
    }
}

/// 本机在剪贴板事件里的名字：默认用设备名，可用 --origin 覆盖（同一台机器跑两个日志用）。
fn origin_of(args: &[String]) -> String {
    opt(args, "--origin").unwrap_or_else(|| local_device().name)
}

#[cfg(test)]
mod tests {
    use super::endpoint_host;

    /// 发现报文里的地址与用户手填的地址可能只有端口不同，比对时只看主机部分。
    #[test]
    fn endpoint_host_strips_port_only() {
        assert_eq!(endpoint_host("203.0.113.10:53318"), "203.0.113.10");
        assert_eq!(endpoint_host("203.0.113.10"), "203.0.113.10");
        // IPv6 方括号形式：取最后一个冒号之前，方括号保留原样
        assert_eq!(endpoint_host("[::1]:53318"), "[::1]");
    }
}
