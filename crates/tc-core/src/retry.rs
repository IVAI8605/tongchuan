//! 发送失败后的自动重试节奏与「值不值得重试」的判断。
//!
//! 只重试网络类错误：对端锁屏、刚重启、Wi-Fi 抖动这些情况过一会儿自己就好了；
//! 而配对码不对、未信任、版本不一致、路径不存在这类错误重试一百次结果一样，
//! 让调用方白等十几分钟反而像卡死。所以先分类，再按梯度退避。

use std::io;
use std::time::Duration;

/// 默认退避：30 秒 → 2 分钟 → 10 分钟，最多补试 3 次。
/// 这个梯度能让「对方正在重启」在一分钟内接上，又不会在对方长时间离线时反复占端口。
pub const DEFAULT_DELAYS_SECS: [u64; 3] = [30, 120, 600];

/// 生效的退避表。
/// 环境变量 `TC_RETRY_DELAYS=1,2` 只给离线验证用——按真实节奏跑一次门禁要等十几分钟。
pub fn delays() -> Vec<Duration> {
    parse_delays(std::env::var("TC_RETRY_DELAYS").ok().as_deref())
}

/// 解析退避表。写坏了（空串、全非数字）退回默认节奏：
/// 宁可多等几轮，也不要静默变成「一次都不重试」，那是用户看不见的能力缺失。
fn parse_delays(spec: Option<&str>) -> Vec<Duration> {
    let list: Vec<u64> = spec
        .unwrap_or("")
        .split(',')
        .filter_map(|p| p.trim().parse::<u64>().ok())
        .collect();
    let secs: Vec<u64> = if list.is_empty() {
        DEFAULT_DELAYS_SECS.to_vec()
    } else {
        list
    };
    secs.into_iter().map(Duration::from_secs).collect()
}

/// 值得重试的错误：连接被拒/超时/被重置/对端提前关连接。
/// 其余错误（参数错、路径不存在、配对码不对、协议不兼容）立刻返回，不浪费用户时间。
pub fn is_retryable(e: &io::Error) -> bool {
    matches!(classify(e), ErrorClass::Network)
}

/// 错误来源类别：把「网络抖动」和「磁盘满 / 无权限」这类重试无用的错误分开。
/// 接收端写盘失败时据此在本地日志里写明是扩容还是查网络，排障少绕弯子。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorClass {
    /// 连接被拒/超时/重置等，过一会儿可能自愈
    Network,
    /// 磁盘满、配额用尽、单文件超限
    DiskFull,
    /// 无写权限、只读文件系统
    Permission,
    /// 参数错、路径不存在、协议不兼容等，重试没有意义
    Fatal,
}

impl ErrorClass {
    /// 中文短标签，直接拼进日志/报错文本。
    pub fn label_zh(self) -> &'static str {
        match self {
            ErrorClass::Network => "网络错误",
            ErrorClass::DiskFull => "磁盘空间不足",
            ErrorClass::Permission => "无写权限",
            ErrorClass::Fatal => "其它错误",
        }
    }
}

/// 按 `ErrorKind` 归类。磁盘满与无权限必须和网络错误区分开——它们重试多少次都一样，
/// 却最容易被当成「网络抖动」白等十几分钟。
pub fn classify(e: &io::Error) -> ErrorClass {
    use io::ErrorKind::*;
    match e.kind() {
        ConnectionRefused | ConnectionReset | ConnectionAborted | TimedOut | UnexpectedEof
        | NotConnected | AddrNotAvailable | BrokenPipe | WouldBlock => ErrorClass::Network,
        StorageFull | QuotaExceeded | FileTooLarge => ErrorClass::DiskFull,
        PermissionDenied | ReadOnlyFilesystem => ErrorClass::Permission,
        _ => ErrorClass::Fatal,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io;

    #[test]
    fn default_schedule_is_thirty_seconds_two_minutes_ten_minutes() {
        let d = parse_delays(None);
        assert_eq!(d.len(), 3);
        assert_eq!(d[0], Duration::from_secs(30));
        assert_eq!(d[1], Duration::from_secs(120));
        assert_eq!(d[2], Duration::from_secs(600));
    }

    #[test]
    fn env_override_is_parsed_and_broken_values_fall_back() {
        assert_eq!(parse_delays(Some("1,2")), vec![Duration::from_secs(1), Duration::from_secs(2)]);
        // 写坏时不能变成「不重试」，必须回到默认节奏
        assert_eq!(parse_delays(Some("abc")).len(), 3);
        assert_eq!(parse_delays(Some("")).len(), 3);
    }

    #[test]
    fn only_network_errors_are_retryable() {
        let net = io::Error::from(io::ErrorKind::ConnectionRefused);
        assert!(is_retryable(&net));
        assert!(is_retryable(&io::Error::from(io::ErrorKind::UnexpectedEof)));
        // 配对码不对、版本不一致在项目里都是 InvalidData，重试无意义
        assert!(!is_retryable(&io::Error::new(io::ErrorKind::InvalidData, "pair code mismatch")));
        assert!(!is_retryable(&io::Error::from(io::ErrorKind::NotFound)));
        assert!(!is_retryable(&io::Error::from(io::ErrorKind::PermissionDenied)));
    }

    #[test]
    fn classify_separates_disk_and_permission_from_network() {
        use std::io::ErrorKind::*;
        assert_eq!(classify(&io::Error::from(ConnectionRefused)), ErrorClass::Network);
        assert_eq!(classify(&io::Error::from(StorageFull)), ErrorClass::DiskFull);
        assert_eq!(classify(&io::Error::from(QuotaExceeded)), ErrorClass::DiskFull);
        assert_eq!(classify(&io::Error::from(FileTooLarge)), ErrorClass::DiskFull);
        assert_eq!(classify(&io::Error::from(PermissionDenied)), ErrorClass::Permission);
        assert_eq!(classify(&io::Error::from(ReadOnlyFilesystem)), ErrorClass::Permission);
        assert_eq!(classify(&io::Error::from(InvalidData)), ErrorClass::Fatal);

        // 磁盘满/无权限绝不能当网络错误去重试，否则白等十几分钟
        assert!(!is_retryable(&io::Error::from(StorageFull)));
        assert!(!is_retryable(&io::Error::from(PermissionDenied)));
        assert_eq!(ErrorClass::DiskFull.label_zh(), "磁盘空间不足");
        assert_eq!(ErrorClass::Permission.label_zh(), "无写权限");
    }
}
