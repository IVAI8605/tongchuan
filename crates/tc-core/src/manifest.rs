use crate::frame::BLOCK_SIZE;
use crate::hash::{hash_bytes, hash_file};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceInfo {
    pub id: String,
    pub name: String,
    pub platform: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Root {
    pub id: String,
    pub name: String,
    /// 发送端意图；接收端在 MANIFEST_ACK 里用 accept/skip 回应，目录级选择性接收靠这一对字段。
    pub mode: String,
    /// 显式列出所有目录（含空目录），空串表示根本身。LocalSend 只枚举文件，空目录必丢。
    pub dirs: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileEntry {
    pub id: String,
    #[serde(rename = "rootId")]
    pub root_id: String,
    #[serde(rename = "relPath")]
    pub rel_path: String,
    pub size: u64,
    pub mtime: i64,
    pub mode: u32,
    pub blake3: String,
    #[serde(rename = "blockHashes")]
    pub block_hashes: Vec<String>,
}

impl FileEntry {
    pub fn block_count(&self) -> u32 {
        ((self.size + BLOCK_SIZE as u64 - 1) / BLOCK_SIZE as u64) as u32
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    #[serde(rename = "txnId")]
    pub txn_id: String,
    pub device: DeviceInfo,
    pub roots: Vec<Root>,
    pub files: Vec<FileEntry>,
    #[serde(rename = "totalBytes")]
    pub total_bytes: u64,
    #[serde(rename = "totalFiles")]
    pub total_files: u64,
}

/// 扫描若干根目录，一次算出清单与所有哈希。
/// read_dir 不保证顺序，这里显式排序，否则发送端算出的 fileId 序号在两端可能对不上。
///
/// 入参既可以是目录，也可以是单个文件：
/// - 目录：以目录名为根，整棵子树一起发（原有行为）
/// - 文件：按父目录分组，以父目录名为根、文件名作为相对路径。
///   这样「从相册选 3 张图」会落到 `dest/Camera/xxx.jpg`，而不是把整个 Camera 目录都发过去。
pub fn build_manifest(paths: &[PathBuf], device: DeviceInfo) -> io::Result<Manifest> {
    build_manifest_with_progress(paths, device, None)
}

/// 带扫描进度的清单构建。回调参数是 (已处理文件数, 已处理字节数)：
/// 大目录要逐个文件算 BLAKE3，没有这个回调时界面在几分钟内完全看不到变化。
///
/// 分两遍走：第一遍只枚举目录、取元数据（顺序决定 fileId，必须稳定且很便宜），
/// 第二遍才把真正耗时的 BLAKE3 并行化。实测 6.9 GB 单线程要 21.5 秒，
/// 是「点下发送到第一块数据出网」这段空窗的主要来源。
pub fn build_manifest_with_progress(
    paths: &[PathBuf],
    device: DeviceInfo,
    mut on_file: Option<&mut dyn FnMut(u64, u64)>,
) -> io::Result<Manifest> {
    let mut roots = Vec::new();
    let mut jobs: Vec<HashJob> = Vec::new();
    let mut total_bytes = 0u64;

    let specs = plan_roots(paths)?;
    for (i, (name, root_path, preset)) in specs.iter().enumerate() {
        let root_id = format!("r{i}");
        let mut dirs = vec![String::new()];
        let mut raw_files: Vec<(String, PathBuf)> = preset.clone();
        // 目录根才需要递归；文件根的文件清单已经在 plan_roots 里备好
        if raw_files.is_empty() {
            collect(root_path, Path::new(""), &mut dirs, &mut raw_files)?;
        }
        dirs.sort();
        raw_files.sort_by(|a, b| a.0.cmp(&b.0));

        for (rel, abs) in raw_files {
            let meta = fs::metadata(&abs)?;
            let size = meta.len();
            let mtime = meta
                .modified()?
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs() as i64;
            total_bytes += size;
            jobs.push(HashJob {
                id: format!("f{}", jobs.len()),
                root_id: root_id.clone(),
                rel_path: rel,
                abs,
                size,
                mtime,
                mode: file_mode(&meta),
            });
        }

        roots.push(Root {
            id: root_id,
            name: name.clone(),
            mode: "send".into(),
            dirs,
        });
    }

    let total_files = jobs.len() as u64;
    let hashed = hash_jobs(&jobs, &mut on_file)?;
    let files: Vec<FileEntry> = jobs
        .into_iter()
        .zip(hashed)
        .map(|(j, (blake3, block_hashes))| FileEntry {
            id: j.id,
            root_id: j.root_id,
            rel_path: j.rel_path,
            size: j.size,
            mtime: j.mtime,
            mode: j.mode,
            blake3,
            block_hashes,
        })
        .collect();

    Ok(Manifest {
        txn_id: new_txn_id(),
        device,
        roots,
        total_files,
        total_bytes,
        files,
    })
}

/// 待哈希的一个文件。把元数据和路径先定下来，
/// 后面多线程只负责「读文件、算哈希」，不去碰任何共享的可变状态。
struct HashJob {
    id: String,
    root_id: String,
    rel_path: String,
    abs: PathBuf,
    size: u64,
    mtime: i64,
    mode: u32,
}

/// 并行算所有文件的 BLAKE3。线程数按 CPU 核数取但上限 7：
/// 手机 8 核（1 超大 + 3 大 + 4 小），留一颗小核给系统中断、FUSE 转发和 Wi-Fi 收发。
/// 冷缓存实测：7 线程 4.02 s、8 线程 4.04 s——第 8 根线程没有收益，
/// 所以停在上限 7，把最后一颗小核留给系统和 FUSE（详见规划文档 §12.8）。
fn hash_jobs(
    jobs: &[HashJob],
    on_file: &mut Option<&mut dyn FnMut(u64, u64)>,
) -> io::Result<Vec<(String, Vec<String>)>> {
    if jobs.is_empty() {
        return Ok(Vec::new());
    }
    let total = jobs.len();
    let cores = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);
    let workers = cores.clamp(1, 7).min(total);

    let next = AtomicUsize::new(0);
    let done = AtomicUsize::new(0);
    let done_bytes = AtomicU64::new(0);
    let error: Mutex<Option<io::Error>> = Mutex::new(None);
    let out: Mutex<Vec<Option<(String, Vec<String>)>>> = Mutex::new(vec![None; total]);

    std::thread::scope(|s| {
        for idx in 0..workers {
            // 命名线程：排查时用 `top -H` 能直接分辨哈希线程，而不是一堆 unnamed。
            // 优先级不在这里调——前台 App 的线程 nice 已经是 -10（系统给的），
            // 再往下降需要 CAP_SYS_NICE，普通应用拿不到。
            // 显式取引用再 move 进闭包：直接 move 会把原子计数器本身移走，后续线程就用不了了。
            let (next, done, done_bytes, error, out) = (&next, &done, &done_bytes, &error, &out);
            let builder = std::thread::Builder::new().name(format!("tc-hash-{idx}"));
            // 极端资源不足时线程可能起不来，退回匿名线程继续跑，不影响正确性
            let _ = builder.spawn_scoped(s, move || loop {
                let i = next.fetch_add(1, Ordering::Relaxed);
                if i >= total {
                    break;
                }
                // 任意一个文件失败就够了，其余线程尽快退出，不再白读盘
                if error.lock().map(|g| g.is_some()).unwrap_or(true) {
                    break;
                }
                match hash_file(&jobs[i].abs) {
                    Ok(v) => {
                        if let Ok(mut g) = out.lock() {
                            g[i] = Some(v);
                        }
                        done.fetch_add(1, Ordering::Relaxed);
                        done_bytes.fetch_add(jobs[i].size, Ordering::Relaxed);
                    }
                    Err(e) => {
                        if let Ok(mut g) = error.lock() {
                            if g.is_none() {
                                *g = Some(e);
                            }
                        }
                        break;
                    }
                }
            });
        }
        // 进度回调只能在主线程调用：FnMut 不是 Sync，不能跨线程共享；
        // 这里轮流看一眼原子计数，间隔 150ms，界面上足够跟手又不浪费 CPU
        loop {
            let d = done.load(Ordering::Relaxed);
            if let Some(cb) = on_file.as_mut() {
                cb(d as u64, done_bytes.load(Ordering::Relaxed));
            }
            if d >= total || error.lock().map(|g| g.is_some()).unwrap_or(true) {
                break;
            }
            std::thread::sleep(Duration::from_millis(150));
        }
    });

    if let Some(e) = error.into_inner().unwrap_or(None) {
        return Err(e);
    }
    out.into_inner()
        .unwrap_or_default()
        .into_iter()
        .map(|v| v.ok_or_else(|| io::Error::new(io::ErrorKind::Other, "hash job missing")))
        .collect()
}

/// 统一的根规划：目录 → 自身；文件 → 按父目录归组。
/// 返回 (根名, 根基准目录, 预置文件清单)。
/// build_manifest 与发送端的源路径映射都依赖这一份规则，避免两边各算一套导致 fileId 对不上。
pub fn plan_roots(paths: &[PathBuf]) -> io::Result<Vec<(String, PathBuf, Vec<(String, PathBuf)>)>> {
    let mut specs: Vec<(String, PathBuf, Vec<(String, PathBuf)>)> = Vec::new();
    let mut file_groups: Vec<(PathBuf, Vec<PathBuf>)> = Vec::new();
    for p in paths {
        if p.is_dir() {
            let name = p
                .file_name()
                .and_then(|s| s.to_str())
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "bad root name"))?
                .to_string();
            specs.push((name, p.clone(), Vec::new()));
        } else if p.is_file() {
            let parent = p.parent().unwrap_or_else(|| Path::new(".")).to_path_buf();
            match file_groups.iter_mut().find(|(d, _)| *d == parent) {
                Some((_, list)) => list.push(p.clone()),
                None => file_groups.push((parent, vec![p.clone()])),
            }
        } else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("不是文件也不是目录: {}", p.display()),
            ));
        }
    }
    for (parent, list) in file_groups {
        let name = parent
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("共享")
            .to_string();
        let mut items = Vec::new();
        for f in list {
            let fname = f
                .file_name()
                .and_then(|s| s.to_str())
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "bad file name"))?
                .to_string();
            items.push((fname, f));
        }
        specs.push((name, parent, items));
    }
    Ok(specs)
}

fn collect(
    root: &Path,
    rel: &Path,
    dirs: &mut Vec<String>,
    out: &mut Vec<(String, PathBuf)>,
) -> io::Result<()> {
    for entry in fs::read_dir(root.join(rel))? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name
            .to_str()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "non-utf8 file name"))?
            .to_string();
        let child_rel = rel.join(&name);
        let rel_text = child_rel.to_string_lossy().replace('\\', "/");
        let kind = entry.file_type()?;
        if kind.is_dir() {
            dirs.push(rel_text);
            collect(root, &child_rel, dirs, out)?;
        } else if kind.is_file() {
            // 符号链接等其它类型直接跳过，避免接收端跟随链接写到目标目录之外
            out.push((rel_text, entry.path()));
        }
    }
    Ok(())
}

#[cfg(unix)]
fn file_mode(meta: &fs::Metadata) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    meta.permissions().mode()
}

#[cfg(not(unix))]
fn file_mode(_meta: &fs::Metadata) -> u32 {
    0o644 // Windows 没有 POSIX 权限位，落盘时由系统自行决定
}

fn new_txn_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("{nanos:x}-{:x}", std::process::id())
}

/// 设备 ID 用名称哈希，重启后保持稳定，便于以后做「已配对设备」表。
pub fn local_device() -> DeviceInfo {
    let name = std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "unknown".into());
    let id = hash_bytes(name.as_bytes())[..16].to_string();
    DeviceInfo {
        id,
        name,
        platform: std::env::consts::OS.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_count_matches_hashes() {
        let e = FileEntry {
            id: "f0".into(),
            root_id: "r0".into(),
            rel_path: "a.bin".into(),
            size: (BLOCK_SIZE * 2 + 1) as u64,
            mtime: 0,
            mode: 0o644,
            blake3: String::new(),
            block_hashes: vec![String::new(); 3],
        };
        assert_eq!(e.block_count(), 3);
    }

    #[test]
    fn empty_file_has_zero_blocks() {
        let e = FileEntry {
            id: "f0".into(),
            root_id: "r0".into(),
            rel_path: "a.bin".into(),
            size: 0,
            mtime: 0,
            mode: 0o644,
            blake3: String::new(),
            block_hashes: vec![],
        };
        assert_eq!(e.block_count(), 0);
    }
}
