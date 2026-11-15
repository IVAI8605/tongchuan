//! v0.19：接收端哈希缓存。
//!
//! 重复往同一台机器发同一个目录时，接收端原本要对每个「同名同大小」的已有文件重读一遍、
//! 重算 BLAKE3 才能确认「确实一样、可以跳过」。这张表记住「路径 + 大小 + 修改时间 → 指纹」，
//! 三项全对就直接采信，省掉读盘。
//!
//! 默认关闭：内容变了、但大小与修改时间都没变（例如 `cp -p` 覆盖或从备份还原）时，
//! 缓存会把「不一样」误判成「一样」而跳过——那是静默漏传，与项目红线冲突，
//! 所以只作为高级开关，由明确知道风险的用户手动打开。
//!
//! 存储是一行一条 JSON 的追加文件；坏行跳过、后写覆盖先写。
//! 缓存只影响速度，任何读写失败都必须降级为「照旧重算」，绝不能挡住传输。

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// 缓存里的一行：落点路径 + 大小 + 修改时间 → 校验通过时算出的指纹。
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Row {
    path: String,
    size: u64,
    mtime: i64,
    blake3: String,
}

struct State {
    rows: HashMap<String, Row>,
    /// 文件里已有的行数（含重复与被覆盖的旧行），用来判断是否需要压缩
    lines: usize,
    /// 追加句柄；建不出来时仍能读缓存，只是本次记录不落盘（下次重算，不影响正确性）
    sink: Option<File>,
}

/// 线程安全的哈希缓存。daemon 里多场会话共享同一份，记录与查询都在锁内完成。
pub struct HashCache {
    path: PathBuf,
    state: Mutex<State>,
}

impl HashCache {
    /// 从文件加载。文件不存在、读失败、个别行损坏都按「没有这条」处理，
    /// 因为缓存缺失只会让下次多读一遍盘，不能反过来阻止服务启动。
    pub fn load(path: &Path) -> Self {
        let mut rows = HashMap::new();
        let mut lines = 0usize;
        if let Ok(text) = fs::read_to_string(path) {
            for line in text.lines() {
                if line.trim().is_empty() {
                    continue;
                }
                lines += 1;
                if let Ok(row) = serde_json::from_str::<Row>(line) {
                    // 同一路径后写覆盖先写：与追加写入的顺序语义一致
                    rows.insert(row.path.clone(), row);
                }
            }
        }
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let sink = OpenOptions::new().append(true).create(true).open(path).ok();
        let cache = Self {
            path: path.to_path_buf(),
            state: Mutex::new(State { rows, lines, sink }),
        };
        cache.compact_if_bloated();
        cache
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 命中条件：路径存在且大小、修改时间都与记录一致。返回当时算出的指纹。
    pub fn get(&self, file: &Path, size: u64, mtime: i64) -> Option<String> {
        let key = key_of(file);
        let st = self.state.lock().ok()?;
        let row = st.rows.get(&key)?;
        (row.size == size && row.mtime == mtime).then(|| row.blake3.clone())
    }

    /// 记一条校验通过的指纹。三项都与旧记录相同就不重复追加，避免一次会话把同一行写多遍。
    pub fn record(&self, file: &Path, size: u64, mtime: i64, blake3: &str) {
        let key = key_of(file);
        let Ok(mut st) = self.state.lock() else {
            return;
        };
        if let Some(old) = st.rows.get(&key) {
            if old.size == size && old.mtime == mtime && old.blake3.eq_ignore_ascii_case(blake3) {
                return;
            }
        }
        let row = Row {
            path: key.clone(),
            size,
            mtime,
            blake3: blake3.to_string(),
        };
        let Ok(line) = serde_json::to_string(&row) else {
            return;
        };
        if let Some(sink) = st.sink.as_mut() {
            // 追加失败就放弃落盘：内存里仍然有效，本次会话还能命中，下次重算而已
            if writeln!(sink, "{line}").is_err() {
                st.sink = None;
            } else {
                st.lines += 1;
            }
        }
        st.rows.insert(key, row);
    }

    /// 当前有效条目数（测试与日志用）。
    pub fn len(&self) -> usize {
        self.state.lock().map(|s| s.rows.len()).unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// 追加写会让同一路径的旧记录越积越多；重复行明显多于有效条目时整份重写一次。
    /// 直接截断重写、不建临时文件：缓存本来就是可丢弃数据，中途失败最多下次重算。
    fn compact_if_bloated(&self) {
        let Ok(mut st) = self.state.lock() else {
            return;
        };
        if st.lines <= st.rows.len() + 4096 {
            return;
        }
        let mut text = String::new();
        for row in st.rows.values() {
            if let Ok(line) = serde_json::to_string(row) {
                text.push_str(&line);
                text.push('\n');
            }
        }
        // Windows 下目标文件仍被追加句柄占用时无法截断，先放掉句柄再写
        st.sink = None;
        if fs::write(&self.path, text.as_bytes()).is_ok() {
            st.lines = st.rows.len();
        }
        st.sink = OpenOptions::new().append(true).create(true).open(&self.path).ok();
    }
}

fn key_of(file: &Path) -> String {
    file.to_string_lossy().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_cache(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("tc-hashcache-{}-{}", name, std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        dir.join("hash-cache.jsonl")
    }

    /// 三项全对才命中；大小或时间任一不同都必须落空，让调用方重新算。
    #[test]
    fn hit_requires_path_size_and_mtime() {
        let path = temp_cache("hit");
        let cache = HashCache::load(&path);
        let file = path.with_file_name("a.bin");
        cache.record(&file, 100, 1_700_000_000, "abc");

        assert_eq!(
            cache.get(&file, 100, 1_700_000_000).as_deref(),
            Some("abc")
        );
        assert_eq!(cache.get(&file, 101, 1_700_000_000), None, "大小变了");
        assert_eq!(cache.get(&file, 100, 1_700_000_001), None, "时间变了");
        assert_eq!(cache.get(&file.with_file_name("b.bin"), 100, 1_700_000_000), None);
    }

    /// 重启后能从文件恢复；坏行跳过，后面的好行照常生效。
    #[test]
    fn reload_skips_corrupt_lines() {
        let path = temp_cache("reload");
        fs::write(
            &path,
            "这不是 JSON\n{\"path\":\"/x/a\",\"size\":1,\"mtime\":2,\"blake3\":\"aa\"}\n",
        )
        .unwrap();
        let cache = HashCache::load(&path);
        assert_eq!(cache.len(), 1);
        assert_eq!(cache.get(Path::new("/x/a"), 1, 2).as_deref(), Some("aa"));
    }

    /// 同一路径重复记录同一份内容不应反复追加；内容或时间变化则追加新行并覆盖旧值。
    #[test]
    fn record_dedupes_identical_rows() {
        let path = temp_cache("dedupe");
        let cache = HashCache::load(&path);
        let file = Path::new("/x/a");
        cache.record(file, 1, 2, "aa");
        cache.record(file, 1, 2, "aa");
        assert_eq!(fs::read_to_string(&path).unwrap().lines().count(), 1);

        cache.record(file, 1, 3, "bb");
        assert_eq!(fs::read_to_string(&path).unwrap().lines().count(), 2);
        assert_eq!(cache.get(file, 1, 3).as_deref(), Some("bb"));
    }
}
