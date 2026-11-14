//! v0.3 守护进程的共享状态：TOML 配置、日志、历史、当前会话、已发现设备。
//! 日志和历史都只按时间过滤展示，旧文件/旧行一律保留不删（遵守「不彻底删除任何文件」）。

use crate::queue::now_secs;
use crate::trust::TrustStore;
use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};

pub const LOG_WINDOW_SECS: i64 = 2 * 24 * 3600;

/// 日志级别与数值的对应：数值越大越啰嗦，写盘判据是「行的级别 <= 当前级别」。
/// 只认这四个值，写错一律按 info 处理（见 DaemonState::new 的告警）。
pub fn log_level_rank(level: &str) -> Option<u8> {
    match level.trim().to_ascii_lowercase().as_str() {
        "error" => Some(0),
        "warn" => Some(1),
        "info" => Some(2),
        "debug" => Some(3),
        _ => None,
    }
}

/// 数值转回可读名字；给启动日志与告警用，避免两处各写一份映射。
pub fn log_level_name(rank: u8) -> &'static str {
    match rank {
        0 => "error",
        1 => "warn",
        3 => "debug",
        _ => "info",
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub alias: String,
    /// TLS 传输监听地址
    pub listen: String,
    /// 管理页监听地址
    pub manage: String,
    pub destination: PathBuf,
    pub identity_dir: PathBuf,
    /// 允许写入的目录白名单；destination 必须在其中，否则拒绝启动
    pub allow: Vec<PathBuf>,
    pub pair_code: Option<String>,
    pub auto_accept: bool,
    /// 信任策略：closed（默认，只放行信任表内的设备）| open（排障/老设备过渡，切换必须留日志）
    pub trust_mode: String,
    /// 同时处理的会话上限。超出上限的接入会立刻收到 BUSY，而不是排队挂到超时。
    pub max_sessions: usize,
    pub log_dir: PathBuf,
    pub log_days: i64,
    /// 日志级别：error | warn | info（默认） | debug。
    /// 排障时不必改文件，用环境变量 TC_LOG_LEVEL 临时覆盖，进程重启即生效。
    pub log_level: String,
    /// 单个日志文件的大小上限（字节）。超过就把 daemon.log 改名归档、另起一份；0 = 不轮转。
    /// 只改名不删除（规范 #21），归档文件的清理由人工按批次列清单确认。
    pub log_max_bytes: u64,
    pub queue_dir: PathBuf,
    /// 剪贴板中继监听地址（v0.6）。空串=不启动：剪贴板里可能有密码，默认关闭。
    pub clip_listen: String,
}

impl Default for Config {
    fn default() -> Self {
        let base = PathBuf::from(".");
        Self {
            alias: crate::manifest::local_device().name,
            listen: "0.0.0.0:53318".into(),
            manage: "0.0.0.0:53319".into(),
            destination: base.join("received"),
            identity_dir: base.join("identity"),
            allow: vec![base.join("received")],
            pair_code: None,
            auto_accept: true,
            // 默认拒绝陌生设备：宁可让用户手动批准一次，也不默认放开落盘入口
            trust_mode: "closed".into(),
            // 家用局域网同时投递的通常是两三台设备，4 足够；再多也只是多几个线程
            max_sessions: 4,
            log_dir: base.join("logs"),
            log_days: 2,
            // 默认 info：日常排障够用；debug 会记录每条管理页请求，只在查问题时开
            log_level: "info".into(),
            // 4 MiB：按单条约 150 字节估算约 2.7 万行，家用场景下大约几周一份，够回溯又不占盘
            log_max_bytes: 4 * 1024 * 1024,
            queue_dir: base.join("queue"),
            clip_listen: String::new(),
        }
    }
}

impl Config {
    /// 文件不存在时生成默认配置；已存在则只读，避免覆盖用户改过的设置。
    pub fn load_or_create(path: &Path) -> io::Result<Self> {
        if path.exists() {
            let text = fs::read_to_string(path)?;
            return toml::from_str(&text).map_err(|e| io::Error::other(e.to_string()));
        }
        let cfg = Self::default();
        cfg.save(path)?;
        Ok(cfg)
    }

    /// 只读已存在的配置：给 CLI 解析 identity_dir 用。
    /// 不能用 load_or_create——工具命令不该在用户当前目录里顺手新建一个 config.toml。
    pub fn load(path: &Path) -> io::Result<Option<Self>> {
        match fs::read_to_string(path) {
            Ok(text) => toml::from_str(&text)
                .map(Some)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string())),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    pub fn save(&self, path: &Path) -> io::Result<()> {
        if let Some(p) = path.parent() {
            fs::create_dir_all(p)?;
        }
        let text = toml::to_string_pretty(self).map_err(|e| io::Error::other(e.to_string()))?;
        fs::write(path, text)
    }

    /// 落点必须在白名单内；这是 NAS 上防止「变成任意写盘入口」的关键一道闸。
    pub fn validate(&self) -> io::Result<()> {
        if self.allow.is_empty() {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "allow list is empty"));
        }
        let dest = normalize(&self.destination);
        let allowed = self.allow.iter().any(|a| {
            let a = normalize(a);
            dest == a || dest.starts_with(&a)
        });
        if !allowed {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("destination {} is outside the allow list", self.destination.display()),
            ));
        }
        Ok(())
    }
}

fn normalize(p: &Path) -> PathBuf {
    // 只做词法归一，不触碰文件系统；相对路径统一按当前目录展开
    let abs = if p.is_absolute() {
        p.to_path_buf()
    } else {
        std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")).join(p)
    };
    let mut out = PathBuf::new();
    for c in abs.components() {
        match c {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub ts: i64,
    pub txn_id: String,
    pub files: u64,
    pub bytes: u64,
    pub verified: bool,
    pub detail: String,
    // 以下几项是 v0.5.3 新增（来源设备与落点）。
    // 旧 history.jsonl 的行没有这些键，所以逐个 #[serde(default)]：
    // 少了就按空值读，绝不能因为多了字段就把旧历史整行丢掉。
    #[serde(default)]
    pub peer_name: String,
    #[serde(default)]
    pub peer_id: String,
    #[serde(default)]
    pub peer_platform: String,
    #[serde(default)]
    pub peer_addr: String,
    /// 实际落点的根目录名（相对接收目录），不记录每个文件，避免历史行随文件数膨胀
    #[serde(default)]
    pub roots: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PeerInfo {
    pub addr: String,
    pub name: String,
    pub fingerprint: String,
    pub last_seen: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct CurrentTransfer {
    pub txn_id: String,
    pub files: u64,
    pub bytes: u64,
    pub started_at: i64,
}

/// 管理页发起的一次发送任务。
/// 只保留最近一条：页面同一时间只允许一个发送任务，
/// 两个任务并行会把进度和「谁在发」搅在一起，而家用场景没有并发的必要。
#[derive(Debug, Clone, Serialize)]
pub struct SendJob {
    pub target: String,
    pub path: String,
    /// scanning（扫描/算哈希，没有总字节）| sending | done | failed
    pub phase: String,
    pub percent: u32,
    pub files: u64,
    pub bytes_sent: u64,
    pub total_bytes: u64,
    pub blocks_sent: u64,
    pub total_blocks: u64,
    pub current: String,
    pub speed_bps: u64,
    /// 接收端按「同路径同内容」跳过的文件数（v0.9 增量同步）
    pub files_skipped: u64,
    pub ok: bool,
    pub reason: Option<String>,
    pub started_at: i64,
    pub finished_at: Option<i64>,
}

impl SendJob {
    pub fn running(&self) -> bool {
        self.phase == "scanning" || self.phase == "sending"
    }
}

pub struct DaemonState {
    pub config: Mutex<Config>,
    pub config_path: PathBuf,
    pub log_path: PathBuf,
    pub history_path: PathBuf,
    /// 管理页与 CLI 共用同一个文件路径，不各自维护副本，状态天然一致。
    /// Arc 是为了把同一个实例交给并发会话的信任门禁，避免门禁另开一份缓存。
    pub trust: Arc<TrustStore>,
    pub started_at: i64,
    pub current: Mutex<Option<CurrentTransfer>>,
    pub peers: Mutex<Vec<PeerInfo>>,
    pub log_lines: Mutex<Vec<(i64, String, String)>>,
    pub history: Mutex<Vec<HistoryEntry>>,
    /// 管理页的发送任务（最近一次）。页面按它渲染进度与失败原因。
    pub send_job: Mutex<Option<SendJob>>,
    /// 当前生效的日志级别（log_level_rank 的数值）。
    /// 用原子量而不是每次读 config 锁：log() 会在若干已持锁的路径里被调用，
    /// 再取 config 锁有自锁风险。
    log_level: AtomicU8,
    /// 日志文件大小上限，同样用原子量避免在 log() 里取 config 锁；0 = 不轮转
    log_max_bytes: AtomicU64,
}

impl DaemonState {
    pub fn new(config: Config, config_path: PathBuf) -> io::Result<Self> {
        config.validate()?;
        fs::create_dir_all(&config.destination)?;
        fs::create_dir_all(&config.log_dir)?;
        fs::create_dir_all(&config.queue_dir)?;
        let log_path = config.log_dir.join("daemon.log");
        let history_path = config.log_dir.join("history.jsonl");
        let trust = Arc::new(TrustStore::new(&config.identity_dir));
        // 配置里的级别在这里定一次；非法取值立刻写一条 warn 说明实际按 info 运行，
        // 与 trust_mode 的处理一致——悄悄回落会让人以为开关生效了。
        let configured = config.log_level.clone();
        let rank = log_level_rank(&configured);
        let max_bytes = config.log_max_bytes;
        let state = Self {
            config: Mutex::new(config),
            config_path,
            log_path,
            history_path,
            trust,
            started_at: now_secs(),
            current: Mutex::new(None),
            peers: Mutex::new(Vec::new()),
            log_lines: Mutex::new(Vec::new()),
            history: Mutex::new(Vec::new()),
            send_job: Mutex::new(None),
            log_level: AtomicU8::new(rank.unwrap_or(2)),
            log_max_bytes: AtomicU64::new(max_bytes),
        };
        if rank.is_none() {
            state.log(
                "warn",
                &format!("log_level={configured} 非法，已按 info 记录（可选 error / warn / info / debug）"),
            );
        }
        Ok(state)
    }

    /// 切换级别，供环境变量覆盖与后续运行期调整使用。
    /// 非法取值不改动当前级别并返回 false，由调用方决定怎么提示。
    pub fn set_log_level(&self, level: &str) -> bool {
        match log_level_rank(level) {
            Some(rank) => {
                self.log_level.store(rank, Ordering::Relaxed);
                true
            }
            None => false,
        }
    }

    pub fn log_level(&self) -> &'static str {
        log_level_name(self.log_level.load(Ordering::Relaxed))
    }

    pub fn log(&self, level: &str, msg: &str) {
        // 低于当前级别的行直接丢：内存与磁盘都不留，免得「调试关了日志还在涨」
        let rank = log_level_rank(level).unwrap_or(2);
        if rank > self.log_level.load(Ordering::Relaxed) {
            return;
        }
        let ts = now_secs();
        {
            let mut lines = self.log_lines.lock().expect("log_lines poisoned");
            lines.push((ts, level.into(), msg.into()));
            // 内存里只保留窗口内的行，磁盘上的旧行不删
            let cutoff = ts - LOG_WINDOW_SECS;
            lines.retain(|(t, _, _)| *t >= cutoff);
        }
        if let Ok(mut f) = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.log_path)
        {
            let _ = writeln!(f, "{ts}\t{level}\t{msg}");
        }
        // 写完再判断大小：轮转点落在整行之后，归档文件不会切出半行
        self.rotate_if_needed();
    }

    /// 超过大小上限就把 daemon.log 改名成 daemon_<YYYYMMDD-HHmm>.log，再新建一份继续写。
    /// 只改名不删除（规范 #21）；同一分钟内重复触发时文件名追加序号，不覆盖既有归档。
    fn rotate_if_needed(&self) {
        let limit = self.log_max_bytes.load(Ordering::Relaxed);
        if limit == 0 {
            return;
        }
        let size = fs::metadata(&self.log_path).map(|m| m.len()).unwrap_or(0);
        if size <= limit {
            return;
        }
        let stamp = local_stamp();
        let mut target = self.log_path.with_file_name(format!("daemon_{stamp}.log"));
        let mut n = 2;
        while target.exists() {
            target = self.log_path.with_file_name(format!("daemon_{stamp}-{n}.log"));
            n += 1;
        }
        // 改名失败（占用/权限）就不硬来：宁可继续写大文件，也不能把日志写丢
        if fs::rename(&self.log_path, &target).is_err() {
            return;
        }
        // 新文件开头留一行去向说明，免得看日志时误以为中间断档
        if let Some(name) = target.file_name().and_then(|n| n.to_str()) {
            if let Ok(mut f) = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.log_path)
            {
                let _ = writeln!(
                    f,
                    "{}\tinfo\t日志轮转：上一份归档为 {name}（上限 {limit} 字节）",
                    now_secs()
                );
            }
        }
    }

    pub fn recent_logs(&self) -> Vec<(i64, String, String)> {
        let cutoff = now_secs() - LOG_WINDOW_SECS;
        // 优先读磁盘，这样进程重启后日志仍在（2 天窗口）
        let mut out = Vec::new();
        if let Ok(text) = fs::read_to_string(&self.log_path) {
            for line in text.lines() {
                let mut it = line.splitn(3, '\t');
                let ts = it.next().and_then(|s| s.parse::<i64>().ok()).unwrap_or(0);
                if ts < cutoff {
                    continue;
                }
                let level = it.next().unwrap_or("info").to_string();
                let msg = it.next().unwrap_or("").to_string();
                out.push((ts, level, msg));
            }
        }
        out
    }

    pub fn add_history(&self, entry: HistoryEntry) {
        self.history.lock().expect("history poisoned").push(entry.clone());
        if let Ok(mut f) = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.history_path)
        {
            if let Ok(line) = serde_json::to_string(&entry) {
                let _ = writeln!(f, "{line}");
            }
        }
    }

    pub fn recent_history(&self) -> Vec<HistoryEntry> {
        let cutoff = now_secs() - LOG_WINDOW_SECS;
        if let Ok(text) = fs::read_to_string(&self.history_path) {
            return text
                .lines()
                .filter_map(|l| serde_json::from_str::<HistoryEntry>(l).ok())
                .filter(|e| e.ts >= cutoff)
                .collect();
        }
        self.history
            .lock()
            .expect("history poisoned")
            .iter()
            .filter(|e| e.ts >= cutoff)
            .cloned()
            .collect()
    }

    pub fn upsert_peer(&self, peer: PeerInfo) {
        let mut peers = self.peers.lock().expect("peers poisoned");
        if let Some(p) = peers.iter_mut().find(|p| p.addr == peer.addr) {
            *p = peer;
        } else {
            peers.push(peer);
        }
    }
}

/// 崩溃现场留痕（AGENTS #13：时间 + 退出码 + 最后 20 行日志）。
/// 直接读磁盘尾部而不查内存缓冲：panic 可能正好发生在持有 log_lines 锁的路径上，
/// 再去取锁会二次 panic，现场就什么都留不下了。
pub fn install_panic_hook(state: Arc<DaemonState>) {
    std::panic::set_hook(Box::new(move |info| {
        let payload = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| (*s).to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "（非文本 panic 内容）".into());
        let at = info
            .location()
            .map(|l| format!("{}:{}", l.file(), l.line()))
            .unwrap_or_else(|| "-".into());
        // 退出码按 Rust 默认 panic 退出码记 101，写死与实测保持一致
        let record = format!(
            "{}\terror\t崩溃退出 退出码=101 位置={at} 原因={payload}\n--- 最后 20 行日志 ---\n{}--- 日志末尾 ---\n",
            now_secs(),
            tail_lines(&state.log_path, 20)
        );
        if let Ok(mut f) = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&state.log_path)
        {
            let _ = f.write_all(record.as_bytes());
        }
        // 同时打到 stderr：前台运行时不用翻文件就能看到
        eprint!("{record}");
    }));
}

/// 归档文件名里的本地时间戳（YYYYMMDD-HHmm），按规范 5.4 的命名模式。
/// 取不到本机时区（个别精简环境）退回 UTC：差几小时也比日志写不下去、轮转不了强。
fn local_stamp() -> String {
    let now = time::OffsetDateTime::now_local().unwrap_or_else(|_| time::OffsetDateTime::now_utc());
    format!(
        "{:04}{:02}{:02}-{:02}{:02}",
        now.year(),
        now.month() as u8,
        now.day(),
        now.hour(),
        now.minute()
    )
}

/// 取文件末尾 n 行；读失败返回空串——崩溃留痕自身绝不能再 panic。
fn tail_lines(path: &Path, n: usize) -> String {
    let text = fs::read_to_string(path).unwrap_or_default();
    let all: Vec<&str> = text.lines().collect();
    let mut out = all[all.len().saturating_sub(n)..].join("\n");
    if !out.is_empty() {
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn destination_outside_allow_list_is_rejected() {
        let mut c = Config::default();
        c.destination = PathBuf::from("D:/elsewhere");
        c.allow = vec![PathBuf::from("D:/allowed")];
        assert!(c.validate().is_err());
    }

    #[test]
    fn config_roundtrip() {
        let dir = std::env::temp_dir().join(format!("tc-config-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let p = dir.join("config.toml");
        let c = Config::default();
        c.save(&p).unwrap();
        let back = Config::load_or_create(&p).unwrap();
        assert_eq!(back.listen, c.listen);
        assert_eq!(back.log_days, 2);
        assert_eq!(back.log_level, "info");
    }

    /// 调试开关的「关」：默认 info 下 debug 行既不进内存也不进磁盘。
    #[test]
    fn debug_lines_are_dropped_at_info_level() {
        let st = test_state("tc-loglevel-off");
        assert_eq!(st.log_level(), "info");
        st.log("debug", "这条不该出现");
        st.log("info", "这条要出现");
        let text = fs::read_to_string(&st.log_path).unwrap();
        assert!(!text.contains("这条不该出现"));
        assert!(text.contains("这条要出现"));
        assert!(st.recent_logs().iter().all(|(_, _, m)| !m.contains("这条不该出现")));
    }

    /// 调试开关的「开」：切到 debug 后 debug 行照常落盘。
    #[test]
    fn debug_lines_are_written_after_switching_to_debug() {
        let st = test_state("tc-loglevel-on");
        assert!(st.set_log_level("debug"));
        st.log("debug", "这条要出现");
        let text = fs::read_to_string(&st.log_path).unwrap();
        assert!(text.contains("这条要出现"));
    }

    /// 级别写错时按 info 运行，并留一条 warn 说明，避免误以为 debug 已生效。
    #[test]
    fn invalid_log_level_falls_back_to_info_with_warning() {
        let st = test_state_with_level("tc-loglevel-bad", "trace");
        let text = fs::read_to_string(&st.log_path).unwrap();
        assert!(text.contains("log_level=trace 非法"));
        assert_eq!(st.log_level(), "info");
        st.log("info", "信息仍记录");
        assert!(fs::read_to_string(&st.log_path).unwrap().contains("信息仍记录"));
    }

    /// 崩溃留痕：能拿到最后 N 行；文件不存在时也不能 panic。
    #[test]
    fn tail_lines_reads_last_lines_and_tolerates_missing_file() {
        let dir = std::env::temp_dir().join("tc-taillines");
        fs::create_dir_all(&dir).unwrap();
        let p = dir.join("x.log");
        let body: String = (1..=30).map(|i| format!("第{i}行\n")).collect();
        fs::write(&p, body).unwrap();
        let tail = tail_lines(&p, 20);
        assert!(tail.starts_with("第11行"));
        assert!(tail.ends_with("第30行\n"));
        assert_eq!(tail.lines().count(), 20);
        assert_eq!(tail_lines(&dir.join("missing.log"), 20), "");
    }

    /// panic 钩子要真的把「时间 + 退出码 + 最后 20 行」写进日志。
    /// 用 catch_unwind 拦住 panic，测完恢复原钩子，避免影响同进程里的其他用例输出。
    #[test]
    fn panic_hook_writes_crash_record_with_tail() {
        let st = Arc::new(test_state("tc-panic-hook"));
        st.log("info", "崩溃前的最后一条");
        let prev = std::panic::take_hook();
        install_panic_hook(Arc::clone(&st));
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            panic!("验证崩溃留痕")
        }));
        std::panic::set_hook(prev);
        assert!(r.is_err());

        let text = fs::read_to_string(&st.log_path).unwrap();
        assert!(text.contains("崩溃退出 退出码=101"));
        assert!(text.contains("验证崩溃留痕"));
        assert!(text.contains("崩溃前的最后一条"));
    }

    /// 按大小轮转：越过上限后 daemon.log 改名成带日期戳的归档，新文件从小体积重新开始；
    /// 归档文件保留不删，且新文件首行写明去向，便于顺藤摸瓜。
    #[test]
    fn log_rotates_when_size_limit_exceeded() {
        let st = test_state_with("tc-rotate", "info", 512);
        for i in 0..40 {
            st.log("info", &format!("轮转测试第 {i} 行：为尽快越过 512 字节上限，这行写长一点"));
        }
        let dir = st.log_path.parent().unwrap();
        let names: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.starts_with("daemon_"))
            .collect();
        assert!(!names.is_empty(), "越过上限后应出现归档文件");
        assert!(names.iter().all(|n| n.ends_with(".log")), "归档应为 .log：{names:?}");
        // 归档名形如 daemon_20261113-1953.log
        let core = names[0].trim_start_matches("daemon_").trim_end_matches(".log");
        assert!(core.chars().take(8).all(|c| c.is_ascii_digit()), "日期段应为 8 位数字：{core}");
        assert_eq!(core.chars().nth(8), Some('-'));

        let current = fs::metadata(&st.log_path).unwrap().len();
        assert!(current <= 1024, "轮转后当前文件应远小于上限，实际 {current} 字节");
        assert!(
            fs::read_to_string(&st.log_path).unwrap().contains("日志轮转：上一份归档为"),
            "新文件首行应说明归档去向"
        );
    }

    /// 上限为 0 表示不轮转：一直写在同一个文件里。
    #[test]
    fn zero_limit_disables_rotation() {
        let st = test_state_with("tc-rotate-off", "info", 0);
        for i in 0..80 {
            st.log("info", &format!("不轮转第 {i} 行"));
        }
        let names: Vec<String> = fs::read_dir(st.log_path.parent().unwrap())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.starts_with("daemon_"))
            .collect();
        assert!(names.is_empty(), "关闭轮转时不应产生归档：{names:?}");
        assert!(fs::metadata(&st.log_path).unwrap().len() > 512);
    }

    fn test_state(name: &str) -> DaemonState {
        test_state_with_level(name, "info")
    }

    fn test_state_with_level(name: &str, level: &str) -> DaemonState {
        // 轮转上限取默认值，避免日志测试意外触发轮转
        test_state_with(name, level, Config::default().log_max_bytes)
    }

    fn test_state_with(name: &str, level: &str, max_bytes: u64) -> DaemonState {
        let dir = std::env::temp_dir().join(format!("{name}-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let mut cfg = Config::default();
        cfg.destination = dir.join("received");
        cfg.identity_dir = dir.join("identity");
        cfg.log_dir = dir.join("logs");
        cfg.queue_dir = dir.join("queue");
        cfg.allow = vec![dir.join("received")];
        cfg.log_level = level.into();
        cfg.log_max_bytes = max_bytes;
        DaemonState::new(cfg, dir.join("config.toml")).unwrap()
    }

    #[test]
    fn only_two_days_of_logs_are_returned_and_nothing_is_deleted() {
        let dir = std::env::temp_dir().join(format!("tc-log-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let mut cfg = Config::default();
        cfg.destination = dir.join("received");
        cfg.identity_dir = dir.join("identity");
        cfg.log_dir = dir.join("logs");
        cfg.queue_dir = dir.join("queue");
        cfg.allow = vec![dir.join("received")];
        let st = DaemonState::new(cfg, dir.join("config.toml")).unwrap();

        let old = now_secs() - 3 * 24 * 3600;
        fs::write(&st.log_path, format!("{old}\tinfo\t旧日志\n")).unwrap();
        st.log("info", "新日志");

        let logs = st.recent_logs();
        assert_eq!(logs.len(), 1);
        assert!(logs[0].2.contains("新日志"));
        // 过期行只是不展示，文件必须还在
        assert!(st.log_path.exists());
        assert!(fs::read_to_string(&st.log_path).unwrap().contains("旧日志"));
    }

    /// 升级前写下的历史行没有来源与落点字段，读历史时不能因此整行丢失。
    #[test]
    fn history_lines_written_before_v053_still_load() {
        let dir = std::env::temp_dir().join(format!("tc-hist-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let mut cfg = Config::default();
        cfg.destination = dir.join("received");
        cfg.identity_dir = dir.join("identity");
        cfg.log_dir = dir.join("logs");
        cfg.queue_dir = dir.join("queue");
        cfg.allow = vec![dir.join("received")];
        let st = DaemonState::new(cfg, dir.join("config.toml")).unwrap();

        // 时间用当前值，否则会被 2 天窗口过滤掉，测不到「解析」这一步
        let ts = now_secs();
        let legacy = format!(
            r#"{{"ts":{ts},"txn_id":"old-1","files":3,"bytes":900,"verified":true,"detail":"完成"}}"#
        );
        let with_peer = format!(
            r#"{{"ts":{ts},"txn_id":"new-1","files":1,"bytes":10,"verified":true,"detail":"完成","peer_name":"客厅笔记本","peer_id":"dev-9","peer_platform":"android","peer_addr":"203.0.113.7:50001","roots":["照片"]}}"#
        );
        fs::write(&st.history_path, format!("{legacy}\n{with_peer}\n")).unwrap();

        let hist = st.recent_history();
        assert_eq!(hist.len(), 2, "旧行与新行都应可读");
        assert_eq!(hist[0].txn_id, "old-1");
        assert!(hist[0].peer_name.is_empty() && hist[0].roots.is_empty());
        assert_eq!(hist[1].peer_name, "客厅笔记本");
        assert_eq!(hist[1].roots, vec!["照片".to_string()]);
    }

    /// 升级前的 config.toml 没有 trust_mode：必须按 closed 读，不能解析失败也不能变成空串。
    #[test]
    fn legacy_config_without_trust_mode_defaults_to_closed() {
        let dir = std::env::temp_dir().join(format!("tc-trustcfg-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let p = dir.join("config.toml");
        fs::write(
            &p,
            "listen = \"127.0.0.1:1\"\nmanage = \"127.0.0.1:2\"\ndestination = \"received\"\nallow = [\"received\"]\n",
        )
        .unwrap();
        let c = Config::load_or_create(&p).unwrap();
        assert_eq!(c.trust_mode, "closed");
    }
}
