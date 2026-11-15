use crate::frame::BLOCK_SIZE;
use crate::hash::hash_reader;
use crate::manifest::FileEntry;
use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// 位图头：`#tc-map v3 <块大小> <文件大小> <源根哈希>`。
/// v2 只有块大小，无法区分「同名同大小但内容不同」的文件；v3 补上文件大小与源根哈希，
/// 四项全部匹配才允许续传，否则旧半成品按不可信处理。
const MAP_HEADER: &str = "#tc-map v3";

fn map_header(entry: &FileEntry) -> String {
    format!("{MAP_HEADER} {BLOCK_SIZE} {} {}\n", entry.size, entry.blake3)
}

/// 位图头是否与本文件身份一致。只有 v3 且块大小、文件大小、源根哈希全对才算可续传；
/// v2、无头、DONE 标记以及任何一项不符都返回 false。
fn bitmap_matches(text: &str, entry: &FileEntry) -> bool {
    let Some(header) = text.lines().next() else {
        return false;
    };
    let parts: Vec<&str> = header.split_whitespace().collect();
    parts.len() == 5
        && parts[0] == "#tc-map"
        && parts[1] == "v3"
        && parts[2].parse::<usize>().ok() == Some(BLOCK_SIZE)
        && parts[3].parse::<u64>().ok() == Some(entry.size)
        && parts[4].eq_ignore_ascii_case(&entry.blake3)
}

/// 给将被隔离的文件找一个不撞名的后缀。命名规则是 `原文件名.stale-<秒级时间戳>`，
/// 同秒重复时追加 `-2`、`-3`，不覆盖任何已有文件。
fn isolation_target(path: &Path) -> PathBuf {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let base = path
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "part".into());
    for n in 0..1000u32 {
        let name = if n == 0 {
            format!("{base}.stale-{secs}")
        } else {
            format!("{base}.stale-{secs}-{n}")
        };
        let candidate = path.with_file_name(name);
        if !candidate.exists() {
            return candidate;
        }
    }
    path.with_file_name(format!("{base}.stale-{secs}-overflow"))
}

/// 把文件改名隔离到旁边。不存在时什么也不做；改名失败原样返回错误，由调用方决定是否继续。
fn isolate(path: &Path) -> io::Result<Option<PathBuf>> {
    if !path.exists() {
        return Ok(None);
    }
    let target = isolation_target(path);
    fs::rename(path, &target)?;
    Ok(Some(target))
}

/// 半成品文件 + 已收块位图。两者分开存，进程重启后能从位图直接算出缺口，不用重传整文件。
pub struct PartFile {
    /// 只用于日志和错误信息。Android SAF 下这不是可访问的真实路径，只有 fd 可用。
    pub path: PathBuf,
    pub map_path: PathBuf,
    total_blocks: u32,
    file_size: u64,
    received: HashSet<u32>,
    /// v0.14：句柄改为可释放。小文件成批到达时，同时打开几千个文件会把 fd 用尽
    /// （实测 NAS 上 5000 个小文件触发 Too many open files）。
    file: Option<File>,
    /// 位图追加写句柄：每收一块只 append 一行块号（O(1)），不再整份重写。
    /// 老写法在 250 GB 量级会累计多写约 12 GB（见传输提速-规划 §13.4）。
    /// 解析路径不变——旧格式（排序后的全量块号）与新格式（追加行）都是一行一个数字。
    map_file: Option<File>,
    /// 句柄能否按路径重新打开：`open()` 可以；Android 的 `from_file` 只有 fd，关掉就没了，
    /// 所以那条路径保持常驻（`close_handles` 对它是空操作）。
    reopen_safe: bool,
    /// v0.18：清单里每块的哈希。新写入的块在写之前先比对，比对通过就记为「已验」，
    /// 收尾时不再整份重读；只有续传继承下来的块才需要回读校验。
    block_hashes: Vec<String>,
    verified_on_write: HashSet<u32>,
}

impl PartFile {
    pub fn open(dest: &Path, entry: &FileEntry) -> io::Result<Self> {
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)?;
        }
        let map_path = PathBuf::from(format!("{}.map", dest.to_string_lossy()));
        // v0.17：先看旧位图身份。身份不符时把半成品与位图一起改名隔离，再按全新传输重建；
        // 必须在打开 dest 之前做——Windows 下文件仍被句柄占用时无法改名。
        if let Ok(text) = fs::read_to_string(&map_path) {
            if !text.trim().is_empty() && !bitmap_matches(&text, entry) {
                // 隔离失败不阻断本次传输：最坏情况是覆盖旧半成品，但不会再信任旧位图
                if let Err(e) = isolate(&map_path) {
                    eprintln!("WARN: 旧位图隔离失败 {}: {e}", map_path.display());
                }
                if let Err(e) = isolate(dest) {
                    eprintln!("WARN: 旧半成品隔离失败 {}: {e}", dest.display());
                }
            }
        }
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open(dest)?;
        let mut part = Self::from_file(file, dest.to_path_buf(), map_path, entry)?;
        // 走真实路径创建的半成品文件，句柄可以随时关掉再按路径重开
        part.reopen_safe = true;
        Ok(part)
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

        // 位图第一行是头，记录块大小、文件大小与源根哈希。四项全对才允许续传：
        // 块大小变了会按错误偏移写（v0.5.3 的 4→8 MiB）；同名同大小但内容不同的文件
        // 没有根哈希就分不出来，会把旧块当成新文件的进度（v0.17 修的就是这条）。
        // 没有 v3 头或任何一项不符，一律视为不可用——宁可重传，也不能写坏用户文件。
        let mut received = HashSet::new();
        let mut map_usable = false;
        let text = fs::read_to_string(&map_path).unwrap_or_default();
        if !text.trim().is_empty() {
            if bitmap_matches(&text, entry) {
                let mut valid = true;
                for line in text.lines().skip(1) {
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
            } else {
                // SAF 入口拿不到可改名的半成品路径，只能先把位图隔离；数据本身会按缺块全量覆盖
                if let Err(e) = isolate(&map_path) {
                    eprintln!("WARN: 旧位图隔离失败 {}: {e}", map_path.display());
                }
            }
        }
        // 位图不可用时先重写头：否则新的追加行会接在 "DONE" 或旧块号后面，
        // 下次启动解析仍然失败，看起来像「每次都要重传」。
        if !map_usable {
            fs::write(&map_path, map_header(entry))?;
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
            file: Some(file),
            map_file: Some(map_file),
            // 默认按「句柄不可重开」处理，由 open() 显式打开这个能力
            reopen_safe: false,
            block_hashes: entry.block_hashes.clone(),
            verified_on_write: HashSet::new(),
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

    /// v0.14：释放这个半成品文件占用的句柄，位图与已收块状态都保留。
    /// 只有能按路径重开的场合才真正关闭；Android 的 fd 一关就回不来了，那里保持常驻。
    pub fn close_handles(&mut self) {
        if self.reopen_safe {
            self.file = None;
            self.map_file = None;
        }
    }

    /// v0.17：校验失败时作废这个文件的半成品与位图——两者都改名隔离，不删除。
    /// 否则坏块会被位图记为「已收」，下次重发继续跳过，永远校验不过。
    /// SAF 场景半成品只有 fd、无法按路径改名，这里只隔离可写的位图。
    pub fn invalidate(&mut self) -> io::Result<()> {
        self.map_file = None;
        let mut first_err = None;
        if let Err(e) = isolate(&self.map_path) {
            first_err = Some(e);
        }
        if self.reopen_safe {
            // 先释放句柄：Windows 下文件仍被打开时改名会失败
            self.file = None;
            if let Err(e) = isolate(&self.path) {
                if first_err.is_none() {
                    first_err = Some(e);
                }
            }
        }
        match first_err {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    fn ensure_file(&mut self) -> io::Result<&mut File> {
        if self.file.is_none() {
            self.file = Some(OpenOptions::new().read(true).write(true).open(&self.path)?);
        }
        Ok(self.file.as_mut().expect("just opened"))
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
        // v0.18：清单带了每块哈希时，写之前先比对。对不上就拒收这一块，
        // 不让坏数据落盘；对上则记入「已验」，收尾不用再整份重读。
        if let Some(want) = self.block_hashes.get(idx as usize) {
            if !crate::hash::hash_bytes(data).eq_ignore_ascii_case(want) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("block {idx} hash mismatch"),
                ));
            }
            self.verified_on_write.insert(idx);
        }
        {
            let file = self.ensure_file()?;
            file.seek(SeekFrom::Start(idx as u64 * BLOCK_SIZE as u64))?;
            file.write_all(data)?;
            file.flush()?;
        }
        self.received.insert(idx);
        self.append_map(idx)
    }

    /// 每收一块只追加一行块号。断电/被杀后仍能从缺口续传；
    /// 最坏情况是丢掉最后一行，代价只是重传那一个 4 MiB 块，不影响正确性。
    fn append_map(&mut self, idx: u32) -> io::Result<()> {
        // 句柄被释放过就按位图路径重新追加打开；SAF 场景不释放，这里不会触发
        if self.map_file.is_none() && self.reopen_safe {
            self.map_file = Some(OpenOptions::new().append(true).create(true).open(&self.map_path)?);
        }
        if let Some(f) = self.map_file.as_mut() {
            f.write_all(format!("{idx}\n").as_bytes())?;
            f.flush()?;
        }
        Ok(())
    }

    /// 块哈希 + 整文件根哈希双重校验；不靠肉眼看文件大小。
    /// 必须复用已经打开的句柄回读：SAF 只有 fd，没有可以再 open 一次的路径。
    /// v0.18 Merkle 分段校验：清单每块哈希齐全时，只回读「不是本次写入」的块。
    /// 本次写入的块已在落盘前逐块比对过，不再整份重读；续传继承下来的块必须回读，
    /// 因为磁盘上的旧数据不在本次控制范围内。
    /// 清单没有块哈希（极老发送端）时退回整文件 root 校验，保证仍能互通。
    pub fn verify(&mut self, entry: &FileEntry) -> io::Result<()> {
        if !self.complete() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "not all blocks received",
            ));
        }
        if !self.block_hashes.is_empty() {
            if self.block_hashes.len() as u32 != self.total_blocks {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "block count mismatch",
                ));
            }
            let mut pending: Vec<u32> = self
                .received
                .iter()
                .copied()
                .filter(|i| !self.verified_on_write.contains(i))
                .collect();
            pending.sort_unstable();
            for idx in pending {
                let want_len = expected_len(idx, self.total_blocks, self.file_size);
                let mut buf = vec![0u8; want_len];
                let file = self.ensure_file()?;
                file.seek(SeekFrom::Start(idx as u64 * BLOCK_SIZE as u64))?;
                file.read_exact(&mut buf)?;
                let want = self.block_hashes[idx as usize].clone();
                if !crate::hash::hash_bytes(&buf).eq_ignore_ascii_case(&want) {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("block {idx} hash mismatch"),
                    ));
                }
            }
            return Ok(());
        }
        let file = self.ensure_file()?;
        file.seek(SeekFrom::Start(0))?;
        let (root, blocks) = hash_reader(file)?;
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
        self.drop_map()?;
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
        self.drop_map()?;
        Ok(final_path.to_path_buf())
    }

    /// SAF 下宿主已经把数据直接写进了正式文档，Rust 侧无法 rename，只能刷新并标记完成。
    pub fn finish_in_place(&mut self) -> io::Result<()> {
        // SAF 场景句柄常驻；桌面路径若已释放句柄，数据在 flush 时已交给系统，这里跳过即可
        if let Some(f) = self.file.as_mut() {
            f.sync_all()?;
        }
        self.drop_map()
    }

    /// v0.17.1：成功后把位图删掉，不在用户目录里留 `.tcpart.map`。
    /// 这是程序自己生成的临时元数据，不是用户文件；删除失败不阻断交付（数据已经改名落盘），
    /// 只把错误交给调用方决定是否记日志。
    fn drop_map(&mut self) -> io::Result<()> {
        self.map_file = None;
        match fs::remove_file(&self.map_path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        }
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

    /// v0.18：写之前逐块比对，坏块直接拒收，不会落盘。
    #[test]
    fn merkle_rejects_wrong_block_before_write() {
        let dir = std::env::temp_dir().join(format!("tc-store-merkle-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let dest = dir.join("a.bin.tcpart");
        let entry = sample_entry(16);
        let mut p = PartFile::open(&dest, &entry).unwrap();
        let err = p.write_block(0, &[9u8; 16]).unwrap_err();
        assert!(err.to_string().contains("hash mismatch"));
        assert!(!p.has(0), "拒收的块不能记成已收");
    }

    /// v0.18：续传继承下来的块必须回读校验；本次写入的块不重复读。
    #[test]
    fn merkle_checks_inherited_blocks_only() {
        let dir = std::env::temp_dir().join(format!("tc-store-merkle2-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let dest = dir.join("a.bin.tcpart");
        let size = BLOCK_SIZE as u64 + 10;
        let entry = sample_entry(size);
        {
            let mut p = PartFile::open(&dest, &entry).unwrap();
            p.write_block(0, &vec![7u8; BLOCK_SIZE]).unwrap();
        }
        // 模拟上次落盘的数据已经坏了：继承块不能盲信
        let mut data = fs::read(&dest).unwrap();
        data[..8].fill(0xff);
        fs::write(&dest, &data).unwrap();

        let mut p = PartFile::open(&dest, &entry).unwrap();
        p.write_block(1, &[7u8; 10]).unwrap();
        let err = p.verify(&entry).unwrap_err();
        assert!(err.to_string().contains("block 0"), "应指出坏的是继承块");
    }

    fn sample_entry(size: u64) -> FileEntry {
        sample_entry_with(7u8, size)
    }

    fn sample_entry_with(byte: u8, size: u64) -> FileEntry {
        let data = vec![byte; size as usize];
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
        assert!(!map.exists(), "v0.17.1：SAF 完成路径也不应留下位图");
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

    /// v0.17：同名同大小但源根哈希不同，必须拒绝续传，把旧半成品与位图改名隔离后整份重传。
    #[test]
    fn same_size_different_hash_isolates_old_part() {
        let dir = std::env::temp_dir().join(format!("tc-store-ident-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let dest = dir.join("a.bin.tcpart");
        let map = PathBuf::from(format!("{}.map", dest.to_string_lossy()));
        let size = BLOCK_SIZE as u64 * 2 + 10;
        let old = sample_entry_with(7, size);
        let new = sample_entry_with(9, size);

        // 先按旧身份写满一块，留下可被误信的位图与半成品
        {
            let mut p = PartFile::open(&dest, &old).unwrap();
            p.write_block(0, &vec![7u8; BLOCK_SIZE]).unwrap();
        }
        assert!(map.exists() && dest.exists());

        let p = PartFile::open(&dest, &new).unwrap();
        assert_eq!(p.missing().len(), 3, "身份不符必须整份重传");
        // 旧的同名文件已被改名，原名位置是这次新建的 v3 位图与空半成品
        let text = fs::read_to_string(&map).unwrap();
        assert!(text.contains(&new.blake3), "新位图必须带本次源根哈希");
        assert!(!text.contains(&old.blake3), "旧身份不能留在当前位图里");
        let stale: Vec<String> = fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.contains(".stale-"))
            .collect();
        assert_eq!(stale.len(), 2, "半成品与位图各留一份可找回的隔离副本");
    }

    /// 校验失败时作废：invalidate 后半成品与位图都被改名，下一次打开必须从零开始。
    #[test]
    fn invalidate_moves_part_and_bitmap_aside() {
        let dir = std::env::temp_dir().join(format!("tc-store-inv-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let dest = dir.join("a.bin.tcpart");
        let map = PathBuf::from(format!("{}.map", dest.to_string_lossy()));
        let entry = sample_entry(16);

        let mut p = PartFile::open(&dest, &entry).unwrap();
        p.write_block(0, &[7u8; 16]).unwrap();
        p.invalidate().unwrap();
        assert!(!dest.exists());
        assert!(!map.exists());

        let p = PartFile::open(&dest, &entry).unwrap();
        assert!(!p.complete(), "作废后不能继承旧进度");
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
        assert!(!map.exists(), "v0.17.1：成功后不应残留 .tcpart.map");
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
