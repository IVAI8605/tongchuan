//! v0.16：手机指定接收位置。
//! 覆盖三件事：清单里的 destId 被映射到配置目录、未知标识在落盘前被拒、查询能拿到同一份清单。

use std::collections::HashMap;
use std::fs;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;
use tc_core::manifest::local_device;
use tc_core::transport::{PlainAccepter, PlainOpener};
use tc_core::{
    list_destinations, run_sender_with_opts, Destination, HubReport, ReceiverHub, SendOptions,
};

/// 测试产物只创建、不删除，统一落到 TC_TEST_DIR（缺省用系统临时目录）
fn test_dir(name: &str) -> PathBuf {
    let base = std::env::var("TC_TEST_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| std::env::temp_dir());
    let p = base.join(format!("通传-验证/dests-{name}-{}", std::process::id()));
    fs::create_dir_all(&p).unwrap();
    p
}

fn write_file(p: &Path, data: &[u8]) {
    if let Some(d) = p.parent() {
        fs::create_dir_all(d).unwrap();
    }
    fs::write(p, data).unwrap();
}

/// 起一个带两个落点的接收入口：默认 destA，另一个 destB（id = "b"）。
fn start_hub(root: &Path) -> (Arc<ReceiverHub>, String, PathBuf, PathBuf) {
    let dest_a = root.join("dest-a");
    let dest_b = root.join("dest-b");
    fs::create_dir_all(&dest_a).unwrap();
    fs::create_dir_all(&dest_b).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let hub = Arc::new(
        ReceiverHub::new(
            Arc::new(PlainAccepter { listener }),
            dest_a.clone(),
            None,
            HashMap::new(),
            2,
        )
        .with_data_wait_secs(2)
        .with_destinations(vec![
            Destination {
                id: "a".into(),
                name: "位置A".into(),
                path: dest_a.clone(),
            },
            Destination {
                id: "b".into(),
                name: "位置B".into(),
                path: dest_b.clone(),
            },
        ]),
    );
    let serving = Arc::clone(&hub);
    thread::spawn(move || {
        let cb: Arc<dyn Fn(HubReport) + Send + Sync> = Arc::new(|_| {});
        let _ = serving.serve(cb);
    });
    (hub, addr, dest_a, dest_b)
}

fn send_with_dest(addr: &str, src: &Path, dest_id: Option<&str>) -> std::io::Result<tc_core::SendReport> {
    run_sender_with_opts(
        &PlainOpener { addr: addr.into() },
        &[src.to_path_buf()],
        None,
        SendOptions {
            force: false,
            dest_id: dest_id.map(str::to_string),
        },
    )
}

#[test]
fn chosen_destination_is_used() {
    let root = test_dir("chosen");
    let src = root.join("src/项目");
    write_file(&src.join("a.txt"), b"hello-b");
    let (_hub, addr, dest_a, dest_b) = start_hub(&root);

    let sent = send_with_dest(&addr, &src, Some("b")).unwrap();
    assert_eq!(sent.verdict, "OK");
    assert_eq!(
        fs::read_to_string(dest_b.join("项目/a.txt")).unwrap(),
        "hello-b",
        "指定 b 时必须落在 dest-b"
    );
    assert!(
        !dest_a.join("项目/a.txt").exists(),
        "默认目录不该出现这次的文件"
    );
}

#[test]
fn missing_dest_id_uses_default() {
    let root = test_dir("default");
    let src = root.join("src/项目");
    write_file(&src.join("d.txt"), b"hello-default");
    let (_hub, addr, dest_a, dest_b) = start_hub(&root);

    let sent = send_with_dest(&addr, &src, None).unwrap();
    assert_eq!(sent.verdict, "OK");
    assert_eq!(
        fs::read_to_string(dest_a.join("项目/d.txt")).unwrap(),
        "hello-default",
        "不带 destId 时必须走默认落点"
    );
    assert!(!dest_b.join("项目/d.txt").exists());
}

#[test]
fn unknown_destination_is_rejected_before_transfer() {
    let root = test_dir("unknown");
    let src = root.join("src/项目");
    write_file(&src.join("x.txt"), b"nope");
    let (_hub, addr, dest_a, dest_b) = start_hub(&root);

    let err = send_with_dest(&addr, &src, Some("nope")).unwrap_err();
    let text = err.to_string();
    assert!(
        text.contains("unknown destination") && text.contains("a, b"),
        "拒绝原因要能看懂且列出可用标识，实际：{text}"
    );
    assert!(!dest_a.join("项目/x.txt").exists());
    assert!(!dest_b.join("项目/x.txt").exists());
}

#[test]
fn list_destinations_matches_config() {
    let root = test_dir("list");
    let (_hub, addr, _a, _b) = start_hub(&root);
    let list = list_destinations(
        &PlainOpener { addr: addr.into() },
        None,
        local_device(),
    )
    .unwrap();
    let pairs: Vec<(String, String)> = list.into_iter().map(|d| (d.id, d.name)).collect();
    assert_eq!(
        pairs,
        vec![
            ("a".to_string(), "位置A".to_string()),
            ("b".to_string(), "位置B".to_string())
        ]
    );
}
