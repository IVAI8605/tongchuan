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
    use io::ErrorKind::*;
    matches!(
        e.kind(),
        ConnectionRefused
            | ConnectionReset
            | ConnectionAborted
            | TimedOut
            | UnexpectedEof
            | NotConnected
            | AddrNotAvailable
            | BrokenPipe
            | WouldBlock
    )
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
}
