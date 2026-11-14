//! v0.2 加密层：自签证书 + TLS 1.3 + 指纹固定。
//! 不走公共 CA 链，也不做域名校验——局域网里设备 IP 会变，证书是自签的，
//! 唯一稳定的身份是「证书本身的哈希」。这与 LocalSend 的做法一致，但我们是自己实现。

use rcgen::{generate_simple_self_signed, CertifiedKey};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{ring::default_provider, CryptoProvider};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName, UnixTime};
use rustls::server::danger::{ClientCertVerified, ClientCertVerifier};
use rustls::{
    crypto, ClientConfig, DigitallySignedStruct, DistinguishedName, Error as TlsError,
    ServerConfig, SignatureScheme,
};
use std::fs;
use std::io;
use std::path::Path;
use std::sync::Arc;

const CERT_FILE: &str = "identity.cer";
const KEY_FILE: &str = "identity.key";

/// 一台设备的长期身份。证书和私钥存成 DER 二进制而不是 PEM，省掉一层解析依赖。
#[derive(Clone)]
pub struct Identity {
    pub cert_der: Vec<u8>,
    pub key_der: Vec<u8>,
    pub fingerprint: [u8; 32],
}

impl Identity {
    pub fn load_or_create(dir: &Path) -> io::Result<Self> {
        fs::create_dir_all(dir)?;
        let cert_path = dir.join(CERT_FILE);
        let key_path = dir.join(KEY_FILE);
        let (cert_der, key_der) = if cert_path.exists() && key_path.exists() {
            (fs::read(&cert_path)?, fs::read(&key_path)?)
        } else {
            let CertifiedKey { cert, key_pair } =
                generate_simple_self_signed(vec!["localhost".into()])
                    .map_err(|e| io::Error::other(e.to_string()))?;
            let c = cert.der().to_vec();
            let k = key_pair.serialize_der();
            fs::write(&cert_path, &c)?;
            fs::write(&key_path, &k)?;
            (c, k)
        };
        Ok(Self {
            fingerprint: *blake3::hash(&cert_der).as_bytes(),
            cert_der,
            key_der,
        })
    }

    pub fn fingerprint_hex(&self) -> String {
        hex(&self.fingerprint)
    }

    pub fn parse_fingerprint(text: &str) -> io::Result<[u8; 32]> {
        let s = text.trim().to_ascii_lowercase();
        if s.len() != 64 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "fingerprint must be 64 hex characters",
            ));
        }
        let mut out = [0u8; 32];
        for i in 0..32 {
            out[i] = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e.to_string()))?;
        }
        Ok(out)
    }
}

fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

pub fn server_config(id: &Identity) -> io::Result<Arc<ServerConfig>> {
    let provider = Arc::new(default_provider());
    let cert = CertificateDer::from(id.cert_der.clone());
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(id.key_der.clone()));
    let config = ServerConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .map_err(|e| io::Error::other(e.to_string()))?
        // 索取客户端证书，但不在握手层拒绝：信任与否由会话层判定，
        // 这样能回一句「untrusted <指纹>」，而不是让对端只看到 TLS alert。
        .with_client_cert_verifier(Arc::new(OfferAnyClientCert { provider }))
        .with_single_cert(vec![cert], key)
        .map_err(|e| io::Error::other(e.to_string()))?;
    Ok(Arc::new(config))
}

pub fn client_config(expected: [u8; 32]) -> io::Result<Arc<ClientConfig>> {
    build_client_config(expected, None)
}

/// 发送端带自己的长期证书：接收端的信任门禁要靠它拿到「发送方是谁」的证据。
pub fn client_config_with_identity(
    expected: [u8; 32],
    id: &Identity,
) -> io::Result<Arc<ClientConfig>> {
    build_client_config(expected, Some(id))
}

fn build_client_config(
    expected: [u8; 32],
    id: Option<&Identity>,
) -> io::Result<Arc<ClientConfig>> {
    let provider = Arc::new(default_provider());
    let verifier = PinVerifier {
        expected,
        provider: provider.clone(),
    };
    let builder = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| io::Error::other(e.to_string()))?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(verifier));
    let config = match id {
        Some(id) => builder
            .with_client_auth_cert(
                vec![CertificateDer::from(id.cert_der.clone())],
                PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(id.key_der.clone())),
            )
            .map_err(|e| io::Error::other(e.to_string()))?,
        // 旧路径保留：只用于明确选择不带证书的排障场景
        None => builder.with_no_client_auth(),
    };
    Ok(Arc::new(config))
}

/// 客户端证书一律收下，只把「是谁」交给上层。
/// `client_auth_mandatory = false`：旧客户端不带证书也能完成握手，
/// 随后由版本检查给出可读提示，而不是在这里丢一句 TLS error。
#[derive(Debug)]
struct OfferAnyClientCert {
    provider: Arc<CryptoProvider>,
}

impl ClientCertVerifier for OfferAnyClientCert {
    fn offer_client_auth(&self) -> bool {
        true
    }

    fn client_auth_mandatory(&self) -> bool {
        false
    }

    fn root_hint_subjects(&self) -> &[DistinguishedName] {
        // 我们不按 CA 链信任任何证书，给空提示让客户端「有证书就出示」
        &[]
    }

    fn verify_client_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _now: UnixTime,
    ) -> Result<ClientCertVerified, TlsError> {
        Ok(ClientCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, TlsError> {
        crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, TlsError> {
        crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

/// 只认「证书 DER 的 BLAKE3 等于期望指纹」，不看 CA、不看 SAN、不看有效期。
/// 局域网里 IP 会变、证书是自签的，强行做域名校验只会让每次换网络都连不上。
/// 签名验证仍然委托给 ring，所以中间人即便伪造同样长度的哈希也过不了握手。
#[derive(Debug)]
struct PinVerifier {
    expected: [u8; 32],
    provider: Arc<CryptoProvider>,
}

impl ServerCertVerifier for PinVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, TlsError> {
        if !intermediates.is_empty() || *blake3::hash(end_entity.as_ref()).as_bytes() != self.expected
        {
            return Err(TlsError::General("certificate fingerprint mismatch".into()));
        }
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, TlsError> {
        crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, TlsError> {
        crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_roundtrip() {
        let id = Identity {
            cert_der: vec![1, 2, 3],
            key_der: vec![],
            fingerprint: [7u8; 32],
        };
        let text = id.fingerprint_hex();
        assert_eq!(Identity::parse_fingerprint(&text).unwrap(), [7u8; 32]);
        assert!(Identity::parse_fingerprint("abc").is_err());
    }
}
