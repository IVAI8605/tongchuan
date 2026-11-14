use crate::frame::BLOCK_SIZE;
use crate::hash::hash_reader;
use crate::manifest::FileEntry;
use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

/// 位图头：`#tc-map v2 <块大小>`。块号只有配合块大小才有意义，
/// 头部就是为了在块大小变化（4 MiB → 8 MiB）后能判定旧位图不可复用。
const MAP_HEADER: &str = "#tc-map v2";

fn map_header() -> String {
    format!("{MAP_HEADER} {BLOCK_SIZE}\n")
}

/// 半成品文件 + 已收块位图。两者分开存，进程重启后能从位图直接算出缺口，不用重传整文件。
pub struct PartFile {
    /// 只用于日志和错误信息。Android SAF 下这不是可访问的真实路径，只有 fd 可用。
    pub path: PathBuf,
    pub map_path: PathBuf,
    total_blocks: u32,
    file_size: u64,
    received: HashSet<u32>,
    file: File,
    /// 位图追加写句柄：每收一块只 append 一行块号（O(1)），不再整份重写。
    /// 老写法在 250 GB 量级会累计多写约 12 GB（见传输提速-规划 §13.4）。
    /// 解析路径不变——旧格式（排序后的全量块号）与新格式（追加行）都是一行一个数字。
    map_file: Option<File>,
}

impl PartFile {
    pub fn open(dest: &Path, entry: &FileEntry) -> io::Result<Self> {
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)?;
        }
        let map_path = PathBuf::from(format!("{}.map", dest.to_string_lossy()));
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open(dest)?;
        Self::from_file(file, dest.to_path_buf(), map_path, entry)
    }

    /// Android SAF 入口：文件句柄由宿主（Kotlin）用 openFileDescriptor 拿到后传进来，
    /// 位图另行落在应用私有目录，所以 label 只当标识、map_path 指向真实可写的路径。
    pub fn from_file(
        file: File,
        label: PathBuf,
        map_path: PathBuf,
        entry: &FileEntry,
    ) -> io::Result<Self> {
        let total_blocks = entry.block_count();
        if let Some(parent) = map_path.parent() {
            fs::create_dir_all(parent)?;
        }

        // 位图第一行是头，记录块大小。块号只有在块大小相同时才有意义：
        // v0.5.3 把块从 4 MiB 改成 8 MiB，旧位图若被当成可续传，会按错误偏移写文件（数据损坏）。
        // 没有头的位图一律视为不可用——宁可重传，也不能写坏用户文件。
        let mut received = HashSet::new();
        let mut map_usable = false;
        let text = fs::read_to_string(&map_path).unwrap_or_default();
        if !text.trim().is_empty() {
            let mut lines = text.lines();
            let header_ok = lines
                .next()
                .map(|h| {
                    h.starts_with(MAP_HEADER)
                        && h.split_whitespace()
                            .nth(2)
                            .and_then(|s| s.parse::<usize>().ok())
                            == Some(BLOCK_SIZE)
                })
                .unwrap_or(false);
            if header_ok {
                let mut valid = true;
                for line in lines {
                    match line.trim().parse::<u32>() {
                        Ok(i) if i < total_blocks => {
                            received.insert(i);
                        }
                        _ => {
                            valid = false;
                            break;
                        }
                    }
                }
                if !valid {
                    received.clear();
                } else {
                    map_usable = true;
                }
            }
        }
        // 位图不可用时先重写头：否则新的追加行会接在 "DONE" 或旧块号后面，
        // 下次启动解析仍然失败，看起来像「每次都要重传」。
        if !map_usable {
            fs::write(&map_path, map_header())?;
        }
        // 追加句柄常驻，避免每块重复 open/close；写入量从 O(块数) 降到每块一行
        let map_file = OpenOptions::new().append(true).create(true).open(&map_path)?;

        if received.is_empty() {
            // 预分配到最终大小，这样按块号 seek 写入不会留下空洞。
            // SAF 的 fd 未必支持 ftruncate，失败不算错：每块都按绝对偏移写，写完最后一块大小自然就对了。
            let _ = file.set_len(entry.size);
        } else {
            let len = file.metadata()?.len();
            if len != entry.size {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "part file size does not match manifest; refusing to resume",
                ));
            }
        }

        Ok(Self {
            path: label,
            map_path,
            total_blocks,
            file_size: entry.size,
            received,
            file,
            map_file: Some(map_file),
        })
    }

    pub fn has(&self, idx: u32) -> bool {
        self.received.contains(&idx)
    }

    pub fn missing(&self) -> Vec<u32> {
        (0..self.total_blocks)
            .filter(|i| !self.received.contains(i))
            .collect()
    }

    pub fn complete(&self) -> bool {
        self.received.len() as u32 == self.total_blocks
    }

    pub fn write_block(&mut self, idx: u32, data: &[u8]) -> io::Result<()> {
        if idx >= self.total_blocks {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "block index out of range",
            ));
        }
        let expected = expected_len(idx, self.total_blocks, self.file_size);
        if data.len() != expected {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "block {idx} length mismatch: got {}, want {expected}",
                    data.len()
                ),
            ));
        }
        self.file.seek(SeekFrom::Start(idx as u64 * BLOCK_SIZE as u64))?;
        self.file.write_all(data)?;
        self.file.flush()?;
        self.received.insert(idx);
        self.append_map(idx)
    }

    /// 每收一块只追加一行块号。断电/被杀后仍能从缺口续传；
    /// 最坏情况是丢掉最后一行，代价只是重传那一个 4 MiB 块，不影响正确性。
    fn append_map(&mut self, idx: u32) -> io::Result<()> {
        if let Some(f) = self.map_file.as_mut() {
            f.write_all(format!("{idx}\n").as_bytes())?;
            f.flush()?;
        }
        Ok(())
    }

    /// 块哈希 + 整文件根哈希双重校验；不靠肉眼看文件大小。
    /// 必须复用已经打开的句柄回读：SAF 只有 fd，没有可以再 open 一次的路径。
    pub fn verify(&mut self, entry: &FileEntry) -> io::Result<()> {
        if !self.complete() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "not all blocks received",
            ));
        }
        self.file.seek(SeekFrom::Start(0))?;
        let (root, blocks) = hash_reader(&self.file)?;
        if root != entry.blake3 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "root hash mismatch",
            ));
        }
        if blocks.len() != entry.block_hashes.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "block count mismatch",
            ));
        }
        for (i, (got, want)) in blocks.iter().zip(&entry.block_hashes).enumerate() {
            if got != want {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("block {i} hash mismatch"),
                ));
            }
        }
        Ok(())
    }

    /// 校验通过后才改成正式文件名；同名时加 (n)，默认永不覆盖。
    pub fn finalize(&mut self, final_path: &Path) -> io::Result<PathBuf> {
        let target = unique_path(final_path);
        fs::rename(&self.path, &target)?;
        self.mark_done()?;
        Ok(target)
    }

    /// 强制重传（v0.12）：本次文件保留源文件名，目标位置的同名旧文件按 `(n)` 改名让位。
    ///
    /// 顺序刻意是「先给旧文件找好空闲名字 → 改名 → 再把新文件放到原名」：
    /// 任何一步失败都不会让用户原有的文件消失；新文件放不到原名时把旧文件改回去，
    /// 最多是这次传输失败，不会丢数据。
    pub fn finalize_aside(&mut self, final_path: &Path) -> io::Result<PathBuf> {
        if final_path.exists() {
            let aside = unique_path(final_path);
            fs::rename(final_path, &aside)?;
            if let Err(e) = fs::rename(&self.path, final_path) {
                // 回滚：旧文件改回原名。改不回去也不能再覆盖一次，只能把错误往上抛
                let _ = fs::rename(&aside, final_path);
                return Err(e);
            }
        } else {
            fs::rename(&self.path, final_path)?;
        }
        self.mark_done()?;
        Ok(final_path.to_path_buf())
    }

    /// SAF 下宿主已经把数据直接写进了正式文档，Rust 侧无法 rename，只能刷新并标记完成。
    pub fn finish_in_place(&mut self) -> io::Result<()> {
        self.file.sync_all()?;
        self.mark_done()
    }

    fn mark_done(&mut self) -> io::Result<()> {
        // 位图不删（用户数据/审计一律不彻底删除），改成 DONE 标记，
        // 下次同名传输时 open() 会因解析失败而按全新会话重建，不会误续到旧进度。
        // 先释放追加句柄再覆盖：Windows 下同一文件还有句柄时 fs::write 会失败（分享冲突）。
        self.map_file = None;
        fs::write(&self.map_path, "DONE")
    }
}

/// 接收端落盘后端。
/// 桌面和 NAS 有真实路径，建目录、改名都是本地操作；Android 走 SAF，只有 fd，
/// 建目录与命名由宿主负责，Rust 侧只承接「按块写 + 校验 + 标记完成」。
/// 把这层差异收在一个 trait 里，会话状态机本身不必感知平台。
pub trait Store {
    fn make_dirs(&self, dirs: &[PathBuf]) -> io::Result<()>;
    fn open_part(&mut self, part_path: &Path, entry: &FileEntry) -> io::Result<PartFile>;
    /// 返回最终落点的展示用标识（SAF 下是文档 URI 或相对路径）。
    fn finalize(&mut self, part: &mut PartFile, final_path: &Path) -> io::Result<String>;
    /// 强制重传：同名旧文件改名让位，本次文件占源文件名。
    /// 只有路径后端能做改名；让位语义没法在 SAF 上兑现时退回普通收尾，绝不覆盖已有文件。
    fn finalize_aside(&mut self, part: &mut PartFile, final_path: &Path) -> io::Result<String> {
        self.finalize(part, final_path)
    }
}

/// 默认后端：路径可用的一切平台（Windows / Linux / NAS）。
pub struct PathStore;

impl Store for PathStore {
    fn make_dirs(&self, dirs: &[PathBuf]) -> io::Result<()> {
        for d in dirs {
            fs::create_dir_all(d)?;
        }
        Ok(())
    }

    fn open_part(&mut self, part_path: &Path, entry: &FileEntry) -> io::Result<PartFile> {
        PartFile::open(part_path, entry)
    }

    fn finalize(&mut self, part: &mut PartFile, final_path: &Path) -> io::Result<String> {
        Ok(part.finalize(final_path)?.to_string_lossy().to_string())
    }

    fn finalize_aside(&mut self, part: &mut PartFile, final_path: &Path) -> io::Result<String> {
        Ok(part
            .finalize_aside(final_path)?
            .to_string_lossy()
            .to_string())
    }
}

fn expected_len(idx: u32, total_blocks: u32, file_size: u64) -> usize {
    if idx + 1 < total_blocks {
        BLOCK_SIZE
    } else {
        (file_size - idx as u64 * BLOCK_SIZE as u64) as usize
    }
}

pub fn unique_path(path: &Path) -> PathBuf {
    if !path.exists() {
        return path.to_path_buf();
    }
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("file");
    let ext = path.extension().and_then(|s| s.to_str());
    for n in 1..10_000u32 {
        let name = match ext {
            Some(e) => format!("{stem} ({n}).{e}"),
            None => format!("{stem} ({n})"),
        };
        let candidate = path.with_file_name(name);
        if !candidate.exists() {
            return candidate;
        }
    }
    path.with_file_name(format!("{}-{}", stem, std::process::id()))
}

/// 读整个 .tcpart 做校验时用；单独留个函数是为了让测试也能验证位图内容。
pub fn read_map_text(path: &Path) -> io::Result<String> {
    let mut s = String::new();
    File::open(path)?.read_to_string(&mut s)?;
    Ok(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_entry(size: u64) -> FileEntry {
        let data = vec![7u8; size as usize];
        let (blake3, block_hashes) = crate::hash::hash_reader(&data[..]).unwrap();
        FileEntry {
            id: "f0".into(),
            root_id: "r0".into(),
            rel_path: "a.bin".into(),
            size,
            mtime: 0,
            mode: 0o644,
            blake3,
            block_hashes,
        }
    }

    #[test]
    fn resume_reads_existing_bitmap() {
        let dir = std::env::temp_dir().join(format!("tc-store-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let dest = dir.join("a.bin.tcpart");
        let entry = sample_entry(16);
        {
            let mut p = PartFile::open(&dest, &entry).unwrap();
            assert!(p.missing().len() == 1);
            p.write_block(0, &[7u8; 16]).unwrap();
        }
        // 重新打开应认为已收齐，不再请求任何块
        let p = PartFile::open(&dest, &entry).unwrap();
        assert!(p.complete());
        assert!(p.missing().is_empty());
    }

    /// 模拟 Android SAF：文件由外部创建好，只把句柄交进来，位图落在另一个目录。
    /// 这条用例是 v0.4 能否在没有真实路径时正常收文件的最低验证。
    #[test]
    fn from_file_accepts_foreign_handle_and_verifies_in_place() {
        let dir = std::env::temp_dir().join(format!("tc-store-fd-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let target = dir.join("文档.bin");
        let map = dir.join("maps").join("文档.bin.map");
        let entry = sample_entry(16);

        let f = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open(&target)
            .unwrap();
        let mut p = PartFile::from_file(f, target.clone(), map.clone(), &entry).unwrap();
        p.write_block(0, &[7u8; 16]).unwrap();
        assert!(p.complete());
        p.verify(&entry).unwrap();
        p.finish_in_place().unwrap();
        assert_eq!(fs::read(&target).unwrap(), vec![7u8; 16]);
        assert_eq!(read_map_text(&map).unwrap(), "DONE");
    }

    /// 位图改成追加写后：行数应等于写入次数（含重复块），集合仍按唯一块号算。
    #[test]
    fn bitmap_appends_one_line_per_block() {
        let dir = std::env::temp_dir().join(format!("tc-store-add-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let dest = dir.join("a.bin.tcpart");
        let map = PathBuf::from(format!("{}.map", dest.to_string_lossy()));
        let size = BLOCK_SIZE as u64 * 2 + 10;
        let entry = sample_entry(size);
        let mut p = PartFile::open(&dest, &entry).unwrap();
        p.write_block(0, &vec![7u8; BLOCK_SIZE]).unwrap();
        p.write_block(1, &vec![7u8; BLOCK_SIZE]).unwrap();
        // 重复写同一块（续传后重传）：追加第二行，但集合里仍只有一个 1
        p.write_block(1, &vec![7u8; BLOCK_SIZE]).unwrap();
        drop(p);

        let text = read_map_text(&map).unwrap();
        assert_eq!(text.lines().count(), 4, "头 + 每块一行");
        assert!(text.starts_with(MAP_HEADER), "位图必须带块大小头");
        let p = PartFile::open(&dest, &entry).unwrap();
        assert!(!p.complete());
        assert_eq!(p.missing(), vec![2]);
    }

    /// v0.5.2 及更早的位图没有头、且块是 4 MiB。块号含义已经不同，
    /// 必须整份丢弃重传——按旧块号续传会写到错误偏移，那是数据损坏。
    #[test]
    fn legacy_bitmap_without_header_is_discarded() {
        let dir = std::env::temp_dir().join(format!("tc-store-legacy-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let dest = dir.join("a.bin.tcpart");
        let map = PathBuf::from(format!("{}.map", dest.to_string_lossy()));
        let size = BLOCK_SIZE as u64 * 2 + 10;
        let entry = sample_entry(size);
        fs::write(&dest, vec![7u8; size as usize]).unwrap();
        fs::write(&map, "0\n1\n").unwrap();

        let p = PartFile::open(&dest, &entry).unwrap();
        assert_eq!(p.missing().len(), 3, "无头旧位图必须整份重传");
    }

    /// DONE 之后再次收到同名文件，必须完全重传，不能误续到旧进度。
    #[test]
    fn done_marker_forces_fresh_session() {
        let dir = std::env::temp_dir().join(format!("tc-store-done-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let dest = dir.join("a.bin.tcpart");
        let map = PathBuf::from(format!("{}.map", dest.to_string_lossy()));
        let size = BLOCK_SIZE as u64 * 2 + 10;
        let entry = sample_entry(size);
        {
            let mut p = PartFile::open(&dest, &entry).unwrap();
            p.write_block(0, &vec![7u8; BLOCK_SIZE]).unwrap();
            p.finalize(&dir.join("final.bin")).unwrap();
        }
        assert_eq!(read_map_text(&map).unwrap(), "DONE");
        // 数据文件已被 rename 走；同名再来一次时应按全新会话重建，不能读成已收 0 块
        let p = PartFile::open(&dest, &entry).unwrap();
        assert_eq!(p.missing().len(), 3);
    }

    #[test]
    fn unique_path_does_not_overwrite() {
        let dir = std::env::temp_dir().join(format!("tc-unique-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let p = dir.join("a.txt");
        fs::write(&p, b"first").unwrap();
        let q = unique_path(&p);
        assert_ne!(p, q);
        assert!(q.to_string_lossy().contains("(1)"));
    }

    /// v0.12 强制重传：本次文件保留源文件名，旧文件按 (1) 改名让位，绝不覆盖。
    #[test]
    fn force_finalize_moves_old_file_aside_and_keeps_source_name() {
        let dir = std::env::temp_dir().join(format!("tc-aside-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let final_path = dir.join("a.txt");
        fs::write(&final_path, b"old content").unwrap();

        let entry = sample_entry(16);
        let part = dir.join("a.txt.tcpart");
        let mut p = PartFile::open(&part, &entry).unwrap();
        p.write_block(0, &[7u8; 16]).unwrap();
        p.finalize_aside(&final_path).unwrap();

        assert_eq!(fs::read(&final_path).unwrap(), vec![7u8; 16], "本次文件占源文件名");
        assert_eq!(
            fs::read(dir.join("a (1).txt")).unwrap(),
            b"old content",
            "旧文件只改名，内容不变"
        );
    }
}
