//! `tc diag`：把「报障时要看的东西」收进一个目录，敏感项在落盘前就抹掉。
//!
//! 导出边界（红线，改这里前先看规范「敏感信息与令牌」）：
//! - 不导出管理令牌、身份私钥、配对码原文；
//! - 指纹只保留前 8 位（设备唯一标识不落原文）；
//! - 日志与历史照原样复制，但历史里的 `peerFp` 会先截短。
//!
//! 只新建文件，不删除、不移动任何既有文件；输出目录已存在时直接往里写同名文件（覆盖本次导出）。

use crate::daemon::Config;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

const LOG_TAIL_LINES: usize = 2000;
const HISTORY_TAIL_LINES: usize = 100;

/// 导出参数。所有路径都由调用方解析好，这里只负责读与写，避免两套默认值到处漂。
pub struct DiagOptions {
    /// 输出目录；调用方负责给出不含敏感信息的路径
    pub out_dir: PathBuf,
    pub config: Option<Config>,
    pub config_path: Option<PathBuf>,
    pub log_file: Option<PathBuf>,
    pub send_history: Option<PathBuf>,
    pub recv_history: Option<PathBuf>,
    pub identity_dir: Option<PathBuf>,
    pub queue_dir: Option<PathBuf>,
}

pub fn run(opts: &DiagOptions) -> io::Result<PathBuf> {
    let out = &opts.out_dir;
    fs::create_dir_all(out.join("logs"))?;
    fs::create_dir_all(out.join("history"))?;

    let mut report = String::new();
    report.push_str("通传诊断\n");
    let now = time::OffsetDateTime::now_local().unwrap_or_else(|_| time::OffsetDateTime::now_utc());
    report.push_str(&format!(
        "生成时间：{:04}-{:02}-{:02} {:02}:{:02}（24 小时制）\n",
        now.year(),
        now.month() as u8,
        now.day(),
        now.hour(),
        now.minute()
    ));
    report.push_str(&format!("版本：{}\n", env!("CARGO_PKG_VERSION")));
    report.push_str(&format!(
        "平台：{} {}\n",
        std::env::consts::OS,
        std::env::consts::ARCH
    ));
    report.push_str(&format!(
        "本机设备名：{}\n",
        crate::manifest::local_device().name
    ));
    report.push_str("------------------------------------------------\n");

    match &opts.config {
        Some(cfg) => {
            report.push_str(&format!("设备别名：{}\n", cfg.alias));
            report.push_str(&format!("传输监听：{}\n", cfg.listen));
            report.push_str(&format!("管理页：{}\n", cfg.manage));
            report.push_str(&format!("信任模式：{}\n", cfg.trust_mode));
            report.push_str(&format!(
                "自动接收：{}\n",
                if cfg.auto_accept { "开" } else { "关" }
            ));
            report.push_str(&format!(
                "配对码：{}\n",
                if cfg.pair_code.as_deref().unwrap_or("").trim().is_empty() {
                    "未配置"
                } else {
                    "已配置（值不导出）"
                }
            ));
            report.push_str(&format!("落点目录：{}\n", cfg.destination.display()));
            let allow: Vec<String> = cfg
                .allow
                .iter()
                .map(|p| p.display().to_string())
                .collect();
            report.push_str(&format!("接收白名单：{}\n", allow.join("；")));
            report.push_str(&format!("日志目录：{}\n", cfg.log_dir.display()));
            report.push_str(&format!(
                "日志级别：{}（单文件上限 {} 字节，0=不轮转）\n",
                cfg.log_level, cfg.log_max_bytes
            ));
            report.push_str(&format!("并发会话上限：{}\n", cfg.max_sessions));
            report.push_str(&format!(
                "并行数据流：{}（1 = 关闭）\n",
                cfg.streams.max(1)
            ));
            report.push_str(&format!(
                "可选接收位置：{} 个{}\n",
                cfg.destinations.len(),
                if cfg.destinations.is_empty() {
                    "（只有默认落点）".to_string()
                } else {
                    format!(
                        "（{}）",
                        cfg.destinations
                            .iter()
                            .map(|d| d.name.clone())
                            .collect::<Vec<_>>()
                            .join("、")
                    )
                }
            ));
            if let Some(p) = &opts.config_path {
                report.push_str(&format!("配置文件：{}\n", p.display()));
            }
            if let Some(dir) = &opts.identity_dir {
                let count = crate::trust::TrustStore::new(dir)
                    .list()
                    .map(|v| v.len())
                    .unwrap_or(0);
                report.push_str(&format!("信任设备数：{count}\n"));
                // 只读已有证书：没有身份文件时不调用 load_or_create，诊断命令不该顺手生成私钥
                if dir.join("identity.cer").exists() && dir.join("identity.key").exists() {
                    if let Ok(id) = crate::tls::Identity::load_or_create(dir) {
                        let fp = id.fingerprint_hex();
                        report.push_str(&format!("本机指纹（前 8 位）：{}…\n", &fp[..8]));
                    }
                }
            }
            if let Some(q) = &opts.queue_dir {
                let pending = crate::queue::pending(q, crate::queue::DEFAULT_WINDOW_MINUTES)
                    .map(|v| v.len())
                    .unwrap_or(0);
                report.push_str(&format!("待发队列：{pending} 条（10 分钟窗口）\n"));
            }
            // 配置另存一份脱敏副本：排查字段名/取值时比看这段摘要更直接
            let mut safe = cfg.clone();
            safe.pair_code = None;
            if let Ok(text) = toml::to_string_pretty(&safe) {
                let mut f = File::create(out.join("config.redacted.toml"))?;
                writeln!(
                    f,
                    "# 配对码与令牌等凭据已移除；字段含义见 configs/config.example.toml"
                )?;
                f.write_all(text.as_bytes())?;
            }
        }
        None => report.push_str("未找到配置文件：只导出日志与历史\n"),
    }

    report.push_str("------------------------------------------------\n");
    report.push_str(&format!(
        "导出内容（源文件不存在时跳过，不生成空文件）：\n- logs/daemon.tail.log（最近 {LOG_TAIL_LINES} 行）\n- history/send-history.tail.jsonl（最近 {HISTORY_TAIL_LINES} 条，peerFp 已截短）\n- history/recv-history.tail.jsonl（最近 {HISTORY_TAIL_LINES} 条）\n- config.redacted.toml（配对码已移除）\n"
    ));
    report.push_str("说明：管理令牌、身份私钥、配对码原文一律不导出；指纹只保留前 8 位。\n");

    let mut f = File::create(out.join("diag.txt"))?;
    f.write_all(report.as_bytes())?;

    if let Some(log) = &opts.log_file {
        if log.exists() {
            let text = tail_lines(log, LOG_TAIL_LINES);
            fs::write(out.join("logs").join("daemon.tail.log"), text)?;
        }
    }
    if let Some(h) = &opts.send_history {
        if h.exists() {
            let text = tail_lines(h, HISTORY_TAIL_LINES);
            fs::write(
                out.join("history").join("send-history.tail.jsonl"),
                redact_history(&text),
            )?;
        }
    }
    if let Some(h) = &opts.recv_history {
        if h.exists() {
            let text = tail_lines(h, HISTORY_TAIL_LINES);
            fs::write(
                out.join("history").join("recv-history.tail.jsonl"),
                redact_history(&text),
            )?;
        }
    }

    Ok(out.clone())
}

/// 取文件末尾 n 行；读失败返回空串——诊断自身不能因为缺文件就整个失败。
fn tail_lines(path: &Path, n: usize) -> String {
    let text = fs::read_to_string(path).unwrap_or_default();
    let all: Vec<&str> = text.lines().collect();
    let mut out = all[all.len().saturating_sub(n)..].join("\n");
    if !out.is_empty() {
        out.push('\n');
    }
    out
}

/// 历史行里的 `peerFp` 截短到前 8 位；坏行原样保留（诊断需要看到坏在哪）。
fn redact_history(text: &str) -> String {
    let mut out = String::new();
    for line in text.lines() {
        let fixed = match serde_json::from_str::<serde_json::Value>(line) {
            Ok(mut v) => {
                if let Some(obj) = v.as_object_mut() {
                    if let Some(fp) = obj.get_mut("peerFp").and_then(|x| x.as_str().map(String::from)) {
                        obj.insert(
                            "peerFp".into(),
                            serde_json::Value::String(short_fp(&fp)),
                        );
                    }
                }
                serde_json::to_string(&v).unwrap_or_else(|_| line.to_string())
            }
            Err(_) => line.to_string(),
        };
        out.push_str(&fixed);
        out.push('\n');
    }
    out
}

pub fn short_fp(fp: &str) -> String {
    let head: String = fp.chars().take(8).collect();
    if fp.chars().count() > 8 {
        format!("{head}…")
    } else {
        head
    }
}

/// 默认输出目录：当前目录下的 `tc-diag-<年月日-时分>`，与日志轮转用同一套时间口径。
/// 放当前目录而不是固定目录，是因为这个文件夹要交给别人看，用户得知道它生在哪。
pub fn default_out_dir() -> PathBuf {
    let now = time::OffsetDateTime::now_local().unwrap_or_else(|_| time::OffsetDateTime::now_utc());
    PathBuf::from(format!(
        "tc-diag-{:04}{:02}{:02}-{:02}{:02}",
        now.year(),
        now.month() as u8,
        now.day(),
        now.hour(),
        now.minute()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("tc-diag-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn diag_never_exports_pair_code_token_or_full_fingerprint() {
        let root = temp_dir("redact");
        let id_dir = root.join("identity");
        fs::create_dir_all(&id_dir).unwrap();
        // 令牌文件必须原样留在原地、不能出现在导出物里
        fs::write(id_dir.join("manage.token"), "SECRET-TOKEN-0123456789abcdef").unwrap();
        let mut cfg = Config::default();
        cfg.pair_code = Some("pair-secret-42".into());
        cfg.identity_dir = id_dir.clone();
        cfg.destination = root.join("received");
        fs::create_dir_all(&cfg.destination).unwrap();
        cfg.allow = vec![cfg.destination.clone()];
        cfg.log_dir = root.join("logs");
        cfg.queue_dir = root.join("queue");

        let full_fp = "a".repeat(64);
        let hist = root.join("send-history.jsonl");
        fs::write(
            &hist,
            format!(
                "{{\"id\":\"x\",\"ts\":1,\"to\":\"127.0.0.1:1\",\"paths\":[],\"peerFp\":\"{full_fp}\",\"files\":0,\"bytes\":0,\"verdict\":\"OK\",\"ok\":true}}\n"
            ),
        )
        .unwrap();
        let daemon_log = root.join("daemon.log");
        fs::write(&daemon_log, "1\tinfo\t守护进程启动\n").unwrap();

        let out = root.join("out");
        run(&DiagOptions {
            out_dir: out.clone(),
            config: Some(cfg),
            config_path: Some(root.join("config.toml")),
            log_file: Some(daemon_log),
            send_history: Some(hist),
            recv_history: None,
            identity_dir: Some(id_dir),
            queue_dir: Some(root.join("queue")),
        })
        .unwrap();

        let all: String = walk(&out);
        assert!(!all.contains("pair-secret-42"), "配对码不能落进导出物");
        assert!(!all.contains("SECRET-TOKEN"), "管理令牌不能落进导出物");
        assert!(!all.contains(&full_fp), "指纹不能按原文导出");
        assert!(all.contains("aaaaaaaa…"), "指纹应保留前 8 位供对账");
        assert!(all.contains("配对码：已配置"), "要能看出配对码配过，但不能看到值");
    }

    fn walk(dir: &Path) -> String {
        let mut out = String::new();
        for e in fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                out.push_str(&walk(&p));
            } else if let Ok(t) = fs::read_to_string(&p) {
                out.push_str(&t);
            }
        }
        out
    }
}
