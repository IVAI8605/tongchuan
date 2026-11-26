use std::process::Command;

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_tc"))
}

const NAMES: &[&str] = &[
    "version", "identity", "send", "dests", "history", "diag", "resend", "recv", "daemon",
    "discover", "queue", "flush", "trust",
];

#[test]
#[ignore = "Task 3 实现结构化帮助后启用"]
fn every_command_help_lists_its_name() {
    for name in NAMES {
        let out = bin().arg(name).arg("--help").output().unwrap();
        assert!(
            out.status.success(),
            "{} --help 应成功: {}",
            name,
            String::from_utf8_lossy(&out.stderr)
        );
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(text.contains(name), "{} --help 应含命令名", name);
    }
}

#[test]
fn unknown_option_is_rejected_with_nonzero_exit() {
    // 用 version 而不是 send：避免未知选项漏过时真的发起一次传输。
    let out = bin().args(["version", "--bogus"]).output().unwrap();
    assert!(!out.status.success(), "未知选项应返回非 0");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("unknown option"), "stderr 应指出未知选项: {err}");
}

#[test]
fn streams_range_is_validated() {
    // 用 version 而不是 recv：校验发生在分发之前，且 version 不会起监听、测试不会挂。
    let out = bin().args(["version", "--streams", "0"]).output().unwrap();
    assert!(!out.status.success(), "--streams 0 应返回非 0");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("1-8") || err.contains("streams"), "应说明取值范围: {err}");
}

#[test]
fn global_json_flag_is_accepted_before_command() {
    let out = bin().args(["--json", "version"]).output().unwrap();
    assert!(out.status.success(), "--json 放命令前应被接受: {}", String::from_utf8_lossy(&out.stderr));
}

