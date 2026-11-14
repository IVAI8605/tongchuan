//! 剪贴板内核的离线验证（v0.6 验收口径）：
//! 去重、顺序（lamport 递增）、超长拒绝、未信任设备被拒、暂停标记。
//! 全部走本机回环 TLS，不碰真机、不碰系统剪贴板。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;
use tc_core::clip::{
    self, ClipClient, ClipMsg, ClipServer, CLIP_MAX_TEXT,
};
use tc_core::manifest::local_device;
use tc_core::tls::{client_config_with_identity, server_config, Identity};
use tc_core::transport::{TlsAccepter, TlsOpener};
use tc_core::trust::TrustStore;

fn test_dir(name: &str) -> PathBuf {
    let base = std::env::var("TC_TEST_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| std::env::temp_dir());
    let p = base.join(format!("通传-验证/clip-{}-{}", name, std::process::id()));
    std::fs::create_dir_all(&p).unwrap();
    p
}

/// 起一个剪贴板中继，返回（地址、服务端身份目录、停止开关）。
fn start_server(root: &Path, trust: Option<Arc<TrustStore>>) -> (String, Arc<AtomicBool>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let sdir = root.join("srv");
    let srv_id = Identity::load_or_create(&sdir).unwrap();
    let server = ClipServer::new(root.join("clip-history.jsonl"), trust);
    let acc = Arc::new(TlsAccepter {
        listener,
        config: server_config(&srv_id).unwrap(),
    });
    let stop = Arc::new(AtomicBool::new(false));
    let s = Arc::clone(&server);
    let st = Arc::clone(&stop);
    thread::spawn(move || {
        let _ = s.serve(acc, st);
    });
    thread::sleep(Duration::from_millis(200));
    (addr, stop)
}

fn opener_for(addr: &str, server_dir: &Path, client_id: &Identity) -> TlsOpener {
    let srv = Identity::load_or_create(server_dir).unwrap();
    let expected = Identity::parse_fingerprint(&srv.fingerprint_hex()).unwrap();
    TlsOpener {
        addr: addr.into(),
        config: client_config_with_identity(expected, client_id).unwrap(),
    }
}

#[test]
fn clip_relay_dedups_orders_and_rejects_oversize() {
    let root = test_dir("core");
    let cli_dir = root.join("cli");
    let cli_id = Identity::load_or_create(&cli_dir).unwrap();
    let srv_dir = root.join("srv");
    let trust = Arc::new(TrustStore::new(&srv_dir));
    trust
        .approve(&cli_id.fingerprint_hex(), "pc", "pc", "windows", "test")
        .unwrap();

    let (addr, stop) = start_server(&root, Some(Arc::clone(&trust)));
    let opener = opener_for(&addr, &srv_dir, &cli_id);
    let dev = local_device();

    // ① 去重：同一条内容推两次，只应留下一条历史
    clip::push_once(&opener, dev.clone(), "phone-A", "重复内容", false).unwrap();
    clip::push_once(&opener, dev.clone(), "phone-A", "重复内容", false).unwrap();

    // ② 顺序：再推一条不同内容，lamport 必须比上一条大
    let second = clip::push_once(&opener, dev.clone(), "phone-A", "第二条内容", false).unwrap();

    let mut conn = ClipClient::connect(&opener, dev.clone(), "pc-B").unwrap();
    conn.send(&ClipMsg::Status).unwrap();
    match conn.recv().unwrap() {
        ClipMsg::StatusReply { lamport, latest, .. } => {
            let latest = latest.expect("应有一条最新事件");
            assert_eq!(latest.preview, "第二条内容");
            assert!(latest.lamport > 0 && lamport >= latest.lamport, "lamport 应单调递增");
            assert_eq!(latest.id, second.id);
        }
        other => panic!("expected StatusReply, got {other:?}"),
    }

    // ③ 超长：>64 KiB 直接拒绝，不做截断
    let big = "x".repeat(CLIP_MAX_TEXT + 1);
    let err = clip::push_once(&opener, dev.clone(), "phone-A", &big, false).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);

    // 历史只应有两条：重复的被去重，超长的没进来
    let hist = clip::read_history(&root.join("clip-history.jsonl"), 10).unwrap();
    assert_eq!(hist.len(), 2, "去重与超长拒绝都应体现在历史里");
    assert_eq!(hist[0].preview, "重复内容");
    assert_eq!(hist[1].preview, "第二条内容");
    // 落盘历史必须带正文，否则订阅端拿不到内容去写剪贴板
    assert_eq!(hist[1].text.as_deref(), Some("第二条内容"));

    stop.store(true, Ordering::Relaxed);
}

#[test]
fn untrusted_peer_is_denied() {
    let root = test_dir("deny");
    let cli_dir = root.join("cli");
    let cli_id = Identity::load_or_create(&cli_dir).unwrap();
    let srv_dir = root.join("srv");
    // 信任表故意为空：剪贴板可能含密码，未批准一律拒绝
    let trust = Arc::new(TrustStore::new(&srv_dir));

    let (addr, stop) = start_server(&root, Some(trust));
    let opener = opener_for(&addr, &srv_dir, &cli_id);
    let err = match ClipClient::connect(&opener, local_device(), "cli") {
        Ok(_) => panic!("未信任设备不应连上"),
        Err(e) => e,
    };
    assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied);
    assert!(err.to_string().contains("untrusted"), "原因应是可读的中文提示：{err}");

    stop.store(true, Ordering::Relaxed);
}

#[test]
fn pause_marker_roundtrip() {
    let root = test_dir("pause");
    let p = root.join("clip.pause");
    assert!(clip::paused_until(&p).is_none(), "没有标记时不应算暂停");
    let until = clip::set_pause(&p, 15).unwrap();
    assert!(until > 0);
    assert_eq!(clip::paused_until(&p), Some(until));
    clip::clear_pause(&p).unwrap();
    assert!(clip::paused_until(&p).is_none(), "恢复后不应再算暂停");
}
