use std::path::{Component, Path, PathBuf};

// Windows 非法字符；POSIX 只禁 '/'，这里按三端交集从严处理，避免文件在设备间再拷贝时被改名。
const ILLEGAL_CHARS: &[char] = &['<', '>', ':', '"', '/', '\\', '|', '?', '*'];

// Windows 保留设备名，带扩展名也算（con.txt 同样非法），所以按点号前的词干判断。
const RESERVED_NAMES: &[&str] = &[
    "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8",
    "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
];

const MAX_SEGMENT_LEN: usize = 255; // ext4 / APFS / NTFS 的通用上限

/// 把对端给的相对路径逐段清洗。
/// 遇到 `..`、绝对路径、盘符、UNC 一律「拒绝」而不是改写：改写会掩盖攻击意图，排查时更难定位。
pub fn sanitize_relative(rel: &str) -> Result<PathBuf, String> {
    if rel.is_empty() {
        return Err("empty relative path".into());
    }
    let bytes = rel.as_bytes();
    if rel.starts_with('/') || rel.starts_with('\\') {
        return Err(format!("absolute path rejected: {rel}"));
    }
    if bytes.len() >= 2 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic() {
        return Err(format!("drive path rejected: {rel}"));
    }

    let mut out = PathBuf::new();
    for seg in rel.replace('\\', "/").split('/') {
        if seg.is_empty() || seg == "." {
            continue;
        }
        if seg == ".." {
            return Err(format!("path traversal rejected: {rel}"));
        }
        check_segment(seg)?;
        out.push(seg);
    }
    if out.as_os_str().is_empty() {
        return Err(format!("path has no usable segment: {rel}"));
    }
    Ok(out)
}

fn check_segment(seg: &str) -> Result<(), String> {
    if seg.len() > MAX_SEGMENT_LEN {
        return Err(format!("segment too long: {seg}"));
    }
    if seg.as_bytes().iter().any(|&b| b < 0x20) {
        return Err("control character in segment".into());
    }
    if seg.chars().any(|c| ILLEGAL_CHARS.contains(&c)) {
        return Err(format!("illegal character in segment: {seg}"));
    }
    if seg.ends_with('.') || seg.ends_with(' ') {
        return Err(format!("trailing dot or space: {seg}"));
    }
    let stem = seg.to_ascii_lowercase().split('.').next().unwrap_or("").to_string();
    if RESERVED_NAMES.contains(&stem.as_str()) {
        return Err(format!("reserved device name: {seg}"));
    }
    Ok(())
}

/// 第二道闸：拼完路径再确认没有越出目标根目录。
/// 单独留这一层，是因为将来若有人改了 sanitize_relative 的规则，落盘处仍有一道独立校验。
pub fn ensure_within(base: &Path, rel: &Path) -> Result<PathBuf, String> {
    for c in rel.components() {
        if !matches!(c, Component::Normal(_)) {
            return Err(format!("non-normal path component: {rel:?}"));
        }
    }
    let joined = base.join(rel);
    if !joined.starts_with(base) {
        return Err(format!("path escapes destination: {rel:?}"));
    }
    Ok(joined)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normal_nested_path_passes() {
        let p = sanitize_relative("项目A/src/ui/main.rs").unwrap();
        assert_eq!(p, PathBuf::from("项目A/src/ui/main.rs"));
    }

    #[test]
    fn traversal_and_absolute_paths_are_rejected() {
        assert!(sanitize_relative("../../etc/passwd").is_err());
        assert!(sanitize_relative("a/../b").is_err());
        assert!(sanitize_relative("/etc/passwd").is_err());
        assert!(sanitize_relative("C:\\Windows\\system32").is_err());
        assert!(sanitize_relative("\\\\server\\share").is_err());
    }

    #[test]
    fn windows_reserved_names_and_illegal_chars_are_rejected() {
        assert!(sanitize_relative("con.txt").is_err());
        assert!(sanitize_relative("a:b.txt").is_err());
        assert!(sanitize_relative("a?b.txt").is_err());
        assert!(sanitize_relative("name.").is_err());
    }

    #[test]
    fn dot_segments_are_dropped() {
        assert_eq!(
            sanitize_relative("./a/./b.txt").unwrap(),
            PathBuf::from("a/b.txt")
        );
    }

    #[test]
    fn within_check_blocks_escape() {
        let base = Path::new("D:/recv");
        let bad = Path::new("../outside.txt");
        assert!(ensure_within(base, bad).is_err());
        assert!(ensure_within(base, Path::new("ok/a.txt")).is_ok());
    }
}
