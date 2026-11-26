/// 命令定义表：help、参数校验、JSON schema 的唯一事实源。
/// 参数名以 main.rs 代码实际读取的为准，不照抄 usage 行（history 的 `--dir` 是笔误）。
#[derive(Debug, Clone, Copy)]
pub struct ArgSpec {
    pub name: &'static str,
    pub takes_value: bool,
    pub required: bool,
    pub repeatable: bool,
    pub help: &'static str,
}

#[derive(Debug, Clone, Copy)]
pub struct CmdSpec {
    pub name: &'static str,
    pub group: &'static str,
    pub usage: &'static str,
    pub args: &'static [ArgSpec],
}

pub const EXPECTED_NAMES: &[&str] = &[
    "version", "identity", "send", "dests", "history", "diag", "resend", "recv", "daemon",
    "discover", "queue", "flush", "trust",
];

// 参数构造简写，避免 13 条定义里重复样板。
const fn val(name: &'static str, required: bool, help: &'static str) -> ArgSpec {
    ArgSpec {
        name,
        takes_value: true,
        required,
        repeatable: false,
        help,
    }
}
const fn flag(name: &'static str, help: &'static str) -> ArgSpec {
    ArgSpec {
        name,
        takes_value: false,
        required: false,
        repeatable: false,
        help,
    }
}
const fn many(name: &'static str, required: bool, help: &'static str) -> ArgSpec {
    ArgSpec {
        name,
        takes_value: true,
        required,
        repeatable: true,
        help,
    }
}

static VERSION_ARGS: &[ArgSpec] = &[];
static IDENTITY_ARGS: &[ArgSpec] = &[val("--dir", true, "证书与信任表目录")];
static SEND_ARGS: &[ArgSpec] = &[
    val("--to", true, "对端 IP:PORT"),
    many("--dir", true, "要发送的目录，可重复"),
    val("--peer-fp", false, "对端证书指纹（不给则自动发现）"),
    val("--pair-code", false, "配对码"),
    val("--identity", false, "本机身份目录"),
    val("--queue-dir", false, "失败时入队的目录"),
    val("--history-dir", false, "发送历史目录"),
    flag("--retry", "失败按 30s/2m/10m 自动补发"),
    flag("--force", "强制重传并让同名文件让位"),
    val("--dest-id", false, "对端落点标识"),
];
static DESTS_ARGS: &[ArgSpec] = &[
    val("--to", true, "对端 IP:PORT"),
    val("--peer-fp", false, "对端证书指纹"),
    val("--pair-code", false, "配对码"),
    val("--identity", false, "本机身份目录"),
];
static HISTORY_ARGS: &[ArgSpec] = &[
    val("--history-dir", false, "发送历史目录"),
    val("--limit", false, "最多显示条数"),
];
static DIAG_ARGS: &[ArgSpec] = &[
    val("--config", false, "配置文件路径"),
    val("--identity", false, "身份目录"),
    val("--out", false, "导出目录"),
    val("--log", false, "日志文件"),
    val("--history-dir", false, "发送历史目录"),
];
static RESEND_ARGS: &[ArgSpec] = &[
    val("--id", false, "历史记录 id，默认 last"),
    flag("--failed", "只重发该记录里失败的文件"),
    val("--history-dir", false, "发送历史目录"),
    val("--pair-code", false, "配对码"),
    val("--identity", false, "本机身份目录"),
    flag("--retry", "失败自动补发"),
    flag("--force", "强制重传"),
    val("--dest-id", false, "对端落点标识"),
];
static RECV_ARGS: &[ArgSpec] = &[
    val("--listen", true, "监听地址 IP:PORT"),
    val("--dest", true, "接收落点目录"),
    val("--identity", false, "身份目录（给了才启用 TLS）"),
    val("--pair-code", false, "配对码"),
    val("--trust-mode", false, "closed | open"),
    val("--root-map", false, "根映射 r0=名称，可重复"),
    val("--streams", false, "并行流数 1-8"),
    flag("--announce", "广播本机可接收"),
];
static DAEMON_ARGS: &[ArgSpec] = &[
    val("--config", true, "配置文件路径"),
    flag("--verify", "本次忽略哈希缓存、全部重算"),
];
static DISCOVER_ARGS: &[ArgSpec] = &[
    val("--timeout", false, "发现监听秒数，默认 3"),
    val("--identity", false, "身份目录"),
];
static QUEUE_ARGS: &[ArgSpec] = &[
    val("--dir", true, "队列目录"),
    val("--window", false, "时间窗口分钟数"),
];
static FLUSH_ARGS: &[ArgSpec] = &[
    val("--dir", true, "队列目录"),
    val("--window", false, "时间窗口分钟数"),
    val("--history-dir", false, "发送历史目录"),
];
static TRUST_ARGS: &[ArgSpec] = &[
    val("--alias", false, "设备别名（approve/rename 用）"),
    val("--fp", false, "指纹（位置参数之外的写法）"),
    val("--identity", false, "身份目录"),
    val("--config", false, "配置文件路径"),
];

/// 全局选项：所有命令都接受，AI 靠它们切输出模式与行为。
pub static GLOBAL_ARGS: &[ArgSpec] = &[
    flag("--help", "打印帮助"),
    flag("--help-json", "以 JSON 输出命令 schema"),
    flag("--json", "以 JSON 输出事件（默认 key=value）"),
    flag("--strict-exit", "启用分类退出码 2/3/4"),
    flag("--version", "打印版本号"),
];

pub static COMMANDS: &[CmdSpec] = &[
        CmdSpec {
            name: "version",
            group: "诊断",
            usage: "tc version",
            args: VERSION_ARGS,
        },
        CmdSpec {
            name: "identity",
            group: "设备与信任",
            usage: "tc identity --dir DIR",
            args: IDENTITY_ARGS,
        },
        CmdSpec {
            name: "send",
            group: "传输",
            usage: "tc send --to IP:PORT --dir PATH [--dir PATH...] [--peer-fp HEX] [--pair-code CODE] [--identity DIR] [--queue-dir DIR] [--history-dir DIR] [--retry] [--force] [--dest-id ID]",
            args: SEND_ARGS,
        },
        CmdSpec {
            name: "dests",
            group: "传输",
            usage: "tc dests --to IP:PORT [--peer-fp HEX] [--pair-code CODE] [--identity DIR]",
            args: DESTS_ARGS,
        },
        CmdSpec {
            name: "history",
            group: "诊断",
            usage: "tc history [--history-dir DIR] [--limit N]",
            args: HISTORY_ARGS,
        },
        CmdSpec {
            name: "diag",
            group: "诊断",
            usage: "tc diag [--config FILE] [--identity DIR] [--out DIR] [--log FILE] [--history-dir DIR]",
            args: DIAG_ARGS,
        },
        CmdSpec {
            name: "resend",
            group: "传输",
            usage: "tc resend [--id ID|last] [--failed] [--history-dir DIR] [--pair-code CODE] [--identity DIR] [--retry] [--force] [--dest-id ID]",
            args: RESEND_ARGS,
        },
        CmdSpec {
            name: "recv",
            group: "接收",
            usage: "tc recv --listen IP:PORT --dest PATH [--identity DIR] [--pair-code CODE] [--trust-mode closed|open] [--root-map r0=NAME] [--streams N] [--announce]",
            args: RECV_ARGS,
        },
        CmdSpec {
            name: "daemon",
            group: "接收",
            usage: "tc daemon --config FILE [--verify]",
            args: DAEMON_ARGS,
        },
        CmdSpec {
            name: "discover",
            group: "设备与信任",
            usage: "tc discover [--timeout SEC] [--identity DIR]",
            args: DISCOVER_ARGS,
        },
        CmdSpec {
            name: "queue",
            group: "传输",
            usage: "tc queue --dir DIR [--window MIN]",
            args: QUEUE_ARGS,
        },
        CmdSpec {
            name: "flush",
            group: "传输",
            usage: "tc flush --dir DIR [--window MIN] [--history-dir DIR]",
            args: FLUSH_ARGS,
        },
        CmdSpec {
            name: "trust",
            group: "设备与信任",
            usage: "tc trust list|pending|approve FP|revoke FP|rename FP --alias NAME [--identity DIR | --config FILE]",
            args: TRUST_ARGS,
        },
    ];

pub fn commands() -> &'static [CmdSpec] {
    COMMANDS
}
