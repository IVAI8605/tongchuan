use crate::cli::spec::{commands, CmdSpec, GLOBAL_ARGS};

/// 人类可读帮助：按 group 分组，每条命令一行 usage + 参数说明。
pub fn render_text(cmd: Option<&str>) -> String {
    if let Some(name) = cmd {
        if let Some(spec) = commands().iter().find(|s| s.name == name) {
            return render_one(spec);
        }
        return format!("unknown command {name}\n");
    }

    let mut out = String::new();
    out.push_str("通传命令行 — AI 可用 --help-json 获取完整 schema\n\n");
    let mut groups: Vec<&str> = commands().iter().map(|s| s.group).collect();
    groups.sort();
    groups.dedup();
    for group in groups {
        out.push_str(&format!("【{group}】\n"));
        for spec in commands().iter().filter(|s| s.group == group) {
            out.push_str(&format!("  {:<10} {}\n", spec.name, first_line(spec.usage)));
        }
        out.push('\n');
    }
    out.push_str("【全局选项】\n");
    for arg in GLOBAL_ARGS {
        out.push_str(&format!("  {:<14} {}\n", arg.name, arg.help));
    }
    out.push_str("\n用 tc <命令> --help 查看参数详情。\n");
    out
}

fn first_line(usage: &str) -> &str {
    usage.split('\n').next().unwrap_or(usage)
}

fn render_one(spec: &CmdSpec) -> String {
    let mut out = format!("Usage: {}\n", spec.usage);
    if !spec.args.is_empty() {
        out.push_str("\n参数:\n");
        for arg in spec.args {
            let req = if arg.required { "必填" } else { "可选" };
            let val = if arg.takes_value { " 值" } else { "" };
            out.push_str(&format!("  {:<16} {:<4}{:<4} {}\n", arg.name, req, val, arg.help));
        }
    }
    out
}

/// 机器可读 schema：AI 通过 `tc --help-json` 自发现全部命令与参数。
pub fn render_json() -> serde_json::Value {
    let cmds: Vec<serde_json::Value> = commands()
        .iter()
        .map(|s| {
            let args: Vec<serde_json::Value> = s
                .args
                .iter()
                .map(|a| {
                    serde_json::json!({
                        "name": a.name,
                        "takes_value": a.takes_value,
                        "required": a.required,
                        "repeatable": a.repeatable,
                        "help": a.help,
                    })
                })
                .collect();
            serde_json::json!({
                "name": s.name,
                "group": s.group,
                "usage": s.usage,
                "args": args,
            })
        })
        .collect();
    let globals: Vec<serde_json::Value> = GLOBAL_ARGS
        .iter()
        .map(|a| {
            serde_json::json!({
                "name": a.name,
                "takes_value": a.takes_value,
                "required": a.required,
                "repeatable": a.repeatable,
                "help": a.help,
            })
        })
        .collect();
    serde_json::json!({
        "kind": "HELP_SCHEMA",
        "version": env!("CARGO_PKG_VERSION"),
        "commands": cmds,
        "global_args": globals,
    })
}
