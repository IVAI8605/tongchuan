//! 连接建立抽象：明文 TCP 与 TLS 共用同一套会话逻辑。
//! 会话层只认「一条可读可写的流」，换加密方式时不用改帧、清单和续传代码。

use rustls::{ClientConfig, ClientConnection, ServerConfig, ServerConnection, StreamOwned};
use std::io::{self, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;

/// `Send`：并发接收时每条连接会被交给独立线程处理，流必须能跨线程移动。
/// `peer_cert_fingerprint`：TLS 接收端握手完成后才能取到客户端证书，
/// 信任门禁要在读完 HELLO 之后调用它，因此不能塞进 Accepter 的返回值里。
pub trait Stream: Read + Write + Send {
    fn peer_cert_fingerprint(&self) -> Option<[u8; 32]> {
        None
    }
}

pub type BoxedStream = Box<dyn Stream>;

pub trait Opener {
    fn open(&self) -> io::Result<BoxedStream>;
}

/// `Send + Sync`：并发 hub 用 Arc 在多个会话线程间共享同一个监听器。
pub trait Accepter: Send + Sync {
    fn accept(&self) -> io::Result<BoxedStream>;

    /// 同时给出对端地址（形如 `203.0.113.7:51234`）。
    /// 地址只来自 TCP 层、不进线上协议：给历史记录标明「谁发来的」用，
    /// 默认返回 None，内存接入或测试接入不必关心它。
    fn accept_with_peer(&self) -> io::Result<(BoxedStream, Option<String>)> {
        Ok((self.accept()?, None))
    }
}

pub struct PlainStream(TcpStream);

impl Read for PlainStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.0.read(buf)
    }
}

impl Write for PlainStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}

impl Stream for PlainStream {}

pub struct TlsClientStream {
    inner: StreamOwned<ClientConnection, TcpStream>,
}

impl Read for TlsClientStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.inner.read(buf)
    }
}

impl Write for TlsClientStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.inner.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

impl Stream for TlsClientStream {}

pub struct TlsServerStream {
    inner: StreamOwned<ServerConnection, TcpStream>,
}

impl Read for TlsServerStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.inner.read(buf)
    }
}

impl Write for TlsServerStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.inner.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

impl Stream for TlsServerStream {
    /// 握手在第一次 read/write 时完成；会话层读完 HELLO 再取，此时证书已经就位。
    /// 旧客户端不带证书时返回 None，由上层给出「无证书/疑似旧版」的明确提示。
    fn peer_cert_fingerprint(&self) -> Option<[u8; 32]> {
        self.inner
            .conn
            .peer_certificates()
            .and_then(|certs| certs.first())
            .map(|cert| *blake3::hash(cert.as_ref()).as_bytes())
    }
}

pub struct PlainOpener {
    pub addr: String,
}

impl Opener for PlainOpener {
    fn open(&self) -> io::Result<BoxedStream> {
        Ok(Box::new(PlainStream(TcpStream::connect(&self.addr)?)))
    }
}

pub struct TlsOpener {
    pub addr: String,
    pub config: Arc<ClientConfig>,
}

impl Opener for TlsOpener {
    fn open(&self) -> io::Result<BoxedStream> {
        let tcp = TcpStream::connect(&self.addr)?;
        // 自定义验证器不检查域名，这里给个固定名字只是为了满足 rustls 的 API 要求
        let name = rustls::pki_types::ServerName::try_from("localhost")
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e.to_string()))?
            .to_owned();
        let conn = ClientConnection::new(self.config.clone(), name)
            .map_err(|e| io::Error::other(e.to_string()))?;
        Ok(Box::new(TlsClientStream {
            inner: StreamOwned::new(conn, tcp),
        }))
    }
}

pub struct PlainAccepter {
    pub listener: TcpListener,
}

impl Accepter for PlainAccepter {
    fn accept(&self) -> io::Result<BoxedStream> {
        let (tcp, _) = self.listener.accept()?;
        Ok(Box::new(PlainStream(tcp)))
    }

    fn accept_with_peer(&self) -> io::Result<(BoxedStream, Option<String>)> {
        let (tcp, peer) = self.listener.accept()?;
        Ok((Box::new(PlainStream(tcp)), Some(peer.to_string())))
    }
}

pub struct TlsAccepter {
    pub listener: TcpListener,
    pub config: Arc<ServerConfig>,
}

impl Accepter for TlsAccepter {
    fn accept(&self) -> io::Result<BoxedStream> {
        let (tcp, _) = self.listener.accept()?;
        let conn = ServerConnection::new(self.config.clone())
            .map_err(|e| io::Error::other(e.to_string()))?;
        Ok(Box::new(TlsServerStream {
            inner: StreamOwned::new(conn, tcp),
        }))
    }

    fn accept_with_peer(&self) -> io::Result<(BoxedStream, Option<String>)> {
        // 地址要在包装成 StreamOwned 之前取：之后就拿不到底层 TcpStream 了
        let (tcp, peer) = self.listener.accept()?;
        let conn = ServerConnection::new(self.config.clone())
            .map_err(|e| io::Error::other(e.to_string()))?;
        Ok((
            Box::new(TlsServerStream {
                inner: StreamOwned::new(conn, tcp),
            }),
            Some(peer.to_string()),
        ))
    }
}
