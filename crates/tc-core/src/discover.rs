//! v0.2 局域网发现：UDP 组播 + 定向广播 + 本机回环，三条腿都不依赖 mDNS/avahi。
//! mDNS 需要额外依赖和 5353 端口，留到 v0.3；手机与 NAS 上「能不能被发现」先用最朴素的办法解决。

use crate::manifest::DeviceInfo;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io;
use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

pub const DISCOVERY_GROUP: Ipv4Addr = Ipv4Addr::new(224, 0, 0, 167);
pub const DISCOVERY_PORT: u16 = 53318;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Announcement {
    pub device: DeviceInfo,
    /// 证书指纹，收到后用于 TLS 指纹固定
    pub fingerprint: String,
    /// TCP 传输端口（不是本 UDP 端口）
    pub port: u16,
    pub version: String,
    pub ts: i64,
}

#[derive(Debug, Clone)]
pub struct Peer {
    pub addr: String,
    pub fingerprint: String,
    pub name: String,
    pub platform: String,
}

pub fn encode(a: &Announcement) -> io::Result<Vec<u8>> {
    serde_json::to_vec(a).map_err(|e| io::Error::other(e.to_string()))
}

pub fn decode(bytes: &[u8]) -> io::Result<Announcement> {
    serde_json::from_slice(bytes).map_err(|e| io::Error::other(e.to_string()))
}

/// 发 3 个包：组播、广播、本机回环。
/// 回环那条专门给「同一台机器跑两个进程」的验证场景用，正式部署时它只是多一个无害副本。
pub fn announce(a: &Announcement) -> io::Result<()> {
    announce_to(a, DISCOVERY_PORT)
}

/// 指定端口发送公告：正式路径固定 53318，测试用独立端口验证「两个监听共存」，
/// 免得受本机正在跑的旧版守护进程影响。
fn announce_to(a: &Announcement, port: u16) -> io::Result<()> {
    let payload = encode(a)?;
    let sock = UdpSocket::bind("0.0.0.0:0")?;
    sock.set_broadcast(true)?;
    sock.set_multicast_loop_v4(true)?;
    let _ = sock.send_to(&payload, (DISCOVERY_GROUP, port));
    let _ = sock.send_to(&payload, (Ipv4Addr::BROADCAST, port));
    let _ = sock.send_to(&payload, (Ipv4Addr::LOCALHOST, port));
    Ok(())
}

pub fn announcement_for(
    device: DeviceInfo,
    fingerprint: String,
    port: u16,
    version: &str,
) -> Announcement {
    Announcement {
        device,
        fingerprint,
        port,
        version: version.into(),
        ts: crate::queue::now_secs(),
    }
}

/// 以「端口可复用」的方式绑定 IPv4 UDP。标准库没有暴露 SO_REUSEADDR / SO_REUSEPORT，
/// 所以这里用最小 FFI 自己建 socket、先设选项再 bind。
/// 目标很明确：同一台机器上可以同时有多个发现监听（守护进程 + CLI + 测试）。
#[cfg(windows)]
fn bind_shared(port: u16) -> io::Result<UdpSocket> {
    mod win {
        use std::io;
        use std::net::UdpSocket;
        use std::os::windows::io::FromRawSocket;

        const AF_INET: i32 = 2;
        const SOCK_DGRAM: i32 = 2;
        const IPPROTO_UDP: i32 = 17;
        const SOL_SOCKET: i32 = 0xffff;
        const SO_REUSEADDR: i32 = 0x0004;
        /// Windows 的 INVALID_SOCKET 是 usize::MAX
        const INVALID_SOCKET: usize = usize::MAX;

        #[repr(C)]
        struct SockAddrIn {
            family: u16,
            port: u16,
            addr: u32,
            zero: [u8; 8],
        }

        #[link(name = "ws2_32")]
        extern "system" {
            fn socket(af: i32, ty: i32, proto: i32) -> usize;
            fn setsockopt(s: usize, level: i32, name: i32, val: *const u8, len: i32) -> i32;
            fn bind(s: usize, name: *const u8, len: i32) -> i32;
            fn closesocket(s: usize) -> i32;
        }

        pub fn open(port: u16) -> io::Result<UdpSocket> {
            unsafe {
                let s = socket(AF_INET, SOCK_DGRAM, IPPROTO_UDP);
                if s == INVALID_SOCKET {
                    return Err(io::Error::last_os_error());
                }
                let on: i32 = 1;
                let rc = setsockopt(
                    s,
                    SOL_SOCKET,
                    SO_REUSEADDR,
                    &on as *const i32 as *const u8,
                    std::mem::size_of::<i32>() as i32,
                );
                if rc != 0 {
                    let e = io::Error::last_os_error();
                    closesocket(s);
                    return Err(e);
                }
                let sa = SockAddrIn {
                    family: AF_INET as u16,
                    port: port.to_be(),
                    addr: 0,
                    zero: [0; 8],
                };
                let rc = bind(
                    s,
                    &sa as *const SockAddrIn as *const u8,
                    std::mem::size_of::<SockAddrIn>() as i32,
                );
                if rc != 0 {
                    let e = io::Error::last_os_error();
                    closesocket(s);
                    return Err(e);
                }
                Ok(UdpSocket::from_raw_socket(s as u64))
            }
        }
    }
    // 先用标准库建一个临时 socket，保证 Winsock 已初始化：
    // 没初始化时直接调 ws2_32 的 socket 会拿到 WSANOTINITIALISED。
    let _ = UdpSocket::bind(("127.0.0.1", 0));
    win::open(port)
}

#[cfg(unix)]
fn bind_shared(port: u16) -> io::Result<UdpSocket> {
    mod unix {
        use std::io;
        use std::net::UdpSocket;
        use std::os::unix::io::FromRawFd;

        const AF_INET: i32 = 2;
        const SOCK_DGRAM: i32 = 2;
        const SOL_SOCKET: i32 = 1;
        const SO_REUSEADDR: i32 = 2;
        /// Linux / Android 上 SO_REUSEPORT 是 15；只有它能让多个监听都收到组播
        const SO_REUSEPORT: i32 = 15;

        #[repr(C)]
        struct SockAddrIn {
            family: u16,
            port: u16,
            addr: u32,
            zero: [u8; 8],
        }

        extern "C" {
            fn socket(af: i32, ty: i32, proto: i32) -> i32;
            fn setsockopt(fd: i32, level: i32, name: i32, val: *const u8, len: u32) -> i32;
            fn bind(fd: i32, name: *const u8, len: u32) -> i32;
            fn close(fd: i32) -> i32;
        }

        fn set_opt(fd: i32, name: i32) -> io::Result<()> {
            let on: i32 = 1;
            let rc = unsafe {
                setsockopt(
                    fd,
                    SOL_SOCKET,
                    name,
                    &on as *const i32 as *const u8,
                    std::mem::size_of::<i32>() as u32,
                )
            };
            if rc == 0 {
                Ok(())
            } else {
                Err(io::Error::last_os_error())
            }
        }

        pub fn open(port: u16) -> io::Result<UdpSocket> {
            unsafe {
                let fd = socket(AF_INET, SOCK_DGRAM, 0);
                if fd < 0 {
                    return Err(io::Error::last_os_error());
                }
                // 两项都要设：ADDR 允许同端口绑定，REUSEPORT 让组播投递给所有监听者。
                // REUSEPORT 在个别内核/容器里不可用，设不上不算失败（ADDR 已能避免 AddrInUse）。
                let addr = set_opt(fd, SO_REUSEADDR);
                let _ = set_opt(fd, SO_REUSEPORT);
                if let Err(e) = addr {
                    close(fd);
                    return Err(e);
                }
                let sa = SockAddrIn {
                    family: AF_INET as u16,
                    port: port.to_be(),
                    addr: 0,
                    zero: [0; 8],
                };
                let rc = bind(
                    fd,
                    &sa as *const SockAddrIn as *const u8,
                    std::mem::size_of::<SockAddrIn>() as u32,
                );
                if rc != 0 {
                    let e = io::Error::last_os_error();
                    close(fd);
                    return Err(e);
                }
                Ok(UdpSocket::from_raw_fd(fd))
            }
        }
    }
    unix::open(port)
}

/// 其余平台（本项目未发布）退回普通绑定，保持可编译。
#[cfg(not(any(windows, unix)))]
fn bind_shared(port: u16) -> io::Result<UdpSocket> {
    UdpSocket::bind(("0.0.0.0", port))
}

/// 收公告直到超时；按「IP + 指纹」去重，并过滤掉自己发的包。
pub fn listen(timeout: Duration, self_fingerprint: &str) -> io::Result<Vec<Peer>> {
    let mut seen: HashMap<String, Peer> = HashMap::new();
    // 复用同一套绑定与解码逻辑，避免两条路径对「什么算一台设备」判断不一致
    listen_each(timeout, self_fingerprint, |p| {
        seen.insert(p.addr.clone(), p.clone());
    })?;
    Ok(seen.into_values().collect())
}

/// 边收边回调：每收到一条有效公告就交给调用方处理，供守护进程即时更新设备列表。
/// 常驻端不能用 listen 的「窗口结束才返回」——那样新设备最多要等一整轮窗口才出现在管理页。
pub fn listen_each(
    timeout: Duration,
    self_fingerprint: &str,
    mut on_peer: impl FnMut(&Peer),
) -> io::Result<()> {
    listen_on_each(DISCOVERY_PORT, timeout, self_fingerprint, &mut on_peer)
}

/// 指定端口监听，仅供测试：正式路径固定 53318，测试用独立端口避免受本机旧版守护进程影响
/// （Windows 上旧监听没设 SO_REUSEADDR 时，后来者连「共享绑定」都会被拒）。
#[cfg(test)]
fn listen_on(port: u16, timeout: Duration, self_fingerprint: &str) -> io::Result<Vec<Peer>> {
    let mut seen: HashMap<String, Peer> = HashMap::new();
    listen_on_each(port, timeout, self_fingerprint, &mut |p: &Peer| {
        seen.insert(p.addr.clone(), p.clone());
    })?;
    Ok(seen.into_values().collect())
}

fn listen_on_each(
    port: u16,
    timeout: Duration,
    self_fingerprint: &str,
    on_peer: &mut dyn FnMut(&Peer),
) -> io::Result<()> {
    // v0.12：发现监听必须能和常驻进程共用一个端口。桌面守护进程一直在听 53318，
    // 命令行 `tc discover`、e2e 用例再听一次就会 AddrInUse（v0.11 之前跑门禁得先退托盘）。
    // 先按「端口可复用」绑，失败再退回普通绑定，行为至少不比旧版更差。
    let sock = bind_shared(port).or_else(|first| {
        UdpSocket::bind(("0.0.0.0", port)).map_err(|second| {
            io::Error::new(
                second.kind(),
                format!("发现端口 UDP {port} 无法监听：{first}；普通绑定也失败：{second}"),
            )
        })
    })?;
    sock.set_read_timeout(Some(Duration::from_millis(200)))?;
    // 组播加入失败不致命：广播和回环两条腿仍然可用
    let _ = sock.join_multicast_v4(&DISCOVERY_GROUP, &Ipv4Addr::UNSPECIFIED);

    let deadline = std::time::Instant::now() + timeout;
    let mut buf = [0u8; 2048];
    while std::time::Instant::now() < deadline {
        match sock.recv_from(&mut buf) {
            Ok((n, from)) => {
                if let Ok(a) = decode(&buf[..n]) {
                    if a.fingerprint == self_fingerprint {
                        continue; // 不把自己列进设备列表
                    }
                    let ip = match from {
                        SocketAddr::V4(v4) => *v4.ip(),
                        SocketAddr::V6(_) => continue,
                    };
                    let addr = format!("{ip}:{}", a.port);
                    on_peer(&Peer {
                        addr,
                        fingerprint: a.fingerprint.clone(),
                        name: a.device.name.clone(),
                        platform: a.device.platform.clone(),
                    });
                }
            }
            Err(e)
                if e.kind() == io::ErrorKind::WouldBlock
                    || e.kind() == io::ErrorKind::TimedOut =>
            {
                continue;
            }
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// 常驻端用：一直广播，直到 stop 置位。
/// 分片睡眠是为了让停止能在一个分片内生效，而不是干等满一个广播间隔——
/// 手机上这直接关系到用户关掉接收后还能不能马上安静下来。
pub fn announce_loop(a: Announcement, interval: Duration, stop: &AtomicBool) {
    let slice = Duration::from_millis(200);
    while !stop.load(Ordering::Relaxed) {
        let _ = announce(&a);
        let mut waited = Duration::ZERO;
        while waited < interval && !stop.load(Ordering::Relaxed) {
            std::thread::sleep(slice);
            waited += slice;
        }
    }
}

/// 进程内一直广播（桌面 CLI 用，进程退出即停）。
pub fn announce_forever(a: Announcement, interval: Duration) {
    let never = AtomicBool::new(false);
    announce_loop(a, interval, &never);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn announcement_roundtrip() {
        let a = Announcement {
            device: DeviceInfo {
                id: "x".into(),
                name: "我的电脑".into(),
                platform: "windows".into(),
            },
            fingerprint: "ab".repeat(32),
            port: 53318,
            version: "0.2.0".into(),
            ts: 1,
        };
        let bytes = encode(&a).unwrap();
        let back = decode(&bytes).unwrap();
        assert_eq!(back.fingerprint, a.fingerprint);
        assert_eq!(back.port, 53318);
    }

    /// 停止要快：手机上用户关掉接收后，不能还在继续广播。
    #[test]
    fn announce_loop_stops_promptly() {
        let a = Announcement {
            device: DeviceInfo {
                id: "x".into(),
                name: "手机".into(),
                platform: "android".into(),
            },
            fingerprint: "cd".repeat(32),
            port: 53318,
            version: "0.4.0".into(),
            ts: 1,
        };
        let stop = std::sync::Arc::new(AtomicBool::new(false));
        let stop2 = stop.clone();
        // 广播间隔设成 30 秒，若停止逻辑失效，这个 join 会拖到 30 秒，测试会明显变慢
        let h = std::thread::spawn(move || {
            announce_loop(a, Duration::from_secs(30), &stop2);
        });
        std::thread::sleep(Duration::from_millis(300));
        stop.store(true, Ordering::Relaxed);
        let t0 = std::time::Instant::now();
        h.join().unwrap();
        assert!(t0.elapsed() < Duration::from_secs(2), "停止太慢：{:?}", t0.elapsed());
    }

    /// v0.12：两个监听必须能同时绑同一个发现端口。
    /// 之前守护进程一直在听，`tc discover` 与 e2e 用例再听一次就 AddrInUse，
    /// 于是跑门禁前得先退托盘——这条用例锁住「共存」这个结论。
    #[test]
    fn two_listeners_can_share_the_discovery_port() {
        // 用一次性端口而不是 53318：本机可能还跑着旧版守护进程（没设 SO_REUSEADDR），
        // Windows 会直接拒绝任何后来者，测出来的就不是本次改动的问题了。
        let probe = UdpSocket::bind(("0.0.0.0", 0)).unwrap();
        let port = probe.local_addr().unwrap().port();
        drop(probe);

        let a = Announcement {
            device: DeviceInfo {
                id: "x".into(),
                name: "共享测试".into(),
                platform: "windows".into(),
            },
            fingerprint: "ef".repeat(32),
            port: 53318,
            version: "0.12.0".into(),
            ts: 1,
        };
        let h1 = std::thread::spawn(move || listen_on(port, Duration::from_millis(2500), ""));
        let h2 = std::thread::spawn(move || listen_on(port, Duration::from_millis(2500), ""));
        // 等两个监听都绑好再发：先发的话后绑的那个收不到
        std::thread::sleep(Duration::from_millis(400));
        for _ in 0..3 {
            announce_to(&a, port).unwrap();
            std::thread::sleep(Duration::from_millis(200));
        }
        let p1 = h1.join().unwrap().expect("第一个监听不该失败");
        let p2 = h2.join().unwrap().expect("第二个监听不该失败");
        // 组播在个别网卡组合下可能只投递给其中一个，所以断言「至少一边收到」，
        // 真正的回归点是两次 listen 都成功返回而不是 AddrInUse。
        assert!(
            !p1.is_empty() || !p2.is_empty(),
            "两个监听都成功绑定后，至少应有一边收到公告"
        );
    }
}
