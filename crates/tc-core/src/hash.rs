use crate::frame::{read_full_block, BLOCK_SIZE};
use blake3::Hasher;
use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

pub fn hash_bytes(data: &[u8]) -> String {
    blake3::hash(data).to_hex().to_string()
}

/// 一次读盘同时算出整文件根哈希和每块哈希。
/// 清单构建阶段再读一遍文件去算根哈希，对 10 GB 级目录会白白多一次全量 IO，所以两件事合并做。
/// 每块必须严格填满 BLOCK_SIZE（最后一块除外），否则与接收端按块号 seek 的边界对不上。
///
/// 直接读 File 而不再套 BufReader：每块本来就要 4 MiB，BufReader 的默认 8 KiB 缓冲
/// 既帮不上忙，还要在开头多拷一次（Android 上这层拷贝尤其不划算）。
pub fn hash_file(path: &Path) -> io::Result<(String, Vec<String>)> {
    hash_reader(File::open(path)?)
}

/// 接收端校验必须走这个入口：Android SAF 只有 fd、没有可重开的路径，
/// 校验只能复用手里那个已经打开的句柄，不能再去 open 一次。
pub fn hash_reader<R: Read>(mut reader: R) -> io::Result<(String, Vec<String>)> {
    let mut root = Hasher::new();
    let mut blocks = Vec::new();
    let mut buf = vec![0u8; BLOCK_SIZE];
    loop {
        let n = read_full_block(&mut reader, &mut buf)?;
        if n == 0 {
            break;
        }
        root.update(&buf[..n]);
        blocks.push(hash_bytes(&buf[..n]));
    }
    Ok((root.finalize().to_hex().to_string(), blocks))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn streaming_root_equals_one_shot_hash() {
        let dir = std::env::temp_dir().join(format!("tc-hash-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("sample.bin");
        let data = vec![0x5au8; BLOCK_SIZE + 123];
        let mut f = File::create(&p).unwrap();
        f.write_all(&data).unwrap();
        drop(f);

        let (root, blocks) = hash_file(&p).unwrap();
        assert_eq!(root, hash_bytes(&data));
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0], hash_bytes(&data[..BLOCK_SIZE]));
        assert_eq!(blocks[1], hash_bytes(&data[BLOCK_SIZE..]));
    }
}
