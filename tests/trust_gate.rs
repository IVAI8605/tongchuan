//! P2 信任门禁的端到端验证：双向 TLS、closed/open、配对码自动入信任、撤销即时生效、
//! 以及两端版本不匹配时必须给出可读提示。
//! 全部走本机回环，不依赖真机与固定端口。

use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::thread;

use tc_core::frame::{read_message, write_message};
use tc_core::manifest::local_device;
use tc_core::session::{Control, TrustGate, TrustMode};
use tc_core::tls::{client_config_with_identity, server_config, Identity};
use tc_core::transport::{PlainAccepter, PlainOpener, TlsAccepter, TlsOpener};
use tc_core::trust::TrustStore;
use tc_core::{
    protocol_compatible, run_receiver_with, run_receiver_with_gate, run_sender_with, SendReport,
    PROTOCOL_VERSION,
};

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn test_dir(tag: &str) -> PathBuf {
    // 进程号 + 计数：并行跑用例时不会互相踩目录
    let id = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "tc-gate-{}-{}-{tag}-{id}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis()
    ));
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn make_source(dir: &Path) -> PathBuf {
    let src = dir.join("src");
    fs::create_dir_all(src.join("sub")).unwrap();
    fs::write(src.join("a.txt"), vec![7u8; 4096]).unwrap();
    fs::write(src.join("sub/b.txt"), b"hello-p2").unwrap();
    src
}

/// 起一次完整的 TLS 会话（服务端 + 客户端各带自己的身份证书）。
/// 返回发送端结果；接收端错误转为字符串，避免调用方只看到「发送失败」而看不到拒绝原因。
fn run_once(
    server: &Identity,
    client: &Identity,
    gate: Arc<TrustGate>,
    pair_code: Option<&str>,
    src: &Path,
    dest: &Path,
) -> Result<SendReport, String> {
    fs::create_dir_all(dest).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let sc = server_config(server).unwrap();
    let cfg = client_config_with_identity(server.fingerprint, client).unwrap();
    let dest = dest.to_path_buf();
    let receiver = thread::spawn(move || {
        let acc = TlsAccepter {
            listener,
            config: sc,
        };
        run_receiver_with_gate(&acc, &dest, None, None, &HashMap::new(), Some(&gate))
            .map(|r| r.verified)
            .map_err(|e| e.to_string())
    });
    let opener = TlsOpener { addr, config: cfg };
    let sent = run_sender_with(&opener, &[src.to_path_buf()], pair_code);
    let recv = receiver.join().expect("receiver thread panicked");
    match sent {
        Ok(r) => {
            recv?;
            Ok(r)
        }
        Err(e) => Err(e.to_string()),
    }
}

#[test]
fn closed_gate_rejects_unknown_then_approve_then_revoke() {
    let root = test_dir("closed");
    let src = make_source(&root);
    let server = Identity::load_or_create(&root.join("srv")).unwrap();
    let client = Identity::load_or_create(&root.join("cli")).unwrap();
    let store = Arc::new(TrustStore::new(&root.join("srv")));
    let gate = Arc::new(TrustGate::new(store.clone(), TrustMode::Closed, None));
    let dest = root.join("recv");

    // 1) 未信任：必须被拦，且原因里带指纹前缀
    let err = run_once(&server, &client, gate.clone(), None, &src, &dest).unwrap_err();
    assert!(err.contains("untrusted"), "unexpected error: {err}");
    assert!(
        err.contains(&client.fingerprint_hex()[..8]),
        "拒绝原因应带指纹前缀，实际: {err}"
    );

    // 2) 被拒尝试要能在「待确认」里看到，否则用户无从批准
    let pending = store.pending().unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].fp, client.fingerprint_hex());

    // 3) 批准后立即放行，待确认列表随之清空
    store
        .approve(&client.fingerprint_hex(), "", "测试手机", "android", "manual")
        .unwrap();
    assert!(store.pending().unwrap().is_empty());
    let report = run_once(&server, &client, gate.clone(), None, &src, &dest).unwrap();
    assert_eq!(report.verdict, "OK");

    // 4) 撤销必须在下一场会话立刻生效，而不是等进程重启
    store.revoke(&client.fingerprint_hex(), "manual", "test").unwrap();
    let err = run_once(&server, &client, gate.clone(), None, &src, &dest).unwrap_err();
    assert!(err.contains("untrusted"), "revoke not effective: {err}");
}

#[test]
fn non_empty_pair_code_auto_trusts() {
    let root = test_dir("pair");
    let src = make_source(&root);
    let server = Identity::load_or_create(&root.join("srv")).unwrap();
    let client = Identity::load_or_create(&root.join("cli")).unwrap();
    let store = Arc::new(TrustStore::new(&root.join("srv")));
    let gate = Arc::new(TrustGate::new(
        store.clone(),
        TrustMode::Closed,
        Some("246810".into()),
    ));

    let report = run_once(
        &server,
        &client,
        gate,
        Some("246810"),
        &src,
        &root.join("recv"),
    )
    .unwrap();
    assert_eq!(report.verdict, "OK");
    assert!(store.is_trusted(&client.fingerprint_hex()).unwrap());
    // 自动入信任也要留痕，管理页能看到来源是 pair-code
    let peers = store.list().unwrap();
    assert_eq!(peers.len(), 1);
    assert_eq!(peers[0].added_by, "pair-code");
}

#[test]
fn empty_pair_code_never_auto_trusts() {
    let root = test_dir("empty-pair");
    let src = make_source(&root);
    let server = Identity::load_or_create(&root.join("srv")).unwrap();
    let client = Identity::load_or_create(&root.join("cli")).unwrap();
    let store = Arc::new(TrustStore::new(&root.join("srv")));
    // 配置为空串：即便发送端给了配对码也不得自动信任
    let gate = Arc::new(TrustGate::new(
        store.clone(),
        TrustMode::Closed,
        Some(String::new()),
    ));

    let err = run_once(
        &server,
        &client,
        gate,
        Some("000000"),
        &src,
        &root.join("recv"),
    )
    .unwrap_err();
    assert!(err.contains("untrusted"), "empty pair code must not trust: {err}");
    assert!(!store.is_trusted(&client.fingerprint_hex()).unwrap());
}

#[test]
fn open_mode_allows_and_records_reason() {
    let root = test_dir("open");
    let src = make_source(&root);
    let server = Identity::load_or_create(&root.join("srv")).unwrap();
    let client = Identity::load_or_create(&root.join("cli")).unwrap();
    let store = Arc::new(TrustStore::new(&root.join("srv")));
    let gate = Arc::new(TrustGate::new(store.clone(), TrustMode::Open, None));

    let report = run_once(&server, &client, gate, None, &src, &root.join("recv")).unwrap();
    assert_eq!(report.verdict, "OK");
    let peers = store.list().unwrap();
    assert_eq!(peers.len(), 1);
    assert_eq!(peers[0].added_by, "open");
}

#[test]
fn receiver_rejects_old_sender_with_readable_reason() {
    let root = test_dir("old-sender");
    let dest = root.join("recv");
    fs::create_dir_all(&dest).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let d = dest.clone();
    let receiver = thread::spawn(move || {
        let acc = PlainAccepter { listener };
        run_receiver_with(&acc, &d, None, None, &HashMap::new())
            .map(|_| ())
            .map_err(|e| e.to_string())
    });

    let mut sock = TcpStream::connect(addr).unwrap();
    // 旧发送端不上报版本：serde 默认成空串，接收端应回「不兼容」而不是 untrusted
    write_message(
        &mut sock,
        &Control::Hello {
            txn_id: "old-1".into(),
            device: local_device(),
            pair_code: None,
            version: String::new(),
            // 冒充旧发送端：既没版本也没块大小，反序列化默认值就是 0
            block_size: 0,
            caps: vec![],
        },
    )
    .unwrap();
    let reply: Control = read_message(&mut sock).unwrap();
    match reply {
        Control::Busy { reason } => {
            assert!(reason.contains("不兼容"), "reason: {reason}");
            assert!(reason.contains(PROTOCOL_VERSION), "reason: {reason}");
        }
        other => panic!("expected Busy, got {other:?}"),
    }
    let err = receiver.join().unwrap().unwrap_err();
    assert!(err.contains("不兼容"), "receiver error: {err}");
}

#[test]
fn sender_reports_old_receiver_with_readable_reason() {
    let root = test_dir("old-receiver");
    let src = make_source(&root);
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let fake = thread::spawn(move || {
        let (mut sock, _) = listener.accept().unwrap();
        let _hello: Control = read_message(&mut sock).unwrap();
        // 冒充旧接收端：只回一个过时的 HelloAck 就断开
        write_message(
            &mut sock,
            &Control::HelloAck {
                version: "0.0.1".into(),
                block_size: 0,
                caps: vec![],
                destinations: vec![],
            },
        )
        .unwrap();
        sock.flush().unwrap();
    });

    let opener = PlainOpener {
        addr: addr.to_string(),
    };
    let err = run_sender_with(&opener, &[src], None).unwrap_err();
    fake.join().unwrap();
    let msg = err.to_string();
    assert!(msg.contains("0.0.1"), "version missing from message: {msg}");
    assert!(msg.contains("不兼容"), "message not actionable: {msg}");
}

#[test]
fn version_compatibility_rules() {
    assert!(protocol_compatible(PROTOCOL_VERSION));
    // 补丁号不同仍互通：只要求主次版本一致
    let mut it = PROTOCOL_VERSION.split('.');
    let patch_probe = format!("{}.{}.99", it.next().unwrap(), it.next().unwrap());
    assert!(protocol_compatible(&patch_probe));
    assert!(!protocol_compatible("0.0.1"));
    assert!(!protocol_compatible(""));
    assert!(!protocol_compatible("not-a-version"));
}
