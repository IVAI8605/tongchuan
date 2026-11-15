use crate::frame::{
    read_frame, read_full_block, read_message, write_frame, write_message, FrameHeader, FrameType,
    BLOCK_SIZE,
};
use crate::manifest::{local_device, DeviceInfo, Manifest};
use crate::pathsafe::{ensure_within, sanitize_relative};
use crate::store::{PartFile, PathStore, Store};
use crate::transport::{Accepter, BoxedStream, Opener, PlainAccepter, PlainOpener, Stream};
use crate::trust::TrustStore;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};
use std::fs::{self, File};
use std::io::{self, Seek, SeekFrom};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

/// 本机协议版本，唯一来源是 workspace 的包版本。
/// P2 起 Hello / HelloAck 都带它，两端不同版本时给出「需要升级」的明确提示。
pub const PROTOCOL_VERSION: &str = env!("CARGO_PKG_VERSION");

/// 版本兼容口径：主次版本相同即可互通，补丁号不同视为兼容。
/// 0.x 阶段小版本就代表协议变更，所以不能只看主版本。
pub fn protocol_compatible(version: &str) -> bool {
    fn major_minor(v: &str) -> Option<(u64, u64)> {
        let mut it = v.trim().split('.');
        let major = it.next()?.parse().ok()?;
        let minor = it.next()?.parse().ok()?;
        Some((major, minor))
    }
    major_minor(version).is_some() && major_minor(version) == major_minor(PROTOCOL_VERSION)
}

/// 接收端拒绝旧发送端时的提示：必须说清双方版本与升级动作，
/// 否则用户只看到一句「untrusted」或 TLS error，根本不知道是版本问题。
pub fn version_reject_reason(sender: &str) -> String {
    let sender = if sender.trim().is_empty() {
        "旧版（未上报版本）".to_string()
    } else {
        format!("v{}", sender.trim())
    };
    format!(
        "version mismatch: 接收端 v{PROTOCOL_VERSION} 与本机 {sender} 不兼容，请把两端升级到同一版本"
    )
}

/// 发送端收到 HelloAck 后的版本提示：PC 旧 / 手机新 时由这里告知「接收端过旧」。
pub fn version_reject_from_receiver(receiver: &str) -> String {
    let receiver = if receiver.trim().is_empty() {
        "旧版（未上报版本）".to_string()
    } else {
        format!("v{}", receiver.trim())
    };
    format!(
        "接收端 {receiver} 与本机 v{PROTOCOL_VERSION} 不兼容，请把两端升级到同一版本"
    )
}

/// v0.16：接收端对外公布的可用落点。只给标识与人可读名字，绝不带真实路径——
/// 发送方只能「选一个已有标识」，不能凭它拼出任意目录。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DestEntry {
    pub id: String,
    pub name: String,
}

/// v0.14：一次请求多块的帧负载。用 (fileId, blockIndex) 列表表达，
/// 顺序即发送顺序；接收端按同样顺序逐帧核对 BlockData。
#[derive(Debug, Serialize, Deserialize)]
struct PackRequest {
    blocks: Vec<(u64, u32)>,
}

/// 单个 pack 的块数与字节上限：块数控制 JSON 大小与句柄切换次数，
/// 字节数控制接收端在途内存（每块最多 8 MiB）。
const PACK_MAX_BLOCKS: usize = 256;
const PACK_MAX_BYTES: usize = 64 * 1024 * 1024;

/// v0.14：本次支持的能力。只有双方都声明才启用 pack（一次请求多块）。
const CAP_PACK: &str = "pack";

/// v0.15：双方都声明才启用多数据流。
const CAP_STREAMS: &str = "streams";

/// 默认并行流数。接收端按配置与工作量收敛后写进 MANIFEST_ACK。
pub const DEFAULT_STREAMS: u8 = 4;

/// 并行流上限：再多只增加线程与内存（每流 8 MiB 缓冲），链路先到瓶颈。
const MAX_STREAMS: u8 = 8;

/// v0.16：接收端配置里的一个可选落点。`path` 必须落在 `allow` 内，
/// 校验在配置加载时做一次；会话期只按 `id` 查表，不接受发送方传任意路径。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Destination {
    pub id: String,
    pub name: String,
    pub path: PathBuf,
}

/// 对外的可用落点清单。配置里没配 destinations 时派生一条「default」，
/// 旧配置零改动；名字取默认目录的最后一段，方便用户认。
fn dest_list(destinations: &[Destination], default: &Path) -> Vec<DestEntry> {
    if destinations.is_empty() {
        let name = default
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "默认位置".to_string());
        return vec![DestEntry {
            id: "default".into(),
            name,
        }];
    }
    destinations
        .iter()
        .map(|d| DestEntry {
            id: d.id.clone(),
            name: d.name.clone(),
        })
        .collect()
}

/// 把发送端选的标识映射成真实目录。未知标识、空标识都拒绝并给出可用列表，
/// 错误信息直接回给发送端，用户要能照着改。
fn resolve_destination(
    destinations: &[Destination],
    id: Option<&str>,
    default: &Path,
) -> Result<PathBuf, String> {
    match id {
        None => Ok(default.to_path_buf()),
        // 旧配置派生出来的唯一落点：发送端会看到 "default"，允许原样回传
        Some("default") if destinations.is_empty() => Ok(default.to_path_buf()),
        Some(id) => destinations
            .iter()
            .find(|d| d.id == id)
            .map(|d| d.path.clone())
            .ok_or_else(|| {
                let ids: Vec<&str> = destinations.iter().map(|d| d.id.as_str()).collect();
                format!(
                    "unknown destination: {id}（可用：{}）",
                    if ids.is_empty() { "无".to_string() } else { ids.join(", ") }
                )
            }),
    }
}

/// 实际开几条数据连接：对端不支持、或本次块太少时退回单流。
/// 模拟中断（max_blocks）必须单流，否则多发出的请求会让续传语义对不上。
fn effective_streams(
    requested: u8,
    peer_caps: &[String],
    blocks: u64,
    max_blocks: Option<usize>,
) -> u8 {
    if max_blocks.is_some() || !peer_caps.iter().any(|c| c == CAP_STREAMS) {
        return 1;
    }
    let want = requested.clamp(1, MAX_STREAMS) as u64;
    // 8 块以下的传输，开连接的握手成本比并行省下的时间还多
    let by_work = if blocks >= 8 { want } else { 1 };
    by_work.min(blocks.max(1)) as u8
}

/// 块大小不一致的提示。只报「校验失败」用户查不出原因，必须点明两边数值与动作。
pub fn block_size_reject_reason(peer: u32) -> String {
    let peer_text = if peer == 0 {
        "旧版（未上报，4 MiB）".to_string()
    } else {
        format!("{} MiB", peer as usize / (1024 * 1024))
    };
    format!(
        "block size mismatch: 本机 {} MiB 与对端 {peer_text} 不一致，请把两端升级到同一版本",
        BLOCK_SIZE / (1024 * 1024)
    )
}

/// 信任策略。默认 closed；open 只作排障/老设备过渡，切换由调用方写日志。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrustMode {
    Closed,
    Open,
}

impl TrustMode {
    /// 非法值一律按 closed 处理：宁可多拦一次，也不能因为配置写错就默认放行
    pub fn parse(text: &str) -> Self {
        if text.trim().eq_ignore_ascii_case("open") {
            Self::Open
        } else {
            Self::Closed
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Closed => "closed",
            Self::Open => "open",
        }
    }
}

/// 会话门禁的判定结果。拒绝原因直接回给发送端，必须人能读懂。
pub enum GateVerdict {
    Allow,
    Deny(String),
}

/// 接收端的信任门禁：持有与 CLI/管理页同一份信任库，按当前策略判定每场会话。
pub struct TrustGate {
    pub store: Arc<TrustStore>,
    pub mode: TrustMode,
    pub pair_code: Option<String>,
}

impl TrustGate {
    pub fn new(store: Arc<TrustStore>, mode: TrustMode, pair_code: Option<String>) -> Self {
        Self {
            store,
            mode,
            pair_code,
        }
    }

    /// 顺序：已信任 → 配对码（仅配置非空时）→ open 放行 → closed 拒绝并记 denied。
    /// 撤销立刻生效，因为每次都先 stat 文件重放，不缓存「启动时的信任表」。
    pub fn evaluate(
        &self,
        fp: Option<[u8; 32]>,
        device: &DeviceInfo,
        pair_code: Option<&str>,
    ) -> io::Result<GateVerdict> {
        let fp_hex = fp.map(|f| {
            let mut s = String::with_capacity(64);
            for b in f {
                s.push_str(&format!("{b:02x}"));
            }
            s
        });

        if let Some(fp) = &fp_hex {
            if self.store.is_trusted(fp)? {
                return Ok(GateVerdict::Allow);
            }
        }

        // D3：只有配置里写了非空配对码，匹配才自动入信任；空配对码一律人工批准
        let configured = self
            .pair_code
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty());
        let pair_ok = match (configured, pair_code) {
            (Some(expected), Some(got)) => expected == got,
            _ => false,
        };
        if pair_ok {
            if let Some(fp) = &fp_hex {
                self.store
                    .approve(fp, "", &device.name, &device.platform, "pair-code")?;
            }
            return Ok(GateVerdict::Allow);
        }

        if self.mode == TrustMode::Open {
            if let Some(fp) = &fp_hex {
                // 只记第一次，避免同一台设备每次发送都追加一条 add
                if !self.store.is_trusted(fp)? {
                    self.store
                        .approve(fp, "", &device.name, &device.platform, "open")?;
                }
            }
            return Ok(GateVerdict::Allow);
        }

        let reason = match &fp_hex {
            Some(fp) => format!("untrusted {}", &fp[..8]),
            None => format!(
                "untrusted: 未提供客户端证书（疑似旧版），请把发送端升级到 v{PROTOCOL_VERSION}"
            ),
        };
        if let Some(fp) = &fp_hex {
            self.store
                .record_denied(fp, &device.name, &device.platform, &reason)?;
        }
        Ok(GateVerdict::Deny(reason))
    }
}

/// 一场会话的门禁输入：单会话与并发 hub 各自从连接上取到证书指纹后传入。
#[derive(Clone, Copy, Default)]
struct SessionGate<'a> {
    trust: Option<&'a TrustGate>,
    peer_fp: Option<[u8; 32]>,
}

/// 控制消息。PROBE / PROBE_ACK / RESUME 在 v0.1 只定义不实现，
/// 目的是把「探活 + 待发队列 + 离线补收」的消息名先冻住，等 v0.4 加 Android 时不用改协议。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Control {
    Hello {
        #[serde(rename = "txnId")]
        txn_id: String,
        device: DeviceInfo,
        /// 首次配对时双方人工核对的 6 位数字；已配对设备可不填。
        #[serde(rename = "pairCode", skip_serializing_if = "Option::is_none")]
        pair_code: Option<String>,
        /// P2 新增。旧发送端没有这个键，serde 默认给空串，
        /// 接收端据此明确回「版本不兼容」而不是继续说「untrusted」。
        #[serde(default)]
        version: String,
        /// v0.5.3 新增：分块大小（字节）。块大小是编译期常量、帧头里没有，
        /// 4 MiB 与 8 MiB 两端混用时会在校验阶段报块哈希不匹配，很难看出原因。
        /// 旧端没有这个键，serde 默认 0，接收端据此明确回「块大小不一致，请同版升级」。
        #[serde(rename = "blockSize", default)]
        block_size: u32,
        /// v0.14：能力声明。旧端没有这个键，serde 默认空表，接收端据此走逐块兼容路径。
        /// 目前只定义 `pack`：支持一次请求多块。
        #[serde(default)]
        caps: Vec<String>,
    },
    HelloAck {
        version: String,
        /// 与 HELLO 对称：新发送端据此拒绝旧接收端，避免传到一半才失败。
        #[serde(rename = "blockSize", default)]
        block_size: u32,
        /// v0.14：服务端能力，与 HELLO 的 `caps` 取交集后才启用 pack。
        #[serde(default)]
        caps: Vec<String>,
        /// v0.16：可选的接收落点清单。旧发送端忽略这个键，继续发默认落点。
        #[serde(default)]
        destinations: Vec<DestEntry>,
    },
    /// 数据连接的身份声明。发送端开第二条连接后立刻发它，接收端才能按 txnId 把连接对回会话。
    /// 没有这条时接收端只能按 accept 顺序猜，第二台设备接入就会把控制连接当成数据连接。
    DataHello {
        #[serde(rename = "txnId")]
        txn_id: String,
        /// v0.15：这条数据连接在本会话里的序号（从 0 起）。
        /// 只用于日志与排障；投递仍按 txnId + 到达顺序，旧端没有这个键也不影响。
        #[serde(default)]
        stream: u8,
    },
    /// 明确拒绝：并发已满、落点冲突、会话不存在。让对方立刻拿到原因，
    /// 而不是一直等 ACK 直到连接超时（旧实现里的「挂住」就是这么来的）。
    Busy {
        reason: String,
    },
    Manifest {
        manifest: Manifest,
        /// v0.12 新增：发送端要求「忽略接收端已有文件，全部重传」。
        /// 跳过判断本来就只在接收端做，所以发送端只带一个标记即可；
        /// 旧发送端没有这个键，serde 默认 false，行为与 v0.11 完全一致。
        #[serde(default)]
        force: bool,
        /// v0.16：本次落点标识（取自 HELLO_ACK 的 destinations）。缺省 = 默认落点；
        /// 旧接收端不认识这个键会忽略，仍落在默认位置。
        #[serde(rename = "destId", default, skip_serializing_if = "Option::is_none")]
        dest_id: Option<String>,
    },
    /// v0.16：只查可用落点，不传文件。放在 HELLO_ACK 之后，复用同一次握手与信任判定。
    Dests,
    DestsAck {
        #[serde(default)]
        destinations: Vec<DestEntry>,
    },
    ManifestAck {
        #[serde(rename = "acceptedRoots")]
        accepted_roots: Vec<String>,
        #[serde(rename = "skippedRoots")]
        skipped_roots: Vec<String>,
        #[serde(rename = "acceptedFileIds")]
        accepted_file_ids: Vec<String>,
        /// v0.15：接收端要求发送端开几条数据连接。0/1 = 单流。
        /// 旧发送端不认识这个键会忽略，继续只开 1 条，行为不变。
        #[serde(default)]
        streams: u8,
    },
    Probe {
        #[serde(rename = "txnId")]
        txn_id: String,
    },
    ProbeAck {
        online: bool,
    },
    Resume {
        #[serde(rename = "txnId")]
        txn_id: String,
    },
    Done {
        #[serde(rename = "txnId")]
        txn_id: String,
    },
    VerifyOk {
        #[serde(rename = "txnId")]
        txn_id: String,
        /// v0.11 新增：接收端实际落点的根目录名（相对它的接收目录）。
        /// 发送端据此告诉用户「传到哪去了」。旧接收端没有这个字段，serde 默认空表。
        #[serde(rename = "destDirs", default)]
        dest_dirs: Vec<String>,
    },
    VerifyFail {
        reason: String,
    },
}

#[derive(Debug)]
pub struct SendReport {
    pub txn_id: String,
    pub files: u64,
    pub bytes: u64,
    pub blocks_sent: u64,
    /// v0.14：本次收到的 PACK_REQ 包数。为 0 说明走的是逐块兼容路径，
    /// 用来在测试与排障时确认「pack 到底有没有生效」。
    pub packs: u64,
    /// v0.15：本次实际开了几条数据连接。1 表示单流（旧接收端或配置关闭）。
    pub streams: u8,
    /// 接收端按「同路径同内容」跳过的文件数（增量同步）。日志里要能看出省了多少。
    pub files_skipped: u64,
    /// 接收端回报的落点根目录名（v0.11）。界面用它显示「传到了哪个文件夹」。
    pub dest_dirs: Vec<String>,
    pub verdict: String,
}

#[derive(Debug)]
pub struct ReceiverReport {
    pub txn_id: String,
    pub files: u64,
    pub bytes: u64,
    pub blocks_written: u64,
    pub resumed_blocks: u64,
    /// v0.15：本场会话实际使用的数据连接数，1 = 单流。
    pub streams_used: u8,
    /// v0.16：这只是一次「查可用落点」的会话，没有传文件。
    /// daemon 与 Android 据此跳过历史与计数，避免查询把「最近接收」刷满。
    pub query_only: bool,
    pub verified: bool,
    pub interrupted: bool,
    /// 来源设备（HELLO 里带的）。历史记录要能看出「谁发来的」，不再只有一句 txnId。
    pub device: Option<DeviceInfo>,
    /// 实际落点的根目录名（相对接收目录）。只记根目录不记每个文件，
    /// 一场几万文件的传输也不会把历史行撑大。
    pub roots: Vec<String>,
    /// 对端 TCP 地址，来自接入层；用于排查「这台机器是谁」。
    pub peer_addr: Option<String>,
    /// P0 打点：等对端回块数据的累计耗时（微秒）。只用于性能诊断，不进协议。
    pub wait_us: u64,
    /// P0 打点：写盘（含每块位图重写）的累计耗时（微秒）
    pub write_us: u64,
    /// P0 打点：向下发 BLOCK_REQ 的累计耗时（微秒）。
    /// socket 发送缓冲满时这段时间会阻塞，但不计入「等数据」，
    /// 是手机墙钟比「等数据+写盘」多出 8–14 秒的主要嫌疑（见传输提速-规划 §12.10）。
    pub req_us: u64,
    /// P0 打点：收完后整文件校验的累计耗时（微秒）
    pub verify_us: u64,
    /// 增量同步：目标路径上已是同一份内容而直接跳过的文件数与字节数。
    /// 跳过的文件不再请求数据块，但它们的哈希在跳过前已经核对过。
    pub skipped_files: u64,
    pub skipped_bytes: u64,
}

fn io_err(msg: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.into())
}

/// 增量校验：一边收块一边算整文件根哈希，收完就不用再把整个文件读一遍。
/// 实测 6.9 GB 的全量校验要 4.8 s，而这段 CPU 时间完全可以藏在等数据的空档里。
///
/// 只对「全新传输」启用：续传时到达的是缺块集合，顺序不连续，根哈希无法增量累积，
/// 只能退回原来的全量读盘校验。块序是安全的——请求按「文件 → 块号递增」生成，
/// 发送端也按同一顺序回包，即使开了预取窗口也不会乱序。
struct IncVerify {
    enabled: bool,
    hasher: blake3::Hasher,
    /// 当前正在累积的文件下标
    file: Option<usize>,
    /// 第一个失败原因；非空即表示增量校验不可信，调用方应退回全量校验
    failed: Option<String>,
}

impl IncVerify {
    fn new(enabled: bool) -> Self {
        Self {
            enabled,
            hasher: blake3::Hasher::new(),
            file: None,
            failed: None,
        }
    }

    fn feed(&mut self, files: &[crate::manifest::FileEntry], file_idx: usize, block_idx: u32, data: &[u8]) {
        if !self.enabled || self.failed.is_some() {
            return;
        }
        let entry = &files[file_idx];
        match entry.block_hashes.get(block_idx as usize) {
            Some(want) if want.as_str() == crate::hash::hash_bytes(data).as_str() => {}
            Some(_) => {
                self.failed = Some(format!("{}: block {block_idx} hash mismatch", entry.rel_path));
                return;
            }
            None => {
                self.failed = Some(format!("{}: block {block_idx} out of range", entry.rel_path));
                return;
            }
        }
        if self.file != Some(file_idx) {
            // 换文件前先把上一个文件的根哈希结掉
            if let Some(prev) = self.file {
                if self.hasher.finalize().to_hex().to_string() != files[prev].blake3 {
                    self.failed = Some(format!("{}: root hash mismatch", files[prev].rel_path));
                    return;
                }
                self.hasher = blake3::Hasher::new();
            }
            self.file = Some(file_idx);
        }
        self.hasher.update(data);
    }

    /// 收尾：结掉最后一个文件，并单独核对 0 字节文件（它们没有任何块，不会经过 feed）。
    fn finish(&mut self, files: &[crate::manifest::FileEntry]) -> Option<String> {
        if !self.enabled {
            return Some("incremental verify disabled".into());
        }
        if let Some(e) = self.failed.take() {
            return Some(e);
        }
        if let Some(idx) = self.file {
            if self.hasher.finalize().to_hex().to_string() != files[idx].blake3 {
                return Some(format!("{}: root hash mismatch", files[idx].rel_path));
            }
        }
        for f in files {
            if f.block_count() == 0 && crate::hash::hash_bytes(&[]) != f.blake3 {
                return Some(format!("{}: empty file hash mismatch", f.rel_path));
            }
        }
        None
    }
}

struct Layout {
    dirs: Vec<PathBuf>,
    part_paths: Vec<PathBuf>,
    final_paths: Vec<PathBuf>,
    /// 各根目录的实际落点名（相对 dest），顺序与清单里的 roots 一致
    roots: Vec<String>,
}

/// 单独暴露给测试：证明接收侧真的会拒绝恶意清单，而不只是工具函数自己能拒绝。
pub fn validate_manifest_paths(dest: &Path, m: &Manifest) -> io::Result<()> {
    plan_layout(dest, m, &HashMap::new()).map(|_| ())
}

/// 先把整张清单的落点算完并校验，再开始收数据。
/// 「边传边校验路径」会出现前半批已经写盘、后半批才发现穿越的半吊子状态，所以一律前置拒绝。
/// root_map 的 value 是接收端想要的根目录名（相对 dest 的路径），不是绝对路径。
/// 这样「改名」天然仍在目标目录内，不会出现映射把文件写到别处的风险。
fn plan_layout(
    dest: &Path,
    m: &Manifest,
    root_map: &HashMap<String, String>,
) -> io::Result<Layout> {
    let mut labels: HashMap<String, String> = HashMap::new();
    let mut roots = Vec::new();
    let mut dirs = Vec::new();
    let mut part_paths = Vec::new();
    let mut final_paths = Vec::new();

    for r in &m.roots {
        let rel = match root_map.get(&r.id) {
            Some(name) => sanitize_relative(name).map_err(io_err)?,
            None => sanitize_relative(&r.name).map_err(io_err)?,
        };
        dirs.push(ensure_within(dest, &rel).map_err(io_err)?);
        let label = rel.to_string_lossy().replace('\\', "/");
        labels.insert(r.id.clone(), label.clone());
        roots.push(label);
    }

    for r in &m.roots {
        let label = &labels[&r.id];
        for d in &r.dirs {
            if d.is_empty() {
                continue; // 空串表示根本身，上面已经建了
            }
            let rel = sanitize_relative(&format!("{label}/{d}")).map_err(io_err)?;
            dirs.push(ensure_within(dest, &rel).map_err(io_err)?);
        }
    }

    for f in &m.files {
        let label = labels
            .get(&f.root_id)
            .ok_or_else(|| io_err(format!("unknown rootId: {}", f.root_id)))?;
        let rel = sanitize_relative(&format!("{label}/{}", f.rel_path)).map_err(io_err)?;
        let final_path = ensure_within(dest, &rel).map_err(io_err)?;
        part_paths.push(PathBuf::from(format!("{}.tcpart", final_path.to_string_lossy())));
        final_paths.push(final_path);
    }
    Ok(Layout {
        dirs,
        part_paths,
        final_paths,
        roots,
    })
}

/// 增量同步（只增不删）：落点路径上已是同一份内容的文件直接跳过，不再重传。
///
/// 判断顺序是「大小 → BLAKE3」：只看 mtime/size 会把「同大小不同内容」误判成已完成，
/// 那是静默丢数据；读一遍本地盘（SSD 上 GB/s 级）远比重传几 GB 划算，而且结论是确定的。
/// 任何一步读失败都当成「没收到过」，退回普通传输——检查本身出错绝不能导致少传文件。
/// 同名但内容不同的文件仍按原规则另起名字，不覆盖。
fn already_present(final_paths: &[PathBuf], files: &[crate::manifest::FileEntry]) -> Vec<usize> {
    let mut out = Vec::new();
    for (i, f) in files.iter().enumerate() {
        // plan_layout 对每个文件都给了一条最终路径，两者长度一致
        let Some(path) = final_paths.get(i) else {
            continue;
        };
        match fs::metadata(path) {
            Ok(meta) if meta.is_file() && meta.len() == f.size => {
                if crate::hash::hash_file(path)
                    .map(|(root, _)| root == f.blake3)
                    .unwrap_or(false)
                {
                    out.push(i);
                }
            }
            _ => {}
        }
    }
    out
}

/// 发送端句柄缓存：固定容量 + 最久未用淘汰。
///
/// 旧实现只插不删，一个大文件上千块确实省下了上千次 open，
/// 但 5 万个小文件会把句柄一路堆到进程 FD 上限（Android 通常 1024），传输中途失败。
/// 请求大体按文件顺序到达，容量取预取窗口 + 1 的余量即可；
/// 万一乱序超出容量，最坏只是多一次 open，不影响正确性。
struct HandleCache {
    cap: usize,
    order: VecDeque<u64>,
    map: HashMap<u64, File>,
}

impl HandleCache {
    fn new(cap: usize) -> Self {
        Self {
            cap,
            order: VecDeque::new(),
            map: HashMap::new(),
        }
    }

    fn get_mut(&mut self, id: u64, path: &Path) -> io::Result<&mut File> {
        if self.map.contains_key(&id) {
            // 命中：把 id 挪到队尾，保持「队首是最久未用」
            if let Some(pos) = self.order.iter().position(|x| *x == id) {
                self.order.remove(pos);
            }
        } else {
            if self.map.len() >= self.cap {
                if let Some(old) = self.order.pop_front() {
                    self.map.remove(&old);
                }
            }
            self.map.insert(id, File::open(path)?);
        }
        self.order.push_back(id);
        Ok(self.map.get_mut(&id).expect("just inserted"))
    }

    #[cfg(test)]
    fn open_count(&self) -> usize {
        self.map.len()
    }
}

/// 发送端：控制连接谈清单，数据连接按接收端的 BLOCK_REQ 供块。
/// v0.1 只开 1 条数据连接；帧里已经带 fileId/blockIndex，v0.6 扩到 N 条不用改协议。
pub fn run_sender(to: &str, paths: &[PathBuf]) -> io::Result<SendReport> {
    run_sender_with(&PlainOpener { addr: to.into() }, paths, None)
}

/// 通用入口：明文与 TLS 共用同一套会话流程，换传输层不动协议。
pub fn run_sender_with(
    opener: &dyn Opener,
    paths: &[PathBuf],
    pair_code: Option<&str>,
) -> io::Result<SendReport> {
    run_sender_with_device(opener, paths, pair_code, local_device())
}

/// 带选项的发送入口（v0.12 CLI 的 `--force` 走这里）。
/// 与 `run_sender_with` 共用同一份会话实现，只是把选项透传下去。
pub fn run_sender_with_opts(
    opener: &dyn Opener,
    paths: &[PathBuf],
    pair_code: Option<&str>,
    options: SendOptions,
) -> io::Result<SendReport> {
    run_sender_with_device_control_opts(
        opener,
        paths,
        pair_code,
        local_device(),
        None,
        None,
        options,
    )
}

/// 带设备名的发送入口。Android 上取不到 COMPUTERNAME/HOSTNAME，
/// 设备名只能由宿主（如 Build.MODEL）传入，否则对端历史与信任列表里全是 unknown。
pub fn run_sender_with_device(
    opener: &dyn Opener,
    paths: &[PathBuf],
    pair_code: Option<&str>,
    device: DeviceInfo,
) -> io::Result<SendReport> {
    run_sender_with_device_progress(opener, paths, pair_code, device, None)
}

/// 发送进度：**只在本地观测用，不进协议**。回调运行在数据连接线程上，别在里面做重活。
#[derive(Debug, Clone, Default, Serialize)]
pub struct SendProgress {
    pub files: u64,
    pub bytes_sent: u64,
    pub total_bytes: u64,
    pub blocks_sent: u64,
    pub total_blocks: u64,
    /// 当前块所属的相对路径，界面可以显示「正在传哪个文件」
    pub current: String,
    /// true 表示还在扫描/算校验值阶段（此时没有总字节，界面应显示「扫描中」而不是百分比）
    pub scanning: bool,
    pub scan_files: u64,
    pub scan_bytes: u64,
    /// 瞬时速率（字节/秒），按最近约 1 秒的滑动窗口算。
    /// 单块 4 MiB 在高吞吐下只有几十毫秒，逐块相除抖动太大，必须做窗口平滑才可用于判断趋势。
    pub speed_bps: u64,
}

/// 发送控制：界面上的「暂停 / 取消」。用原子值而不是回调：
/// 数据线程每块前读一次即可，不取锁、也不影响原来的发送路径。
#[derive(Default)]
pub struct SendControl {
    state: AtomicU8,
}

const CTRL_RUN: u8 = 0;
const CTRL_PAUSE: u8 = 1;
const CTRL_CANCEL: u8 = 2;

impl SendControl {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn pause(&self) {
        // 已经取消的不再退回暂停：取消是终态，避免界面按顺序点两下把状态搅乱
        if !self.is_cancelled() {
            self.state.store(CTRL_PAUSE, Ordering::Relaxed);
        }
    }

    pub fn resume(&self) {
        if self.state.load(Ordering::Relaxed) == CTRL_PAUSE {
            self.state.store(CTRL_RUN, Ordering::Relaxed);
        }
    }

    pub fn cancel(&self) {
        self.state.store(CTRL_CANCEL, Ordering::Relaxed);
    }

    pub fn is_paused(&self) -> bool {
        self.state.load(Ordering::Relaxed) == CTRL_PAUSE
    }

    pub fn is_cancelled(&self) -> bool {
        self.state.load(Ordering::Relaxed) == CTRL_CANCEL
    }
}

/// 带进度回调的发送入口。桌面端不传回调时与 `run_sender_with_device` 完全一致，
/// 所以界面进度属于「加观测」，不改任何线上行为。
pub fn run_sender_with_device_progress(
    opener: &dyn Opener,
    paths: &[PathBuf],
    pair_code: Option<&str>,
    device: DeviceInfo,
    progress: Option<&mut dyn FnMut(SendProgress)>,
) -> io::Result<SendReport> {
    run_sender_with_device_control(opener, paths, pair_code, device, progress, None)
}

/// 发送端可选的会话行为（v0.12）。目前只有两项，但单独一个结构体，
/// 以后加限速、断点策略之类不必再动所有发送入口的签名。
#[derive(Debug, Clone, Default)]
pub struct SendOptions {
    /// true = 忽略接收端已有的同名同内容文件，本次全部重传。
    /// 接收端会把目标位置上的同名旧文件改名为 `xxx (1).ext` 让位，
    /// 本次到达的文件保留源文件名（用户 2026-11-14 明确的口径）。
    pub force: bool,
    /// v0.16：接收端公布的落点标识；None = 用接收端的默认落点（旧行为）。
    pub dest_id: Option<String>,
}

/// 带「暂停 / 取消」的发送入口（v0.11）。控制位只在本机读，不进协议：
/// 暂停时不再向接收端供块，对端最多 4 个在途请求发完自然停住；取消直接返回错误，
/// 接收端留下 `.tcpart`，下次同名发送可以续传。
pub fn run_sender_with_device_control(
    opener: &dyn Opener,
    paths: &[PathBuf],
    pair_code: Option<&str>,
    device: DeviceInfo,
    progress: Option<&mut dyn FnMut(SendProgress)>,
    control: Option<&SendControl>,
) -> io::Result<SendReport> {
    run_sender_with_device_control_opts(
        opener,
        paths,
        pair_code,
        device,
        progress,
        control,
        SendOptions::default(),
    )
}

/// 需要强制重传等选项时走这个入口；其余发送入口都以默认选项转调它，
/// 保证「普通发送」永远只有一份实现。
pub fn run_sender_with_device_control_opts(
    opener: &dyn Opener,
    paths: &[PathBuf],
    pair_code: Option<&str>,
    device: DeviceInfo,
    mut progress: Option<&mut dyn FnMut(SendProgress)>,
    control: Option<&SendControl>,
    options: SendOptions,
) -> io::Result<SendReport> {
    // 扫描阶段也回报，否则大目录会连续几分钟没有任何界面反馈（本轮实测踩到）
    let manifest = {
        let mut scan_cb = |files: u64, bytes: u64| {
            if let Some(cb) = progress.as_mut() {
                cb(SendProgress {
                    scanning: true,
                    scan_files: files,
                    scan_bytes: bytes,
                    ..Default::default()
                });
            }
        };
        crate::manifest::build_manifest_with_progress(paths, device, Some(&mut scan_cb))?
    };
    let mut ctrl = opener.open()?;

    write_message(
        &mut ctrl,
        &Control::Hello {
            txn_id: manifest.txn_id.clone(),
            device: manifest.device.clone(),
            pair_code: pair_code.map(|s| s.to_string()),
            version: PROTOCOL_VERSION.into(),
            block_size: BLOCK_SIZE as u32,
            caps: vec![CAP_PACK.to_string(), CAP_STREAMS.to_string()],
        },
    )?;
    match read_message::<_, Control>(&mut ctrl)? {
        // 接收端过旧时由这里给出明确提示：只靠 TLS 握手失败，用户查不出是版本问题
        Control::HelloAck {
            version,
            block_size,
            ..
        } => {
            if !protocol_compatible(&version) {
                return Err(io_err(version_reject_from_receiver(&version)));
            }
            // 块大小对不上就根本传不了，早停比传到校验阶段再报错清楚得多
            if block_size != BLOCK_SIZE as u32 {
                return Err(io_err(block_size_reject_reason(block_size)));
            }
        }
        Control::Busy { reason } => return Err(io_err(format!("receiver is busy: {reason}"))),
        other => return Err(io_err(format!("unexpected hello reply: {other:?}"))),
    }

    write_message(
        &mut ctrl,
        &Control::Manifest {
            manifest: manifest.clone(),
            force: options.force,
            dest_id: options.dest_id.clone(),
        },
    )?;
    let (accepted_ids, streams): (HashSet<String>, u8) = match read_message::<_, Control>(&mut ctrl)? {
        Control::ManifestAck {
            accepted_file_ids,
            streams,
            ..
        } => (accepted_file_ids.into_iter().collect(), streams),
        Control::Busy { reason } => return Err(io_err(format!("receiver rejected manifest: {reason}"))),
        other => return Err(io_err(format!("unexpected manifest reply: {other:?}"))),
    };
    // 增量同步：接收端在目标位置找到同一份内容的文件就不会请数据块，
    // 用「清单总数 - 接受数」还原被跳过的文件数，日志与历史里直接可见。
    let files_skipped = manifest.total_files.saturating_sub(accepted_ids.len() as u64);

    let source_map = build_source_map(paths, &manifest)?;
    // v0.15：接收端在 MANIFEST_ACK 里给出并行流数；旧接收端不回这个字段（0）就是单流。
    let streams = streams.max(1).min(MAX_STREAMS);
    // 每条数据连接先自报 txnId 与序号：并发接多台设备时靠 txnId 找回会话，
    // 序号只用于日志与排障（接收入口按到达顺序投递给会话）。
    let mut conns: Vec<BoxedStream> = Vec::with_capacity(streams as usize);
    for k in 0..streams {
        let mut s = opener.open()?;
        write_message(
            &mut s,
            &Control::DataHello {
                txn_id: manifest.txn_id.clone(),
                stream: k,
            },
        )?;
        conns.push(s);
    }
    // 汇总计数用原子值：多条流各自在跑，主线程只读总数回调进度
    let blocks_sent = AtomicU64::new(0);
    let bytes_sent = AtomicU64::new(0);
    // v0.14：收到的 PACK_REQ 数，进 SendReport 供测试与排障判断 pack 是否生效
    let packs_received = AtomicU64::new(0);
    // 任一条流失败就置位，其余流处理完当前块后退出，整场会话按失败收尾
    let abort = AtomicBool::new(false);
    // 总块数按清单累加：接收端只会请求已接受的块，用总数做分母足够稳定
    let total_blocks: u64 = manifest.files.iter().map(|f| f.block_count() as u64).sum();
    let shared = SenderShared {
        manifest: &manifest,
        accepted: &accepted_ids,
        sources: &source_map,
        blocks: &blocks_sent,
        bytes: &bytes_sent,
        packs: &packs_received,
        abort: &abort,
        total_blocks,
    };

    // 0 号流由主线程服务，顺便回调进度；接收端按请求下标轮转切分，
    // 保证 0 号流也始终有活，进度不会因为「活都跑到别的流上」而长时间停住。
    let mut conns_iter = conns.into_iter();
    let mut first = conns_iter.next().expect("至少有一条数据连接");
    let mut speed = SpeedWindow::default();
    let mut worker_err: Option<io::Error> = None;
    thread::scope(|scope| {
        let mut workers = Vec::new();
        for mut conn in conns_iter {
            let sh = &shared;
            workers.push(scope.spawn(move || {
                serve_sender_stream(sh, &mut *conn, control, None, None)
            }));
        }
        // 进度回调只在主线程这条流上用：把它从 Option 里取出来交给服务循环，
        // 避免在闭包里再借一层导致的寿命问题
        let main_progress = progress.take();
        if let Err(e) = serve_sender_stream(
            &shared,
            &mut *first,
            control,
            main_progress,
            Some(&mut speed),
        ) {
            abort.store(true, Ordering::Relaxed);
            worker_err = Some(e);
        }
        for w in workers {
            match w.join() {
                Ok(Ok(())) => {}
                Ok(Err(e)) => {
                    abort.store(true, Ordering::Relaxed);
                    if worker_err.is_none() {
                        worker_err = Some(e);
                    }
                }
                // 工作线程 panic：把原因带出去，不能当成正常传完
                Err(_) => {
                    abort.store(true, Ordering::Relaxed);
                    if worker_err.is_none() {
                        worker_err = Some(io_err("数据流线程异常退出"));
                    }
                }
            }
        }
    });
    if let Some(e) = worker_err {
        return Err(e);
    }

    write_message(
        &mut ctrl,
        &Control::Done {
            txn_id: manifest.txn_id.clone(),
        },
    )?;
    let (verdict, dest_dirs) = match read_message::<_, Control>(&mut ctrl)? {
        Control::VerifyOk { dest_dirs, .. } => ("OK".to_string(), dest_dirs),
        Control::VerifyFail { reason } => (reason, Vec::new()),
        other => return Err(io_err(format!("unexpected verdict: {other:?}"))),
    };

    Ok(SendReport {
        txn_id: manifest.txn_id,
        files: manifest.total_files,
        bytes: manifest.total_bytes,
        blocks_sent: blocks_sent.load(Ordering::Relaxed),
        packs: packs_received.load(Ordering::Relaxed),
        streams,
        files_skipped,
        dest_dirs,
        verdict,
    })
}

/// 多流发送时各线程共享的只读上下文与汇总计数。
struct SenderShared<'a> {
    manifest: &'a Manifest,
    accepted: &'a HashSet<String>,
    sources: &'a HashMap<String, PathBuf>,
    blocks: &'a AtomicU64,
    bytes: &'a AtomicU64,
    packs: &'a AtomicU64,
    abort: &'a AtomicBool,
    total_blocks: u64,
}

/// 速率滑动窗口：只留最近约 1 秒的采样点，用首尾差算平均速率。
/// 保留至少两个点，否则第一块时算不出差值。
const SPEED_WINDOW: Duration = Duration::from_millis(1000);

#[derive(Default)]
struct SpeedWindow {
    samples: VecDeque<(Instant, u64)>,
}

impl SpeedWindow {
    fn sample(&mut self, total: u64) -> u64 {
        let now = Instant::now();
        self.samples.push_back((now, total));
        while self.samples.len() > 2 && now.duration_since(self.samples[0].0) > SPEED_WINDOW {
            self.samples.pop_front();
        }
        if self.samples.len() < 2 {
            return 0;
        }
        let (t0, b0) = self.samples[0];
        let dt = now.duration_since(t0).as_secs_f64();
        if dt > 0.0 {
            ((total - b0) as f64 / dt) as u64
        } else {
            0
        }
    }
}

/// 一条数据流上的供块循环。多流时每条流一个线程，各自持有句柄缓存与读缓冲；
/// 清单、源路径映射与计数是共享只读/原子的，不需要加锁。
fn serve_sender_stream(
    shared: &SenderShared<'_>,
    stream: &mut dyn Stream,
    control: Option<&SendControl>,
    mut progress: Option<&mut dyn FnMut(SendProgress)>,
    mut speed: Option<&mut SpeedWindow>,
) -> io::Result<()> {
    // 缓存最近用过的文件句柄：一个大文件上千块不用重复 open；
    // 容量固定，切到别的文件时关掉最久未用的，避免小文件多时撞 FD 上限。
    let mut handles = HandleCache::new(4);
    // 一块 8 MiB，缓冲复用避免每块一次大分配
    let mut block_buf = vec![0u8; BLOCK_SIZE];

    loop {
        // 别的流失败后本流尽快退出，不把剩下的块白传完
        if shared.abort.load(Ordering::Relaxed) {
            return Err(io_err("另一条数据流失败，本次发送中止"));
        }
        // 每块之前看一次控制位。暂停时不再供块：接收端最多 4 个在途请求发完自然停住，
        // 期间连接不断、位图与已传块都还在，继续后从断点接着发。
        if let Some(c) = control {
            if c.is_cancelled() {
                return Err(io_err("发送已取消"));
            }
            while c.is_paused() {
                if c.is_cancelled() {
                    return Err(io_err("发送已取消"));
                }
                thread::sleep(Duration::from_millis(120));
            }
        }
        let (req, payload) = read_frame(stream)?;
        match req.kind {
            FrameType::BlockReq => {
                let rel = send_block(
                    shared,
                    &mut handles,
                    &mut block_buf,
                    stream,
                    req.file_id,
                    req.block_index,
                    "BLOCK_REQ",
                )?;
                report_progress(&mut progress, &mut speed, shared, &rel);
            }
            FrameType::PackReq => {
                // v0.14：一个请求带多块，按列表顺序连续回 BlockData；
                // 帧格式与单块路径完全一致，接收端照旧逐块落盘与校验。
                shared.packs.fetch_add(1, Ordering::Relaxed);
                let pr: PackRequest = serde_json::from_slice(&payload)
                    .map_err(|e| io_err(format!("PACK_REQ 解析失败: {e}")))?;
                if pr.blocks.len() > PACK_MAX_BLOCKS {
                    return Err(io_err("PACK_REQ 块数超过上限"));
                }
                for (file_id, block_index) in pr.blocks {
                    let rel = send_block(
                        shared,
                        &mut handles,
                        &mut block_buf,
                        stream,
                        file_id,
                        block_index,
                        "PACK_REQ",
                    )?;
                    report_progress(&mut progress, &mut speed, shared, &rel);
                }
            }
            FrameType::Done => return Ok(()),
            other => return Err(io_err(format!("unexpected frame: {other:?}"))),
        }
    }
}

/// 读一个块并写回 BLOCK_DATA，返回该块所属的相对路径（给进度用）。
fn send_block(
    shared: &SenderShared<'_>,
    handles: &mut HandleCache,
    block_buf: &mut [u8],
    stream: &mut dyn Stream,
    file_id: u64,
    block_index: u32,
    what: &str,
) -> io::Result<String> {
    let entry = shared
        .manifest
        .files
        .get(file_id as usize)
        .ok_or_else(|| io_err(format!("{what} references unknown fileId")))?;
    if !shared.accepted.contains(&entry.id) {
        return Err(io_err("receiver requested a file it did not accept"));
    }
    let abs = shared
        .sources
        .get(&entry.id)
        .ok_or_else(|| io_err("missing source path"))?;
    let f = handles.get_mut(file_id, abs)?;
    f.seek(SeekFrom::Start(block_index as u64 * BLOCK_SIZE as u64))?;
    let n = read_full_block(f, block_buf)?;
    let out = FrameHeader::new(FrameType::BlockData, file_id, block_index);
    write_frame(stream, &out, &block_buf[..n])?;
    shared.blocks.fetch_add(1, Ordering::Relaxed);
    shared.bytes.fetch_add(n as u64, Ordering::Relaxed);
    Ok(entry.rel_path.clone())
}

/// 进度回调只在主线程那条流上发生：字节与块数取原子总数，因此显示的是全会话进度。
fn report_progress(
    progress: &mut Option<&mut dyn FnMut(SendProgress)>,
    speed: &mut Option<&mut SpeedWindow>,
    shared: &SenderShared<'_>,
    current: &str,
) {
    let bytes = shared.bytes.load(Ordering::Relaxed);
    let speed_bps = speed.as_mut().map(|s| s.sample(bytes)).unwrap_or(0);
    if let Some(cb) = progress.as_mut() {
        cb(SendProgress {
            files: shared.manifest.total_files,
            bytes_sent: bytes,
            total_bytes: shared.manifest.total_bytes,
            blocks_sent: shared.blocks.load(Ordering::Relaxed),
            total_blocks: shared.total_blocks,
            current: current.to_string(),
            // 传输阶段的扫描字段保持默认，界面按 phase 判断该显示百分比
            speed_bps,
            ..Default::default()
        });
    }
}

/// v0.16：只查对端的可用落点，不传任何文件。握手与正式发送完全一致（含版本与信任判定），
/// 查完发一条 DESTS 让接收端干净收尾，避免它把「没发清单」记成异常会话。
pub fn list_destinations(
    opener: &dyn Opener,
    pair_code: Option<&str>,
    device: DeviceInfo,
) -> io::Result<Vec<DestEntry>> {
    let mut ctrl = opener.open()?;
    // 查询用独立 txn 前缀：并发时不会与真实传输的 txnId 撞车
    let txn_id = format!("dests-{}-{}", std::process::id(), crate::queue::now_secs());
    write_message(
        &mut ctrl,
        &Control::Hello {
            txn_id,
            device,
            pair_code: pair_code.map(str::to_string),
            version: PROTOCOL_VERSION.into(),
            block_size: BLOCK_SIZE as u32,
            caps: vec![CAP_PACK.to_string(), CAP_STREAMS.to_string()],
        },
    )?;
    let from_ack = match read_message::<_, Control>(&mut ctrl)? {
        Control::HelloAck {
            version,
            block_size,
            destinations,
            ..
        } => {
            if !protocol_compatible(&version) {
                return Err(io_err(version_reject_from_receiver(&version)));
            }
            if block_size != BLOCK_SIZE as u32 {
                return Err(io_err(block_size_reject_reason(block_size)));
            }
            destinations
        }
        Control::Busy { reason } => return Err(io_err(format!("receiver is busy: {reason}"))),
        other => return Err(io_err(format!("unexpected hello reply: {other:?}"))),
    };
    write_message(&mut ctrl, &Control::Dests)?;
    match read_message::<_, Control>(&mut ctrl)? {
        Control::DestsAck { destinations } => Ok(destinations),
        Control::Busy { reason } => Err(io_err(format!("receiver rejected query: {reason}"))),
        // 0.16 之前的接收端不认识 DESTS：握手时已经给了清单，就用它
        other => {
            let _ = other;
            Ok(from_ack)
        }
    }
}

/// 用本机设备名查询（CLI 与 Android 共用的便捷入口）。
pub fn list_destinations_for(opener: &dyn Opener, pair_code: Option<&str>) -> io::Result<Vec<DestEntry>> {
    list_destinations(opener, pair_code, local_device())
}

fn build_source_map(paths: &[PathBuf], m: &Manifest) -> io::Result<HashMap<String, PathBuf>> {
    let mut map = HashMap::new();
    // 根与入参不是一一对应（单个文件会按父目录归组），所以必须用同一套 plan_roots 规则还原基准目录
    let specs = crate::manifest::plan_roots(paths)?;
    for f in &m.files {
        let idx: usize = f
            .root_id
            .trim_start_matches('r')
            .parse()
            .map_err(|_| io_err(format!("bad rootId: {}", f.root_id)))?;
        let (_, base, _) = specs
            .get(idx)
            .ok_or_else(|| io_err(format!("rootId out of range: {}", f.root_id)))?;
        map.insert(f.id.clone(), base.join(&f.rel_path));
    }
    Ok(map)
}

/// 接收端：手动指定监听地址（v0.1 不做自动发现）。
/// max_blocks 只用于端到端测试里模拟「传一半断线」，正式运行时为 None。
pub fn run_receiver(
    listen: &str,
    dest: &Path,
    max_blocks: Option<usize>,
) -> io::Result<ReceiverReport> {
    let listener = TcpListener::bind(listen)?;
    run_receiver_on(listener, dest, max_blocks)
}

/// 允许调用方先 bind 再交给状态机：测试里可以避免「接收端还没 bind、发送端就 connect」的竞态，
/// 常驻监听也能复用同一个入口。
pub fn run_receiver_on(
    listener: TcpListener,
    dest: &Path,
    max_blocks: Option<usize>,
) -> io::Result<ReceiverReport> {
    run_receiver_with(
        &PlainAccepter { listener },
        dest,
        max_blocks,
        None,
        &HashMap::new(),
    )
}

/// 通用入口。配对码在握手之后、任何落盘之前校验；root_map 让接收端能给每个根目录改名。
/// 单会话语义：一次只服务一场传输。数据连接靠 DATA_HELLO 里的 txnId 认领，
/// 不再按 accept 顺序猜（那正是两台设备同时接入会串台的原因）。
pub fn run_receiver_with(
    accepter: &dyn Accepter,
    dest: &Path,
    max_blocks: Option<usize>,
    expected_pair_code: Option<&str>,
    root_map: &HashMap<String, String>,
) -> io::Result<ReceiverReport> {
    run_receiver_with_store(
        accepter,
        dest,
        max_blocks,
        expected_pair_code,
        root_map,
        &mut PathStore,
    )
}

/// v0.15：并行流数可配的明文入口（排障 / 测速对比用）。
pub fn run_receiver_with_streams(
    accepter: &dyn Accepter,
    dest: &Path,
    max_blocks: Option<usize>,
    expected_pair_code: Option<&str>,
    root_map: &HashMap<String, String>,
    streams: u8,
) -> io::Result<ReceiverReport> {
    run_receiver_with_store_gate_streams(
        accepter,
        dest,
        max_blocks,
        expected_pair_code,
        root_map,
        &mut PathStore,
        None,
        streams,
    )
}

/// 带落盘后端的版本：桌面/NAS 用 PathStore，Android SAF 用 tc-jni 提供的 fd 后端。
/// dest 在 SAF 下只是一个虚拟根，真正落点由后端按相对路径决定。
pub fn run_receiver_with_store(
    accepter: &dyn Accepter,
    dest: &Path,
    max_blocks: Option<usize>,
    expected_pair_code: Option<&str>,
    root_map: &HashMap<String, String>,
    store: &mut dyn Store,
) -> io::Result<ReceiverReport> {
    run_receiver_with_store_gate(
        accepter,
        dest,
        max_blocks,
        expected_pair_code,
        root_map,
        store,
        None,
    )
}

/// 带信任门禁的单会话入口（`tc recv` 用）。
/// 门禁要读到客户端证书指纹，因此必须在读完 HELLO、握手完成之后再判定。
pub fn run_receiver_with_gate(
    accepter: &dyn Accepter,
    dest: &Path,
    max_blocks: Option<usize>,
    expected_pair_code: Option<&str>,
    root_map: &HashMap<String, String>,
    gate: Option<&TrustGate>,
) -> io::Result<ReceiverReport> {
    run_receiver_with_store_gate(
        accepter,
        dest,
        max_blocks,
        expected_pair_code,
        root_map,
        &mut PathStore,
        gate,
    )
}

pub fn run_receiver_with_store_gate(
    accepter: &dyn Accepter,
    dest: &Path,
    max_blocks: Option<usize>,
    expected_pair_code: Option<&str>,
    root_map: &HashMap<String, String>,
    store: &mut dyn Store,
    gate: Option<&TrustGate>,
) -> io::Result<ReceiverReport> {
    run_receiver_with_store_gate_streams(
        accepter,
        dest,
        max_blocks,
        expected_pair_code,
        root_map,
        store,
        gate,
        DEFAULT_STREAMS,
    )
}

/// v0.15：并行流数可配的版本。`streams` 只是「最多开几条」，
/// 实际条数还要看对端是否声明 `streams` 能力与本次工作量（见 `effective_streams`）。
pub fn run_receiver_with_gate_streams(
    accepter: &dyn Accepter,
    dest: &Path,
    max_blocks: Option<usize>,
    expected_pair_code: Option<&str>,
    root_map: &HashMap<String, String>,
    gate: Option<&TrustGate>,
    streams: u8,
) -> io::Result<ReceiverReport> {
    run_receiver_with_store_gate_streams(
        accepter,
        dest,
        max_blocks,
        expected_pair_code,
        root_map,
        &mut PathStore,
        gate,
        streams,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn run_receiver_with_store_gate_streams(
    accepter: &dyn Accepter,
    dest: &Path,
    max_blocks: Option<usize>,
    expected_pair_code: Option<&str>,
    root_map: &HashMap<String, String>,
    store: &mut dyn Store,
    gate: Option<&TrustGate>,
    streams: u8,
) -> io::Result<ReceiverReport> {
    let (mut ctrl, peer_addr) = accepter.accept_with_peer()?;
    let first = read_message::<_, Control>(&mut ctrl)?;
    // 读到 HELLO 说明 TLS 握手已完成，这时取客户端证书才是可靠的
    let peer_fp = ctrl.peer_cert_fingerprint();
    let mut data = AcceptNext { accepter };
    // 单会话一次只跑一场传输，落点声明随本次调用结束自然失效，不需要跨会话保留
    let claims = PathClaims::default();
    handle_session(
        &mut *ctrl,
        first,
        &mut data,
        dest,
        // v0.16：单会话入口（CLI / Android 接收）不支持多落点，只公布默认项
        &[],
        max_blocks,
        expected_pair_code,
        root_map,
        store,
        &claims,
        peer_addr.as_deref(),
        SessionGate {
            trust: gate,
            peer_fp,
        },
        streams,
    )
}

/// 数据通道的取法。单会话按 accept 顺序取，并发 hub 按 txnId 从路由表取，
/// 两种接入方式共用同一套会话状态机。v0.15 起一次要取 `streams` 条。
pub trait DataConn {
    fn take(&mut self, txn_id: &str, streams: u8) -> io::Result<Vec<BoxedStream>>;
}

/// 单会话模式：继续 accept，直到凑齐声明了本会话 txnId 的 N 条数据连接。
/// 期间接入的其他设备一律回 BUSY，避免它们一直等到超时才发现没人接。
struct AcceptNext<'a> {
    accepter: &'a dyn Accepter,
}

impl DataConn for AcceptNext<'_> {
    fn take(&mut self, txn_id: &str, streams: u8) -> io::Result<Vec<BoxedStream>> {
        let want = streams.max(1) as usize;
        let mut got = Vec::with_capacity(want);
        while got.len() < want {
            let mut stream = self.accepter.accept()?;
            let first = match read_message::<_, Control>(&mut stream) {
                Ok(c) => c,
                // 连上就断、或一上来就发脏数据：丢掉这一条，不能因为路人毁掉正在谈的会话
                Err(_) => continue,
            };
            match first {
                Control::DataHello { txn_id: got_id, .. } if got_id == txn_id => got.push(stream),
                Control::DataHello { txn_id: got, .. } => {
                    let _ = write_message(
                        &mut stream,
                        &Control::Busy {
                            reason: format!("no such session: {got}"),
                        },
                    );
                }
                Control::Hello { .. } => {
                    let _ = write_message(
                        &mut stream,
                        &Control::Busy {
                            reason: "receiver is handling another session".into(),
                        },
                    );
                }
                other => {
                    let _ = write_message(
                        &mut stream,
                        &Control::Busy {
                            reason: format!("unexpected message on new connection: {other:?}"),
                        },
                    );
                }
            }
        }
        Ok(got)
    }
}

/// 并发会话的落点声明表。
/// 两台设备同时发同名文件时，若都按「写 .tcpart 再改名」走，会互相踩掉对方的半成品；
/// 落盘前先占住最终路径，冲突的一方立刻收到 BUSY，而不是写出两个都不完整的结果。
#[derive(Default)]
pub struct PathClaims {
    owners: Mutex<HashMap<PathBuf, String>>,
}

impl PathClaims {
    /// 全部成功才写入：任一路径被别的会话占着就整体失败，避免「占了一半再回滚」。
    pub fn try_claim(&self, txn_id: &str, paths: &[PathBuf]) -> Result<(), Vec<PathBuf>> {
        let mut owners = self.owners.lock().expect("path claims poisoned");
        let conflicts: Vec<PathBuf> = paths
            .iter()
            .filter(|p| matches!(owners.get(*p), Some(owner) if owner != txn_id))
            .cloned()
            .collect();
        if !conflicts.is_empty() {
            return Err(conflicts);
        }
        for p in paths {
            owners.insert(p.clone(), txn_id.to_string());
        }
        Ok(())
    }

    pub fn release(&self, txn_id: &str) {
        let mut owners = self.owners.lock().expect("path claims poisoned");
        owners.retain(|_, owner| owner != txn_id);
    }

    /// 当前被占用的落点数，供测试断言「会话结束后资源已释放」
    pub fn len(&self) -> usize {
        self.owners.lock().expect("path claims poisoned").len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// 一条数据流上的统计，多流时各线程各算一份，收尾相加。
struct StreamStats {
    blocks_written: u64,
    wait_us: u64,
    write_us: u64,
    req_us: u64,
}

/// 取某个文件的半成品锁。上一把锁的持有者写盘失败时互斥量会被标记中毒，
/// 但这里只是收尾报错，取回内部值继续比直接 panic 更有用。
fn lock_part(p: &Arc<Mutex<PartFile>>) -> std::sync::MutexGuard<'_, PartFile> {
    p.lock().unwrap_or_else(|e| e.into_inner())
}

/// 收一块并落盘。同一文件的并发块在这里串行，写完立刻释放句柄（v0.14 的 fd 修复）。
fn write_block_to(
    parts: &HashMap<usize, Arc<Mutex<PartFile>>>,
    file_idx: usize,
    block_idx: u32,
    payload: &[u8],
) -> io::Result<()> {
    let part = parts
        .get(&file_idx)
        .ok_or_else(|| io_err("part not opened for this file"))?;
    let mut part = lock_part(part);
    part.write_block(block_idx, payload)?;
    part.close_handles();
    Ok(())
}

/// 一条数据流上的收块循环：发请求、等块、落盘、最后回一条 DONE。
/// 单流与多流共用：多流时每条连接一个线程，`requests` 是切给它的那一段。
#[allow(clippy::too_many_arguments)]
fn recv_stream(
    stream: &mut dyn Stream,
    requests: &[(u64, u32)],
    manifest: &Manifest,
    parts: &HashMap<usize, Arc<Mutex<PartFile>>>,
    pack_enabled: bool,
    window: usize,
    max_blocks: Option<usize>,
    mut inc: Option<&mut IncVerify>,
    abort: &AtomicBool,
) -> io::Result<StreamStats> {
    let mut st = StreamStats {
        blocks_written: 0,
        wait_us: 0,
        write_us: 0,
        req_us: 0,
    };

    // v0.14：pack 路径。把待传块按「块数 ≤256、字节 ≤64 MiB」切包，
    // 一包只发一个 PackReq，然后连续收它的 N 个 BlockData。
    // 小文件时这是把「每文件一次往返」压成「每包一次」的关键。
    if pack_enabled {
        let mut idx = 0usize;
        while idx < requests.len() {
            if abort.load(Ordering::Relaxed) {
                return Err(io_err("另一条数据流失败，本次接收中止"));
            }
            let mut chunk: Vec<(u64, u32)> = Vec::new();
            let mut chunk_bytes = 0usize;
            while idx < requests.len() && chunk.len() < PACK_MAX_BLOCKS {
                let (f, b) = requests[idx];
                let fsize = manifest.files[f as usize].size;
                let start = b as u64 * BLOCK_SIZE as u64;
                let size = if fsize > start {
                    (fsize - start).min(BLOCK_SIZE as u64) as usize
                } else {
                    0
                };
                if !chunk.is_empty() && chunk_bytes + size > PACK_MAX_BYTES {
                    break;
                }
                chunk.push((f, b));
                chunk_bytes += size;
                idx += 1;
            }
            let payload = serde_json::to_vec(&PackRequest {
                blocks: chunk.clone(),
            })
            .map_err(|e| io_err(format!("PACK_REQ 编码失败: {e}")))?;
            let t_req = Instant::now();
            write_frame(stream, &FrameHeader::new(FrameType::PackReq, 0, 0), &payload)?;
            st.req_us += t_req.elapsed().as_micros() as u64;
            for (file_idx, block_idx) in chunk {
                let t_wait = Instant::now();
                let (resp, payload) = read_frame(stream)?;
                st.wait_us += t_wait.elapsed().as_micros() as u64;
                if resp.kind != FrameType::BlockData
                    || resp.file_id != file_idx
                    || resp.block_index != block_idx
                {
                    return Err(io_err("BLOCK_DATA does not match the packed request"));
                }
                let t_write = Instant::now();
                write_block_to(parts, file_idx as usize, block_idx, &payload)?;
                st.write_us += t_write.elapsed().as_micros() as u64;
                if let Some(v) = inc.as_mut() {
                    v.feed(&manifest.files, file_idx as usize, block_idx, &payload);
                }
                st.blocks_written += 1;
            }
        }
        return finish_stream(stream, st);
    }

    let mut next_req = 0usize;
    let mut inflight: VecDeque<(u64, u32)> = VecDeque::new();
    while inflight.len() < window && next_req < requests.len() {
        let (f, b) = requests[next_req];
        let t_req = Instant::now();
        write_frame(stream, &FrameHeader::new(FrameType::BlockReq, f, b), &[])?;
        st.req_us += t_req.elapsed().as_micros() as u64;
        inflight.push_back((f, b));
        next_req += 1;
    }

    while let Some((file_idx, block_idx)) = inflight.pop_front() {
        let t_wait = Instant::now();
        let (resp, payload) = read_frame(stream)?;
        st.wait_us += t_wait.elapsed().as_micros() as u64;
        if resp.kind != FrameType::BlockData
            || resp.file_id != file_idx
            || resp.block_index != block_idx
        {
            return Err(io_err("BLOCK_DATA does not match the request"));
        }
        let t_write = Instant::now();
        write_block_to(parts, file_idx as usize, block_idx, &payload)?;
        st.write_us += t_write.elapsed().as_micros() as u64;
        // 顺路把块哈希和根哈希算掉：这段 CPU 本来就在等下一块的空档里，等于白捡
        if let Some(v) = inc.as_mut() {
            v.feed(&manifest.files, file_idx as usize, block_idx, &payload);
        }
        st.blocks_written += 1;
        if let Some(max) = max_blocks {
            if st.blocks_written as usize >= max {
                // 模拟中断：位图已落盘，下次启动只补缺口
                return finish_stream(stream, st);
            }
        }
        if inflight.is_empty() && next_req >= requests.len() {
            break;
        }
        // 补发下一个请求，保持窗口始终有货在天上
        if next_req < requests.len() {
            let (f, b) = requests[next_req];
            let t_req = Instant::now();
            write_frame(stream, &FrameHeader::new(FrameType::BlockReq, f, b), &[])?;
            st.req_us += t_req.elapsed().as_micros() as u64;
            inflight.push_back((f, b));
            next_req += 1;
        }
    }
    finish_stream(stream, st)
}

/// 每条流各自收尾：发送端按这条流上的 DONE 退出它对应的线程。
fn finish_stream(stream: &mut dyn Stream, st: StreamStats) -> io::Result<StreamStats> {
    write_frame(stream, &FrameHeader::new(FrameType::Done, 0, 0), &[])?;
    Ok(st)
}

/// 一场传输的状态机。控制通道已就绪，数据通道由 data_conn 提供。
/// 抽出来的目的是让单会话（tc recv / Android）与并发 hub 走同一份协议逻辑，行为不会分叉。
fn handle_session(
    ctrl: &mut dyn Stream,
    first: Control,
    data_conn: &mut dyn DataConn,
    dest: &Path,
    // v0.16：可被发送方选择的落点（来自配置）。空 = 只有默认落点。
    destinations: &[Destination],
    max_blocks: Option<usize>,
    expected_pair_code: Option<&str>,
    root_map: &HashMap<String, String>,
    store: &mut dyn Store,
    claims: &PathClaims,
    peer_addr: Option<&str>,
    gate: SessionGate<'_>,
    streams: u8,
) -> io::Result<ReceiverReport> {
    // 设备信息随 HELLO 一起到，这里先留下：历史记录要写清来源，
    // 不能等到协议跑完再回头找（那时控制连接可能已经断开）
    let (txn_id, device, version, pair_code, peer_block_size, peer_caps) = match first {
        Control::Hello {
            txn_id,
            pair_code,
            device,
            version,
            block_size,
            caps,
        } => (txn_id, device, version, pair_code, block_size, caps),
        other => {
            // 不是 HELLO 的连接要明确回绝，否则对端只能干等到超时
            let _ = write_message(
                ctrl,
                &Control::Busy {
                    reason: format!("expected HELLO, got {other:?}"),
                },
            );
            return Err(io_err(format!("expected HELLO, got {other:?}")));
        }
    };

    // 版本门禁放在信任判定之前：旧端没有客户端证书，先拦版本才能给出「升级」这条正确提示
    if !protocol_compatible(&version) {
        let reason = version_reject_reason(&version);
        let _ = write_message(
            ctrl,
            &Control::Busy {
                reason: reason.clone(),
            },
        );
        return Err(io_err(reason));
    }

    // 块大小先于信任判定：这是编译期常量，两端不一致时传到最后只会得到看不懂的哈希错误。
    // 旧端不带这个字段（反序列化为 0），提示里直接写明「旧版 4 MiB」并给出升级动作。
    if peer_block_size != BLOCK_SIZE as u32 {
        let reason = block_size_reject_reason(peer_block_size);
        let _ = write_message(
            ctrl,
            &Control::Busy {
                reason: reason.clone(),
            },
        );
        return Err(io_err(reason));
    }

    if let Some(g) = gate.trust {
        // 会话层拿着与 CLI/管理页同一份信任库；撤销后下一场立即按新状态判定
        if let GateVerdict::Deny(reason) =
            g.evaluate(gate.peer_fp, &device, pair_code.as_deref())?
        {
            let _ = write_message(
                ctrl,
                &Control::Busy {
                    reason: reason.clone(),
                },
            );
            return Err(io_err(reason));
        }
    } else if let Some(expected) = expected_pair_code {
        // 无门禁的旧路径（明文/单测）保持原语义，避免测试与排障入口行为漂移
        if pair_code.as_deref() != Some(expected) {
            // 先回一句可读原因再断开：不回的话发送端只看到 EOF，
            // 会被当成网络抖动去退避重试——这明明是「重试多少次都没用」的错误
            let reason = "pair code mismatch";
            let _ = write_message(ctrl, &Control::Busy { reason: reason.into() });
            return Err(io_err(reason));
        }
    }
    write_message(
        ctrl,
        &Control::HelloAck {
            version: PROTOCOL_VERSION.into(),
            block_size: BLOCK_SIZE as u32,
            caps: vec![CAP_PACK.to_string(), CAP_STREAMS.to_string()],
            destinations: dest_list(destinations, dest),
        },
    )?;

    let (manifest, force, dest_id) = match read_message::<_, Control>(ctrl)? {
        Control::Manifest {
            manifest,
            force,
            dest_id,
        } => (manifest, force, dest_id),
        // v0.16：只查落点。回完就结束，不写历史、不占会话计数（报告里标 query_only）
        Control::Dests => {
            write_message(
                ctrl,
                &Control::DestsAck {
                    destinations: dest_list(destinations, dest),
                },
            )?;
            return Ok(ReceiverReport {
                txn_id,
                files: 0,
                bytes: 0,
                blocks_written: 0,
                resumed_blocks: 0,
                streams_used: 0,
                query_only: true,
                verified: false,
                interrupted: false,
                device: Some(device),
                roots: Vec::new(),
                peer_addr: peer_addr.map(str::to_string),
                wait_us: 0,
                write_us: 0,
                req_us: 0,
                verify_us: 0,
                skipped_files: 0,
                skipped_bytes: 0,
            });
        }
        other => return Err(io_err(format!("expected MANIFEST, got {other:?}"))),
    };
    // v0.16：落点只在接收端配置里映射，未知标识在回 ACK 之前就拒绝
    let dest = match resolve_destination(destinations, dest_id.as_deref(), dest) {
        Ok(p) => p,
        Err(reason) => {
            write_message(ctrl, &Control::Busy { reason: reason.clone() })?;
            return Err(io_err(reason));
        }
    };
    let layout = plan_layout(&dest, &manifest, root_map)?;

    // 增量同步：目标位置上已经是同一份内容的文件不再请求数据块。
    // 必须在 ACK 之前算出来——发送端就是按 accepted_file_ids 决定「哪些文件不用回数据」。
    // v0.12：发送端勾了「强制重传」就整份不跳过，全部按新文件收。
    let skipped = if force {
        Vec::new()
    } else {
        already_present(&layout.final_paths, &manifest.files)
    };
    let skipped_set: HashSet<usize> = skipped.iter().copied().collect();
    let skipped_bytes: u64 = skipped.iter().map(|&i| manifest.files[i].size).sum();
    // 跳过的文件不必占用落点声明：它们不会被写，也不该挡住同时进行的另一场会话
    let active_paths: Vec<PathBuf> = layout
        .final_paths
        .iter()
        .enumerate()
        .filter(|(i, _)| !skipped_set.contains(i))
        .map(|(_, p)| p.clone())
        .collect();

    // 并发会话共享同一个落点：先占住最终路径再回 MANIFEST_ACK，
    // 冲突的一方在开始写盘之前就拿到拒绝，不会留下半成品
    if let Err(conflicts) = claims.try_claim(&txn_id, &active_paths) {
        let reason = format!(
            "destination busy: {}",
            conflicts
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        );
        write_message(ctrl, &Control::Busy { reason: reason.clone() })?;
        return Err(io_err(reason));
    }

    let accepted_roots: Vec<String> = manifest.roots.iter().map(|r| r.id.clone()).collect();
    let accepted_file_ids: Vec<String> = manifest
        .files
        .iter()
        .enumerate()
        .filter(|(i, _)| !skipped_set.contains(i))
        .map(|(_, f)| f.id.clone())
        .collect();
    // v0.15：并行条数要在 ACK 里告诉发送端，所以先按「本次要传多少块」收敛一次。
    // 这里只算总数，不打开任何文件；真正的缺块集合在 ACK 之后才逐文件读位图。
    let active_blocks: u64 = manifest
        .files
        .iter()
        .enumerate()
        .filter(|(i, _)| !skipped_set.contains(i))
        .map(|(_, f)| f.block_count() as u64)
        .sum();
    let streams = effective_streams(streams, &peer_caps, active_blocks, max_blocks);
    write_message(
        ctrl,
        &Control::ManifestAck {
            accepted_roots,
            skipped_roots: Vec::new(),
            accepted_file_ids,
            streams,
        },
    )?;

    store.make_dirs(&layout.dirs)?;

    // v0.15：每个文件一把锁。多流同时拉同一个大文件的不同块时，
    // 同文件的写盘与位图追加必须串行，不同文件之间互不阻塞。
    let mut parts: HashMap<usize, Arc<Mutex<PartFile>>> = HashMap::new();
    let mut requests: Vec<(u64, u32)> = Vec::new();
    let mut resumed_blocks = 0u64;
    for (i, f) in manifest.files.iter().enumerate() {
        if skipped_set.contains(&i) {
            continue;
        }
        let mut part = store.open_part(&layout.part_paths[i], f)?;
        let missing = part.missing();
        resumed_blocks += f.block_count() as u64 - missing.len() as u64;
        for b in missing {
            requests.push((i as u64, b));
        }
        // v0.14：位图读进内存后立刻释放文件与位图句柄。
        // 否则几千个小文件会在传输开始前就各占两个 fd，NAS 上直接 Too many open files。
        part.close_handles();
        parts.insert(i, Arc::new(Mutex::new(part)));
    }

    // 数据通道在这里才取：之前必须先回 MANIFEST_ACK，发送端收到它才会开数据连接
    let conns = data_conn.take(&txn_id, streams)?;
    // P0：分别累计「等数据」与「写盘」的耗时，用来判断慢在哪一段
    let mut wait_us = 0u64;
    let mut write_us = 0u64;
    // 发请求本身也可能阻塞（对端收得慢时 socket 发送缓冲会满），单独累计用于归因
    let mut req_us = 0u64;
    // 预取窗口：先抛出几个请求再开始收，让「等下一块的数据」与「写上一块」重叠。
    // 只有模拟中断（max_blocks）时退回窗口 1，否则多发出的请求会让续传语义对不上。
    // v0.14：双方都声明 pack 才启用；模拟中断仍走逐块，保证续传测试语义不变。
    let pack_enabled = max_blocks.is_none() && peer_caps.iter().any(|c| c == CAP_PACK);
    let window = if max_blocks.is_some() { 1 } else { 4 };
    // 全新传输才开增量校验：续传的块集合不连续，累积不出正确根哈希；
    // 多流时块到达顺序按流交错，也无法累积根哈希，收尾一律退回全量校验。
    let mut inc = IncVerify::new(resumed_blocks == 0 && conns.len() <= 1);
    let mut blocks_written = 0u64;
    let abort = AtomicBool::new(false);

    if conns.len() <= 1 {
        let mut conn = conns
            .into_iter()
            .next()
            .ok_or_else(|| io_err("data channel did not arrive"))?;
        let st = recv_stream(
            &mut *conn,
            &requests,
            &manifest,
            &parts,
            pack_enabled,
            window,
            max_blocks,
            Some(&mut inc),
            &abort,
        )?;
        blocks_written += st.blocks_written;
        wait_us += st.wait_us;
        write_us += st.write_us;
        req_us += st.req_us;
    } else {
        // v0.15：按请求下标轮转切分，同一文件的不同块会落到不同流上。
        let n = conns.len();
        let mut split: Vec<Vec<(u64, u32)>> = vec![Vec::new(); n];
        for (i, r) in requests.iter().enumerate() {
            split[i % n].push(*r);
        }
        let mut stats: Vec<io::Result<StreamStats>> = Vec::new();
        // 传引用进去而不是把整个清单/表 move 进闭包：每条流都要看同一份只读数据
        let manifest_ref = &manifest;
        let parts_ref = &parts;
        let abort_ref = &abort;
        thread::scope(|scope| {
            let mut handles = Vec::new();
            for (k, mut conn) in conns.into_iter().enumerate() {
                let reqs = &split[k];
                handles.push(scope.spawn(move || {
                    recv_stream(
                        &mut *conn,
                        reqs,
                        manifest_ref,
                        parts_ref,
                        pack_enabled,
                        window,
                        None,
                        None,
                        abort_ref,
                    )
                }));
            }
            // 收尾在主线程做：等所有流把各自的缺口收完。
            // 一旦某条流报错就立刻置 abort，其余流在处理完当前块后退出，不再白跑剩下的块。
            for h in handles {
                match h.join() {
                    Ok(Ok(st)) => stats.push(Ok(st)),
                    Ok(Err(e)) => {
                        abort_ref.store(true, Ordering::Relaxed);
                        stats.push(Err(e));
                    }
                    Err(_) => {
                        abort_ref.store(true, Ordering::Relaxed);
                        stats.push(Err(io_err("数据流线程异常退出")));
                    }
                }
            }
        });
        let mut failure: Option<io::Error> = None;
        for r in stats {
            match r {
                Ok(st) => {
                    blocks_written += st.blocks_written;
                    wait_us += st.wait_us;
                    write_us += st.write_us;
                    req_us += st.req_us;
                }
                Err(e) => {
                    // 已有流报错：让其余流也尽快停，不再把剩余的块白传完
                    abort.store(true, Ordering::Relaxed);
                    if failure.is_none() {
                        failure = Some(e);
                    }
                }
            }
        }
        if let Some(e) = failure {
            return Err(e);
        }
    }

    let interrupted = (blocks_written as usize) < requests.len();

    // 协议第 5 步：发送端会在控制通道补一条 DONE，必须先读掉再回裁决。
    // 漏掉这一步会让控制连接残留未读数据，Windows 关闭时发 RST，发送端就收不到 VERIFY_OK。
    match read_message::<_, Control>(ctrl)? {
        Control::Done { .. } => {}
        other => return Err(io_err(format!("expected DONE, got {other:?}"))),
    }

    let base = ReceiverReport {
        txn_id: txn_id.clone(),
        files: manifest.total_files,
        bytes: manifest.total_bytes,
        blocks_written,
        resumed_blocks,
        streams_used: streams,
        query_only: false,
        verified: false,
        interrupted,
        device: Some(device),
        roots: layout.roots.clone(),
        peer_addr: peer_addr.map(str::to_string),
        wait_us,
        write_us,
        req_us,
        verify_us: 0,
        skipped_files: skipped.len() as u64,
        skipped_bytes,
    };

    if interrupted {
        // 给发送端一个明确的收尾，避免它卡在等裁决；正式协议会有单独的 INTERRUPTED 消息
        write_message(
            ctrl,
            &Control::VerifyFail {
                reason: "interrupted (resume pending)".into(),
            },
        )?;
        return Ok(base);
    }

    let mut failure: Option<String> = None;
    let t_verify = std::time::Instant::now();
    // 增量校验已经把每个块的哈希和每个文件的根哈希都核对过了，这里只需要确认块确实收齐，
    // 再省掉一遍全量读盘；任何一项不满足（续传、块缺失、哈希不符）都退回原路径重算，绝不跳过校验。
    let inc_ok = inc.finish(&manifest.files).is_none()
        && parts.values().all(|p| lock_part(p).complete());
    for (i, f) in manifest.files.iter().enumerate() {
        // 增量跳过的文件没有 .tcpart：它们的内容在 ACK 之前就已核对过，
        // 这里只需要处理真正收过数据的文件
        let Some(part) = parts.get(&i) else {
            continue;
        };
        let mut part = lock_part(part);
        if !inc_ok {
            if let Err(e) = part.verify(f) {
                failure = Some(format!("{}: {e}", f.rel_path));
                break;
            }
        }
        // v0.14：校验后立即释放句柄，避免收尾阶段几千个文件再次占满 fd
        part.close_handles();
        if let Err(e) = if force {
            // 强制重传：先让旧文件改名让位，本次文件保留源文件名
            store.finalize_aside(&mut part, &layout.final_paths[i])
        } else {
            store.finalize(&mut part, &layout.final_paths[i])
        } {
            failure = Some(format!("{}: {e}", f.rel_path));
            break;
        }
    }

    let verify_us = t_verify.elapsed().as_micros() as u64;
    match failure {
        Some(reason) => {
            write_message(ctrl, &Control::VerifyFail { reason })?;
            Ok(ReceiverReport { verify_us, ..base })
        }
        None => {
            // 顺带回传落点根目录名：发送端据此显示「传到哪个文件夹了」（v0.11）
            write_message(
                ctrl,
                &Control::VerifyOk {
                    txn_id,
                    dest_dirs: layout.roots.clone(),
                },
            )?;
            Ok(ReceiverReport {
                verified: true,
                verify_us,
                ..base
            })
        }
    }
}

/// 一条会话结束后的结果，交给调用方记历史/日志。
pub struct HubReport {
    pub txn_id: String,
    /// 对端地址：被拒绝的会话没有 ReceiverReport，日志里仍要能标出是谁
    pub peer_addr: Option<String>,
    pub result: io::Result<ReceiverReport>,
}

/// 会话等数据连接的上限。正常发送端读完 MANIFEST_ACK 就会开第二条连接；
/// 超过这个时间还没来，说明对方已经放弃，继续占着名额没有意义。
const DATA_WAIT_SECS: u64 = 60;

/// 并发接收入口（daemon 用）。
/// 每条新连接先读第一条控制消息再分流：HELLO 建会话，DATA_HELLO 把连接交给对应会话。
/// 会话各占一个线程、各自持有落盘后端，只共享落点声明表，所以两场传输不会互相踩。
pub struct ReceiverHub {
    accepter: Arc<dyn Accepter>,
    dest: PathBuf,
    expected_pair_code: Option<String>,
    root_map: HashMap<String, String>,
    max_sessions: usize,
    /// txnId → 数据连接投递口；这张表的大小同时就是「当前活跃会话数」
    pending: Mutex<HashMap<String, mpsc::Sender<BoxedStream>>>,
    claims: PathClaims,
    /// 信任门禁：daemon 注入；单测与 Android 不注入时保持原有放行语义
    gate: Option<Arc<TrustGate>>,
    /// 等数据连接的秒数；测试会调小它，免得为验证释放逻辑真等一分钟
    data_wait_secs: u64,
    /// v0.15：本实例最多允许的并行流数（1 = 关闭）
    streams: u8,
    /// v0.16：可被发送方选择的落点；空 = 只公布默认项
    destinations: Vec<Destination>,
}

impl ReceiverHub {
    pub fn new(
        accepter: Arc<dyn Accepter>,
        dest: PathBuf,
        expected_pair_code: Option<String>,
        root_map: HashMap<String, String>,
        max_sessions: usize,
    ) -> Self {
        Self {
            accepter,
            dest,
            expected_pair_code,
            root_map,
            // 上限至少为 1，否则谁也别想连上
            max_sessions: max_sessions.max(1),
            pending: Mutex::new(HashMap::new()),
            claims: PathClaims::default(),
            gate: None,
            data_wait_secs: DATA_WAIT_SECS,
            streams: DEFAULT_STREAMS,
            destinations: Vec::new(),
        }
    }

    /// v0.16：设置可被发送方选择的落点。必须在 `Arc::new` 之前调用。
    pub fn with_destinations(mut self, destinations: Vec<Destination>) -> Self {
        self.destinations = destinations;
        self
    }

    /// 设置并行流数（1 = 关闭）。与 `with_gate` 一样，必须在 `Arc::new` 之前调用。
    pub fn with_streams(mut self, streams: u8) -> Self {
        self.streams = streams.clamp(1, MAX_STREAMS);
        self
    }

    /// 注入信任门禁。必须在 `Arc::new` 之前调用，之后 hub 会被多个线程共享。
    pub fn with_gate(mut self, gate: Arc<TrustGate>) -> Self {
        self.gate = Some(gate);
        self
    }

    /// 只给测试用：缩短等待数据连接的时间，验证「会话结束后名额与落点都归还」
    pub fn with_data_wait_secs(mut self, secs: u64) -> Self {
        self.data_wait_secs = secs.max(1);
        self
    }

    /// 当前活跃会话数，供测试与诊断确认会话结束后名额已归还
    pub fn active(&self) -> usize {
        self.pending.lock().expect("hub pending poisoned").len()
    }

    /// 当前被占用的落点数
    pub fn claimed_paths(&self) -> usize {
        self.claims.len()
    }

    /// 常驻接收入口。读首包放到独立线程里做，accept 循环不会被一条沉默的连接卡住。
    pub fn serve(self: &Arc<Self>, on_report: Arc<dyn Fn(HubReport) + Send + Sync>) -> io::Result<()> {
        loop {
            // 对端地址要在这里取：流进线程后只有读写能力，已经拿不到 TCP 层信息
            let (stream, peer_addr) = self.accepter.accept_with_peer()?;
            let hub = Arc::clone(self);
            let callback = Arc::clone(&on_report);
            thread::spawn(move || {
                if let Some(report) = hub.dispatch(stream, peer_addr) {
                    callback(report);
                }
            });
        }
    }

    /// 登记会话并预留名额；超限或 txnId 重复都明确拒绝，而不是排队挂住。
    fn register(&self, txn_id: &str) -> Result<mpsc::Receiver<BoxedStream>, String> {
        let mut pending = self.pending.lock().expect("hub pending poisoned");
        if pending.contains_key(txn_id) {
            return Err(format!("duplicate txnId: {txn_id}"));
        }
        if pending.len() >= self.max_sessions {
            return Err(format!("同时进行的会话已达上限（{}）", self.max_sessions));
        }
        let (tx, rx) = mpsc::channel();
        pending.insert(txn_id.to_string(), tx);
        Ok(rx)
    }

    fn dispatch(&self, mut stream: BoxedStream, peer_addr: Option<String>) -> Option<HubReport> {
        let first: Control = match read_message(&mut stream) {
            Ok(c) => c,
            // 连上就断、或一上来就不是控制消息：直接丢弃，不占会话名额
            Err(_) => return None,
        };
        match first.clone() {
            Control::Hello { .. } => {
                // 首包读完即握手完成，此时才能拿到客户端证书
                let peer_fp = stream.peer_cert_fingerprint();
                self.start_session(stream, first, peer_addr, peer_fp)
            }
            Control::DataHello { txn_id, .. } => {
                self.route_data(txn_id, stream);
                None
            }
            other => {
                let _ = write_message(
                    &mut stream,
                    &Control::Busy {
                        reason: format!("unexpected first message: {other:?}"),
                    },
                );
                None
            }
        }
    }

    fn route_data(&self, txn_id: String, stream: BoxedStream) {
        let sender = self
            .pending
            .lock()
            .expect("hub pending poisoned")
            .get(&txn_id)
            .cloned();
        let Some(sender) = sender else {
            // 会话已结束或根本不存在：必须明确回 BUSY，否则发送端会等一个永远不会来的块请求
            let mut s = stream;
            let _ = write_message(
                &mut s,
                &Control::Busy {
                    reason: format!("no such session: {txn_id}"),
                },
            );
            return;
        };
        // 投递失败说明会话线程已退出（超时或出错），连接随发送端一起丢弃即可
        let _ = sender.send(stream);
    }

    fn start_session(
        &self,
        mut stream: BoxedStream,
        first: Control,
        peer_addr: Option<String>,
        peer_fp: Option<[u8; 32]>,
    ) -> Option<HubReport> {
        let txn_id = match &first {
            Control::Hello { txn_id, .. } => txn_id.clone(),
            _ => unreachable!("dispatch 已保证首条是 HELLO"),
        };
        let rx = match self.register(&txn_id) {
            Ok(rx) => rx,
            Err(reason) => {
                let _ = write_message(&mut stream, &Control::Busy { reason });
                return Some(HubReport {
                    txn_id,
                    peer_addr,
                    result: Err(io_err("session rejected: capacity")),
                });
            }
        };

        let mut data = ChannelData {
            rx,
            wait: Duration::from_secs(self.data_wait_secs),
        };
        // 每个会话一份独立落盘后端：PathStore 无状态，互不共享文件句柄
        let mut store = PathStore;
        let result = handle_session(
            &mut *stream,
            first,
            &mut data,
            &self.dest,
            &self.destinations,
            None,
            self.expected_pair_code.as_deref(),
            &self.root_map,
            &mut store,
            &self.claims,
            peer_addr.as_deref(),
            SessionGate {
                trust: self.gate.as_deref(),
                peer_fp,
            },
            self.streams,
        );

        // 会话结束必须归还名额与落点声明，否则下一次传输会被自己占着的路径挡回去
        self.pending
            .lock()
            .expect("hub pending poisoned")
            .remove(&txn_id);
        self.claims.release(&txn_id);
        Some(HubReport {
            txn_id,
            peer_addr,
            result,
        })
    }
}

/// hub 模式下会话拿数据通道的方式：等分流线程把连接投进通道。
struct ChannelData {
    rx: mpsc::Receiver<BoxedStream>,
    wait: Duration,
}

impl DataConn for ChannelData {
    fn take(&mut self, _txn_id: &str, streams: u8) -> io::Result<Vec<BoxedStream>> {
        let want = streams.max(1) as usize;
        let mut got = Vec::with_capacity(want);
        for _ in 0..want {
            got.push(
                self.rx
                    .recv_timeout(self.wait)
                    .map_err(|_| io_err("data channel did not arrive in time"))?,
            );
        }
        Ok(got)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// v0.15：并行条数必须同时满足「对端声明能力」「工作量够」「非模拟中断」。
    #[test]
    fn effective_streams_needs_capability_and_work() {
        let caps = vec!["pack".to_string(), "streams".to_string()];
        assert_eq!(effective_streams(4, &caps, 100, None), 4);
        assert_eq!(effective_streams(4, &caps, 7, None), 1, "块太少不值得开多流");
        assert_eq!(effective_streams(4, &[], 100, None), 1, "对端没声明必须单流");
        assert_eq!(effective_streams(4, &caps, 100, Some(1)), 1, "模拟中断必须单流");
        assert_eq!(effective_streams(99, &caps, 100, None), MAX_STREAMS);
    }

    /// 小文件多时必须封顶：旧实现只插不删，句柄数会跟着文件数一路涨到 FD 上限。
    #[test]
    fn handle_cache_caps_open_files() {
        let dir = std::env::temp_dir().join(format!("tc-handle-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut paths = Vec::new();
        for i in 0..20u64 {
            let p = dir.join(format!("f{i}.bin"));
            std::fs::File::create(&p).unwrap().write_all(b"x").unwrap();
            paths.push(p);
        }

        let mut cache = HandleCache::new(4);
        for (i, p) in paths.iter().enumerate() {
            cache.get_mut(i as u64, p).unwrap();
            assert!(cache.open_count() <= 4, "句柄数必须封顶在 4");
        }
        // 回头再访问老文件也不该增长（最多是淘汰后重新 open）
        for (i, p) in paths.iter().enumerate().rev() {
            cache.get_mut(i as u64, p).unwrap();
            assert!(cache.open_count() <= 4);
        }
    }
}
