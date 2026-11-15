use serde::de::DeserializeOwned;
use serde::Serialize;
use std::io::{self, Read, Write};

pub const MAGIC: u16 = 0x5443; // "TC"
pub const HEADER_LEN: usize = 20;
/// 分块大小。这是编译期常量、不在帧头里传，两端必须一致——所以 HELLO 里显式带上它，
/// 让不同版本的设备在会话开始就被明确拒绝，而不是传到最后校验时才报块哈希不匹配。
/// 2026-11-12 由 4 MiB 提到 8 MiB（v0.5.3）：块数减半，位图行数与逐块开销同步减半。
pub const BLOCK_SIZE: usize = 8 * 1024 * 1024;
pub const MAX_MESSAGE_LEN: u32 = 64 * 1024 * 1024;

fn invalid(msg: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.into())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameType {
    Hello = 0,
    BlockReq = 1,
    BlockData = 2,
    Done = 3,
    Ping = 4,
    /// v0.14：一次请求多块（可跨文件），payload 是块列表 JSON。
    /// 旧端不认识这个类型会直接报错，所以只有 HELLO 协商出 `pack` 能力后才会发。
    PackReq = 5,
}

impl FrameType {
    fn from_u8(v: u8) -> io::Result<Self> {
        match v {
            0 => Ok(Self::Hello),
            1 => Ok(Self::BlockReq),
            2 => Ok(Self::BlockData),
            3 => Ok(Self::Done),
            4 => Ok(Self::Ping),
            5 => Ok(Self::PackReq),
            other => Err(invalid(format!("unknown frame type: {other}"))),
        }
    }
}

/// 定长 20 字节头：magic(2)+type(1)+flags(1)+fileId(8)+blockIndex(4)+length(4)。
/// fileId 是会话内文件序号，不带字符串，头部才能固定长度。
#[derive(Debug, Clone)]
pub struct FrameHeader {
    pub kind: FrameType,
    pub flags: u8,
    pub file_id: u64,
    pub block_index: u32,
    pub length: u32,
}

impl FrameHeader {
    pub fn new(kind: FrameType, file_id: u64, block_index: u32) -> Self {
        Self {
            kind,
            flags: 0,
            file_id,
            block_index,
            length: 0,
        }
    }

    pub fn encode(&self) -> [u8; HEADER_LEN] {
        let mut b = [0u8; HEADER_LEN];
        b[0..2].copy_from_slice(&MAGIC.to_le_bytes());
        b[2] = self.kind as u8;
        b[3] = self.flags;
        b[4..12].copy_from_slice(&self.file_id.to_le_bytes());
        b[12..16].copy_from_slice(&self.block_index.to_le_bytes());
        b[16..20].copy_from_slice(&self.length.to_le_bytes());
        b
    }

    pub fn decode(raw: &[u8; HEADER_LEN]) -> io::Result<Self> {
        let magic = u16::from_le_bytes([raw[0], raw[1]]);
        if magic != MAGIC {
            return Err(invalid("bad frame magic"));
        }
        let length = u32::from_le_bytes([raw[16], raw[17], raw[18], raw[19]]);
        // 上限略大于一个块，避免对端声明超大负载把内存吃光
        if length as usize > BLOCK_SIZE + 1024 {
            return Err(invalid("frame payload too large"));
        }
        Ok(Self {
            kind: FrameType::from_u8(raw[2])?,
            flags: raw[3],
            file_id: u64::from_le_bytes([
                raw[4], raw[5], raw[6], raw[7], raw[8], raw[9], raw[10], raw[11],
            ]),
            block_index: u32::from_le_bytes([raw[12], raw[13], raw[14], raw[15]]),
            length,
        })
    }
}

pub fn write_frame<W: Write>(w: &mut W, header: &FrameHeader, payload: &[u8]) -> io::Result<()> {
    // 长度以真实负载为准，避免调用方忘记同步 header.length 导致对端读错位
    let mut h = header.clone();
    h.length = payload.len() as u32;
    w.write_all(&h.encode())?;
    w.write_all(payload)?;
    w.flush()
}

pub fn read_frame<R: Read>(r: &mut R) -> io::Result<(FrameHeader, Vec<u8>)> {
    let mut raw = [0u8; HEADER_LEN];
    r.read_exact(&mut raw)?;
    let h = FrameHeader::decode(&raw)?;
    let mut payload = vec![0u8; h.length as usize];
    r.read_exact(&mut payload)?;
    Ok((h, payload))
}

/// 控制通道用「长度前缀 + 一行 JSON」：好调试，也方便以后写 CLI 抓包排查。
/// `?Sized`：会话层会直接传 `&mut dyn Stream`，不额外包一层具体类型。
pub fn write_message<W: Write + ?Sized, T: Serialize>(w: &mut W, msg: &T) -> io::Result<()> {
    let bytes = serde_json::to_vec(msg).map_err(|e| invalid(e.to_string()))?;
    if bytes.len() as u64 > MAX_MESSAGE_LEN as u64 {
        return Err(invalid("control message too large"));
    }
    w.write_all(&(bytes.len() as u32).to_le_bytes())?;
    w.write_all(&bytes)?;
    w.flush()
}

pub fn read_message<R: Read + ?Sized, T: DeserializeOwned>(r: &mut R) -> io::Result<T> {
    let mut len = [0u8; 4];
    r.read_exact(&mut len)?;
    let n = u32::from_le_bytes(len);
    if n > MAX_MESSAGE_LEN {
        return Err(invalid("control message too large"));
    }
    let mut buf = vec![0u8; n as usize];
    r.read_exact(&mut buf)?;
    serde_json::from_slice(&buf).map_err(|e| invalid(e.to_string()))
}

/// 把文件填满一个块再返回；read() 不保证读满，若直接按一次 read 分块，
/// 发送端与接收端的块边界会不一致，续传和校验都会错位。
pub fn read_full_block<R: Read>(r: &mut R, buf: &mut [u8]) -> io::Result<usize> {
    let mut filled = 0usize;
    while filled < buf.len() {
        match r.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(filled)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_roundtrip_is_20_bytes() {
        let h = FrameHeader::new(FrameType::BlockData, 7, 3);
        let raw = h.encode();
        assert_eq!(raw.len(), HEADER_LEN);
        let back = FrameHeader::decode(&raw).unwrap();
        assert_eq!(back.kind, FrameType::BlockData);
        assert_eq!(back.file_id, 7);
        assert_eq!(back.block_index, 3);
    }

    #[test]
    fn bad_magic_is_rejected() {
        let mut raw = FrameHeader::new(FrameType::BlockData, 0, 0).encode();
        raw[0] = 0;
        assert!(FrameHeader::decode(&raw).is_err());
    }

    #[test]
    fn oversized_frame_is_rejected() {
        let mut raw = FrameHeader::new(FrameType::BlockData, 0, 0).encode();
        raw[16..20].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(FrameHeader::decode(&raw).is_err());
    }
}
