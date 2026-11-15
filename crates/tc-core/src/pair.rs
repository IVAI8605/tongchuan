//! v0.20 二维码配对：一次性令牌 + 可扫的配对地址。
//!
//! 为什么不用长期静态配对码：静态码写在配置里长期有效，截图外泄就等于永久后门。
//! 这里改成「接收端临时生成一个 10 分钟、只能用一次的令牌」，用完即失效；
//! 发送端扫到二维码后，仍走原来的 `Hello.pairCode` 字段，协议与旧端完全兼容。

use crate::queue::now_secs;
use ring::rand::SecureRandom;
use serde::{Deserialize, Serialize};
use std::io;
use std::sync::Mutex;

/// 令牌有效期（秒）。关闭二维码对话框只是停止展示，不撤销令牌——
/// 用户关掉再打开、或另一个人扫，10 分钟内都还能用。
pub const PAIR_TTL_SECS: i64 = 600;

/// 发送端本地判过期时允许的时钟误差。真正判定以接收端为准，
/// 这里只用来提示「可能已过期」，误差范围内不拦人。
pub const PAIR_CLOCK_SKEW_SECS: i64 = 300;

/// Crockford Base32：去掉了 I / L / O / U，手抄或口述时不易与 1 / 0 混淆。
const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// 令牌长度。10 个字符 = 50 位随机量，配合 10 分钟有效与失败限速够用，
/// 再长用户抄写/核对就费劲了。
pub const PAIR_TOKEN_LEN: usize = 10;

/// 一分钟内允许的失败次数上限。超过就暂时不再校验令牌，
/// 防止有人在局域网里穷举这 50 位。
pub const PAIR_MAX_FAILURES: usize = 5;
const PAIR_FAILURE_WINDOW_SECS: i64 = 60;

/// 一次配对令牌的校验结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsumeResult {
    /// 命中且未过期：已原子消费，同一个令牌不能再用第二次
    Ok,
    /// 令牌对得上但已过期
    Expired,
    /// 与当前有效令牌不符
    Invalid,
    /// 近期失败次数过多，暂时不受理
    RateLimited,
    /// 当前没有生成过任何令牌
    None,
}

impl ConsumeResult {
    /// 给日志/界面用的短原因。刻意不含令牌本身。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Expired => "expired",
            Self::Invalid => "invalid",
            Self::RateLimited => "rate-limited",
            Self::None => "no-token",
        }
    }
}

/// 当前有效的令牌（给界面画倒计时用）。
#[derive(Debug, Clone, Serialize)]
pub struct ActiveToken {
    pub token: String,
    pub expires: i64,
    pub ttl: i64,
}

struct Inner {
    /// 同一时刻只保留一个令牌：重新生成即让旧码失效，
    /// 避免桌面上同时躺着好几张都能用的码。
    active: Option<(String, i64)>,
    /// 最近的失败时间点（秒），只保留窗口内的
    failures: Vec<i64>,
}

/// 接收端持有的一次性令牌表。桌面端放在 DaemonState，Android 放在进程级单例，
/// 与信任门禁共用同一份实例。
pub struct PairTokens {
    inner: Mutex<Inner>,
    ttl: i64,
}

impl Default for PairTokens {
    fn default() -> Self {
        Self::new()
    }
}

impl PairTokens {
    pub fn new() -> Self {
        Self::with_ttl(PAIR_TTL_SECS)
    }

    pub fn with_ttl(ttl: i64) -> Self {
        Self {
            inner: Mutex::new(Inner {
                active: None,
                failures: Vec::new(),
            }),
            ttl,
        }
    }

    pub fn ttl(&self) -> i64 {
        self.ttl
    }

    /// 生成新令牌，并让上一个立即失效。生成失败（系统随机数不可用）返回错误，
    /// 不会退回可预测的令牌。
    pub fn issue(&self) -> io::Result<ActiveToken> {
        let token = random_token()?;
        let expires = now_secs() + self.ttl;
        let mut g = self.inner.lock().expect("pair tokens poisoned");
        g.active = Some((token.clone(), expires));
        // 重新生成是用户主动作废旧码的行为，失败计数也一并清零
        g.failures.clear();
        Ok(ActiveToken {
            token,
            expires,
            ttl: self.ttl,
        })
    }

    /// 当前仍在有效期内的令牌。已过期的直接顺手清掉，不留「看着有效其实没用」的状态。
    pub fn active(&self) -> Option<ActiveToken> {
        self.active_at(now_secs())
    }

    pub fn active_at(&self, now: i64) -> Option<ActiveToken> {
        let mut g = self.inner.lock().expect("pair tokens poisoned");
        match &g.active {
            Some((token, exp)) if *exp >= now => Some(ActiveToken {
                token: token.clone(),
                expires: *exp,
                ttl: self.ttl,
            }),
            Some(_) => {
                g.active = None;
                None
            }
            None => None,
        }
    }

    /// 立即作废。返回 true 表示确实撤销了一个尚未过期的令牌。
    pub fn revoke(&self) -> bool {
        let mut g = self.inner.lock().expect("pair tokens poisoned");
        let had = g.active.take().is_some();
        g.failures.clear();
        had
    }

    /// 校验并消费令牌。命中即消费，所以同一张二维码只能配一台设备。
    pub fn consume(&self, got: &str) -> ConsumeResult {
        self.consume_at(got, now_secs())
    }

    pub fn consume_at(&self, got: &str, now: i64) -> ConsumeResult {
        let normalized = normalize_token(got);
        // 空值或长度明显不是令牌（例如用户手输的 6 位静态配对码）直接放过去，
        // 不计失败——否则静态码连发几次就会把对手自己限速掉
        if normalized.len() != PAIR_TOKEN_LEN {
            return ConsumeResult::None;
        }
        let mut g = self.inner.lock().expect("pair tokens poisoned");
        let Some((token, exp)) = g.active.clone() else {
            // 没有生成过任何令牌：猜也猜不到，不必记失败
            return ConsumeResult::None;
        };
        // 有令牌时才启用限速：穷举者不该从返回码里得到「差一点猜对」的信息
        g.failures.retain(|t| now - *t < PAIR_FAILURE_WINDOW_SECS);
        if g.failures.len() >= PAIR_MAX_FAILURES {
            return ConsumeResult::RateLimited;
        }
        if !ct_eq(token.as_bytes(), normalized.as_bytes()) {
            g.failures.push(now);
            return ConsumeResult::Invalid;
        }
        // 对得上但过期：清掉这个死令牌，并如实回报原因
        if now > exp {
            g.active = None;
            return ConsumeResult::Expired;
        }
        g.active = None;
        g.failures.clear();
        ConsumeResult::Ok
    }
}

/// 逐字节比较、不提前返回：令牌是短随机串，别让「第几位不匹配」从耗时里漏出去。
/// 这里不引入额外依赖（ring 的同名函数已标记废弃，subtle 只是间接受 rustls 引入）。
fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    std::hint::black_box(diff) == 0
}

/// 生成 10 位 Crockford Base32 令牌。每字节掩掉高位取 5 bit，分布均匀且不需要取模。
fn random_token() -> io::Result<String> {
    let rng = ring::rand::SystemRandom::new();
    let mut buf = [0u8; PAIR_TOKEN_LEN];
    rng.fill(&mut buf)
        .map_err(|_| io::Error::other("系统随机数不可用，无法生成配对令牌"))?;
    Ok(buf
        .iter()
        .map(|b| ALPHABET[(b & 31) as usize] as char)
        .collect())
}

/// 用户手输时的容错：统一大写、去掉空格与连字符，并按 Crockford 规则把 O/I/L 归到 0/1。
fn normalize_token(text: &str) -> String {
    text.trim()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| match c.to_ascii_uppercase() {
            'O' => '0',
            'I' | 'L' => '1',
            other => other,
        })
        .collect()
}

/// 二维码里承载的内容。地址、指纹、令牌一次给全：
/// 发送端扫完就能直接连，不需要再等发现报文，也不会连错设备。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairPayload {
    /// 接收端地址，形如 192.168.1.23:53318
    pub addr: String,
    /// 接收端证书指纹（64 位十六进制），发送端按它做 TLS 固定
    pub fp: String,
    /// 一次性令牌
    pub code: String,
    /// 接收端给出的过期时间（unix 秒）
    pub exp: i64,
    /// 接收端别名，扫码后界面直接显示「发给谁」
    pub name: String,
}

impl PairPayload {
    pub fn encode(&self) -> String {
        format!(
            "tc://pair?v=1&addr={}&fp={}&code={}&exp={}&name={}",
            url_encode(&self.addr),
            url_encode(&self.fp),
            url_encode(&self.code),
            self.exp,
            url_encode(&self.name)
        )
    }

    /// 解析二维码文本。任何一项不合规都明确报错——
    /// 宁可让用户重扫，也不能拿着半截参数去连一台来路不明的接收端。
    pub fn parse(text: &str) -> Result<Self, String> {
        let text = text.trim();
        let rest = text
            .strip_prefix("tc://pair?")
            .ok_or_else(|| "不是通传的配对二维码".to_string())?;
        let mut v = String::new();
        let (mut addr, mut fp, mut code, mut exp, mut name) = (
            String::new(),
            String::new(),
            String::new(),
            String::new(),
            String::new(),
        );
        for pair in rest.split('&') {
            let (k, val) = match pair.split_once('=') {
                Some(x) => x,
                None => continue,
            };
            let val = url_decode(val);
            match k {
                "v" => v = val,
                "addr" => addr = val,
                "fp" => fp = val,
                "code" => code = val,
                "exp" => exp = val,
                "name" => name = val,
                _ => {}
            }
        }
        if v != "1" {
            return Err(format!("配对二维码版本 {v} 不受支持，请升级到 v0.20.0 及以上"));
        }
        if !valid_addr(&addr) {
            return Err(format!("二维码里的地址不合规：{addr}"));
        }
        let fp = fp.trim().to_ascii_lowercase();
        if fp.len() != 64 || !fp.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err("二维码里的指纹不是 64 位十六进制".to_string());
        }
        let code = normalize_token(&code);
        if code.len() != PAIR_TOKEN_LEN || !code.chars().all(|c| c.is_ascii_alphanumeric()) {
            return Err("二维码里的配对令牌格式不对".to_string());
        }
        let exp: i64 = exp
            .trim()
            .parse()
            .map_err(|_| "二维码里的过期时间不是数字".to_string())?;
        if exp <= 0 {
            return Err("二维码里的过期时间不合规".to_string());
        }
        Ok(Self {
            addr,
            fp,
            code,
            exp,
            name: name.trim().to_string(),
        })
    }

    /// 发送端本地判过期。接收端才是权威，这里留 ±5 分钟误差，
    /// 两端时钟差几分钟不该让用户白扫一次。
    pub fn locally_expired(&self, now: i64) -> bool {
        now > self.exp + PAIR_CLOCK_SKEW_SECS
    }

    /// 已过期但还在误差范围内：提示一下，但仍允许尝试。
    pub fn expiry_hint(&self, now: i64) -> Option<&'static str> {
        if now > self.exp {
            if self.locally_expired(now) {
                Some("二维码已过期，请让对方重新生成")
            } else {
                Some("二维码可能刚过期（两端时钟有差异），仍可尝试")
            }
        } else {
            None
        }
    }
}

fn valid_addr(addr: &str) -> bool {
    match addr.rsplit_once(':') {
        Some((host, port)) => {
            !host.trim().is_empty() && port.parse::<u16>().map(|p| p > 0).unwrap_or(false)
        }
        None => false,
    }
}

fn url_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b':' => {
                out.push(*b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn url_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

/// 把二维码渲染成 SVG（管理页用）。Android 侧另有 zxing 出自己的位图，
/// 这里只服务网页，所以不需要 image 特性，二进制也就不会变大。
pub fn qr_svg(text: &str, size: u32) -> io::Result<String> {
    let code = qrcode::QrCode::new(text.as_bytes())
        .map_err(|e| io::Error::other(format!("二维码生成失败: {e}")))?;
    let svg = code
        .render::<qrcode::render::svg::Color>()
        .min_dimensions(size, size)
        .dark_color(qrcode::render::svg::Color("#1d1c1a"))
        .light_color(qrcode::render::svg::Color("#ffffff"))
        .build();
    Ok(svg)
}

/// 猜本机在局域网里的地址。用 UDP connect 让内核选出口网卡——
/// 这个过程不发包，只查路由表，所以断网但连着 Wi-Fi 时也能拿到正确地址。
/// 取不到就返回 None，由界面提示用户手填，绝不瞎猜一个地址写进二维码。
pub fn local_lan_addr() -> Option<String> {
    for target in ["8.8.8.8:80", "223.5.5.5:80", "192.0.2.1:9"] {
        let Ok(sock) = std::net::UdpSocket::bind("0.0.0.0:0") else {
            continue;
        };
        if sock.connect(target).is_err() {
            continue;
        }
        if let Ok(local) = sock.local_addr() {
            let ip = local.ip();
            if !ip.is_loopback() && !ip.is_unspecified() {
                return Some(ip.to_string());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::DeviceInfo;
    use crate::session::{GateVerdict, TrustGate, TrustMode};
    use crate::trust::TrustStore;
    use std::sync::Arc;

    fn fp(n: u8) -> String {
        std::iter::repeat(format!("{n:02x}")).take(32).collect()
    }

    fn device() -> DeviceInfo {
        DeviceInfo {
            id: "dev-1".into(),
            name: "朋友的手机".into(),
            platform: "android".into(),
        }
    }

    #[test]
    fn token_is_issued_once_and_consumed_once() {
        let t = PairTokens::with_ttl(600);
        let a = t.issue().unwrap();
        assert_eq!(a.token.len(), PAIR_TOKEN_LEN);
        assert!(a.token.chars().all(|c| c.is_ascii_alphanumeric()));
        assert_eq!(t.consume(&a.token), ConsumeResult::Ok);
        // 一次性：同一张码配第二台设备必须失败
        assert_ne!(t.consume(&a.token), ConsumeResult::Ok);
        assert!(t.active().is_none());
    }

    #[test]
    fn regenerating_invalidates_the_previous_code() {
        let t = PairTokens::with_ttl(600);
        let old = t.issue().unwrap();
        let new = t.issue().unwrap();
        assert_eq!(t.consume(&old.token), ConsumeResult::Invalid);
        assert_eq!(t.consume(&new.token), ConsumeResult::Ok);
    }

    #[test]
    fn expired_token_is_rejected_and_cleared() {
        let t = PairTokens::with_ttl(600);
        let a = t.issue().unwrap();
        assert_eq!(t.consume_at(&a.token, a.expires + 1), ConsumeResult::Expired);
        assert!(t.active_at(a.expires + 1).is_none());
    }

    #[test]
    fn closing_dialog_does_not_revoke_but_explicit_revoke_does() {
        let t = PairTokens::with_ttl(600);
        let a = t.issue().unwrap();
        // 关对话框只是不展示，令牌仍在
        assert!(t.active().is_some());
        assert!(t.revoke());
        assert_eq!(t.consume(&a.token), ConsumeResult::None);
        assert!(!t.revoke(), "已经作废过就没有可撤销的了");
    }

    #[test]
    fn repeated_failures_are_rate_limited() {
        let t = PairTokens::with_ttl(600);
        let _ = t.issue().unwrap();
        for _ in 0..PAIR_MAX_FAILURES {
            assert_eq!(t.consume("AAAAAAAAAA"), ConsumeResult::Invalid);
        }
        assert_eq!(t.consume("AAAAAAAAAA"), ConsumeResult::RateLimited);
        // 重新生成会清零失败计数，用户可以立刻恢复使用
        let a = t.issue().unwrap();
        assert_eq!(t.consume(&a.token), ConsumeResult::Ok);
    }

    #[test]
    fn payload_roundtrip_keeps_chinese_alias() {
        let p = PairPayload {
            addr: "192.168.1.23:53318".into(),
            fp: fp(0xab),
            code: "7Q2M9XYZ0K".into(),
            exp: 1_700_000_000,
            name: "客厅 NAS".into(),
        };
        let text = p.encode();
        assert!(text.starts_with("tc://pair?v=1&"));
        assert_eq!(PairPayload::parse(&text).unwrap(), p);
    }

    #[test]
    fn payload_rejects_bad_input() {
        let p = PairPayload {
            addr: "192.168.1.23:53318".into(),
            fp: fp(0xab),
            code: "7Q2M9XYZ0K".into(),
            exp: 1_700_000_000,
            name: "x".into(),
        };
        assert!(PairPayload::parse("http://example.com").is_err());
        assert!(PairPayload::parse(&p.encode().replace("v=1", "v=2")).is_err());
        assert!(PairPayload::parse(&p.encode().replace("53318", "0")).is_err());
        assert!(PairPayload::parse(&p.encode().replace(&fp(0xab), "abc")).is_err());
        assert!(PairPayload::parse(&p.encode().replace("7Q2M9XYZ0K", "SHORT")).is_err());
    }

    #[test]
    fn local_expiry_check_has_five_minute_tolerance() {
        let p = PairPayload {
            addr: "192.168.1.23:53318".into(),
            fp: fp(0xab),
            code: "7Q2M9XYZ0K".into(),
            exp: 1_000,
            name: String::new(),
        };
        assert!(!p.locally_expired(1_100));
        assert!(p.expiry_hint(1_100).is_some());
        assert!(!p.locally_expired(1_000 + PAIR_CLOCK_SKEW_SECS));
        assert!(p.locally_expired(1_000 + PAIR_CLOCK_SKEW_SECS + 1));
    }

    fn gate_with(dir: &std::path::Path, tokens: Arc<PairTokens>) -> TrustGate {
        TrustGate::new(Arc::new(TrustStore::new(dir)), TrustMode::Closed, None)
            .with_pair_tokens(tokens)
    }

    #[test]
    fn gate_accepts_valid_token_then_remembers_device() {
        let dir = std::env::temp_dir().join(format!("tc-pair-gate-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let tokens = Arc::new(PairTokens::with_ttl(600));
        let gate = gate_with(&dir, tokens.clone());
        let a = tokens.issue().unwrap();
        let fp_hex = fp(0x11);

        match gate.evaluate(Some([0x11; 32]), &device(), Some(&a.token)).unwrap() {
            GateVerdict::Allow => {}
            GateVerdict::Deny(r) => panic!("应当放行，却被拒绝：{r}"),
        }
        // 配过一次即入信任表，第二次不带令牌也能进
        match gate.evaluate(Some([0x11; 32]), &device(), None).unwrap() {
            GateVerdict::Allow => {}
            GateVerdict::Deny(r) => panic!("已信任设备不应被拒：{r}"),
        }
        let peers = gate.store.list().unwrap();
        assert_eq!(peers.len(), 1);
        assert_eq!(peers[0].fp, fp_hex);
        assert_eq!(peers[0].added_by, "pair-qr");
    }

    #[test]
    fn gate_denies_wrong_or_expired_token() {
        let dir = std::env::temp_dir().join(format!("tc-pair-gate-bad-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let tokens = Arc::new(PairTokens::with_ttl(600));
        let gate = gate_with(&dir, tokens.clone());
        let a = tokens.issue().unwrap();

        assert!(matches!(
            gate.evaluate(Some([0x22; 32]), &device(), Some("ZZZZZZZZZZ")).unwrap(),
            GateVerdict::Deny(_)
        ));
        assert!(matches!(
            gate.evaluate(Some([0x22; 32]), &device(), Some(&a.token)).unwrap(),
            GateVerdict::Allow
        ));
        // 同一令牌已消费，第二台设备拿它来也不行
        assert!(matches!(
            gate.evaluate(Some([0x33; 32]), &device(), Some(&a.token)).unwrap(),
            GateVerdict::Deny(_)
        ));
    }

    #[test]
    fn qr_svg_renders_for_payload() {
        let svg = qr_svg(
            "tc://pair?v=1&addr=192.168.1.23:53318&fp=ab&code=7Q2M9XYZ0K&exp=1&name=x",
            200,
        )
        .unwrap();
        assert!(svg.contains("<svg"));
    }
}
