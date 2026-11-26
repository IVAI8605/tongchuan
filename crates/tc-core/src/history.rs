//! 发送端传输历史：每次发送的结果追加成一行 JSON，供 `tc history` 查看、`tc resend` 重跑。
//! 只追加不删除——记录是用户查账的依据，过期条目只是不再默认列出。
//!
//! 刻意不保存配对码：配对码是本次会话的临时凭证，落盘等于把「谁能连我」写进文件。
//! 重传若对端仍要求配对，由用户现场重新提供，而不是默默复用旧值。

use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub const DEFAULT_LIMIT: usize = 20;
const FILE_NAME: &str = "send-history.jsonl";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SendRecord {
    pub id: String,
    /// 记录时刻，Unix 秒；展示层自行格式化
    pub ts: i64,
    /// 目标 `IP:端口`
    pub to: String,
    pub paths: Vec<PathBuf>,
    /// TLS 目标指纹；明文发送为 None。重传靠它还原当时的连接方式。
    #[serde(rename = "peerFp", default, skip_serializing_if = "Option::is_none")]
    pub peer_fp: Option<String>,
    pub files: u64,
    pub bytes: u64,
    #[serde(rename = "blocksSent", default)]
    pub blocks_sent: u64,
    pub verdict: String,
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// 由哪条记录重传而来，用于把重试链串起来
    #[serde(rename = "retryOf", default, skip_serializing_if = "Option::is_none")]
    pub retry_of: Option<String>,
    /// v0.17：本场判定失败的文件。空表不写字段，旧记录读出来也是空表。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub failed: Vec<FailedFile>,
}

/// v0.17：一个失败文件的记录。
/// `id`/`rel_path` 来自接收端的校验结果；`path` 是发送端把它映射回本机的绝对路径，
/// `tc resend --failed` 只靠这个字段重发。旧接收端不返回该列表时为空。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FailedFile {
    #[serde(default)]
    pub id: String,
    #[serde(rename = "relPath", default)]
    pub rel_path: String,
    #[serde(default)]
    pub reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

/// 历史默认落点。可用 `TC_HOME` 覆盖（验证脚本靠它把产物关进项目内 .tmp）。
pub fn default_dir() -> PathBuf {
    if let Ok(v) = std::env::var("TC_HOME") {
        if !v.trim().is_empty() {
            return PathBuf::from(v).join("history");
        }
    }
    platform_default().unwrap_or_else(|| PathBuf::from("history"))
}

#[cfg(windows)]
fn platform_default() -> Option<PathBuf> {
    std::env::var("LOCALAPPDATA")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .map(|v| PathBuf::from(v).join("tongchuan").join("history"))
}

#[cfg(not(windows))]
fn platform_default() -> Option<PathBuf> {
    std::env::var("HOME")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .map(|v| PathBuf::from(v).join(".local/share/tongchuan/history"))
}

/// 追加一条记录。返回实际写入的文件路径，方便调用方在日志里指明去向。
pub fn record(dir: &Path, rec: &SendRecord) -> io::Result<PathBuf> {
    fs::create_dir_all(dir)?;
    let path = dir.join(FILE_NAME);
    let mut f = OpenOptions::new().create(true).append(true).open(&path)?;
    let line = serde_json::to_string(rec).map_err(|e| io::Error::other(e.to_string()))?;
    // 一次 write_all 落整行：writeln! 的正文+\n 两次 write 会与同时写的进程交错
    f.write_all(format!("{line}\n").as_bytes())?;
    f.flush()?;
    Ok(path)
}

/// 读全部记录后按时间倒序取前 limit 条（文件是追加写的，倒序即最新在前）。
/// 损坏行直接跳过：历史文件可能被外部编辑器改坏，但不能因此让整个命令不可用。
pub fn list(dir: &Path, limit: usize) -> io::Result<Vec<SendRecord>> {
    let path = dir.join(FILE_NAME);
    if !path.exists() {
        return Ok(Vec::new());
    }
    let text = fs::read_to_string(&path)?;
    let mut out: Vec<SendRecord> = text
        .lines()
        .filter_map(|l| serde_json::from_str::<SendRecord>(l).ok())
        .collect();
    out.reverse();
    out.truncate(limit);
    Ok(out)
}

/// `last` 取最新一条，否则按 id 前缀匹配（完整 id 很长，允许只输入前几位）。
/// 前缀命中多条时拒绝执行：重传会真的往网络上发数据，不能靠猜。
pub fn find(dir: &Path, key: &str) -> io::Result<SendRecord> {
    let all = list(dir, usize::MAX)?;
    if key == "last" {
        return all
            .into_iter()
            .next()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "history is empty"));
    }
    let mut hits: Vec<SendRecord> = all.into_iter().filter(|r| r.id.starts_with(key)).collect();
    match hits.len() {
        0 => Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("no record matches id {key}"),
        )),
        1 => Ok(hits.remove(0)),
        n => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("id prefix {key} matches {n} records, use a longer prefix"),
        )),
    }
}

/// 记录 id：时间戳 + 进程号，同一秒同一进程也不会撞（纳秒参与）。
pub fn new_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("{nanos:x}-{:x}", std::process::id())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(id: &str, ok: bool) -> SendRecord {
        SendRecord {
            id: id.into(),
            ts: 100,
            to: "127.0.0.1:53318".into(),
            paths: vec![PathBuf::from("样本")],
            peer_fp: None,
            files: 1,
            bytes: 2,
            blocks_sent: 1,
            verdict: if ok { "OK".into() } else { "ERROR".into() },
            ok,
            reason: None,
            retry_of: None,
            failed: Vec::new(),
        }
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("tc-history-{name}-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn records_are_appended_in_order_and_listed_newest_first() {
        let dir = temp_dir("order");
        record(&dir, &sample("a1", true)).unwrap();
        record(&dir, &sample("a2", false)).unwrap();

        let all = list(&dir, 10).unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].id, "a2");
        assert_eq!(all[1].id, "a1");

        assert_eq!(find(&dir, "last").unwrap().id, "a2");
        assert_eq!(find(&dir, "a1").unwrap().id, "a1");
    }

    /// 外部把某行改坏时，其余记录仍要可读，且文件不能被重写（历史不删不改）
    #[test]
    fn malformed_line_is_skipped_but_file_is_kept() {
        let dir = temp_dir("broken");
        let path = record(&dir, &sample("b1", true)).unwrap();
        let mut text = fs::read_to_string(&path).unwrap();
        text.push_str("{这不是合法 JSON}\n");
        fs::write(&path, &text).unwrap();
        record_wrong_type(&dir);

        let all = list(&dir, 10).unwrap();
        assert_eq!(all.len(), 2, "合法两条应被读出");
        let raw = fs::read_to_string(&path).unwrap();
        assert!(raw.contains("这不是合法 JSON"), "坏行必须原样保留");
    }

    /// 追加一条缺字段的记录（模拟旧版本文件），读取时用 default 补齐而不是整条丢弃
    fn record_wrong_type(dir: &Path) {
        let path = dir.join(FILE_NAME);
        let mut f = OpenOptions::new().append(true).open(&path).unwrap();
        writeln!(
            f,
            r#"{{"id":"b2","ts":101,"to":"127.0.0.1:1","paths":[],"files":0,"bytes":0,"verdict":"OK","ok":true}}"#
        )
        .unwrap();
    }

    /// v0.17：失败清单要能原样读回，旧记录没有该字段时按空表处理。
    #[test]
    fn failed_files_roundtrip_and_legacy_rows_have_empty_list() {
        let dir = temp_dir("failed");
        let mut rec = sample("f1", false);
        rec.failed = vec![FailedFile {
            id: "x".into(),
            rel_path: "相册/a.jpg".into(),
            reason: "root hash mismatch".into(),
            path: Some("D:/源/相册/a.jpg".into()),
        }];
        record(&dir, &rec).unwrap();

        let got = find(&dir, "f1").unwrap();
        assert_eq!(got.failed.len(), 1);
        assert_eq!(got.failed[0].rel_path, "相册/a.jpg");
        assert_eq!(got.failed[0].path.as_deref(), Some("D:/源/相册/a.jpg"));

        // 旧记录（完全没有 failed 字段）读取后应为空表，不报错
        let path = dir.join("send-history.jsonl");
        let mut f = OpenOptions::new().append(true).open(&path).unwrap();
        writeln!(
            f,
            r#"{{"id":"old","ts":1,"to":"127.0.0.1:1","paths":[],"files":0,"bytes":0,"verdict":"ERROR","ok":false}}"#
        )
        .unwrap();
        assert!(find(&dir, "old").unwrap().failed.is_empty());
    }

    #[test]
    fn ambiguous_prefix_is_rejected() {
        let dir = temp_dir("ambiguous");
        record(&dir, &sample("zz-1", true)).unwrap();
        record(&dir, &sample("zz-2", true)).unwrap();
        let err = find(&dir, "zz").unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert!(find(&dir, "zz-2").is_ok());
    }

    #[test]
    fn missing_history_dir_lists_empty() {
        let dir = std::env::temp_dir().join("tc-history-absent-does-not-exist");
        assert!(list(&dir, 5).unwrap().is_empty());
        assert!(find(&dir, "last").is_err());
    }
}
