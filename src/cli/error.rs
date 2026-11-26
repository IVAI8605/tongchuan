use std::io;

/// 错误分类：AI 靠退出码区分「命令写错」「被对端拒绝」「配置错」「运行失败」。
///
/// v1.4.1 现状：`PeerRejected`(3) 与 `Config`(4) 只有映射与单测，命令路由还没有接上，
/// 实际只会出现 0 / 1 / 2。保留这两个变体是为了将来接线时不破坏调用方的映射表，
/// 但注释不能再写成「已经生效」——文档与实际不一致比多两个变体更糟。
#[allow(dead_code)]
#[derive(Debug)]
pub enum CliError {
    Usage(String),    // exit 2：unknown command/option、缺参、数值范围
    PeerRejected(String), // exit 3：对端 BUSY/untrusted/版本不兼容（预留，未接线）
    Config(String),   // exit 4：配置解析失败、白名单不含落点（预留，未接线）
    Io(io::Error),    // exit 1：其余运行错误
}

impl CliError {
    pub fn exit_code(&self) -> i32 {
        match self {
            CliError::Usage(_) => 2,
            CliError::PeerRejected(_) => 3,
            CliError::Config(_) => 4,
            CliError::Io(_) => 1,
        }
    }

    pub fn message(&self) -> String {
        match self {
            CliError::Usage(m) | CliError::PeerRejected(m) | CliError::Config(m) => m.clone(),
            CliError::Io(e) => e.to_string(),
        }
    }
}

impl From<io::Error> for CliError {
    fn from(e: io::Error) -> Self {
        CliError::Io(e)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_codes_are_stable() {
        assert_eq!(CliError::Usage("x".into()).exit_code(), 2);
        assert_eq!(CliError::PeerRejected("x".into()).exit_code(), 3);
        assert_eq!(CliError::Config("x".into()).exit_code(), 4);
        assert_eq!(
            CliError::Io(io::Error::new(io::ErrorKind::Other, "x")).exit_code(),
            1
        );
    }
}
