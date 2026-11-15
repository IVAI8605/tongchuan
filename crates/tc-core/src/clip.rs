//! 剪贴板内核（v0.6）。
//!
//! 设计取舍（与 `功能展望-版本路线.md` §3 对应）：
//! - 独立端口 + 独立协议，不复用文件会话：文件会话是「单向、大批量、接收端拉块」，
//!   剪贴板是「双向、小消息、多端在线」，硬塞会把刚稳定的会话语义再搅一遍。
//! - 线上是 4 字节大端长度前缀 + JSON，单条 ≤ 128 KiB；正文上限 64 KiB。
//! - 元数据与正文分两帧：预览给界面用，正文只在确认要时才传。
//! - 拓扑默认星型：所有端连「主设备」，由它存历史并分发；这样连接数是 N 而不是 N²。
//! - v0.6 的订阅用**带游标的轮询**（Pull{since}），不是长连接推送：
//!   同步写 + 阻塞读的线程模型下，推送要拆读写两半，收益是 1 秒延迟，
//!   不值得在只做内核的这一版引入；v0.7 接安卓时再评估。

use crate::manifest::DeviceInfo;
use crate::transport::{Accepter, BoxedStream, Opener};
use crate::trust::TrustStore;
use serde::{Deserialize, Serialize};
use std::collections::{HashSet, VecDeque};
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

/// 正文上限：超过就不进剪贴板通道（避免把 10 MB 图片塞进去把对端卡死）。
pub const CLIP_MAX_TEXT: usize = 64 * 1024;
/// 单帧上限：正文 + 元数据 + JSON 转义余量。
pub const CLIP_MAX_MSG: usize = 128 * 1024;
pub const DEFAULT_CLIP_PORT: u16 = 53320;
/// 预览长度：界面与通知只显示这么多，足够认出是哪条。
pub const PREVIEW_LEN: usize = 200;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClipEvent {
    /// 事件 id：内容哈希前 16 位，幂等去重用
    pub id: String,
    /// 产生事件的设备名；收到自己的事件一律忽略（回声抑制）
    pub origin: String,
    /// 逻辑时钟：收到事件时取 max(本地, 对端)+1，用于判断「谁更新」
    pub lamport: u64,
    #[serde(rename = "tsMs")]
    pub ts_ms: i64,
    /// 目前只有 "text"；图片/文件走文件通道，这里只放引用
    pub kind: String,
    /// BLAKE3 全文，用于跨端去重
    pub hash: String,
    pub size: usize,
    pub preview: String,
    /// 敏感标记：带标记的事件只落历史、不自动写剪贴板
    pub sensitive: bool,
    /// 正文。线上元数据帧不带它（另起 ClipBody）；落盘历史时带上。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

impl ClipEvent {
    pub fn from_text(origin: &str, lamport: u64, text: &str, sensitive: bool) -> Self {
        let hash = crate::hash::hash_bytes(text.as_bytes());
        Self {
            id: hash.chars().take(16).collect(),
            origin: origin.to_string(),
            lamport,
            ts_ms: now_ms(),
            kind: "text".into(),
            hash,
            size: text.len(),
            preview: preview_of(text),
            sensitive,
            text: None,
        }
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// 预览只取前 PREVIEW_LEN 个字符，并把换行压成空格——通知栏里换行会截断成半句话。
pub fn preview_of(text: &str) -> String {
    let mut s: String = text.chars().take(PREVIEW_LEN).collect();
    s = s.replace(['\r', '\n'], " ");
    if text.chars().count() > PREVIEW_LEN {
        s.push('…');
    }
    s
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ClipMsg {
    Hello {
        device: DeviceInfo,
        origin: String,
        version: String,
    },
    Welcome {
        version: String,
        lamport: u64,
    },
    /// 元数据帧；正文紧跟一条 ClipBody
    Clip(ClipEvent),
    ClipBody {
        id: String,
        text: String,
    },
    Ack {
        id: String,
    },
    /// 拉取：since 之后有没有更新
    Pull {
        since: u64,
    },
    Event {
        event: Option<ClipEvent>,
        text: Option<String>,
        lamport: u64,
    },
    Status,
    StatusReply {
        clients: usize,
        lamport: u64,
        latest: Option<ClipEvent>,
    },
    Ping,
    Pong,
    Bye,
    Deny {
        reason: String,
    },
}

pub fn write_msg<W: Write>(w: &mut W, msg: &ClipMsg) -> io::Result<()> {
    let body = serde_json::to_vec(msg)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
    if body.len() > CLIP_MAX_MSG {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("clip message too large: {} bytes", body.len()),
        ));
    }
    w.write_all(&(body.len() as u32).to_be_bytes())?;
    w.write_all(&body)?;
    w.flush()
}

pub fn read_msg<R: Read>(r: &mut R) -> io::Result<ClipMsg> {
    let mut len = [0u8; 4];
    r.read_exact(&mut len)?;
    let n = u32::from_be_bytes(len) as usize;
    if n > CLIP_MAX_MSG {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("clip message too large: {n} bytes"),
        ));
    }
    let mut buf = vec![0u8; n];
    r.read_exact(&mut buf)?;
    serde_json::from_slice(&buf).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))
}

/// 去重与定序。只放内存：重启后靠历史文件恢复「最近几条」，不依赖它做持久化。
pub struct ClipLedger {
    ids: HashSet<String>,
    id_order: VecDeque<String>,
    recent_hashes: VecDeque<String>,
    lamport: u64,
    latest: Option<ClipEvent>,
}

impl Default for ClipLedger {
    fn default() -> Self {
        Self::new()
    }
}

impl ClipLedger {
    pub fn new() -> Self {
        Self {
            ids: HashSet::new(),
            id_order: VecDeque::new(),
            recent_hashes: VecDeque::new(),
            lamport: 0,
            latest: None,
        }
    }

    pub fn lamport(&self) -> u64 {
        self.lamport
    }

    pub fn latest(&self) -> Option<&ClipEvent> {
        self.latest.as_ref()
    }

    /// 接受一条事件。返回 false 表示重复（同 id 或与最近 5 条内容相同），调用方不应再广播。
    pub fn accept(&mut self, ev: &ClipEvent) -> bool {
        if self.ids.contains(&ev.id) || self.recent_hashes.iter().any(|h| h == &ev.hash) {
            return false;
        }
        self.ids.insert(ev.id.clone());
        self.id_order.push_back(ev.id.clone());
        // 只留最近 1024 个 id，够覆盖断线重连的补发窗口
        while self.id_order.len() > 1024 {
            if let Some(old) = self.id_order.pop_front() {
                self.ids.remove(&old);
            }
        }
        self.recent_hashes.push_back(ev.hash.clone());
        while self.recent_hashes.len() > 5 {
            self.recent_hashes.pop_front();
        }
        self.lamport = self.lamport.max(ev.lamport) + 1;
        let mut stored = ev.clone();
        self.latest = Some(stored.clone());
        stored.text = None;
        true
    }

    /// 从历史恢复：把最近几条塞进去，避免重启后把同样的内容再广播一遍。
    pub fn warm_up(&mut self, events: &[ClipEvent]) {
        for ev in events {
            self.accept(ev);
        }
    }
}

pub fn append_history(path: &Path, ev: &ClipEvent) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut line = serde_json::to_string(ev)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
    line.push('\n');
    let mut f = OpenOptions::new().create(true).append(true).open(path)?;
    f.write_all(line.as_bytes())
}

/// 读最近 limit 条历史；坏行跳过但不重写文件（与发送历史同一口径）。
pub fn read_history(path: &Path, limit: usize) -> io::Result<Vec<ClipEvent>> {
    let text = match fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut out: Vec<ClipEvent> = text
        .lines()
        .filter_map(|l| serde_json::from_str::<ClipEvent>(l).ok())
        .collect();
    if out.len() > limit {
        out = out.split_off(out.len() - limit);
    }
    Ok(out)
}

/// 客户端连接：已经完成 HELLO/WELCOME 握手。
pub struct ClipClient {
    pub stream: BoxedStream,
    pub device: DeviceInfo,
    pub origin: String,
}

impl ClipClient {
    pub fn connect(
        opener: &dyn Opener,
        device: DeviceInfo,
        origin: &str,
    ) -> io::Result<Self> {
        let mut stream = opener.open()?;
        write_msg(
            &mut stream,
            &ClipMsg::Hello {
                device: device.clone(),
                origin: origin.to_string(),
                version: crate::session::PROTOCOL_VERSION.into(),
            },
        )?;
        match read_msg(&mut stream)? {
            ClipMsg::Welcome { version, .. } => {
                if !crate::session::protocol_compatible(&version) {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        crate::session::version_reject_from_receiver(&version),
                    ));
                }
            }
            ClipMsg::Deny { reason } => {
                return Err(io::Error::new(io::ErrorKind::PermissionDenied, reason))
            }
            other => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("unexpected welcome: {other:?}"),
                ))
            }
        }
        Ok(Self {
            stream,
            device,
            origin: origin.to_string(),
        })
    }

    pub fn send(&mut self, msg: &ClipMsg) -> io::Result<()> {
        write_msg(&mut self.stream, msg)
    }

    pub fn recv(&mut self) -> io::Result<ClipMsg> {
        read_msg(&mut self.stream)
    }
}

/// 推一条文本并等 ACK。超过 CLIP_MAX_TEXT 直接拒绝，不做截断（截断等于悄悄改用户内容）。
pub fn push_text(conn: &mut ClipClient, text: &str, sensitive: bool) -> io::Result<ClipEvent> {
    if text.len() > CLIP_MAX_TEXT {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("文本过长（{} 字节，上限 {CLIP_MAX_TEXT}），超大内容请走文件通道", text.len()),
        ));
    }
    let ev = ClipEvent::from_text(&conn.origin, 0, text, sensitive);
    conn.send(&ClipMsg::Clip(ClipEvent {
        text: None,
        ..ev.clone()
    }))?;
    conn.send(&ClipMsg::ClipBody {
        id: ev.id.clone(),
        text: text.to_string(),
    })?;
    match conn.recv()? {
        ClipMsg::Ack { .. } => Ok(ev),
        ClipMsg::Deny { reason } => Err(io::Error::new(io::ErrorKind::PermissionDenied, reason)),
        other => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unexpected push reply: {other:?}"),
        )),
    }
}

/// 拉取比 since 更新的事件。
pub fn pull(
    conn: &mut ClipClient,
    since: u64,
) -> io::Result<(Option<ClipEvent>, Option<String>, u64)> {
    conn.send(&ClipMsg::Pull { since })?;
    match conn.recv()? {
        ClipMsg::Event {
            event,
            text,
            lamport,
        } => Ok((event, text, lamport)),
        ClipMsg::Deny { reason } => Err(io::Error::new(io::ErrorKind::PermissionDenied, reason)),
        other => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unexpected pull reply: {other:?}"),
        )),
    }
}

/// 剪贴板中继。星型拓扑里的「主设备」：存历史、记最新、应答拉取。
pub struct ClipServer {
    pub ledger: Mutex<ClipLedger>,
    pub history: PathBuf,
    pub trust: Option<Arc<TrustStore>>,
    pub clients: Mutex<usize>,
}

impl ClipServer {
    pub fn new(history: PathBuf, trust: Option<Arc<TrustStore>>) -> Arc<Self> {
        Arc::new(Self {
            ledger: Mutex::new(ClipLedger::new()),
            history,
            trust,
            clients: Mutex::new(0),
        })
    }

    /// 启动时用历史预热，避免重启后把同一条内容再广播一次。
    pub fn warm_up(&self, limit: usize) {
        if let Ok(events) = read_history(&self.history, limit) {
            if let Ok(mut l) = self.ledger.lock() {
                l.warm_up(&events);
            }
        }
    }

    /// 阻塞式接受连接，每个连接一线程；stop 置位后由调用方连一次自己唤醒。
    pub fn serve(self: Arc<Self>, acc: Arc<dyn Accepter>, stop: Arc<AtomicBool>) -> io::Result<()> {
        while !stop.load(Ordering::Relaxed) {
            let stream = match acc.accept() {
                Ok(s) => s,
                Err(_) if stop.load(Ordering::Relaxed) => break,
                Err(e) => return Err(e),
            };
            let me = Arc::clone(&self);
            std::thread::spawn(move || {
                if let Ok(mut n) = me.clients.lock() {
                    *n += 1;
                }
                let _ = me.handle(stream);
                if let Ok(mut n) = me.clients.lock() {
                    *n = n.saturating_sub(1);
                }
            });
        }
        Ok(())
    }

    fn handle(&self, mut stream: BoxedStream) -> io::Result<()> {
        // 先握手：设备身份与信任都在这条消息之后判定
        let (origin, version) = match read_msg(&mut stream)? {
            ClipMsg::Hello {
                origin, version, ..
            } => (origin, version),
            ClipMsg::Deny { reason } => {
                let _ = write_msg(&mut stream, &ClipMsg::Deny { reason });
                return Ok(());
            }
            other => {
                let _ = write_msg(
                    &mut stream,
                    &ClipMsg::Deny {
                        reason: format!("expected HELLO, got {other:?}"),
                    },
                );
                return Ok(());
            }
        };
        if !crate::session::protocol_compatible(&version) {
            let reason = crate::session::version_reject_reason(&version);
            let _ = write_msg(&mut stream, &ClipMsg::Deny { reason });
            return Ok(());
        }
        // 剪贴板里可能有密码：只允许已信任设备，且不提供 open 旁路
        if let Some(t) = &self.trust {
            let fp = stream.peer_cert_fingerprint();
            let trusted = match fp {
                Some(f) => {
                    let hex: String = f.iter().map(|b| format!("{b:02x}")).collect();
                    t.is_trusted(&hex).unwrap_or(false)
                }
                None => false,
            };
            if !trusted {
                let _ = write_msg(
                    &mut stream,
                    &ClipMsg::Deny {
                        reason: "untrusted: 剪贴板只对已信任设备开放，请先在管理页批准".into(),
                    },
                );
                return Ok(());
            }
        }
        let lamport = self.ledger.lock().map(|l| l.lamport()).unwrap_or(0);
        write_msg(
            &mut stream,
            &ClipMsg::Welcome {
                version: crate::session::PROTOCOL_VERSION.into(),
                lamport,
            },
        )?;

        let mut pending: Option<ClipEvent> = None;
        loop {
            let msg = match read_msg(&mut stream) {
                Ok(m) => m,
                Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(()),
                Err(e) => return Err(e),
            };
            match msg {
                ClipMsg::Clip(ev) => {
                    pending = Some(ev);
                }
                ClipMsg::ClipBody { id, text } => {
                    let mut ev = match pending.take() {
                        Some(e) if e.id == id => e,
                        _ => {
                            write_msg(
                                &mut stream,
                                &ClipMsg::Deny {
                                    reason: "body without matching clip".into(),
                                },
                            )?;
                            return Ok(());
                        }
                    };
                    if ev.origin == origin {
                        // 回声抑制：只信连接身份，不信消息里自报的 origin
                        ev.origin = origin.clone();
                    }
                    if text.len() > CLIP_MAX_TEXT {
                        write_msg(
                            &mut stream,
                            &ClipMsg::Deny {
                                reason: "text too large".into(),
                            },
                        )?;
                        continue;
                    }
                    let hash = crate::hash::hash_bytes(text.as_bytes());
                    if hash != ev.hash || ev.size != text.len() {
                        write_msg(
                            &mut stream,
                            &ClipMsg::Deny {
                                reason: "hash/size mismatch".into(),
                            },
                        )?;
                        continue;
                    }
                    // 服务端统一改编号与时钟，避免客户端伪造 lamport 抢「最新」
                    let accepted = {
                        let mut l = match self.ledger.lock() {
                            Ok(l) => l,
                            Err(_) => return Ok(()),
                        };
                        let lamport = l.lamport() + 1;
                        let mut stored = ev.clone();
                        stored.origin = origin.clone();
                        stored.lamport = lamport;
                        let ok = l.accept(&stored);
                        if ok {
                            stored.text = Some(text.clone());
                        }
                        (ok, stored)
                    };
                    let (ok, stored) = accepted;
                    if ok {
                        let _ = append_history(&self.history, &stored);
                    }
                    write_msg(&mut stream, &ClipMsg::Ack { id: stored.id.clone() })?;
                }
                ClipMsg::Pull { since } => {
                    let (ev, lamport) = {
                        let l = self.ledger.lock().expect("ledger");
                        let lamport = l.lamport();
                        let ev = l.latest().cloned();
                        (ev, lamport)
                    };
                    // 只回最新一条：剪贴板语义是「最后一条生效」，中间历史用 clip history 查
                    let reply = match ev {
                        Some(e) if e.lamport > since && e.origin != origin => {
                            let text = read_latest_text(&self.history, &e.id);
                            ClipMsg::Event {
                                event: Some(e),
                                text,
                                lamport,
                            }
                        }
                        _ => ClipMsg::Event {
                            event: None,
                            text: None,
                            lamport,
                        },
                    };
                    write_msg(&mut stream, &reply)?;
                }
                ClipMsg::Status => {
                    let (lamport, latest) = {
                        let l = self.ledger.lock().expect("ledger");
                        (l.lamport(), l.latest().cloned())
                    };
                    let clients = self.clients.lock().map(|n| *n).unwrap_or(0);
                    write_msg(
                        &mut stream,
                        &ClipMsg::StatusReply {
                            clients,
                            lamport,
                            latest,
                        },
                    )?;
                }
                ClipMsg::Ping => write_msg(&mut stream, &ClipMsg::Pong)?,
                ClipMsg::Bye => return Ok(()),
                _ => {}
            }
        }
    }
}

/// 从历史里取出指定事件的正文（内存只留元数据，正文以历史为准）。
fn read_latest_text(path: &Path, id: &str) -> Option<String> {
    let events = read_history(path, 64).ok()?;
    events
        .into_iter()
        .rev()
        .find(|e| e.id == id)
        .and_then(|e| e.text)
}

/// 暂停标记的默认位置：与发送历史同一条约定，可用 TC_HOME 把产物关进临时目录。
pub fn default_pause_path() -> PathBuf {
    if let Ok(v) = std::env::var("TC_HOME") {
        if !v.trim().is_empty() {
            return PathBuf::from(v).join("clip.pause");
        }
    }
    let base = std::env::var("LOCALAPPDATA")
        .map(PathBuf::from)
        .or_else(|_| std::env::var("HOME").map(PathBuf::from))
        .unwrap_or_else(|_| std::env::temp_dir());
    base.join("tongchuan").join("clip.pause")
}

/// 判断本机是否处于「暂停同步」状态：暂停标记是一个文件，内容为到期 Unix 秒。
pub fn paused_until(path: &Path) -> Option<i64> {
    let text = fs::read_to_string(path).ok()?;
    let ts: i64 = text.trim().parse().ok()?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    if ts > now {
        Some(ts)
    } else {
        None
    }
}

pub fn set_pause(path: &Path, minutes: u64) -> io::Result<i64> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let until = now + (minutes as i64) * 60;
    if let Some(p) = path.parent() {
        fs::create_dir_all(p)?;
    }
    fs::write(path, until.to_string())?;
    Ok(until)
}

pub fn clear_pause(path: &Path) -> io::Result<()> {
    // 只清空内容，不删文件（项目红线：一律不彻底删除）
    if path.exists() {
        fs::write(path, "0")?;
    }
    Ok(())
}

/// 用流做一次「推一条」的完整动作，供 CLI 与测试共用。
pub fn push_once(
    opener: &dyn Opener,
    device: DeviceInfo,
    origin: &str,
    text: &str,
    sensitive: bool,
) -> io::Result<ClipEvent> {
    let mut conn = ClipClient::connect(opener, device, origin)?;
    let ev = push_text(&mut conn, text, sensitive)?;
    Ok(ev)
}

/// 把对端事件写进本机剪贴板（Windows 走 PowerShell 的 Set-Clipboard，其它平台返回不支持）。
/// 用子进程而不是 FFI：与项目现有 Windows 外壳保持同一套办法，也避免手写剪贴板句柄管理。
pub fn apply_to_clipboard(text: &str) -> io::Result<()> {
    #[cfg(windows)]
    {
        use std::process::{Command, Stdio};
        let mut child = Command::new("powershell")
            .args(["-NoProfile", "-NonInteractive", "-Command", "$input | Set-Clipboard"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(text.as_bytes())?;
        }
        let status = child.wait()?;
        if !status.success() {
            return Err(io::Error::new(
                io::ErrorKind::Other,
                "Set-Clipboard 执行失败",
            ));
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let _ = text;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "当前平台不支持写剪贴板（v0.6 只做 Windows）",
        ))
    }
}

/// 测试与 CLI 共用：直接从一条流里读消息（服务端把 stream 交给闭包时用）。
pub fn read_from(stream: &mut BoxedStream) -> io::Result<ClipMsg> {
    read_msg(stream)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 预览按「字符」截断，多字节中文不会在半个字节处断开；换行压成空格，
    /// 超长才补省略号——通知栏里换行会被截成半句话。
    #[test]
    fn preview_truncates_by_chars_and_flattens_newlines() {
        assert_eq!(preview_of("你好\n世界"), "你好 世界");
        assert!(!preview_of("a\r\nb").contains(['\r', '\n']));

        let long: String = "字".repeat(PREVIEW_LEN + 50);
        let p = preview_of(&long);
        assert_eq!(p.chars().count(), PREVIEW_LEN + 1, "前 N 个字符 + 一个省略号");
        assert!(p.ends_with('…'));
        assert!(p.starts_with('字'));
    }

    /// 事件 id 取内容哈希前 16 位；元数据事件不带正文（正文另起 ClipBody 帧）。
    #[test]
    fn from_text_sets_id_preview_and_omits_body() {
        let ev = ClipEvent::from_text("pc", 7, "hello world", true);
        assert_eq!(ev.hash, crate::hash::hash_bytes(b"hello world"));
        assert_eq!(ev.id, ev.hash.chars().take(16).collect::<String>());
        assert_eq!(ev.id.chars().count(), 16);
        assert_eq!(ev.origin, "pc");
        assert_eq!(ev.lamport, 7);
        assert_eq!(ev.size, "hello world".len());
        assert_eq!(ev.preview, "hello world");
        assert!(ev.text.is_none(), "元数据事件不应带正文");
        assert!(ev.sensitive);
    }

    /// 去重：同 id、或最近 5 条内同内容，都拒绝；接受后 lamport 取 max+1，latest 不留正文。
    #[test]
    fn ledger_dedupes_by_id_and_recent_hash_and_advances_lamport() {
        let mut l = ClipLedger::new();
        let ev = ClipEvent::from_text("pc", 1, "第一条", false);
        assert!(l.accept(&ev), "首次应接受");
        assert!(!l.accept(&ev), "同 id 重复应拒绝");
        assert_eq!(l.lamport(), 2, "lamport = max(本地, 对端) + 1");

        // 同样内容（同 hash）即使换了 id，也应被「最近 5 条」拦下
        let mut same_content = ev.clone();
        same_content.id = "another-id".into();
        same_content.lamport = 99;
        assert!(!l.accept(&same_content), "同内容近 5 条应拒绝");
        assert!(l.latest().unwrap().text.is_none(), "latest 里不应留正文");
    }

    /// 同内容被挤出「最近 5 条」窗口后可以再次接受（断线重连补发场景）。
    #[test]
    fn ledger_allows_old_content_again_after_window() {
        let mut l = ClipLedger::new();
        let first = ClipEvent::from_text("pc", 0, "重复内容", false);
        assert!(l.accept(&first));
        for i in 0..5 {
            let e = ClipEvent::from_text("pc", i, &format!("其它内容{i}"), false);
            assert!(l.accept(&e));
        }
        let mut again = first.clone();
        again.id = "again-id".into();
        assert!(l.accept(&again), "同内容超出最近 5 条后应可再次接受");
    }
}
