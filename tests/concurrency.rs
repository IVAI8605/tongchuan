//! 并发会话的回归测试。
//! 旧实现按 accept 顺序关联控制/数据连接：第一台设备的会话还没拿到数据连接时，
//! 第二台设备接进来，它的控制连接会被当成第一台的数据连接（实测报 `bad frame magic`，
//! 第二台拿到 10054 且永远等不到 HELLO_ACK）。这里验证修复后的行为：
//! 数据连接靠 DATA_HELLO 里的 txnId 认领，多台设备各走各的会话。

use std::collections::HashMap;
use std::fs;
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;
use std::time::Duration;
use tc_core::frame::{read_message, write_message};
use tc_core::manifest::{build_manifest, local_device};
use tc_core::session::Control;
use tc_core::transport::PlainAccepter;
use tc_core::{run_receiver_with, run_sender, HubReport, ReceiverHub, BLOCK_SIZE, PROTOCOL_VERSION};

/// 测试产物只创建、不删除，落点与 e2e 一致
fn test_dir(name: &str) -> PathBuf {
    let base = std::env::var("TC_TEST_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| std::env::temp_dir());
    let p = base.join(format!("通传-验证/{}-{}", name, std::process::id()));
    fs::create_dir_all(&p).unwrap();
    p
}

fn write_file(p: &Path, data: &[u8]) {
    if let Some(d) = p.parent() {
        fs::create_dir_all(d).unwrap();
    }
    fs::write(p, data).unwrap();
}

/// 起一个后台接收入口。测试用的等待数据连接时间压到 1 秒，
/// 这样「会话被放弃后名额归还」不用真等一分钟。
fn start_hub(max_sessions: usize, dest: PathBuf) -> (Arc<ReceiverHub>, String) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let hub = Arc::new(
        ReceiverHub::new(
            Arc::new(PlainAccepter { listener }),
            dest,
            None,
            HashMap::new(),
            max_sessions,
        )
        .with_data_wait_secs(1),
    );
    let serving = Arc::clone(&hub);
    thread::spawn(move || {
        let cb: Arc<dyn Fn(HubReport) + Send + Sync> = Arc::new(|_| {});
        let _ = serving.serve(cb);
    });
    (hub, addr)
}

fn wait_until(mut cond: impl FnMut() -> bool, what: &str) {
    for _ in 0..200 {
        if cond() {
            return;
        }
        thread::sleep(Duration::from_millis(50));
    }
    panic!("等待超时: {what}");
}

/// 用原始连接模拟一台设备：只握手，不真正传文件
fn raw_session(addr: &str, txn_id: &str) -> TcpStream {
    let mut s = TcpStream::connect(addr).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    write_message(
        &mut s,
        &Control::Hello {
            txn_id: txn_id.into(),
            device: local_device(),
            pair_code: None,
            version: PROTOCOL_VERSION.into(),
            block_size: BLOCK_SIZE as u32,
            caps: vec![],
        },
    )
    .unwrap();
    assert!(matches!(
        read_message::<_, Control>(&mut s).unwrap(),
        Control::HelloAck { .. }
    ));
    s
}

/// 块大小是编译期常量、帧头里没有。4 MiB 的老端接进来必须在握手阶段被拒，
/// 否则要等整场传完、校验时才报块哈希不匹配，用户根本看不出原因。
#[test]
fn mismatched_block_size_is_rejected_at_handshake() {
    let root = test_dir("block-size");
    let (_hub, addr) = start_hub(2, root.join("dest"));
    let mut s = TcpStream::connect(&addr).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    write_message(
        &mut s,
        &Control::Hello {
            txn_id: "old-blocksize".into(),
            device: local_device(),
            pair_code: None,
            version: PROTOCOL_VERSION.into(),
            // v0.5.2 及更早用的 4 MiB
            block_size: 4 * 1024 * 1024,
            caps: vec![],
        },
    )
    .unwrap();
    match read_message::<_, Control>(&mut s).unwrap() {
        Control::Busy { reason } => {
            assert!(reason.contains("block size"), "reason: {reason}");
            assert!(reason.contains("4 MiB"), "reason: {reason}");
        }
        other => panic!("应回 BUSY，实际 {other:?}"),
    }
}

/// 旧实现的故障场景：第一场会话还没拿到数据连接，第二台设备就接入。
/// 现在两台都能拿到 HELLO_ACK，谁也不占谁的连接。
#[test]
fn second_device_gets_hello_ack_while_first_session_is_open() {
    let root = test_dir("hub-two-hello");
    let (hub, addr) = start_hub(4, root.join("recv"));

    let a = raw_session(&addr, "txn-a");
    let b = raw_session(&addr, "txn-b");
    assert_eq!(hub.active(), 2);

    drop((a, b));
    // 两场会话都没给数据连接，1 秒后超时退出，名额必须还回来
    wait_until(|| hub.active() == 0, "两场空会话释放名额");
}

/// 并发上传：两份文件同时投递，各自校验通过、各自落盘，结束后名额与落点都释放。
#[test]
fn two_senders_transfer_concurrently() {
    let root = test_dir("hub-concurrent");
    let a = root.join("src/甲");
    write_file(&a.join("a.bin"), &vec![1u8; BLOCK_SIZE * 2 + 5]);
    let b = root.join("src/乙");
    write_file(&b.join("b.bin"), &vec![2u8; BLOCK_SIZE + 7]);
    let dest = root.join("recv");
    fs::create_dir_all(&dest).unwrap();
    let (hub, addr) = start_hub(4, dest.clone());

    let ta = {
        let addr = addr.clone();
        let a = a.clone();
        thread::spawn(move || run_sender(&addr, &[a]))
    };
    let tb = {
        let addr = addr.clone();
        let b = b.clone();
        thread::spawn(move || run_sender(&addr, &[b]))
    };
    let sa = ta.join().unwrap().unwrap();
    let sb = tb.join().unwrap().unwrap();

    assert_eq!(sa.verdict, "OK");
    assert_eq!(sb.verdict, "OK");
    assert_eq!(
        fs::read(dest.join("甲/a.bin")).unwrap(),
        vec![1u8; BLOCK_SIZE * 2 + 5]
    );
    assert_eq!(
        fs::read(dest.join("乙/b.bin")).unwrap(),
        vec![2u8; BLOCK_SIZE + 7]
    );

    wait_until(
        || hub.active() == 0 && hub.claimed_paths() == 0,
        "并发会话结束后名额与落点都释放",
    );
}

/// 并发上限：超出的接入立刻收到 BUSY，而不是挂住；名额释放后新设备能正常进。
#[test]
fn capacity_limit_replies_busy_instead_of_hanging() {
    let root = test_dir("hub-capacity");
    let (hub, addr) = start_hub(1, root.join("recv"));

    let holding = raw_session(&addr, "txn-hold");

    let mut extra = TcpStream::connect(&addr).unwrap();
    extra
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    write_message(
        &mut extra,
        &Control::Hello {
            txn_id: "txn-extra".into(),
            device: local_device(),
            pair_code: None,
            version: PROTOCOL_VERSION.into(),
            block_size: BLOCK_SIZE as u32,
            caps: vec![],
        },
    )
    .unwrap();
    match read_message::<_, Control>(&mut extra).unwrap() {
        Control::Busy { reason } => assert!(reason.contains("上限"), "{reason}"),
        other => panic!("超限时应收到 BUSY，实际 {other:?}"),
    }

    drop(holding);
    wait_until(|| hub.active() == 0, "占用名额的会话结束");
    drop(raw_session(&addr, "txn-after"));
}

/// 数据连接声明了一个不存在的 txnId：必须回 BUSY，不能让它等下去。
#[test]
fn stray_data_hello_is_rejected() {
    let root = test_dir("hub-stray-data");
    let (_hub, addr) = start_hub(2, root.join("recv"));

    let mut s = TcpStream::connect(&addr).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    write_message(
        &mut s,
        &Control::DataHello {
            txn_id: "ghost".into(),
            stream: 0,
        },
    )
    .unwrap();
    match read_message::<_, Control>(&mut s).unwrap() {
        Control::Busy { reason } => assert!(reason.contains("no such session"), "{reason}"),
        other => panic!("应收到 BUSY，实际 {other:?}"),
    }
}

/// 两台设备抢同一个落点：后到的被明确拒绝（BUSY），前一场结束后同一个落点又能正常写。
#[test]
fn duplicate_destination_is_rejected_then_reusable() {
    let root = test_dir("hub-claim");
    let a_dir = root.join("甲/same");
    write_file(&a_dir.join("x.txt"), b"from-a");
    let b_dir = root.join("乙/same");
    write_file(&b_dir.join("x.txt"), b"from-b");
    let dest = root.join("recv");
    fs::create_dir_all(&dest).unwrap();
    let (hub, addr) = start_hub(4, dest.clone());

    // A 单方面占住 dest/same/x.txt：发完清单拿到 MANIFEST_ACK，但一直不给数据连接
    let mut a = raw_session(&addr, "txn-claim");
    let manifest = build_manifest(&[a_dir.clone()], local_device()).unwrap();
    write_message(&mut a, &Control::Manifest { manifest, force: false, dest_id: None }).unwrap();
    assert!(matches!(
        read_message::<_, Control>(&mut a).unwrap(),
        Control::ManifestAck { .. }
    ));

    // B 发同名落点：必须在写盘前就被拒，而不是两边各写半个文件
    let err = run_sender(&addr, &[b_dir.clone()]).unwrap_err();
    assert!(err.to_string().contains("destination busy"), "{err}");

    // A 放弃：数据连接连上就断，接收端报错收尾，名额与落点必须归还
    let mut data = TcpStream::connect(&addr).unwrap();
    write_message(
        &mut data,
        &Control::DataHello {
            txn_id: "txn-claim".into(),
            stream: 0,
        },
    )
    .unwrap();
    drop(data);
    drop(a);
    wait_until(
        || hub.active() == 0 && hub.claimed_paths() == 0,
        "拒绝路径也会释放名额与落点",
    );

    let send = run_sender(&addr, &[b_dir]).unwrap();
    assert_eq!(send.verdict, "OK");
    assert_eq!(
        fs::read_to_string(dest.join("same/x.txt")).unwrap(),
        "from-b"
    );
}

/// 会话报告要带上来源设备、对端地址与落点根目录。
/// 这三项是历史记录补字段的数据来源：没有它们，事后只能看到一串 txnId。
#[test]
fn report_carries_source_device_roots_and_peer_addr() {
    let root = test_dir("hub-report-meta");
    let src = root.join("src/照片");
    write_file(&src.join("p.bin"), &vec![3u8; 64]);
    let dest = root.join("recv");
    fs::create_dir_all(&dest).unwrap();

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let hub = Arc::new(ReceiverHub::new(
        Arc::new(PlainAccepter { listener }),
        dest,
        None,
        HashMap::new(),
        2,
    ));
    // HubReport 不实现 Clone，回调里就地取出需要的字段
    let captured = Arc::new(std::sync::Mutex::new(Vec::new()));
    let tap = Arc::clone(&captured);
    let serving = Arc::clone(&hub);
    thread::spawn(move || {
        let cb: Arc<dyn Fn(HubReport) + Send + Sync> = Arc::new(move |r| {
            if let Ok(rep) = r.result {
                tap.lock().unwrap().push((
                    rep.txn_id,
                    rep.device.map(|d| d.name),
                    rep.roots,
                    rep.peer_addr,
                    rep.verified,
                ));
            }
        });
        let _ = serving.serve(cb);
    });

    let send = run_sender(&addr, &[src]).unwrap();
    assert_eq!(send.verdict, "OK");
    wait_until(|| !captured.lock().unwrap().is_empty(), "会话报告送达");

    let got = captured.lock().unwrap();
    let (txn, name, roots, peer, verified) = &got[0];
    assert_eq!(txn, &send.txn_id);
    assert!(verified);
    assert_eq!(name.as_deref(), Some(local_device().name.as_str()));
    assert_eq!(roots, &vec!["照片".to_string()]);
    let peer = peer.as_deref().unwrap_or_default();
    assert!(
        peer.starts_with("127.0.0.1:"),
        "应记录对端地址，实际 {peer}"
    );
}

/// 单会话入口（tc recv / Android）不退化：正在等数据连接时，第二台设备拿到明确 BUSY，
/// 而不是把第一条连接抢走当数据通道（旧实现正是这样把两场传输一起弄坏的）。
#[test]
fn single_session_rejects_second_device_explicitly() {
    let root = test_dir("single-busy");
    let src = root.join("src");
    write_file(&src.join("x.txt"), b"single");
    let dest = root.join("recv");
    fs::create_dir_all(&dest).unwrap();

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let d = dest.clone();
    let rx = thread::spawn(move || {
        run_receiver_with(
            &PlainAccepter { listener },
            &d,
            None,
            None,
            &HashMap::new(),
        )
    });

    let mut first = raw_session(&addr, "txn-first");
    let manifest = build_manifest(&[src], local_device()).unwrap();
    write_message(&mut first, &Control::Manifest { manifest, force: false, dest_id: None }).unwrap();
    assert!(matches!(
        read_message::<_, Control>(&mut first).unwrap(),
        Control::ManifestAck { .. }
    ));

    // 接收端现在在等数据连接，第二台设备的控制连接进来就该被明确回绝
    let mut second = TcpStream::connect(&addr).unwrap();
    second
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    write_message(
        &mut second,
        &Control::Hello {
            txn_id: "txn-second".into(),
            device: local_device(),
            pair_code: None,
            version: PROTOCOL_VERSION.into(),
            block_size: BLOCK_SIZE as u32,
            caps: vec![],
        },
    )
    .unwrap();
    match read_message::<_, Control>(&mut second).unwrap() {
        Control::Busy { .. } => {}
        other => panic!("第二台设备应收到 BUSY，实际 {other:?}"),
    }

    // 第一台给出数据连接后立刻断开：单会话接收端如实报错退出，不再无限等待
    let mut data = TcpStream::connect(&addr).unwrap();
    write_message(
        &mut data,
        &Control::DataHello {
            txn_id: "txn-first".into(),
            stream: 0,
        },
    )
    .unwrap();
    drop(data);
    drop((first, second));
    assert!(rx.join().unwrap().is_err());
}

