use std::process::Command;

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_tc"))
}

const NAMES: &[&str] = &[
    "version", "identity", "send", "dests", "history", "diag", "resend", "recv", "daemon",
    "discover", "queue", "flush", "trust",
];

#[test]
fn help_json_exposes_full_schema() {
    let out = bin().arg("--help-json").output().unwrap();
    assert!(out.status.success(), "--help-json 应成功");
    let text = String::from_utf8_lossy(&out.stdout);
    let v: serde_json::Value = serde_json::from_str(text.trim()).expect("输出必须是合法 JSON");
    assert_eq!(v["kind"], "HELP_SCHEMA");
    assert!(v["version"].as_str().is_some(), "必须带版本号");
    let cmds = v["commands"].as_array().expect("commands 必须是数组");
    assert_eq!(cmds.len(), 13, "schema 必须覆盖 13 个命令");
    for c in cmds {
        assert!(c["name"].as_str().is_some(), "命令必须有 name");
        assert!(c["group"].as_str().is_some(), "命令必须有 group");
        assert!(c["args"].as_array().is_some(), "命令必须有 args 数组");
    }
    assert!(v["global_args"].as_array().is_some(), "必须有 global_args");
}

#[test]
fn bare_invocation_prints_help() {
    let out = bin().output().unwrap();
    assert!(out.status.success(), "裸调用打印帮助应是成功");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("【传输】"), "总帮助应有分组: {text}");
}

#[test]
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
fn usage_errors_always_exit_two() {
    // AI 靠退出码区分「命令写错了」(2) 和「传输失败」(1)。
    // 这是缺陷修复：旧版未知命令静默 exit 0，AI 无法判断失败。
    let unknown = bin().args(["version", "--bogus"]).output().unwrap();
    assert_eq!(unknown.status.code(), Some(2), "未知选项应 exit 2");

    let missing = bin().args(["send"]).output().unwrap();
    assert_eq!(missing.status.code(), Some(2), "缺必填参数应 exit 2");

    let bad_cmd = bin().arg("nope").output().unwrap();
    assert_eq!(bad_cmd.status.code(), Some(2), "未知命令应 exit 2");
}

#[test]
fn valid_command_still_exits_zero() {
    let out = bin().arg("version").output().unwrap();
    assert_eq!(out.status.code(), Some(0));
}

#[test]
fn json_output_for_core_commands() {
    // version
    let out = bin().args(["--json", "version"]).output().unwrap();
    assert!(out.status.success());
    let v: serde_json::Value = serde_json::from_str(String::from_utf8_lossy(&out.stdout).trim()).unwrap();
    assert_eq!(v["kind"], "VERSION");
    assert!(v["version"].as_str().is_some());

    // discover：短超时，输出 DISCOVER 事件
    let out = bin().args(["--json", "discover", "--timeout", "1"]).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let line = String::from_utf8_lossy(&out.stdout).lines().next().unwrap().to_string();
    let v: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(v["kind"], "DISCOVER");
    assert!(v["count"].is_u64(), "count 应是数字");

    // identity：临时目录
    let dir = std::env::temp_dir().join(format!("tc-cli-identity-{}", std::process::id()));
    let out = bin().args(["--json", "identity", "--dir", dir.to_str().unwrap()]).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let v: serde_json::Value = serde_json::from_str(String::from_utf8_lossy(&out.stdout).trim()).unwrap();
    assert_eq!(v["kind"], "IDENTITY");
    assert!(v["fingerprint"].as_str().is_some());
}

#[test]
fn default_output_is_unchanged_text() {
    // 不加 --json 时 version 仍输出纯版本号（旧契约）
    let out = bin().arg("version").output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(!text.trim().starts_with('{'), "默认输出不应是 JSON");
    assert!(text.trim().chars().all(|c| c.is_ascii_digit() || c == '.'), "默认应为纯版本号");
}

#[test]
fn global_json_flag_is_accepted_before_command() {
    let out = bin().args(["--json", "version"]).output().unwrap();
    assert!(out.status.success(), "--json 放命令前应被接受: {}", String::from_utf8_lossy(&out.stderr));
}

