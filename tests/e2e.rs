use std::collections::HashMap;
use std::fs;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;
use tc_core::discover;
use tc_core::hash::hash_bytes;
use tc_core::manifest::{local_device, DeviceInfo, FileEntry, Manifest, Root};
use tc_core::queue;
use tc_core::tls::{client_config, server_config, Identity};
use tc_core::transport::{PlainAccepter, PlainOpener, TlsAccepter, TlsOpener};
use tc_core::{
    run_receiver_on, run_receiver_with, run_sender, run_sender_with,
    run_sender_with_device_control, run_sender_with_device_progress, run_sender_with_opts,
    validate_manifest_paths, SendControl, SendOptions, BLOCK_SIZE,
};

/// 测试产物只创建、不删除；统一落到用户指定的临时目录，需要清理时另列清单问用户。
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

fn listener_and_addr() -> (TcpListener, String) {
    // 先在测试线程 bind，再交给接收端状态机，避免发送端抢先 connect 的竞态
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap().to_string();
    (l, addr)
}

#[test]
fn full_transfer_keeps_tree_empty_dirs_and_separate_roots() {
    let root = test_dir("full");
    let a = root.join("src/项目A");
    write_file(&a.join("src/main.rs"), b"fn main() {}\n");
    write_file(&a.join("a.txt"), b"hello");
    fs::create_dir_all(a.join("docs/img")).unwrap(); // 空目录：LocalSend 会丢，我们必须保留
    let b = root.join("src/备份");
    write_file(&b.join("src/main.rs"), b"different content"); // 同名子目录、不同根：不能合并

    let dest = root.join("recv");
    fs::create_dir_all(&dest).unwrap();
    let (listener, addr) = listener_and_addr();
    let dest2 = dest.clone();
    let recv = thread::spawn(move || run_receiver_on(listener, &dest2, None).unwrap());
    let send = run_sender(&addr, &[a, b]).unwrap();
    let report = recv.join().unwrap();

    assert_eq!(send.verdict, "OK");
    assert!(report.verified);
    assert!(!report.interrupted);
    assert_eq!(send.files, 3);

    // v1.1.0：真实会话必须记录墙钟与吞吐，且接收端 avg_bps 与「传输字节 / 墙钟」自洽
    assert!(send.wall_us > 0, "发送端端到端墙钟应 > 0");
    assert!(send.data_us > 0, "发送端数据阶段墙钟应 > 0");
    assert!(send.avg_bps > 0, "发送端数据阶段均速应 > 0");
    assert!(report.wall_us > 0, "接收端数据阶段墙钟应 > 0");
    let transferred = report.bytes.saturating_sub(report.skipped_bytes);
    let expect_bps = transferred.saturating_mul(1_000_000) / report.wall_us;
    assert_eq!(report.avg_bps, expect_bps, "接收端均速必须等于 传输字节/墙钟");

    assert_eq!(
        fs::read_to_string(dest.join("项目A/src/main.rs")).unwrap(),
        "fn main() {}\n"
    );
    assert_eq!(fs::read_to_string(dest.join("项目A/a.txt")).unwrap(), "hello");
    assert_eq!(
        fs::read_to_string(dest.join("备份/src/main.rs")).unwrap(),
        "different content"
    );
    assert!(dest.join("项目A/docs/img").is_dir());
}

#[test]
fn interrupted_transfer_resumes_from_bitmap() {
    let root = test_dir("resume");
    let src = root.join("src/大文件");
    let data = vec![0x3cu8; BLOCK_SIZE * 2 + 777]; // 恰好 3 块
    write_file(&src.join("blob.bin"), &data);
    let dest = root.join("recv");
    fs::create_dir_all(&dest).unwrap();

    // 第一次只收 1 块就断，位图已落盘
    let (l1, a1) = listener_and_addr();
    let d1 = dest.clone();
    let h1 = thread::spawn(move || run_receiver_on(l1, &d1, Some(1)).unwrap());
    // 中断是异常路径：接收端提前收尾时发送端可能拿到 interrupted 裁决，
    // 也可能直接连接被重置。核心断言放在接收端确实中断、以及第二次续传成功上。
    let _ = run_sender(&a1, &[src.clone()]);
    let r1 = h1.join().unwrap();
    assert!(r1.interrupted);
    assert_eq!(r1.blocks_written, 1);

    // 第二次只补缺口，不重传已收的块
    let (l2, a2) = listener_and_addr();
    let d2 = dest.clone();
    let h2 = thread::spawn(move || run_receiver_on(l2, &d2, None).unwrap());
    let s2 = run_sender(&a2, &[src]).unwrap();
    let r2 = h2.join().unwrap();
    assert!(r2.verified);
    assert_eq!(r2.resumed_blocks, 1);
    assert_eq!(r2.blocks_written, 2);
    assert_eq!(s2.verdict, "OK");
    assert_eq!(
        hash_bytes(&fs::read(dest.join("大文件/blob.bin")).unwrap()),
        hash_bytes(&data)
    );
}

/// v0.19：单块小文件不续传。上次失败留下的半成品与位图必须在重传前被精准删除，
/// 本次从零重收，成功后目录里不留 `.tcpart` / `.map` / `.stale` 副本。
#[test]
fn small_file_restarts_instead_of_resuming() {
    let root = test_dir("small-retry");
    let src = root.join("src/小文件");
    let data = vec![0x21u8; 4096]; // 一块以内
    write_file(&src.join("tiny.bin"), &data);
    let dest = root.join("recv");
    fs::create_dir_all(&dest).unwrap();

    // 伪造上次失败留下的残留：半成品内容写坏、位图声称收过第 0 块。
    // 小文件不走续传，这两份都应被精准删除后整份重收。
    let dir = dest.join("小文件");
    fs::create_dir_all(&dir).unwrap();
    let part = dir.join("tiny.bin.tcpart");
    let map = PathBuf::from(format!("{}.map", part.to_string_lossy()));
    fs::write(&part, vec![0xffu8; 4096]).unwrap();
    fs::write(&map, "0\n").unwrap();

    let (listener, addr) = listener_and_addr();
    let dest2 = dest.clone();
    let recv = thread::spawn(move || run_receiver_on(listener, &dest2, None).unwrap());
    let send = run_sender(&addr, &[src]).unwrap();
    let report = recv.join().unwrap();

    assert!(report.verified);
    assert_eq!(report.resumed_blocks, 0, "小文件必须整份重传");
    assert_eq!(report.blocks_written, 1);
    assert_eq!(send.verdict, "OK");
    assert_eq!(fs::read(dir.join("tiny.bin")).unwrap(), data);
    assert!(!part.exists(), "成功后不应残留半成品");
    assert!(!map.exists(), "成功后不应残留位图");
    let stale = fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().contains(".stale-"))
        .count();
    assert_eq!(stale, 0, "小文件失败走精准删除，不做改名隔离");
}

/// v0.19：落盘文件的修改时间应等于源文件的修改时间，而不是「收到的时间」。
#[test]
fn received_file_keeps_source_mtime() {
    let root = test_dir("mtime");
    let src = root.join("src/时间");
    let file_path = src.join("stamp.txt");
    write_file(&file_path, b"mtime check");
    let stamp = 1_500_000_000u64;
    let when = std::time::UNIX_EPOCH + Duration::from_secs(stamp);
    let f = fs::OpenOptions::new().write(true).open(&file_path).unwrap();
    f.set_times(fs::FileTimes::new().set_modified(when)).unwrap();
    drop(f);

    let dest = root.join("recv");
    fs::create_dir_all(&dest).unwrap();
    let (listener, addr) = listener_and_addr();
    let dest2 = dest.clone();
    let recv = thread::spawn(move || run_receiver_on(listener, &dest2, None).unwrap());
    let send = run_sender(&addr, &[src]).unwrap();
    assert!(recv.join().unwrap().verified);
    assert_eq!(send.verdict, "OK");

    let got = fs::metadata(dest.join("时间/stamp.txt"))
        .unwrap()
        .modified()
        .unwrap()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    assert_eq!(got, stamp, "接收端必须把源修改时间写回去");
}

/// v0.17：续传时发现半成品数据坏了，必须作废旧位图；下一次重发从零开始并成功。
/// 这是「校验失败 → 位图仍被信任 → 永久失败」那条死循环的回归用例。
#[test]
fn corrupted_resume_invalidates_bitmap_and_next_retry_succeeds() {
    let root = test_dir("corrupt-resume");
    let src = root.join("src/大文件");
    let data = vec![0x5au8; BLOCK_SIZE * 2 + 321];
    write_file(&src.join("blob.bin"), &data);
    let dest = root.join("recv");
    fs::create_dir_all(&dest).unwrap();

    // 第一次只收 1 块，留下位图与半成品
    let (l1, a1) = listener_and_addr();
    let d1 = dest.clone();
    let h1 = thread::spawn(move || run_receiver_on(l1, &d1, Some(1)).unwrap());
    let _ = run_sender(&a1, &[src.clone()]);
    assert!(h1.join().unwrap().interrupted);

    // 把已收块的半成品内容改坏：位图仍声称这一块已收，续传后会校验失败
    let part = dest.join("大文件/blob.bin.tcpart");
    let mut broken = fs::read(&part).unwrap();
    broken[..16].fill(0xff);
    fs::write(&part, &broken).unwrap();

    // 第二次续传：只补缺块 → 校验失败 → 接收端应作废位图并把失败文件回报给发送端
    let (l2, a2) = listener_and_addr();
    let d2 = dest.clone();
    let h2 = thread::spawn(move || run_receiver_on(l2, &d2, None).unwrap());
    let s2 = run_sender(&a2, &[src.clone()]).unwrap();
    let r2 = h2.join().unwrap();
    assert!(!r2.verified, "坏块必须被校验拦下");
    assert_ne!(s2.verdict, "OK");
    assert_eq!(s2.failed.len(), 1, "失败清单应指出具体文件");
    assert_eq!(
        s2.failed[0].path.as_deref(),
        Some(src.join("blob.bin").to_string_lossy().as_ref()),
        "失败文件要映射回本机绝对路径，resend --failed 才有得发"
    );

    // 作废后不能留下仍被信任的活动位图；隔离副本允许保留
    assert!(!part.exists(), "坏半成品应被改名隔离");
    let stale_count = fs::read_dir(dest.join("大文件"))
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().contains(".stale-"))
        .count();
    assert!(stale_count >= 1, "至少保留一份可找回的隔离副本");

    // 第三次重发：没有位图可续，整份重传并校验通过
    let (l3, a3) = listener_and_addr();
    let d3 = dest.clone();
    let h3 = thread::spawn(move || run_receiver_on(l3, &d3, None).unwrap());
    let s3 = run_sender(&a3, &[src]).unwrap();
    let r3 = h3.join().unwrap();
    assert_eq!(s3.verdict, "OK");
    assert!(r3.verified);
    assert_eq!(r3.resumed_blocks, 0, "作废后必须从零开始");
    assert_eq!(
        hash_bytes(&fs::read(dest.join("大文件/blob.bin")).unwrap()),
        hash_bytes(&data)
    );
}

#[test]
fn receiver_rejects_traversal_in_manifest() {
    let m = Manifest {
        txn_id: "t".into(),
        device: DeviceInfo {
            id: "x".into(),
            name: "x".into(),
            platform: "test".into(),
        },
        roots: vec![Root {
            id: "r0".into(),
            name: "ok".into(),
            mode: "send".into(),
            dirs: vec![String::new()],
        }],
        files: vec![FileEntry {
            id: "f0".into(),
            root_id: "r0".into(),
            rel_path: "../../evil.txt".into(),
            size: 1,
            mtime: 0,
            mode: 0o644,
            blake3: String::new(),
            block_hashes: vec![],
        }],
        total_bytes: 1,
        total_files: 1,
    };
    assert!(validate_manifest_paths(Path::new("D:/recv"), &m).is_err());
}

fn identity(name: &str) -> Identity {
    let dir = test_dir(name).join("identity");
    Identity::load_or_create(&dir).unwrap()
}

#[test]
fn tls_transfer_verifies_and_keeps_tree() {
    let root = test_dir("tls-full");
    let a = root.join("src/项目A");
    write_file(&a.join("src/main.rs"), b"tls payload\n");
    fs::create_dir_all(a.join("docs/img")).unwrap();
    let dest = root.join("recv");
    fs::create_dir_all(&dest).unwrap();

    let id = identity("tls-full/id");
    let (listener, addr) = listener_and_addr();
    let acc = TlsAccepter {
        listener,
        config: server_config(&id).unwrap(),
    };
    let dest2 = dest.clone();
    let recv = thread::spawn(move || {
        run_receiver_with(&acc, &dest2, None, Some("123456"), &HashMap::new()).unwrap()
    });
    let opener = TlsOpener {
        addr,
        config: client_config(id.fingerprint).unwrap(),
    };
    let send = run_sender_with(&opener, &[a], Some("123456")).unwrap();
    let report = recv.join().unwrap();

    assert_eq!(send.verdict, "OK");
    assert!(report.verified);
    assert_eq!(
        fs::read_to_string(dest.join("项目A/src/main.rs")).unwrap(),
        "tls payload\n"
    );
    assert!(dest.join("项目A/docs/img").is_dir());
}

#[test]
fn tls_interrupted_transfer_resumes() {
    let root = test_dir("tls-resume");
    let src = root.join("src/大文件");
    let data = vec![0x7du8; BLOCK_SIZE * 2 + 333];
    write_file(&src.join("blob.bin"), &data);
    let dest = root.join("recv");
    fs::create_dir_all(&dest).unwrap();
    let id = identity("tls-resume/id");

    let (l1, a1) = listener_and_addr();
    let acc1 = TlsAccepter {
        listener: l1,
        config: server_config(&id).unwrap(),
    };
    let d1 = dest.clone();
    let h1 = thread::spawn(move || {
        run_receiver_with(&acc1, &d1, Some(1), None, &HashMap::new()).unwrap()
    });
    let o1 = TlsOpener {
        addr: a1,
        config: client_config(id.fingerprint).unwrap(),
    };
    // TLS 下接收端提前返回不会发 close_notify，Windows 常给 ConnectionReset；
    // 这里不 unwrap 第一次发送结果，改为断言接收端状态与第二次续传结果。
    let _ = run_sender_with(&o1, &[src.clone()], None);
    assert!(h1.join().unwrap().interrupted);

    let (l2, a2) = listener_and_addr();
    let acc2 = TlsAccepter {
        listener: l2,
        config: server_config(&id).unwrap(),
    };
    let d2 = dest.clone();
    let h2 = thread::spawn(move || {
        run_receiver_with(&acc2, &d2, None, None, &HashMap::new()).unwrap()
    });
    let o2 = TlsOpener {
        addr: a2,
        config: client_config(id.fingerprint).unwrap(),
    };
    let s2 = run_sender_with(&o2, &[src], None).unwrap();
    let r2 = h2.join().unwrap();
    assert_eq!(s2.verdict, "OK");
    assert!(r2.verified);
    assert_eq!(r2.resumed_blocks, 1);
    assert_eq!(
        hash_bytes(&fs::read(dest.join("大文件/blob.bin")).unwrap()),
        hash_bytes(&data)
    );
}

#[test]
fn wrong_fingerprint_is_rejected() {
    let root = test_dir("tls-wrong-fp");
    let a = root.join("src/A");
    write_file(&a.join("x.txt"), b"x");
    let dest = root.join("recv");
    fs::create_dir_all(&dest).unwrap();
    let id = identity("tls-wrong-fp/id");

    let (listener, addr) = listener_and_addr();
    let acc = TlsAccepter {
        listener,
        config: server_config(&id).unwrap(),
    };
    let dest2 = dest.clone();
    let recv = thread::spawn(move || {
        run_receiver_with(&acc, &dest2, None, None, &HashMap::new())
    });
    let mut wrong = id.fingerprint;
    wrong[0] ^= 0xff;
    let opener = TlsOpener {
        addr,
        config: client_config(wrong).unwrap(),
    };
    assert!(run_sender_with(&opener, &[a], None).is_err());
    assert!(recv.join().unwrap().is_err());
}

#[test]
fn wrong_pair_code_is_rejected() {
    let root = test_dir("tls-pair");
    let a = root.join("src/A");
    write_file(&a.join("x.txt"), b"x");
    let dest = root.join("recv");
    fs::create_dir_all(&dest).unwrap();
    let id = identity("tls-pair/id");

    let (listener, addr) = listener_and_addr();
    let acc = TlsAccepter {
        listener,
        config: server_config(&id).unwrap(),
    };
    let dest2 = dest.clone();
    let recv = thread::spawn(move || {
        run_receiver_with(&acc, &dest2, None, Some("111111"), &HashMap::new())
    });
    let opener = TlsOpener {
        addr,
        config: client_config(id.fingerprint).unwrap(),
    };
    assert!(run_sender_with(&opener, &[a], Some("222222")).is_err());
    assert!(recv.join().unwrap().is_err());
}

#[test]
fn root_map_renames_destination_root() {
    let root = test_dir("root-map");
    let a = root.join("src/原名");
    write_file(&a.join("x.txt"), b"mapped");
    let dest = root.join("recv");
    fs::create_dir_all(&dest).unwrap();

    let (listener, addr) = listener_and_addr();
    let mut map = HashMap::new();
    map.insert("r0".into(), "自定义落点".into());
    let dest2 = dest.clone();
    let recv = thread::spawn(move || {
        let acc = PlainAccepter { listener };
        run_receiver_with(&acc, &dest2, None, None, &map).unwrap()
    });
    let send = run_sender(&addr, &[a]).unwrap();
    let report = recv.join().unwrap();

    assert_eq!(send.verdict, "OK");
    assert!(report.verified);
    assert_eq!(
        fs::read_to_string(dest.join("自定义落点/x.txt")).unwrap(),
        "mapped"
    );
    assert!(!dest.join("原名").exists());
}

#[test]
fn discovery_announce_is_received() {
    let own_fp = "s".repeat(64);
    let other_fp = "a".repeat(64);
    let ann = discover::Announcement {
        device: local_device(),
        fingerprint: other_fp.clone(),
        port: 53318,
        version: "0.2.0".into(),
        ts: queue::now_secs(),
        reply: false,
    };
    let own = own_fp.clone();
    let listener =
        thread::spawn(move || discover::listen(Duration::from_secs(2), &own).unwrap());
    thread::sleep(Duration::from_millis(200));
    discover::announce(&ann).unwrap();
    let peers = listener.join().unwrap();
    assert!(peers.iter().any(|p| p.fingerprint == other_fp));
}

/// 进度回调：必须单调递增、最后一次等于总字节。这是界面百分比的唯一依据，
/// 若回调不触发或回退，界面就会显示 0% 或倒跳。
#[test]
fn sender_reports_monotonic_progress() {
    let root = test_dir("progress");
    let src = root.join("src");
    // 3 个整块 + 1 字节：块大小是 4 MiB，必须跨多块才能拿到多个进度点，
    // 末尾那 1 字节同时覆盖「最后一块不满」的截断分支
    let size = 3 * BLOCK_SIZE + 1;
    write_file(&src.join("big.bin"), &vec![7u8; size]);
    let dest = root.join("recv");
    fs::create_dir_all(&dest).unwrap();
    let (listener, addr) = listener_and_addr();
    let dest2 = dest.clone();
    let recv = thread::spawn(move || run_receiver_on(listener, &dest2, None).unwrap());

    let samples = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = samples.clone();
    let opener = PlainOpener { addr: addr.clone() };
    let report = run_sender_with_device_progress(
        &opener,
        &[src],
        None,
        local_device(),
        Some(&mut |p| sink.lock().unwrap().push((p.bytes_sent, p.total_bytes))),
    )
    .unwrap();
    recv.join().unwrap();

    let v = samples.lock().unwrap().clone();
    assert!(v.len() >= 2, "至少应有多个进度点，实际 {}", v.len());
    for w in v.windows(2) {
        assert!(w[1].0 >= w[0].0, "进度出现回退: {:?}", w);
    }
    let (last_bytes, total) = *v.last().unwrap();
    assert_eq!(last_bytes, total, "最后一次进度应等于总字节");
    assert_eq!(total, size as u64);
    assert_eq!(report.verdict, "OK");
}

/// 单个文件入参：按父目录归组，落点是 dest/<父目录名>/<文件名>。
/// 关键是不能把父目录里没选中的文件一起发出去。
#[test]
fn sending_individual_files_groups_by_parent_dir() {
    let root = test_dir("files");
    let camera = root.join("src/Camera");
    write_file(&camera.join("a.jpg"), b"aaa");
    write_file(&camera.join("b.jpg"), b"bbb");
    write_file(&camera.join("unselected.jpg"), b"must-not-be-sent");

    let dest = root.join("recv");
    fs::create_dir_all(&dest).unwrap();
    let (listener, addr) = listener_and_addr();
    let dest2 = dest.clone();
    let recv = thread::spawn(move || run_receiver_on(listener, &dest2, None).unwrap());

    let send = run_sender(&addr, &[camera.join("a.jpg"), camera.join("b.jpg")]).unwrap();
    recv.join().unwrap();

    assert_eq!(send.verdict, "OK");
    assert_eq!(send.files, 2, "只应发送被选中的两个文件");
    assert!(dest.join("Camera/a.jpg").exists());
    assert!(dest.join("Camera/b.jpg").exists());
    assert!(!dest.join("Camera/unselected.jpg").exists(), "未选中的文件不能跟着发出去");
}

/// v0.11：成功发送时接收端回报落点文件夹；取消在块边界生效，接收端留下可续传的半成品。
#[test]
fn dest_folder_is_reported_and_cancel_stops_transfer() {
    let root = test_dir("destcancel");
    let src = root.join("src/Download");
    // 两个整块 + 10 字节：足够在第一块之后触发取消，剩下的一定没发完
    write_file(&src.join("big.bin"), &vec![3u8; BLOCK_SIZE * 2 + 10]);
    let dest = root.join("recv");
    fs::create_dir_all(&dest).unwrap();

    // ---- 第一轮：正常发完，必须回报落点文件夹 ----
    let (l1, addr1) = listener_and_addr();
    let d1 = dest.clone();
    let h1 = thread::spawn(move || run_receiver_on(l1, &d1, None).unwrap());
    let r1 = run_sender(&addr1, &[src.clone()]).unwrap();
    h1.join().unwrap();
    assert_eq!(r1.verdict, "OK");
    assert_eq!(r1.dest_dirs, vec!["Download".to_string()], "要能告诉用户传到哪个文件夹");
    assert!(dest.join("Download/big.bin").exists());

    // ---- 第二轮：新的目标目录，第一块之后取消 ----
    let dest2 = root.join("recv2");
    fs::create_dir_all(&dest2).unwrap();
    let (l2, addr2) = listener_and_addr();
    let d2 = dest2.clone();
    let h2 = thread::spawn(move || run_receiver_on(l2, &d2, None));
    let ctl = std::sync::Arc::new(SendControl::new());
    let ctl2 = ctl.clone();
    let mut cb = |p: tc_core::SendProgress| {
        if p.blocks_sent >= 1 {
            ctl2.cancel();
        }
    };
    let r2 = run_sender_with_device_control(
        &PlainOpener { addr: addr2.clone() },
        &[src],
        None,
        local_device(),
        Some(&mut cb),
        Some(ctl.as_ref()),
    );
    let err = r2.unwrap_err().to_string();
    assert!(err.contains("取消"), "取消应给出可读原因，实际：{err}");
    // 接收端要么报错退出、要么正常收尾；关键是留了 .tcpart 供下次续传
    let _ = h2.join();
    assert!(has_part_file(&dest2), "取消后应留下 .tcpart 以便续传：{dest2:?}");
}

/// 递归找 `.tcpart`：取消时半成品在落点子目录里，不是接收目录的根下。
fn has_part_file(dir: &Path) -> bool {
    let Ok(rd) = fs::read_dir(dir) else {
        return false;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            if has_part_file(&p) {
                return true;
            }
        } else if p.to_string_lossy().contains(".tcpart") {
            return true;
        }
    }
    false
}

/// 增量同步（只增不删）：同一目录再发一次，接收端对「同路径同内容」的文件直接跳过，
/// 既不请求数据块，也不产生第二份「xxx (1)」副本；
/// 但内容变了的文件必须照常传，跳过判断不能把该传的漏掉。
#[test]
fn second_send_skips_identical_files_but_still_sends_changed_ones() {
    let root = test_dir("incremental");
    let album = root.join("src/相册");
    write_file(&album.join("a.txt"), b"same content");
    write_file(&album.join("sub/b.bin"), &[7u8; 100]);
    let dest = root.join("recv");
    fs::create_dir_all(&dest).unwrap();

    // 第一轮：全新的目标，正常全量传输
    let (l1, addr1) = listener_and_addr();
    let d1 = dest.clone();
    let h1 = thread::spawn(move || run_receiver_on(l1, &d1, None).unwrap());
    let s1 = run_sender(&addr1, &[album.clone()]).unwrap();
    let r1 = h1.join().unwrap();
    assert_eq!(s1.verdict, "OK");
    assert_eq!((s1.files_skipped, r1.skipped_files), (0, 0), "第一次没有可跳过的文件");

    // 第二轮：内容一字不差，全部跳过
    let (l2, addr2) = listener_and_addr();
    let d2 = dest.clone();
    let h2 = thread::spawn(move || run_receiver_on(l2, &d2, None).unwrap());
    let s2 = run_sender(&addr2, &[album.clone()]).unwrap();
    let r2 = h2.join().unwrap();
    assert_eq!(s2.verdict, "OK");
    assert_eq!(s2.blocks_sent, 0, "全部命中时不应再传任何数据块");
    assert_eq!((s2.files_skipped, r2.skipped_files), (2, 2));
    assert_eq!(r2.blocks_written, 0);
    assert!(r2.verified);
    // 只留原文件，不生成「a (1).txt」这种副本
    assert!(!dest.join("相册/a (1).txt").exists(), "同内容文件不应重复落盘");
    assert!(!dest.join("相册/sub/b (1).bin").exists());

    // 第三轮：改掉一个文件，只有它被重传，另一个继续跳过
    write_file(&album.join("a.txt"), b"changed content!");
    let (l3, addr3) = listener_and_addr();
    let d3 = dest.clone();
    let h3 = thread::spawn(move || run_receiver_on(l3, &d3, None).unwrap());
    let s3 = run_sender(&addr3, &[album]).unwrap();
    let r3 = h3.join().unwrap();
    assert_eq!(s3.verdict, "OK");
    assert_eq!((s3.files_skipped, r3.skipped_files), (1, 1));
    assert!(s3.blocks_sent >= 1, "改过的文件必须真的传过去");
    // 同名不同内容仍按原规则另起名字，绝不覆盖已收到的版本
    assert_eq!(fs::read(dest.join("相册/a.txt")).unwrap(), b"same content");
    assert_eq!(
        fs::read(dest.join("相册/a (1).txt")).unwrap(),
        b"changed content!"
    );
}

/// v0.12 强制重传：同内容再发一次必须真的再传一遍，
/// 且本次文件保留源文件名、旧文件按 `(1)` 改名让位（只改名不删除）。
#[test]
fn force_resend_retransmits_and_moves_old_file_aside() {
    let root = test_dir("force");
    let album = root.join("src/相册");
    write_file(&album.join("a.txt"), b"same content");
    let dest = root.join("recv");
    fs::create_dir_all(&dest).unwrap();

    // 第一轮：普通发送，目标位置没有文件，不产生让位
    let (l1, addr1) = listener_and_addr();
    let d1 = dest.clone();
    let h1 = thread::spawn(move || run_receiver_on(l1, &d1, None).unwrap());
    let s1 = run_sender(&addr1, &[album.clone()]).unwrap();
    h1.join().unwrap();
    assert_eq!((s1.verdict.as_str(), s1.files_skipped), ("OK", 0));

    // 第二轮：内容一字不差，但带强制重传 —— 不跳过，数据块必须重发
    let (l2, addr2) = listener_and_addr();
    let d2 = dest.clone();
    let h2 = thread::spawn(move || run_receiver_on(l2, &d2, None).unwrap());
    let s2 = run_sender_with_opts(
        &PlainOpener { addr: addr2 },
        &[album],
        None,
        SendOptions {
            force: true,
            dest_id: None,
        },
    )
    .unwrap();
    let r2 = h2.join().unwrap();
    assert_eq!(s2.verdict, "OK");
    assert_eq!(s2.files_skipped, 0, "强制重传不能跳过任何文件");
    assert!(s2.blocks_sent >= 1, "必须真的把数据重传一遍");
    assert_eq!(r2.skipped_files, 0);

    // 本次文件占源文件名；上一份改名成 a (1).txt，内容没变
    assert_eq!(fs::read(dest.join("相册/a.txt")).unwrap(), b"same content");
    assert_eq!(
        fs::read(dest.join("相册/a (1).txt")).unwrap(),
        b"same content",
        "旧文件只改名，内容不受影响"
    );
}

/// v0.14：一批小文件必须走 pack 路径。`SendReport.packs` 为 0 说明降级成了逐块，
/// 这个断言就是「pack 到底有没有生效」的守门。
#[test]
fn many_small_files_use_pack_path() {
    let base = test_dir("pack-many-small");
    let src = base.join("src");
    fs::create_dir_all(&src).unwrap();
    for i in 0..50 {
        write_file(
            &src.join(format!("f{i}.txt")),
            format!("hello-{i}").as_bytes(),
        );
    }
    let dest = base.join("dest");
    fs::create_dir_all(&dest).unwrap();
    let (listener, addr) = listener_and_addr();
    let d = dest.clone();
    let recv = thread::spawn(move || run_receiver_on(listener, &d, None).unwrap());
    let sent = run_sender(&addr, &[src.clone()]).unwrap();
    let r = recv.join().unwrap();

    assert_eq!(sent.verdict, "OK");
    assert!(
        sent.packs > 0,
        "50 个小文件应走 pack 路径，实际 packs={}",
        sent.packs
    );
    assert_eq!(sent.blocks_sent, 50, "每个小文件一块");
    assert!(r.verified, "接收端校验必须通过");
    for i in 0..50 {
        let p = dest.join("src").join(format!("f{i}.txt"));
        assert_eq!(fs::read(&p).unwrap(), format!("hello-{i}").as_bytes());
    }
}

/// v0.15：多流要真的开起来；同一个大文件被切到多条流上并行拉，
/// 落盘后仍必须拼回正确内容（多流路径不走增量校验，这里顺带验证全量校验兜底）。
#[test]
fn parallel_streams_split_one_large_file() {
    let base = test_dir("streams-large");
    let src = base.join("src");
    // 9 块：超过「少于 8 块退回单流」的门槛，确保真的开多流
    let data: Vec<u8> = (0..(BLOCK_SIZE * 8 + 4096))
        .map(|i| (i % 251) as u8)
        .collect();
    write_file(&src.join("big.bin"), &data);
    let dest = base.join("dest");
    fs::create_dir_all(&dest).unwrap();
    let (listener, addr) = listener_and_addr();
    let d = dest.clone();
    let recv = thread::spawn(move || run_receiver_on(listener, &d, None).unwrap());
    let sent = run_sender(&addr, &[src]).unwrap();
    let r = recv.join().unwrap();

    assert_eq!(sent.verdict, "OK");
    assert_eq!(sent.streams, 4, "默认应开 4 条数据流，实际 {}", sent.streams);
    assert_eq!(r.streams_used, 4, "接收端报告的数据流数应与协商一致");
    assert!(r.verified, "多流乱序落盘后仍须通过全量校验");
    assert_eq!(
        hash_bytes(&fs::read(dest.join("src/big.bin")).unwrap()),
        hash_bytes(&data),
        "多流拼回的内容必须与源文件一致"
    );
}

/// v0.22：批量小文件落盘要做到「懒创建 + 单块不建位图」——传完目录里不留 .tcpart / .map，
/// 内容与源 mtime 都对。这是手机往 NAS 机械盘传大量照片时的主要元数据开销点。
#[test]
fn many_small_files_leave_no_part_or_map_residue() {
    let base = test_dir("lazy-small");
    let src = base.join("src");
    fs::create_dir_all(&src).unwrap();
    let stamp = 1_550_000_000u64;
    let when = std::time::UNIX_EPOCH + Duration::from_secs(stamp);
    for i in 0..40 {
        let p = src.join(format!("p{i}.jpg"));
        write_file(&p, format!("photo-{i}").as_bytes());
        let f = fs::OpenOptions::new().write(true).open(&p).unwrap();
        f.set_times(fs::FileTimes::new().set_modified(when)).unwrap();
    }
    let dest = base.join("dest");
    fs::create_dir_all(&dest).unwrap();
    let (listener, addr) = listener_and_addr();
    let d = dest.clone();
    let recv = thread::spawn(move || run_receiver_on(listener, &d, None).unwrap());
    let sent = run_sender(&addr, &[src.clone()]).unwrap();
    let r = recv.join().unwrap();

    assert_eq!(sent.verdict, "OK");
    assert!(r.verified, "接收端校验必须通过");

    let dir = dest.join("src");
    for i in 0..40 {
        let p = dir.join(format!("p{i}.jpg"));
        assert_eq!(fs::read(&p).unwrap(), format!("photo-{i}").as_bytes());
        let secs = fs::metadata(&p)
            .unwrap()
            .modified()
            .unwrap()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        assert_eq!(secs, stamp, "p{i}.jpg 的 mtime 应等于源时间");
    }
    let residue: Vec<String> = fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| n.ends_with(".tcpart") || n.ends_with(".map"))
        .collect();
    assert!(residue.is_empty(), "不应残留半成品/位图，实际: {residue:?}");
}
