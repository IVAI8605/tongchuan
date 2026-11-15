//! 10 分钟待发队列：对方不在线时先把任务存盘，上线后用 tc flush 补发。
//! 过期任务只「不再列出」，文件一律保留——用户数据和审计记录都不彻底删除。

use serde::{Deserialize, Serialize};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub const DEFAULT_WINDOW_MINUTES: i64 = 10;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueuedTask {
    pub id: String,
    pub to: String,
    pub paths: Vec<PathBuf>,
    pub created_at: i64,
    /// TLS 目标指纹；明文任务为 None
    pub fingerprint: Option<String>,
    pub identity_dir: Option<PathBuf>,
    pub pair_code: Option<String>,
    /// v0.16：入队时选的接收落点；旧队列文件没有这个键，按默认落点补发
    #[serde(default)]
    pub dest_id: Option<String>,
}

pub fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

pub fn save(dir: &Path, task: &QueuedTask) -> io::Result<PathBuf> {
    fs::create_dir_all(dir)?;
    let path = dir.join(format!("{}.json", task.id));
    fs::write(
        &path,
        serde_json::to_vec_pretty(task).map_err(|e| io::Error::other(e.to_string()))?,
    )?;
    Ok(path)
}

/// 只返回「未过期且未完成」的任务。过期和已完成的 JSON 都留在磁盘上。
pub fn pending(dir: &Path, window_minutes: i64) -> io::Result<Vec<(PathBuf, QueuedTask)>> {
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    let now = now_secs();
    let cutoff = now - window_minutes * 60;
    let mut out = Vec::new();
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        if done_marker(&path).exists() {
            continue;
        }
        let bytes = match fs::read(&path) {
            Ok(b) => b,
            Err(_) => continue,
        };
        let task: QueuedTask = match serde_json::from_slice(&bytes) {
            Ok(t) => t,
            Err(_) => continue, // 损坏的任务不影响其它任务
        };
        if task.created_at >= cutoff {
            out.push((path, task));
        }
    }
    out.sort_by(|a, b| a.1.created_at.cmp(&b.1.created_at));
    Ok(out)
}

fn done_marker(task_path: &Path) -> PathBuf {
    PathBuf::from(format!("{}.done", task_path.to_string_lossy()))
}

pub fn mark_done(task_path: &Path) -> io::Result<()> {
    fs::write(done_marker(task_path), now_secs().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(age_secs: i64) -> QueuedTask {
        QueuedTask {
            id: format!("t{}", age_secs),
            to: "127.0.0.1:53318".into(),
            paths: vec![],
            created_at: now_secs() - age_secs,
            fingerprint: None,
            identity_dir: None,
            pair_code: None,
            dest_id: None,
        }
    }

    #[test]
    fn expired_and_done_tasks_are_filtered_but_files_remain() {
        let dir = std::env::temp_dir().join(format!("tc-queue-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let fresh = task(0);
        let old = task(11 * 60);
        let p_fresh = save(&dir, &fresh).unwrap();
        let p_old = save(&dir, &old).unwrap();

        let list = pending(&dir, DEFAULT_WINDOW_MINUTES).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].1.id, fresh.id);

        mark_done(&p_fresh).unwrap();
        assert!(pending(&dir, DEFAULT_WINDOW_MINUTES).unwrap().is_empty());
        // 文件一个都没删
        assert!(p_fresh.exists());
        assert!(p_old.exists());
    }
}
