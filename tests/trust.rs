//! 信任库 P1 验收：事件回放、撤销即时生效、重复批准幂等、坏行跳过、只追加不重写。
//! 全部在系统临时目录内跑，互不干扰；测试产物按约定只建不删。

use std::fs;
use std::path::PathBuf;
use tc_core::trust::{resolve_identity_dir, TrustStore};

// 形状合法的假指纹（64 位十六进制），不涉及任何真实设备
const FP_A: &str = "a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1";
const FP_B: &str = "b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2";

fn dir(tag: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("tc-trust-{tag}-{}", std::process::id()));
    fs::create_dir_all(&p).unwrap();
    p
}

#[test]
fn approve_revoke_and_history_lines_are_kept() {
    let d = dir("basic");
    let s = TrustStore::new(&d);
    // 还没有任何事件时不该凭空创建文件
    assert!(s.list().unwrap().is_empty());
    assert!(!s.path().exists());

    let p = s
        .approve(FP_A, "客厅笔记本", "LAPTOP-01", "windows", "manual")
        .unwrap();
    assert_eq!(p.alias, "客厅笔记本");
    assert_eq!(s.list().unwrap().len(), 1);
    assert!(s.is_trusted(FP_A).unwrap());

    assert!(s.revoke(FP_A, "manual", "换设备").unwrap());
    assert!(!s.is_trusted(FP_A).unwrap());

    // 撤销不删行：add 与 revoke 两行都必须在
    let text = fs::read_to_string(s.path()).unwrap();
    assert_eq!(text.lines().count(), 2);
    assert!(text.contains("\"op\":\"add\""));
    assert!(text.contains("\"op\":\"revoke\""));

    // 撤销一个本来就不在表里的指纹，不写无意义的撤销行
    let n = text.lines().count();
    assert!(!s.revoke(FP_A, "manual", "重复撤销").unwrap());
    assert_eq!(fs::read_to_string(s.path()).unwrap().lines().count(), n);
}

/// daemon 常驻时 CLI 改名/撤销，必须下一场就生效，不能等重启。
#[test]
fn revoke_from_another_store_takes_effect_immediately() {
    let d = dir("live");
    let daemon_side = TrustStore::new(&d);
    daemon_side.approve(FP_A, "旧名", "", "", "manual").unwrap();
    assert!(daemon_side.is_trusted(FP_A).unwrap());

    // 另一个实例等价于 CLI / 管理页写文件
    let cli_side = TrustStore::new(&d);
    assert!(cli_side.revoke(FP_A, "manual", "联调").unwrap());

    assert!(!daemon_side.is_trusted(FP_A).unwrap(), "同一实例应立即看到撤销");
}

#[test]
fn repeated_approve_is_idempotent_and_keeps_first_seen() {
    let d = dir("idem");
    let s = TrustStore::new(&d);
    let p1 = s.approve(FP_A, "旧名", "", "", "manual").unwrap();
    let p2 = s.approve(FP_A, "", "", "", "pair-code").unwrap();

    assert_eq!(p2.alias, "旧名", "空别名不能把已有别名清空");
    assert_eq!(p2.first_seen, p1.first_seen, "重复批准不重置首次时间");
    assert_eq!(p2.added_by, "pair-code", "来源以最后一次批准为准");
    assert_eq!(s.list().unwrap().len(), 1);
}

#[test]
fn rename_only_changes_alias_and_never_creates_trust() {
    let d = dir("rename");
    let s = TrustStore::new(&d);
    s.approve(FP_A, "旧名", "", "", "manual").unwrap();

    assert!(s.rename(FP_A, "书房台式").unwrap());
    let list = s.list().unwrap();
    assert_eq!(list[0].alias, "书房台式");
    assert_eq!(list[0].fp, FP_A);

    // 未知指纹的改名不能凭一条事件造出信任项
    assert!(!s.rename(FP_B, "借用名字").unwrap());
    assert_eq!(s.list().unwrap().len(), 1);
}

#[test]
fn bad_lines_are_skipped_and_counted() {
    let d = dir("badlines");
    let file = d.join("trust.jsonl");
    let good = format!(
        r#"{{"ts":1762779600,"op":"add","fp":"{FP_A}","alias":"可用设备","by":"manual"}}"#
    );
    fs::write(
        &file,
        format!(
            "{good}\n这不是 JSON\n{}\n", // 手工编辑出错的一行
            r#"{"ts":1762779700,"op":"unknown-op","fp":"b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2"}"#
        ),
    )
    .unwrap();

    let s = TrustStore::new(&d);
    let list = s.list().unwrap();
    assert_eq!(list.len(), 1, "坏行不能拖垮整张表");
    assert_eq!(list[0].alias, "可用设备");
    assert_eq!(s.bad_lines().unwrap(), 2, "坏行与未知 op 都应计数");
}

#[test]
fn file_is_append_only() {
    let d = dir("append");
    let s = TrustStore::new(&d);
    s.approve(FP_A, "设备", "", "", "manual").unwrap();
    let before = fs::read(s.path()).unwrap();
    s.revoke(FP_A, "manual", "撤销").unwrap();
    let after = fs::read(s.path()).unwrap();

    // 新内容必须在旧内容之后，不能重写历史
    assert!(after.starts_with(&before));
    assert!(after.len() > before.len());
}

#[test]
fn fingerprint_is_normalized_and_invalid_is_rejected() {
    let d = dir("fp");
    let s = TrustStore::new(&d);
    // 带冒号的大写写法也应该认成同一个指纹
    let messy = FP_A
        .as_bytes()
        .chunks(2)
        .map(|c| String::from_utf8_lossy(c).to_uppercase())
        .collect::<Vec<_>>()
        .join(":");
    s.approve(&messy, "设备", "", "", "manual").unwrap();
    assert!(s.is_trusted(FP_A).unwrap());

    assert!(s.approve("abcd", "", "", "", "manual").is_err());
    assert!(s.revoke("not-a-fingerprint", "manual", "").is_err());
}

/// 不给 --identity 时必须落到配置文件里的 identity_dir，不能落到平台默认目录，
/// 否则 CLI 写一处、daemon 读另一处，用户看到的是「命令成功但管理页没有」。
#[test]
fn identity_dir_falls_back_to_config_file() {
    let d = dir("cfgdir");
    let ident = d.join("data").join("identity");
    let cfg = d.join("config.toml");
    fs::write(
        &cfg,
        format!("identity_dir = '{}'\n", ident.display().to_string().replace('\\', "/")),
    )
    .unwrap();

    let got = resolve_identity_dir(None, Some(cfg.as_path())).unwrap();
    assert_eq!(got, ident);

    // 显式 --identity 优先于配置
    let other = d.join("other");
    assert_eq!(
        resolve_identity_dir(Some(other.as_path()), Some(cfg.as_path())).unwrap(),
        other
    );

    // 明确指了配置却读不到，要报错而不是悄悄改用平台默认目录
    assert!(resolve_identity_dir(None, Some(d.join("missing.toml").as_path())).is_err());
}
