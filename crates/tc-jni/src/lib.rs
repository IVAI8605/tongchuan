//! v0.4 准备层：把接收端内核包成「句柄式」API，供 Android 常驻服务调用。
//! JNI 只做字符串转换和签名转发，核心逻辑全部是纯 Rust，所以不需要 JVM 也能编译、测试。
//! 为什么要句柄：Android 端服务生命周期很长，不能每个会话重建 TLS 上下文与监听端口。

use jni::objects::{GlobalRef, JClass, JString, JValue};
use jni::sys::{jboolean, jlong, jstring};
use jni::{JNIEnv, JavaVM};
use serde_json::json;
use std::collections::HashMap;
use std::fs;
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::{self, JoinHandle};
use tc_core::daemon::Config;
use tc_core::discover::{announce_loop, announcement_for, listen as discover_listen};
use tc_core::manifest::{local_device, DeviceInfo};
// v0.8 剪贴板三端：手机侧直接复用桌面 CLI 的同一套客户端与协议
use tc_core::clip::{self, ClipClient};
use tc_core::tls::{client_config_with_identity, server_config, Identity};
use tc_core::transport::{TlsAccepter, TlsOpener};
// 手机发送（M1，路径模式）：复用桌面端已有的发送会话，不新开协议分支
use tc_core::run_sender_with_device_control_opts;
use tc_core::SendControl;
use tc_core::SendProgress;
// P3：手机才是落盘入口，接收端必须和桌面一样走信任门禁，不能只靠 PC 侧把关
use tc_core::{run_receiver_with_gate, TrustGate, TrustMode, TrustStore};

static NEXT_ID: AtomicU64 = AtomicU64::new(1);
static HANDLES: OnceLock<Mutex<HashMap<u64, Arc<Runtime>>>> = OnceLock::new();
/// 最近一次发送的进度。Android 侧轮询这个快照显示百分比：
/// 用户点发送到结束之间才有意义，页面不必常驻轮询（只在发送期间每 300ms 拉一次）。
static SEND_PROGRESS: OnceLock<Mutex<Option<SendProgressView>>> = OnceLock::new();
static SEND_ACTIVE: AtomicBool = AtomicBool::new(false);
/// 当前这次发送的控制位（暂停 / 取消）。每次 sendJson 换一个新的，
/// 界面通过 sendPause / sendCancel 拿到同一个实例来改状态。
static SEND_CTL: OnceLock<Mutex<Option<Arc<SendControl>>>> = OnceLock::new();

fn ctl_slot() -> &'static Mutex<Option<Arc<SendControl>>> {
    SEND_CTL.get_or_init(|| Mutex::new(None))
}

fn current_ctl() -> Option<Arc<SendControl>> {
    ctl_slot().lock().ok().and_then(|g| g.clone())
}

/// 进度快照（给宿主看的扁平结构，时间戳/百分比由 Rust 侧算好，界面不用再换算）
#[derive(Debug, Clone, Default)]
pub struct SendProgressView {
    pub bytes_sent: u64,
    pub total_bytes: u64,
    pub blocks_sent: u64,
    pub total_blocks: u64,
    pub files: u64,
    pub current: String,
    pub scanning: bool,
    pub scan_files: u64,
    pub scan_bytes: u64,
    /// 瞬时速率（字节/秒），由 tc-core 按 1 秒窗口平滑后给出
    pub speed_bps: u64,
}

fn progress_slot() -> &'static Mutex<Option<SendProgressView>> {
    SEND_PROGRESS.get_or_init(|| Mutex::new(None))
}

/// 当前发送进度：{"active":bool,"percent":0-100,"bytesSent":...,"current":...}
pub fn send_progress_json() -> String {
    let active = SEND_ACTIVE.load(Ordering::Relaxed);
    let snapshot = progress_slot().lock().ok().and_then(|g| g.clone());
    match snapshot {
        Some(p) => {
            let percent = if p.total_bytes == 0 {
                0
            } else {
                ((p.bytes_sent as f64 / p.total_bytes as f64) * 100.0).round() as u64
            };
            json!({
                "active": active,
                "percent": percent.min(100),
                "phase": if p.scanning { "scanning" } else { "sending" },
                "scanFiles": p.scan_files,
                "scanBytes": p.scan_bytes,
                "bytesSent": p.bytes_sent,
                "totalBytes": p.total_bytes,
                "blocksSent": p.blocks_sent,
                "totalBlocks": p.total_blocks,
                "files": p.files,
                "current": p.current,
                "speedBps": p.speed_bps,
            })
            .to_string()
        }
        None => json!({ "active": active, "percent": 0 }).to_string(),
    }
}

/// 一次会话收完后的摘要，交给宿主层去发通知。
/// 只传原始数字不传报表结构体，是为了让 Android 侧不必理解 tc-core 的会话细节。
#[derive(Debug, Clone)]
pub struct TransferEvent {
    pub files: u64,
    pub bytes: u64,
    pub interrupted: bool,
    pub verified: bool,
    /// v0.12：本次按「同路径同内容」跳过的文件数，通知里要能看出省了多少
    pub skipped_files: u64,
    /// v0.12：落点根目录名（相对接收目录），通知里显示「存到了哪个文件夹」
    pub roots: Vec<String>,
    /// v0.12：来源设备名，通知里要能看出「谁发来的」
    pub device: String,
}

/// 收完文件时的回调。用事件而不是让 Java 轮询 statusJson：
/// 常驻轮询会周期性唤醒 CPU，与「尽量省电」冲突。
pub type Notifier = Arc<dyn Fn(TransferEvent) + Send + Sync + 'static>;

fn handles() -> &'static Mutex<HashMap<u64, Arc<Runtime>>> {
    HANDLES.get_or_init(|| Mutex::new(HashMap::new()))
}

struct Runtime {
    stop: Arc<AtomicBool>,
    listen: String,
    /// 实际生效的信任策略，回给界面展示（Android 侧不提供 open 开关，排障改 PC 侧）
    trust_mode: String,
    sessions: AtomicU64,
    bytes: AtomicU64,
    last_error: Mutex<Option<String>>,
    thread: Mutex<Option<JoinHandle<()>>>,
    announce: Mutex<Option<JoinHandle<()>>>,
    notifier: Option<Notifier>,
}

/// 启动一个长期接收端。配置是 JSON（字段与 TOML 配置一致，serde 会自动补默认值）。
/// 返回句柄；失败返回可读原因，方便宿主侧直接显示。
/// 桌面/测试用这个无回调版本，Android 用带回调的 `start_receiver_with`。
pub fn start_receiver(config_json: &str) -> Result<u64, String> {
    start_receiver_with(config_json, None)
}

pub fn start_receiver_with(config_json: &str, notifier: Option<Notifier>) -> Result<u64, String> {
    let cfg: Config = serde_json::from_str(config_json).map_err(|e| e.to_string())?;
    cfg.validate().map_err(|e| e.to_string())?;
    fs::create_dir_all(&cfg.destination).map_err(|e| e.to_string())?;
    let identity = Identity::load_or_create(&cfg.identity_dir).map_err(|e| e.to_string())?;
    let tls = server_config(&identity).map_err(|e| e.to_string())?;
    let listener = TcpListener::bind(&cfg.listen).map_err(|e| e.to_string())?;

    let stop = Arc::new(AtomicBool::new(false));
    let rt = Arc::new(Runtime {
        stop: stop.clone(),
        listen: cfg.listen.clone(),
        trust_mode: cfg.trust_mode.clone(),
        sessions: AtomicU64::new(0),
        bytes: AtomicU64::new(0),
        last_error: Mutex::new(None),
        thread: Mutex::new(None),
        announce: Mutex::new(None),
        notifier,
    });

    let acc = TlsAccepter {
        listener,
        config: tls,
    };
    let rt2 = rt.clone();
    let cfg2 = cfg.clone();
    let stop2 = stop.clone();
    let worker = thread::spawn(move || {
        // 门禁在循环外建一次：信任表每次判定前自己 stat 重放，撤销后下一场会话立即生效，
        // 所以不需要每轮重建。identity_dir 与界面读的信任库是同一个目录。
        let gate = TrustGate::new(
            Arc::new(TrustStore::new(&cfg2.identity_dir)),
            TrustMode::parse(&cfg2.trust_mode),
            cfg2.pair_code.clone(),
        );
        while !stop2.load(Ordering::Relaxed) {
            match run_receiver_with_gate(
                &acc,
                &cfg2.destination,
                None,
                cfg2.pair_code.as_deref(),
                &HashMap::new(),
                Some(&gate),
            ) {
                Ok(r) => {
                    rt2.sessions.fetch_add(1, Ordering::Relaxed);
                    rt2.bytes.fetch_add(r.bytes, Ordering::Relaxed);
                    // 只在会话真正结束（含中断）时通知一次，不按块通知，避免大文件刷屏刷声。
                    if let Some(n) = &rt2.notifier {
                        n(TransferEvent {
                            files: r.files,
                            bytes: r.bytes,
                            interrupted: r.interrupted,
                            verified: r.verified,
                            skipped_files: r.skipped_files,
                            roots: r.roots.clone(),
                            device: r
                                .device
                                .as_ref()
                                .map(|d| d.name.clone())
                                .unwrap_or_default(),
                        });
                    }
                }
                Err(e) => {
                    // 未见过的设备会走到这里：原因（如 untrusted xxxxxxxx）留给界面提示，
                    // 同时门禁已把这次尝试记进 trust.jsonl，界面「待确认」里能直接批准
                    *rt2.last_error.lock().expect("last_error poisoned") = Some(e.to_string());
                }
            }
        }
    });
    *rt.thread.lock().expect("thread poisoned") = Some(worker);

    // 端口取自实际监听地址，而不是写死 53318：测试里用随机端口时公告的也得是同一个。
    // 发送组播不需要 WifiManager 的 MulticastLock（那玩意儿是收组播才要），所以这里不碰 Android API。
    if let Some(port) = cfg.listen.rsplit(':').next().and_then(|p| p.parse::<u16>().ok()) {
        let mut dev = local_device();
        if !cfg.alias.is_empty() {
            dev.name = cfg.alias.clone();
        }
        let ann = announcement_for(
            dev,
            identity.fingerprint_hex(),
            port,
            env!("CARGO_PKG_VERSION"),
        );
        let stop3 = stop.clone();
        let worker = thread::spawn(move || {
            announce_loop(ann, std::time::Duration::from_secs(5), &stop3);
        });
        *rt.announce.lock().expect("announce poisoned") = Some(worker);
    }

    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    handles()
        .lock()
        .expect("handles poisoned")
        .insert(id, rt);
    Ok(id)
}

/// 停止接收端。Accept 是阻塞的，所以主动连一次自己把循环唤醒到检查 stop 的那一轮；
/// 这条唤醒连接会留下一条无害的 EOF 错误日志，不影响数据。
pub fn stop_receiver(id: u64) -> bool {
    let rt = match handles().lock().expect("handles poisoned").get(&id) {
        Some(r) => r.clone(),
        None => return false,
    };
    rt.stop.store(true, Ordering::Relaxed);
    // 广播线程自己分片轮询 stop，最多 200ms 就退出；这里 join 保证停止后不再发公告
    if let Some(h) = rt.announce.lock().expect("announce poisoned").take() {
        let _ = h.join();
    }
    let _ = TcpStream::connect(&rt.listen);
    true
}

pub fn status_json(id: u64) -> Option<String> {
    let rt = handles().lock().expect("handles poisoned").get(&id)?.clone();
    Some(
        json!({
            "running": !rt.stop.load(Ordering::Relaxed),
            "sessions": rt.sessions.load(Ordering::Relaxed),
            "bytes": rt.bytes.load(Ordering::Relaxed),
            "listen": rt.listen,
            "trustMode": rt.trust_mode,
            "lastError": rt.last_error.lock().expect("last_error poisoned").clone(),
        })
        .to_string(),
    )
}

// ---- 发送（M1：路径模式）----
//
// 先用最小实现打通链路：宿主传绝对路径，core 用现成的 build_manifest 递归打包。
// 目录之外的形态（SAF content://）留到 M6 收敛，那时再给发送端加文件来源抽象。

fn err_json(msg: impl Into<String>) -> String {
    json!({ "ok": false, "reason": msg.into() }).to_string()
}

/// 列目录。存在的意义是给宿主一个「应用身份真的能按路径读存储」的实证入口：
/// adb shell 能读不代表 app 能读，这一步是把权限问题与网络问题分开。
pub fn list_path(path: &str) -> String {
    match fs::read_dir(path) {
        Ok(it) => {
            let mut entries = Vec::new();
            for e in it.flatten() {
                let name = e.file_name().to_string_lossy().to_string();
                let (kind, size) = match e.file_type() {
                    Ok(t) if t.is_dir() => ("dir", 0),
                    Ok(t) if t.is_file() => ("file", e.metadata().map(|m| m.len()).unwrap_or(0)),
                    Ok(_) => ("other", 0),
                    Err(_) => ("unknown", 0),
                };
                entries.push(json!({ "name": name, "kind": kind, "size": size }));
                // 目录很大时只回前 500 项：界面只需要确认能读，不需要全量清单
                if entries.len() >= 500 {
                    break;
                }
            }
            json!({ "ok": true, "path": path, "count": entries.len(), "entries": entries })
                .to_string()
        }
        Err(e) => json!({ "ok": false, "path": path, "reason": e.to_string() }).to_string(),
    }
}

/// 发送一组目录到 target（形如 `203.0.113.10:53318`）。
/// `peer_fp` 是对端证书指纹，来自发现报文；传错会在 TLS 握手阶段被拒。
/// `paths_json` 是 JSON 字符串数组；必须是目录，core 的打包只接受目录。
/// `device_name` 为空时退回环境变量推导（桌面端行为），Android 应传 Build.MODEL。
pub fn send_json(
    target: &str,
    peer_fp: &str,
    paths_json: &str,
    pair_code: &str,
    identity_dir: &str,
    device_name: &str,
    force: bool,
) -> String {
    let raw: Vec<String> = match serde_json::from_str(paths_json) {
        Ok(v) => v,
        Err(e) => return err_json(format!("paths 不是字符串数组: {e}")),
    };
    if raw.is_empty() {
        return err_json("没有要发送的目录");
    }
    let paths: Vec<std::path::PathBuf> = raw.into_iter().map(std::path::PathBuf::from).collect();
    // 目录与单个文件都允许：文件会按父目录归组（见 build_manifest 的说明）

    let identity = match Identity::load_or_create(std::path::Path::new(identity_dir)) {
        Ok(i) => i,
        Err(e) => return err_json(format!("本机身份加载失败: {e}")),
    };
    let expected = match Identity::parse_fingerprint(peer_fp) {
        Ok(f) => f,
        Err(e) => return err_json(format!("对端指纹非法: {e}")),
    };
    let config = match client_config_with_identity(expected, &identity) {
        Ok(c) => c,
        Err(e) => return err_json(format!("TLS 配置失败: {e}")),
    };
    let opener = TlsOpener {
        addr: target.into(),
        config,
    };
    let code = if pair_code.trim().is_empty() {
        None
    } else {
        Some(pair_code.trim())
    };
    // 设备名由宿主给：Android 上既没有 COMPUTERNAME 也没有 HOSTNAME，
    // 不传就会让对端历史与信任列表里出现 unknown
    let mut dev: DeviceInfo = local_device();
    if !device_name.trim().is_empty() {
        dev.name = device_name.trim().to_string();
    }

    // 进度只更新内存快照，不落盘、不进协议；发送结束后 active 置 false，快照保留供界面显示最终结果
    SEND_ACTIVE.store(true, Ordering::Relaxed);
    if let Ok(mut g) = progress_slot().lock() {
        *g = Some(SendProgressView::default());
    }
    // 每次发送换一个新的控制位：上一批「已取消」的终态不能带到这一批
    let control = Arc::new(SendControl::new());
    if let Ok(mut g) = ctl_slot().lock() {
        *g = Some(control.clone());
    }
    let mut on_progress = |p: SendProgress| {
        if let Ok(mut g) = progress_slot().lock() {
            *g = Some(SendProgressView {
                bytes_sent: p.bytes_sent,
                total_bytes: p.total_bytes,
                blocks_sent: p.blocks_sent,
                total_blocks: p.total_blocks,
                files: p.files,
                current: p.current,
                scanning: p.scanning,
                scan_files: p.scan_files,
                scan_bytes: p.scan_bytes,
                speed_bps: p.speed_bps,
            });
        }
    };
    let result = run_sender_with_device_control_opts(
        &opener,
        &paths,
        code,
        dev,
        Some(&mut on_progress),
        Some(control.as_ref()),
        tc_core::SendOptions { force },
    );
    SEND_ACTIVE.store(false, Ordering::Relaxed);

    match result {
        Ok(r) => json!({
            "ok": r.verdict == "OK",
            "txnId": r.txn_id,
            "files": r.files,
            "bytes": r.bytes,
            "blocks": r.blocks_sent,
            "skipped": r.files_skipped,
            // v0.11：接收端回报的落点根目录名，界面显示「传到了哪个文件夹」
            "dest": r.dest_dirs,
            "verdict": r.verdict,
            // 本机指纹回传：被拒时用户要拿它去对端批准
            "selfFp": identity.fingerprint_hex(),
        })
        .to_string(),
        Err(e) => {
            let self_fp = identity.fingerprint_hex();
            json!({ "ok": false, "reason": e.to_string(), "selfFp": self_fp }).to_string()
        }
    }
}

/// 听一轮发现公告，返回 {"ok":true,"count":N,"peers":[{addr,name,platform,fingerprint}]}。
/// 收组播需要宿主持 MulticastLock（Android API），JNI 层只负责收包与去重。
pub fn discover_peers_json(timeout_ms: u64, identity_dir: &str) -> String {
    // 用本机指纹过滤掉自己的公告；身份读不到就传空串，最多是把自己也列进去
    let self_fp = Identity::load_or_create(std::path::Path::new(identity_dir))
        .map(|i| i.fingerprint_hex())
        .unwrap_or_default();
    // 夹一下范围：太短收不到包，太长会卡住界面线程
    let ms = timeout_ms.clamp(1000, 30_000);
    match discover_listen(std::time::Duration::from_millis(ms), &self_fp) {
        Ok(peers) => {
            let list: Vec<_> = peers
                .iter()
                .map(|p| {
                    json!({
                        "addr": p.addr,
                        "name": p.name,
                        "platform": p.platform,
                        "fingerprint": p.fingerprint,
                    })
                })
                .collect();
            json!({ "ok": true, "count": list.len(), "peers": list }).to_string()
        }
        Err(e) => err_json(format!("发现失败: {e}")),
    }
}

// ---- JNI 薄包装：包名与 Android 工程保持一致 ----

#[no_mangle]
pub extern "system" fn Java_com_tongchuan_TcNative_startReceiver<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    config: JString<'local>,
) -> jlong {
    let cfg: String = match env.get_string(&config) {
        Ok(s) => s.into(),
        Err(_) => return -1,
    };
    // 回调发生在 Rust 的接收线程，不是 Java 线程，所以必须自己 attach 到 JVM。
    // JavaVM 可跨线程共享；每次会话只 attach 一次，开销可以忽略。
    let vm: JavaVM = match env.get_java_vm() {
        Ok(v) => v,
        Err(_) => return -1,
    };
    // 必须在 Java 线程里先把类对象拿成全局引用：原生线程 attach 之后用的是系统类加载器，
    // 按类名 FindClass 找不到 App 自己的类。之前正是这里静默失败，导致「收到文件没通知」。
    // 缓存住 jclass 后，回调只按方法签名调用，不再做任何类名查找。
    let class = match env.find_class("com/tongchuan/ReceiverService") {
        Ok(c) => c,
        Err(_) => {
            let _ = env.exception_clear();
            return -1;
        }
    };
    let class_ref: GlobalRef = match env.new_global_ref(&class) {
        Ok(g) => g,
        Err(_) => return -1,
    };
    let notifier: Notifier = Arc::new(move |e: TransferEvent| {
        let Ok(mut jenv) = vm.attach_current_thread() else {
            return;
        };
        // v0.12：跳过数、落点根目录、来源设备一起回给界面。
        // 用一条 JSON 而不是继续加基本类型参数：以后补字段不用再改 JNI 签名。
        let info = json!({
            "skipped": e.skipped_files,
            "roots": e.roots,
            "device": e.device,
        })
        .to_string();
        let info = match jenv.new_string(info) {
            Ok(s) => s,
            Err(_) => return,
        };
        let r = jenv.call_static_method(
            &class_ref,
            "onTransfer",
            "(JJZZLjava/lang/String;)V",
            &[
                JValue::Long(e.files as i64),
                JValue::Long(e.bytes as i64),
                JValue::Bool(e.interrupted as u8),
                JValue::Bool(e.verified as u8),
                JValue::Object(&info),
            ],
        );
        // 通知失败不能影响接收循环，但也不能再无声无息：异常先描述再清掉。
        // 只在真出错时才走这条路（正常一次传输只有一次 JNI 调用），不会破坏省电设计。
        if r.is_err() {
            let _ = jenv.exception_describe();
            let _ = jenv.exception_clear();
        }
    });
    start_receiver_with(&cfg, Some(notifier))
        .map(|id| id as jlong)
        .unwrap_or(-1)
}

#[no_mangle]
pub extern "system" fn Java_com_tongchuan_TcNative_stopReceiver(
    _env: JNIEnv<'_>,
    _class: JClass<'_>,
    handle: jlong,
) -> jboolean {
    if stop_receiver(handle as u64) { 1 } else { 0 }
}

#[no_mangle]
pub extern "system" fn Java_com_tongchuan_TcNative_statusJson<'local>(
    env: JNIEnv<'local>,
    _class: JClass<'local>,
    handle: jlong,
) -> jstring {
    match status_json(handle as u64) {
        Some(s) => env
            .new_string(s)
            .map(|v| v.into_raw())
            .unwrap_or(std::ptr::null_mut()),
        None => std::ptr::null_mut(),
    }
}

/// 给 Kotlin 侧校验配置用：不启动任何东西，只回报是否合法。
#[no_mangle]
pub extern "system" fn Java_com_tongchuan_TcNative_validateConfig<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    config: JString<'local>,
) -> jboolean {
    let cfg: String = match env.get_string(&config) {
        Ok(s) => s.into(),
        Err(_) => return 0,
    };
    match serde_json::from_str::<Config>(&cfg) {
        Ok(c) if c.validate().is_ok() => 1,
        _ => 0,
    }
}

// ---- 信任库：APK 内「待确认 / 已信任」界面用 ----
// 手机端不提供命令行，界面就是唯一的批准入口，所以查询与增删都从这里走。
// 三个函数都按 identity_dir 现建 TrustStore：它每次读都按文件 stat 重放，
// 和接收线程里的门禁看到的是同一份数据，不存在两边缓存不一致的问题。

fn trust_snapshot(dir: &str) -> Option<String> {
    let store = TrustStore::new(std::path::Path::new(dir));
    let peers = store.list().ok()?;
    let pending = store.pending().ok()?;
    let peers: Vec<serde_json::Value> = peers
        .iter()
        .map(|p| {
            json!({
                "fp": p.fp,
                "alias": p.alias,
                "name": p.name,
                "platform": p.platform,
                "firstSeen": p.first_seen,
                "lastSeen": p.last_ts,
                "addedBy": p.added_by,
            })
        })
        .collect();
    let pending: Vec<serde_json::Value> = pending
        .iter()
        .map(|e| {
            json!({
                "fp": e.fp,
                "name": e.name,
                "platform": e.platform,
                "ts": e.ts,
                "note": e.note,
            })
        })
        .collect();
    // 本机指纹一并返回：手机被对方拒绝时，用户要拿它去 NAS 管理页批准，
    // 以前只能发一次失败才知道，现在打开界面就能看到。
    let self_fp = Identity::load_or_create(std::path::Path::new(dir))
        .map(|i| i.fingerprint_hex())
        .unwrap_or_default();
    Some(json!({ "selfFp": self_fp, "peers": peers, "pending": pending }).to_string())
}

#[no_mangle]
pub extern "system" fn Java_com_tongchuan_TcNative_trustJson<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    dir: JString<'local>,
) -> jstring {
    let dir: String = match env.get_string(&dir) {
        Ok(s) => s.into(),
        Err(_) => return std::ptr::null_mut(),
    };
    match trust_snapshot(&dir) {
        Some(s) => env
            .new_string(s)
            .map(|v| v.into_raw())
            .unwrap_or(std::ptr::null_mut()),
        None => std::ptr::null_mut(),
    }
}

#[no_mangle]
pub extern "system" fn Java_com_tongchuan_TcNative_trustApprove<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    dir: JString<'local>,
    fp: JString<'local>,
) -> jboolean {
    let (dir, fp): (String, String) = match (env.get_string(&dir), env.get_string(&fp)) {
        (Ok(d), Ok(f)) => (d.into(), f.into()),
        _ => return 0,
    };
    let store = TrustStore::new(std::path::Path::new(&dir));
    // 把被拒时记下的自称与平台带进信任表，用户在手机上只需点一次「批准」，
    // 不用再手填别名（Alias 仍留空，管理页里可以后续改）。
    let (name, platform) = store
        .pending()
        .ok()
        .and_then(|list| list.into_iter().find(|e| e.fp == fp.trim()))
        .map(|e| (e.name, e.platform))
        .unwrap_or_default();
    match store.approve(fp.trim(), "", &name, &platform, "android-ui") {
        Ok(_) => 1,
        Err(_) => 0,
    }
}

#[no_mangle]
pub extern "system" fn Java_com_tongchuan_TcNative_trustRevoke<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    dir: JString<'local>,
    fp: JString<'local>,
) -> jboolean {
    let (dir, fp): (String, String) = match (env.get_string(&dir), env.get_string(&fp)) {
        (Ok(d), Ok(f)) => (d.into(), f.into()),
        _ => return 0,
    };
    let store = TrustStore::new(std::path::Path::new(&dir));
    // 撤销后下一次会话就会被拒，不需要重启接收服务：门禁每次判定前都会重放信任表
    match store.revoke(fp.trim(), "android-ui", "") {
        Ok(true) => 1,
        _ => 0,
    }
}

/// 列目录。返回 JSON 字符串；参数非法时返回 null，由宿主按「接口不可用」处理。
#[no_mangle]
pub extern "system" fn Java_com_tongchuan_TcNative_listPath<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    path: JString<'local>,
) -> jstring {
    let p: String = match env.get_string(&path) {
        Ok(s) => s.into(),
        Err(_) => return std::ptr::null_mut(),
    };
    match env.new_string(list_path(&p)) {
        Ok(v) => v.into_raw(),
        Err(_) => std::ptr::null_mut(),
    }
}

/// 发送。参数全部用字符串，避免在 JNI 层处理数组对象；paths 是 JSON 字符串数组。
#[no_mangle]
pub extern "system" fn Java_com_tongchuan_TcNative_sendJson<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    target: JString<'local>,
    peer_fp: JString<'local>,
    paths_json: JString<'local>,
    pair_code: JString<'local>,
    identity_dir: JString<'local>,
    device_name: JString<'local>,
    force: jboolean,
) -> jstring {
    let grab = |env: &mut JNIEnv<'local>, s: &JString<'local>| -> Option<String> {
        env.get_string(s).ok().map(Into::into)
    };
    let target = grab(&mut env, &target);
    let peer_fp = grab(&mut env, &peer_fp);
    let paths_json = grab(&mut env, &paths_json);
    let pair_code = grab(&mut env, &pair_code);
    let identity_dir = grab(&mut env, &identity_dir);
    let device_name = grab(&mut env, &device_name);
    let (Some(target), Some(peer_fp), Some(paths), Some(code), Some(dir), Some(name)) =
        (target, peer_fp, paths_json, pair_code, identity_dir, device_name)
    else {
        return std::ptr::null_mut();
    };
    match env.new_string(send_json(
        &target,
        &peer_fp,
        &paths,
        &code,
        &dir,
        &name,
        force != 0,
    )) {
        Ok(v) => v.into_raw(),
        Err(_) => std::ptr::null_mut(),
    }
}

/// 当前发送进度（JSON）。发送期间由界面每 300ms 轮询一次，结束后仍可读到最后一次快照。
#[no_mangle]
pub extern "system" fn Java_com_tongchuan_TcNative_sendProgress<'local>(
    env: JNIEnv<'local>,
    _class: JClass<'local>,
) -> jstring {
    match env.new_string(send_progress_json()) {
        Ok(v) => v.into_raw(),
        Err(_) => std::ptr::null_mut(),
    }
}

/// 暂停 / 继续当前发送（v0.11）。暂停在块边界生效：不再向对端供块，
/// 连接、位图与已传块都保留，继续后从断点接着发。
#[no_mangle]
pub extern "system" fn Java_com_tongchuan_TcNative_sendPause<'local>(
    _env: JNIEnv<'local>,
    _class: JClass<'local>,
    paused: jboolean,
) {
    if let Some(c) = current_ctl() {
        if paused != 0 {
            c.pause();
        } else {
            c.resume();
        }
    }
}

/// 取消当前发送（v0.11）。接收端会留下 `.tcpart`，下次同名发送可续传，不算数据损坏。
#[no_mangle]
pub extern "system" fn Java_com_tongchuan_TcNative_sendCancel<'local>(
    _env: JNIEnv<'local>,
    _class: JClass<'local>,
) {
    if let Some(c) = current_ctl() {
        c.cancel();
    }
}

/// 听一轮发现公告，返回 JSON（见 discover_peers_json）。
#[no_mangle]
pub extern "system" fn Java_com_tongchuan_TcNative_discoverPeers<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    timeout_ms: jlong,
    identity_dir: JString<'local>,
) -> jstring {
    let dir: String = match env.get_string(&identity_dir) {
        Ok(s) => s.into(),
        Err(_) => return std::ptr::null_mut(),
    };
    let ms = if timeout_ms <= 0 { 6000 } else { timeout_ms as u64 };
    match env.new_string(discover_peers_json(ms, &dir)) {
        Ok(v) => v.into_raw(),
        Err(_) => std::ptr::null_mut(),
    }
}

/// 剪贴板客户端连接：与 CLI 的 `tc clip` 同一套 TLS + 指纹固定。
/// `device_name` 为空时退回环境变量推导；Android 没有 HOSTNAME，不传会让对端历史里出现 unknown。
fn clip_connect(
    identity_dir: &str,
    target: &str,
    peer_fp: &str,
    device_name: &str,
) -> std::io::Result<ClipClient> {
    let identity = Identity::load_or_create(std::path::Path::new(identity_dir))?;
    let expected = Identity::parse_fingerprint(peer_fp)?;
    let config = client_config_with_identity(expected, &identity)?;
    let opener = TlsOpener {
        addr: target.into(),
        config,
    };
    let mut dev = local_device();
    if !device_name.trim().is_empty() {
        dev.name = device_name.trim().to_string();
    }
    ClipClient::connect(&opener, dev.clone(), &dev.name)
}

/// 推一条文本到对端。
/// 每次调用新建一条连接：剪贴板消息量小，v0.8 先用短连接；真出现延迟问题再改长连接。
pub fn clip_push_json(
    identity_dir: &str,
    target: &str,
    peer_fp: &str,
    text: &str,
    sensitive: bool,
    device_name: &str,
) -> String {
    match clip_connect(identity_dir, target, peer_fp, device_name) {
        Ok(mut conn) => match clip::push_text(&mut conn, text, sensitive) {
            Ok(ev) => json!({ "ok": true, "id": ev.id, "size": ev.size }).to_string(),
            Err(e) => err_json(format!("推送失败: {e}")),
        },
        Err(e) => err_json(format!("连接失败: {e}")),
    }
}

/// 拉取比 `since` 新的一条事件；没有新内容时 `has=false`，`lamport` 供下次调用回填。
pub fn clip_pull_json(
    identity_dir: &str,
    target: &str,
    peer_fp: &str,
    since: u64,
    device_name: &str,
) -> String {
    match clip_connect(identity_dir, target, peer_fp, device_name) {
        Ok(mut conn) => match clip::pull(&mut conn, since) {
            Ok((Some(ev), text, lamport)) => json!({
                "ok": true,
                "has": true,
                "id": ev.id,
                "origin": ev.origin,
                "size": ev.size,
                "sensitive": ev.sensitive,
                "preview": ev.preview,
                "text": text,
                "lamport": lamport,
            })
            .to_string(),
            Ok((None, _, lamport)) => {
                json!({ "ok": true, "has": false, "lamport": lamport }).to_string()
            }
            Err(e) => err_json(format!("拉取失败: {e}")),
        },
        Err(e) => err_json(format!("连接失败: {e}")),
    }
}

/// 拉一条（供常驻服务轮询）：参数与字符串返回值同 clip_pull_json。
#[no_mangle]
pub extern "system" fn Java_com_tongchuan_TcNative_clipPullJson<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    identity_dir: JString<'local>,
    target: JString<'local>,
    peer_fp: JString<'local>,
    since: jlong,
    device_name: JString<'local>,
) -> jstring {
    let grab = |env: &mut JNIEnv<'local>, s: &JString<'local>| -> Option<String> {
        env.get_string(s).ok().map(Into::into)
    };
    let dir = grab(&mut env, &identity_dir);
    let to = grab(&mut env, &target);
    let fp = grab(&mut env, &peer_fp);
    let name = grab(&mut env, &device_name).unwrap_or_default();
    let (Some(dir), Some(to), Some(fp)) = (dir, to, fp) else {
        return std::ptr::null_mut();
    };
    let since = if since < 0 { 0 } else { since as u64 };
    match env.new_string(clip_pull_json(&dir, &to, &fp, since, &name)) {
        Ok(v) => v.into_raw(),
        Err(_) => std::ptr::null_mut(),
    }
}

/// 推一条文本（供界面/磁贴触发）。
#[no_mangle]
pub extern "system" fn Java_com_tongchuan_TcNative_clipPushJson<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    identity_dir: JString<'local>,
    target: JString<'local>,
    peer_fp: JString<'local>,
    text: JString<'local>,
    sensitive: jboolean,
    device_name: JString<'local>,
) -> jstring {
    let grab = |env: &mut JNIEnv<'local>, s: &JString<'local>| -> Option<String> {
        env.get_string(s).ok().map(Into::into)
    };
    let dir = grab(&mut env, &identity_dir);
    let to = grab(&mut env, &target);
    let fp = grab(&mut env, &peer_fp);
    let body = grab(&mut env, &text);
    let name = grab(&mut env, &device_name).unwrap_or_default();
    let (Some(dir), Some(to), Some(fp), Some(body)) = (dir, to, fp, body) else {
        return std::ptr::null_mut();
    };
    match env.new_string(clip_push_json(&dir, &to, &fp, &body, sensitive != 0, &name)) {
        Ok(v) => v.into_raw(),
        Err(_) => std::ptr::null_mut(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    use std::path::PathBuf;
    use tc_core::tls::client_config_with_identity;
    use tc_core::transport::TlsOpener;
    use tc_core::run_sender_with;

    fn base() -> PathBuf {
        std::env::var("TC_TEST_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| std::env::temp_dir())
    }

    #[test]
    fn handle_starts_serves_a_transfer_and_stops() {
        let dir = base().join(format!("通传-验证/tc-jni-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let dest = dir.join("received");
        let id_dir = dir.join("identity");
        fs::create_dir_all(&dest).unwrap();

        // 先占一个空闲端口，再把它交给句柄（避免测试并行时撞端口）
        let probe = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = probe.local_addr().unwrap().to_string();
        drop(probe);

        let cfg = json!({
            "alias": "jni-verify",
            "listen": addr,
            "manage": "127.0.0.1:0",
            "destination": dest,
            "allow": [dest],
            "identity_dir": id_dir,
            "log_dir": dir.join("logs"),
            "queue_dir": dir.join("queue"),
            "log_days": 2,
            "auto_accept": true
        });
        let id = start_receiver(&cfg.to_string()).unwrap();
        let identity = Identity::load_or_create(&id_dir).unwrap();
        // P3 起接收端默认 closed：测试发送端也得先被批准，否则会被门禁挡下
        let sender = Identity::load_or_create(&dir.join("sender")).unwrap();
        TrustStore::new(&id_dir)
            .approve(&sender.fingerprint_hex(), "测试发送端", "test-pc", "windows", "test")
            .unwrap();

        let src = dir.join("src/项目A");
        fs::create_dir_all(src.join("src")).unwrap();
        fs::write(src.join("src/a.txt"), b"jni payload").unwrap();
        let opener = TlsOpener {
            addr,
            config: client_config_with_identity(identity.fingerprint, &sender).unwrap(),
        };
        let send = run_sender_with(&opener, &[src], None).unwrap();
        assert_eq!(send.verdict, "OK");

        // 状态接口必须反映真实会话，而不是「页面能打开」式的存在性检查
        thread::sleep(std::time::Duration::from_millis(200));
        let status: Value = serde_json::from_str(&status_json(id).unwrap()).unwrap();
        assert_eq!(status["running"], true);
        assert_eq!(status["sessions"], 1);
        assert!(status["bytes"].as_u64().unwrap() > 0);
        assert!(dest.join("项目A/src/a.txt").exists());
        assert!(stop_receiver(id));
    }

    /// 真机验收暴露的缺口：收完文件必须能通知宿主，否则用户不打开 App 根本不知道文件到了。
    /// 这条用例锁住「会话结束 → 回调恰好一次，且带上真实文件数与字节数」。
    #[test]
    fn notifier_fires_once_per_completed_transfer() {
        let dir = base().join(format!("通传-验证/tc-jni-notify-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let dest = dir.join("received");
        let id_dir = dir.join("identity");
        fs::create_dir_all(&dest).unwrap();
        let probe = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = probe.local_addr().unwrap().to_string();
        drop(probe);

        let cfg = json!({
            "alias": "notify-verify",
            "listen": addr,
            "manage": "127.0.0.1:0",
            "destination": dest,
            "allow": [dest],
            "identity_dir": id_dir,
            "log_dir": dir.join("logs"),
            "queue_dir": dir.join("queue"),
            "log_days": 2,
            "auto_accept": true
        });
        let hits = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let seen = Arc::new(Mutex::new(Vec::<(u64, u64)>::new()));
        let h2 = hits.clone();
        let s2 = seen.clone();
        let notifier: Notifier = Arc::new(move |e: TransferEvent| {
            h2.fetch_add(1, Ordering::Relaxed);
            s2.lock().unwrap().push((e.files, e.bytes));
        });
        let id = start_receiver_with(&cfg.to_string(), Some(notifier)).unwrap();
        let identity = Identity::load_or_create(&id_dir).unwrap();
        // 同上：先批准发送端身份，这条用例验证的是通知次数而不是门禁
        let sender = Identity::load_or_create(&dir.join("sender")).unwrap();
        TrustStore::new(&id_dir)
            .approve(&sender.fingerprint_hex(), "测试发送端", "test-pc", "windows", "test")
            .unwrap();

        let src = dir.join("src/通知测试");
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("a.txt"), b"notify me").unwrap();
        let opener = TlsOpener {
            addr,
            config: client_config_with_identity(identity.fingerprint, &sender).unwrap(),
        };
        assert_eq!(run_sender_with(&opener, &[src], None).unwrap().verdict, "OK");

        thread::sleep(std::time::Duration::from_millis(300));
        assert_eq!(hits.load(Ordering::Relaxed), 1, "一次会话只该通知一次");
        let got = seen.lock().unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].0, 1, "文件数");
        assert_eq!(got[0].1, 9, "字节数");
        drop(got);
        assert!(stop_receiver(id));
    }

    /// P3 回归：手机端也必须挡住未批准设备（不能只靠 PC 侧），
    /// 且界面「待确认」能读到同一指纹；批准后立刻可传，撤销后又会被拒。
    #[test]
    fn unapproved_sender_is_denied_until_approved() {
        let dir = base().join(format!("通传-验证/tc-jni-trust-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let dest = dir.join("received");
        let id_dir = dir.join("identity");
        fs::create_dir_all(&dest).unwrap();
        let probe = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = probe.local_addr().unwrap().to_string();
        drop(probe);

        let cfg = json!({
            "alias": "trust-verify",
            "listen": addr,
            "manage": "127.0.0.1:0",
            "destination": dest,
            "allow": [dest],
            "identity_dir": id_dir,
            "log_dir": dir.join("logs"),
            "queue_dir": dir.join("queue"),
            "log_days": 2,
            "auto_accept": true
        });
        let id = start_receiver(&cfg.to_string()).unwrap();
        let identity = Identity::load_or_create(&id_dir).unwrap();
        let sender = Identity::load_or_create(&dir.join("sender")).unwrap();
        let opener = TlsOpener {
            addr,
            config: client_config_with_identity(identity.fingerprint, &sender).unwrap(),
        };

        let src = dir.join("src/信任测试");
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("a.txt"), b"trust me").unwrap();

        // 未批准：必须失败且不落盘
        let denied = run_sender_with(&opener, &[src.clone()], None);
        assert!(denied.is_err(), "未批准设备不该传输成功: {denied:?}");
        assert!(
            !dest.join("信任测试/a.txt").exists(),
            "被拒的会话不能留下文件"
        );

        // 界面读到的「待确认」里必须有这台设备，否则用户无从批准
        let snap: Value =
            serde_json::from_str(&trust_snapshot(id_dir.to_str().unwrap()).unwrap()).unwrap();
        assert!(snap["peers"].as_array().unwrap().is_empty());
        assert!(snap["pending"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["fp"] == sender.fingerprint_hex()));

        // 批准后同一台设备立刻可传
        TrustStore::new(&id_dir)
            .approve(&sender.fingerprint_hex(), "", "test-pc", "windows", "test")
            .unwrap();
        assert_eq!(
            run_sender_with(&opener, &[src.clone()], None).unwrap().verdict,
            "OK"
        );
        assert!(dest.join("信任测试/a.txt").exists());

        // 撤销后再传又被拒：门禁每次判定都重放信任表，不需要重启服务
        assert!(TrustStore::new(&id_dir)
            .revoke(&sender.fingerprint_hex(), "test", "")
            .unwrap());
        assert!(run_sender_with(&opener, &[src], None).is_err());

        assert!(stop_receiver(id));
    }

    #[test]
    fn invalid_config_is_rejected_before_starting() {
        let cfg = json!({
            "destination": "D:/outside",
            "allow": ["D:/allowed"],
            "listen": "127.0.0.1:0"
        });
        assert!(start_receiver(&cfg.to_string()).is_err());
    }

    #[test]
    fn unknown_handle_is_reported() {
        assert!(!stop_receiver(u64::MAX - 7));
        assert!(status_json(u64::MAX - 7).is_none());
    }

    /// v0.8 剪贴板三端：手机侧两个 JNI 入口要在离线环境自证能用。
    /// 直接起一个真实 ClipServer（TLS + 信任表），推一条再拉回来，避免只靠真机碰运气。
    #[test]
    fn clip_push_and_pull_roundtrip_through_real_server() {
        use tc_core::clip::ClipServer;
        use tc_core::transport::Accepter;

        // 用一次性目录名而不是先删再建：项目红线是「永不永久删除」，测试也不破例
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let dir = base().join(format!(
            "通传-验证/tc-jni-clip-{}-{stamp}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).unwrap();
        let server_dir = dir.join("server-id");
        let phone_dir = dir.join("phone-id");
        let peer_dir = dir.join("peer-id");
        let server_id = Identity::load_or_create(&server_dir).unwrap();
        let phone_id = Identity::load_or_create(&phone_dir).unwrap();
        let peer_id = Identity::load_or_create(&peer_dir).unwrap();
        let trust = TrustStore::new(&server_dir);
        trust
            .approve(&phone_id.fingerprint_hex(), "测试手机", "test", "android", "test")
            .unwrap();
        // 拉取端必须是另一台设备：服务端会过滤「自己发的事件」，同一身份拉不到自己推的内容
        trust
            .approve(&peer_id.fingerprint_hex(), "测试电脑", "test", "windows", "test")
            .unwrap();

        let probe = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = probe.local_addr().unwrap().to_string();
        drop(probe);
        let listener = TcpListener::bind(&addr).unwrap();
        let srv = ClipServer::new(dir.join("clip-history.jsonl"), Some(Arc::new(trust)));
        let stop = Arc::new(AtomicBool::new(false));
        let srv2 = srv.clone();
        let stop2 = stop.clone();
        let acc: Arc<dyn Accepter> = Arc::new(TlsAccepter {
            listener,
            config: server_config(&server_id).unwrap(),
        });
        thread::spawn(move || {
            let _ = srv2.serve(acc, stop2);
        });

        let dir_s = phone_dir.to_string_lossy().to_string();
        let fp = server_id.fingerprint_hex();
        let pushed: Value =
            serde_json::from_str(&clip_push_json(&dir_s, &addr, &fp, "来自手机的剪贴板", false, "测试手机"))
                .unwrap();
        assert_eq!(pushed["ok"], true, "推送应成功：{pushed}");

        // 服务端按 origin 过滤「自己发的事件」，同一台机器的两个身份设备名相同，
        // 所以拉取这一半在测试里显式给一个不同的 origin，模拟另一台设备
        let opener = TlsOpener {
            addr: addr.clone(),
            config: client_config_with_identity(server_id.fingerprint, &peer_id).unwrap(),
        };
        let mut conn = ClipClient::connect(&opener, local_device(), "测试电脑B").unwrap();
        let (ev, text, lamport) = clip::pull(&mut conn, 0).unwrap();
        assert!(ev.is_some(), "另一台设备应能拉到刚推的事件");
        assert_eq!(text.unwrap_or_default(), "来自手机的剪贴板");
        // 带上刚拿到的 lamport 再拉：不该重复返回同一条，否则界面会反复写剪贴板
        let (again, _, _) = clip::pull(&mut conn, lamport).unwrap();
        assert!(again.is_none(), "同一 lamport 不该再取到事件");

        // JNI 拉取入口本身可用：同 origin 会被服务端过滤，这里只断言调用成功
        let self_pull: Value =
            serde_json::from_str(&clip_pull_json(&dir_s, &addr, &fp, 0, "测试手机")).unwrap();
        assert_eq!(self_pull["ok"], true, "拉取入口应可用：{self_pull}");

        stop.store(true, Ordering::Relaxed);
    }
}
