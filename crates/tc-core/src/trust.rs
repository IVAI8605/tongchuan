//! v0.5.4 信任库：JSONL 事件流，只追加、不删行。
//! 为什么用事件流而不是整表重写：撤销要留痕、误撤可以再批准恢复；
//! daemon 与 CLI 同时改同一文件时各自只追加自己的那一行，不会互相覆盖。
//! 撤销必须立刻生效：每次查询前 stat 文件，变了就重放，不依赖重启。

use crate::queue::now_secs;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

pub const TRUST_FILE: &str = "trust.jsonl";

/// 一次信任变更。`denied` 是 P2 预留的拒绝记录，只用于「待确认」展示，不影响信任状态。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrustEvent {
    pub ts: i64,
    /// add | rename | revoke（P2 起允许 denied）
    pub op: String,
    pub fp: String,
    #[serde(default)]
    pub alias: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub platform: String,
    /// manual | pair-code | open
    #[serde(default)]
    pub by: String,
    #[serde(default)]
    pub note: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct TrustedPeer {
    pub fp: String,
    pub alias: String,
    pub name: String,
    pub platform: String,
    pub first_seen: i64,
    pub added_by: String,
    pub last_ts: i64,
}

struct Inner {
    peers: BTreeMap<String, TrustedPeer>,
    /// P2 起记录被门禁拒绝的尝试，供管理页「待确认」展示。
    /// 只保留每个指纹最近一次，避免同一台设备反复重试把列表刷爆。
    denied: BTreeMap<String, TrustEvent>,
    /// 上次回放时的 (mtime 纳秒, 文件长度)。
    /// daemon 常驻期间别的进程（CLI / 管理页）改了文件，靠它发现并重放。
    stamp: Option<(u128, u64)>,
    bad_lines: u64,
}

pub struct TrustStore {
    path: PathBuf,
    inner: Mutex<Inner>,
}

impl TrustStore {
    /// 只记住路径，不建目录：没有信任记录时不该在磁盘上凭空多出一个空文件。
    pub fn new(dir: &Path) -> Self {
        Self {
            path: dir.join(TRUST_FILE),
            inner: Mutex::new(Inner {
                peers: BTreeMap::new(),
                denied: BTreeMap::new(),
                stamp: None,
                bad_lines: 0,
            }),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 当前信任表。每次调用都先同步文件，保证 CLI 与管理页看到同一份状态。
    pub fn list(&self) -> io::Result<Vec<TrustedPeer>> {
        let mut g = self.inner.lock().expect("trust poisoned");
        self.reload_locked(&mut g)?;
        let mut out: Vec<TrustedPeer> = g.peers.values().cloned().collect();
        // 展示按别名排序，别名相同再按指纹，保证两边顺序稳定
        out.sort_by(|a, b| a.alias.cmp(&b.alias).then_with(|| a.fp.cmp(&b.fp)));
        Ok(out)
    }

    pub fn is_trusted(&self, fp: &str) -> io::Result<bool> {
        let fp = normalize_fp(fp)?;
        let mut g = self.inner.lock().expect("trust poisoned");
        self.reload_locked(&mut g)?;
        Ok(g.peers.contains_key(&fp))
    }

    pub fn get(&self, fp: &str) -> io::Result<Option<TrustedPeer>> {
        let fp = normalize_fp(fp)?;
        let mut g = self.inner.lock().expect("trust poisoned");
        self.reload_locked(&mut g)?;
        Ok(g.peers.get(&fp).cloned())
    }

    /// 被门禁拒绝、仍未被批准的来源设备。已批准的不再显示，撤销后重新出现。
    pub fn pending(&self) -> io::Result<Vec<TrustEvent>> {
        let mut g = self.inner.lock().expect("trust poisoned");
        self.reload_locked(&mut g)?;
        let mut out: Vec<TrustEvent> = g
            .denied
            .values()
            .filter(|e| !g.peers.contains_key(&e.fp))
            .cloned()
            .collect();
        // 最近的尝试排前面；页面只展示一屏，不需要全量
        out.sort_by(|a, b| b.ts.cmp(&a.ts));
        out.truncate(50);
        Ok(out)
    }

    /// 记录一次被拒绝的会话尝试，只追加，不改变信任状态。
    /// 没有指纹（旧端无证书）时调用方不应调用本方法：没有可批准的对象。
    pub fn record_denied(
        &self,
        fp: &str,
        name: &str,
        platform: &str,
        note: &str,
    ) -> io::Result<()> {
        let fp = normalize_fp(fp)?;
        let mut g = self.inner.lock().expect("trust poisoned");
        self.reload_locked(&mut g)?;
        let ev = TrustEvent {
            ts: now_secs(),
            op: "denied".into(),
            fp,
            alias: String::new(),
            name: name.to_string(),
            platform: platform.to_string(),
            by: "gate".into(),
            note: note.to_string(),
        };
        append_line(&self.path, &ev)?;
        force_reload(&self.path, &mut g)
    }

    /// 批准即追加一条 add。重复批准是幂等的：不重置 first_seen，只补空缺字段。
    pub fn approve(
        &self,
        fp: &str,
        alias: &str,
        name: &str,
        platform: &str,
        by: &str,
    ) -> io::Result<TrustedPeer> {
        let fp = normalize_fp(fp)?;
        let mut g = self.inner.lock().expect("trust poisoned");
        self.reload_locked(&mut g)?;
        let ev = TrustEvent {
            ts: now_secs(),
            op: "add".into(),
            fp: fp.clone(),
            alias: alias.trim().to_string(),
            name: name.to_string(),
            platform: platform.to_string(),
            by: if by.trim().is_empty() { "manual".into() } else { by.trim().to_string() },
            note: String::new(),
        };
        append_line(&self.path, &ev)?;
        // 刚写完不能依赖 mtime 精度判断有没有变，直接强制重放
        force_reload(&self.path, &mut g)?;
        Ok(g.peers.get(&fp).cloned().unwrap_or(TrustedPeer {
            fp,
            alias: alias.trim().to_string(),
            name: name.to_string(),
            platform: platform.to_string(),
            first_seen: ev.ts,
            added_by: ev.by.clone(),
            last_ts: ev.ts,
        }))
    }

    /// 撤销只追加事件。本来就不在表里时返回 false，不写无意义的撤销行。
    pub fn revoke(&self, fp: &str, by: &str, note: &str) -> io::Result<bool> {
        let fp = normalize_fp(fp)?;
        let mut g = self.inner.lock().expect("trust poisoned");
        self.reload_locked(&mut g)?;
        if !g.peers.contains_key(&fp) {
            return Ok(false);
        }
        let ev = TrustEvent {
            ts: now_secs(),
            op: "revoke".into(),
            fp: fp.clone(),
            alias: String::new(),
            name: String::new(),
            platform: String::new(),
            by: if by.trim().is_empty() { "manual".into() } else { by.trim().to_string() },
            note: note.to_string(),
        };
        append_line(&self.path, &ev)?;
        force_reload(&self.path, &mut g)?;
        Ok(!g.peers.contains_key(&fp))
    }

    pub fn rename(&self, fp: &str, alias: &str) -> io::Result<bool> {
        let fp = normalize_fp(fp)?;
        let alias = alias.trim();
        if alias.is_empty() {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "alias is empty"));
        }
        let mut g = self.inner.lock().expect("trust poisoned");
        self.reload_locked(&mut g)?;
        if !g.peers.contains_key(&fp) {
            return Ok(false);
        }
        let ev = TrustEvent {
            ts: now_secs(),
            op: "rename".into(),
            fp,
            alias: alias.chars().take(64).collect(),
            name: String::new(),
            platform: String::new(),
            by: "manual".into(),
            note: String::new(),
        };
        append_line(&self.path, &ev)?;
        force_reload(&self.path, &mut g)?;
        Ok(true)
    }

    /// 坏行数量：只用于日志/排障，说明「有行没读进来」，不影响其余记录。
    pub fn bad_lines(&self) -> io::Result<u64> {
        let mut g = self.inner.lock().expect("trust poisoned");
        self.reload_locked(&mut g)?;
        Ok(g.bad_lines)
    }

    fn reload_locked(&self, g: &mut Inner) -> io::Result<()> {
        let cur = file_stamp(&self.path)?;
        if cur == g.stamp {
            return Ok(());
        }
        replay(&self.path, g)?;
        g.stamp = cur;
        Ok(())
    }
}

/// 强制重放：自己刚写过文件，不能指望文件系统时间戳立刻变化。
fn force_reload(path: &Path, g: &mut Inner) -> io::Result<()> {
    replay(path, g)?;
    g.stamp = file_stamp(path)?;
    Ok(())
}

fn replay(path: &Path, g: &mut Inner) -> io::Result<()> {
    let text = match fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e),
    };
    let mut peers = BTreeMap::new();
    let mut denied = BTreeMap::new();
    let mut bad = 0u64;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        match serde_json::from_str::<TrustEvent>(line) {
            // 手工编辑出错、旧版本写的坏行：跳过并计数，绝不能让整张表读废
            Ok(mut ev) => {
                // denied 不进信任表，只喂「待确认」；无指纹的记录直接算坏行
                if ev.op == "denied" {
                    match normalize_fp(&ev.fp) {
                        Ok(fp) => {
                            ev.fp = fp.clone();
                            let keep_new = denied
                                .get(&fp)
                                .map(|old: &TrustEvent| old.ts <= ev.ts)
                                .unwrap_or(true);
                            if keep_new {
                                denied.insert(fp, ev);
                            }
                        }
                        Err(_) => bad += 1,
                    }
                    continue;
                }
                if !apply(&mut peers, &ev) {
                    bad += 1;
                }
            }
            Err(_) => bad += 1,
        }
    }
    g.peers = peers;
    g.denied = denied;
    g.bad_lines = bad;
    Ok(())
}

fn apply(map: &mut BTreeMap<String, TrustedPeer>, ev: &TrustEvent) -> bool {
    let fp = ev.fp.trim().to_ascii_lowercase();
    if fp.is_empty() {
        return false;
    }
    match ev.op.as_str() {
        "add" => {
            let e = map.entry(fp.clone()).or_insert_with(|| TrustedPeer {
                fp: fp.clone(),
                alias: String::new(),
                name: String::new(),
                platform: String::new(),
                first_seen: ev.ts,
                added_by: String::new(),
                last_ts: ev.ts,
            });
            if !ev.alias.trim().is_empty() {
                e.alias = ev.alias.trim().chars().take(64).collect();
            }
            if !ev.name.is_empty() {
                e.name = ev.name.clone();
            }
            if !ev.platform.is_empty() {
                e.platform = ev.platform.clone();
            }
            if !ev.by.is_empty() {
                e.added_by = ev.by.clone();
            }
            e.last_ts = ev.ts;
            true
        }
        // 改名只动别名，未知指纹的 rename 直接忽略（不能凭一条改名事件就造出信任项）
        "rename" => {
            if let Some(e) = map.get_mut(&fp) {
                if !ev.alias.trim().is_empty() {
                    e.alias = ev.alias.trim().chars().take(64).collect();
                    e.last_ts = ev.ts;
                }
            }
            true
        }
        "revoke" => {
            map.remove(&fp);
            true
        }
        // P2 才写入；即使读到也绝不能当成放行依据
        "denied" => true,
        _ => false,
    }
}

fn append_line(path: &Path, ev: &TrustEvent) -> io::Result<()> {
    if let Some(p) = path.parent() {
        fs::create_dir_all(p)?;
    }
    let mut f = OpenOptions::new().create(true).append(true).open(path)?;
    let line = serde_json::to_string(ev).map_err(|e| io::Error::other(e.to_string()))?;
    // 正文与换行拼成一个缓冲区，一次 write_all 落盘：O_APPEND 只保单次 write 原子，
    // writeln! 拆成正文+\n 两次 write，并发追加会把别的行插进中间，整行 JSON 被拼坏。
    f.write_all(format!("{line}\n").as_bytes())?;
    f.flush()
}

fn file_stamp(path: &Path) -> io::Result<Option<(u128, u64)>> {
    match fs::metadata(path) {
        Ok(m) => {
            let mtime = m
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            Ok(Some((mtime, m.len())))
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// 指纹统一小写、去掉冒号与空格；长度必须是 64 位十六进制。
/// 不做前缀匹配：宁可明确报错，也不能让「看起来像」的指纹误放行。
fn normalize_fp(text: &str) -> io::Result<String> {
    let s: String = text
        .trim()
        .chars()
        .filter(|c| *c != ':' && *c != ' ')
        .collect::<String>()
        .to_ascii_lowercase();
    if s.len() != 64 || !s.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "fingerprint must be 64 hex characters",
        ));
    }
    Ok(s)
}

/// 解析信任库目录，优先级：`--identity` > 配置文件里的 `identity_dir` > 平台默认。
/// 必须优先跟 daemon 用同一个 identity_dir，否则会出现「命令成功但管理页看不到」的陷阱。
/// `config` 为 None 时看当前目录的 config.toml；显式给了却读不到则报错，不悄悄换目录。
pub fn resolve_identity_dir(identity: Option<&Path>, config: Option<&Path>) -> io::Result<PathBuf> {
    if let Some(d) = identity {
        return Ok(d.to_path_buf());
    }
    let cfg_path = config
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("config.toml"));
    match crate::daemon::Config::load(&cfg_path)? {
        Some(c) => Ok(c.identity_dir),
        None => {
            if config.is_some() {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("找不到配置文件 {}", cfg_path.display()),
                ));
            }
            Ok(default_identity_dir())
        }
    }
}

/// 信任库默认目录，与 `tc send` 的历史目录同一套约定：TC_HOME 优先（验证脚本靠它隔离）。
pub fn default_identity_dir() -> PathBuf {
    if let Ok(v) = std::env::var("TC_HOME") {
        if !v.trim().is_empty() {
            return PathBuf::from(v).join("identity");
        }
    }
    platform_default().unwrap_or_else(|| PathBuf::from("identity"))
}

#[cfg(windows)]
fn platform_default() -> Option<PathBuf> {
    std::env::var("LOCALAPPDATA")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .map(|v| PathBuf::from(v).join("tongchuan").join("identity"))
}

#[cfg(not(windows))]
fn platform_default() -> Option<PathBuf> {
    std::env::var("HOME")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .map(|v| PathBuf::from(v).join(".local/share/tongchuan/identity"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    /// daemon 与 CLI 同时追加 trust.jsonl：每行必须是完整 JSON，回放一条不丢、坏行为 0。
    /// writeln! 把正文和换行拆成两次 write，O_APPEND 只保单次原子，这里专测这个交错。
    #[test]
    fn concurrent_append_keeps_every_event_intact() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("tc-trust-append-{unique}"));
        let path = dir.join(TRUST_FILE);
        let threads = 16usize;
        let per_thread = 300usize;
        let total = threads * per_thread;

        let mut handles = Vec::new();
        for t in 0..threads {
            let path = path.clone();
            handles.push(thread::spawn(move || {
                for i in 0..per_thread {
                    let n = t * per_thread + i;
                    let ev = TrustEvent {
                        ts: 1_700_000_000 + n as i64,
                        op: "add".into(),
                        fp: format!("{n:064x}"),
                        alias: format!("并发设备-{t}-{i}"),
                        name: String::new(),
                        platform: String::new(),
                        by: "test".into(),
                        note: String::new(),
                    };
                    append_line(&path, &ev).unwrap();
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }

        let text = fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), total, "并发追加不能丢行");
        for (i, line) in lines.iter().enumerate() {
            assert!(
                serde_json::from_str::<TrustEvent>(line).is_ok(),
                "第 {i} 行不是完整 JSON，被别的写入插坏了：{line}"
            );
        }
        let store = TrustStore::new(&dir);
        assert_eq!(store.bad_lines().unwrap(), 0, "回放不应出现坏行");
        assert_eq!(store.list().unwrap().len(), total, "回放后事件条数应完整");
    }
}
