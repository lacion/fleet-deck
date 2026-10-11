//! HTTPS for the board (M2 round 26; PLAN §7 "HTTPS + WS", D13, DL-054).
//! The deck terminates TLS itself: no reverse proxy. The certificate is
//!
//! 1. the owner's `board.crt` + `board.key` (PEM) in the deck directory
//!    when both exist (for example the output of `tailscale cert`), else
//! 2. a self-signed certificate that the deck makes and keeps in
//!    `board-selfsigned.{crt,key}` (key 0600). It names every board
//!    address, `localhost` and the tailnet host name; when that name list
//!    changes, the deck makes a new one. `fleetdeck open` prints its
//!    SHA-256 fingerprint so that the owner can check the browser warning.

use std::fs;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::sync::Arc;

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{CryptoProvider, verify_tls12_signature, verify_tls13_signature};
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime};
use rustls::{ClientConfig, DigitallySignedStruct, ServerConfig, SignatureScheme};

/// The owner's certificate chain (PEM).
pub const OWNER_CERT: &str = "board.crt";
/// The owner's private key (PEM).
pub const OWNER_KEY: &str = "board.key";
const SELF_CERT: &str = "board-selfsigned.crt";
const SELF_KEY: &str = "board-selfsigned.key";
const SELF_NAMES: &str = "board-selfsigned.names";

/// Why the board TLS configuration cannot be made.
#[derive(Debug, thiserror::Error)]
pub enum TlsError {
    #[error("board certificate file `{file}`: {reason}")]
    File { file: String, reason: String },
    #[error("cannot make the board certificate: {0}")]
    Generate(#[from] rcgen::Error),
    #[error("board TLS: {0}")]
    Rustls(#[from] rustls::Error),
    #[error("board certificate: {0}")]
    Io(#[from] std::io::Error),
}

/// The board's TLS configuration and the certificate it presents.
#[derive(Debug, Clone)]
pub struct BoardTls {
    pub config: Arc<ServerConfig>,
    /// The leaf certificate (DER).
    pub cert: CertificateDer<'static>,
    /// SHA-256 of the leaf certificate, lower-case hex with `:`.
    pub sha256: String,
    pub self_signed: bool,
}

fn provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

/// The fingerprint text of a certificate.
#[must_use]
pub fn fingerprint(cert: &CertificateDer<'_>) -> String {
    let d = ring::digest::digest(&ring::digest::SHA256, cert.as_ref());
    d.as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(":")
}

impl BoardTls {
    /// A TLS configuration from a chain and its key (HTTP/1.1 only).
    ///
    /// # Errors
    /// The key does not match or is not supported.
    pub fn new(
        chain: Vec<CertificateDer<'static>>,
        key: PrivateKeyDer<'static>,
        self_signed: bool,
    ) -> Result<Self, TlsError> {
        let cert = chain.first().cloned().ok_or_else(|| TlsError::File {
            file: OWNER_CERT.to_owned(),
            reason: "no certificate in the file".to_owned(),
        })?;
        let mut config = ServerConfig::builder_with_provider(provider())
            .with_safe_default_protocol_versions()?
            .with_no_client_auth()
            .with_single_cert(chain, key)?;
        config.alpn_protocols = vec![b"http/1.1".to_vec()];
        Ok(Self {
            sha256: fingerprint(&cert),
            config: Arc::new(config),
            cert,
            self_signed,
        })
    }

    /// A new self-signed certificate for `names` (IP addresses and DNS
    /// names), in memory.
    ///
    /// # Errors
    /// [`TlsError::Generate`].
    pub fn generate(names: &[String]) -> Result<(String, String), TlsError> {
        let mut params = rcgen::CertificateParams::new(names.to_vec())?;
        params
            .distinguished_name
            .push(rcgen::DnType::CommonName, "Fleet Deck board");
        let key = rcgen::KeyPair::generate()?;
        let cert = params.self_signed(&key)?;
        Ok((cert.pem(), key.serialize_pem()))
    }

    /// A configuration from PEM text.
    ///
    /// # Errors
    /// The PEM text holds no certificate or no key.
    pub fn from_pem(cert_pem: &str, key_pem: &str, self_signed: bool) -> Result<Self, TlsError> {
        let bad = |file: &str, reason: String| TlsError::File {
            file: file.to_owned(),
            reason,
        };
        let chain = CertificateDer::pem_slice_iter(cert_pem.as_bytes())
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| bad(OWNER_CERT, e.to_string()))?;
        let key = PrivateKeyDer::from_pem_slice(key_pem.as_bytes())
            .map_err(|e| bad(OWNER_KEY, e.to_string()))?;
        Self::new(chain, key, self_signed)
    }

    /// The owner's certificate, or the kept self-signed one for `names`
    /// (made again when the names change).
    ///
    /// # Errors
    /// [`TlsError`].
    pub fn load_or_create(dir: &Path, names: &[String]) -> Result<Self, TlsError> {
        let (cert, key) = (dir.join(OWNER_CERT), dir.join(OWNER_KEY));
        if cert.exists() && key.exists() {
            return Self::from_pem(&fs::read_to_string(cert)?, &read_private(&key)?, false);
        }
        let list = names.join("\n");
        let (cert, key, kept) = (
            dir.join(SELF_CERT),
            dir.join(SELF_KEY),
            dir.join(SELF_NAMES),
        );
        if fs::read_to_string(&kept).ok().as_deref() == Some(list.as_str())
            && let (Ok(c), Ok(k)) = (fs::read_to_string(&cert), read_private(&key))
        {
            return Self::from_pem(&c, &k, true);
        }
        let (c, k) = Self::generate(names)?;
        write_file(&key, &k, 0o600)?;
        write_file(&cert, &c, 0o644)?;
        write_file(&kept, &list, 0o644)?;
        Self::from_pem(&c, &k, true)
    }
}

/// A key file that other users can read is refused.
fn read_private(path: &Path) -> Result<String, TlsError> {
    use std::os::unix::fs::MetadataExt;
    let meta = fs::metadata(path)?;
    if meta.mode() & 0o077 != 0 {
        return Err(TlsError::File {
            file: path.display().to_string(),
            reason: format!(
                "mode {:o}: a key file must not be readable by others (chmod 600)",
                meta.mode() & 0o777
            ),
        });
    }
    Ok(fs::read_to_string(path)?)
}

fn write_file(path: &Path, text: &str, mode: u32) -> Result<(), TlsError> {
    let tmp = path.with_extension("tmp");
    let _ = fs::remove_file(&tmp);
    let mut f = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .open(&tmp)?;
    f.write_all(text.as_bytes())?;
    f.sync_all()?;
    fs::rename(&tmp, path)?;
    Ok(())
}

/// A client configuration that accepts exactly `cert` (CLI and tests).
#[must_use]
pub fn pinned_client(cert: CertificateDer<'static>) -> Arc<ClientConfig> {
    let provider = provider();
    let verifier = Arc::new(Pinned {
        cert,
        provider: Arc::clone(&provider),
    });
    let mut config = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map(|b| {
            b.dangerous()
                .with_custom_certificate_verifier(verifier)
                .with_no_client_auth()
        })
        .unwrap_or_else(|_| unreachable_config());
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    Arc::new(config)
}

// The ring provider supports the safe default versions; this branch is
// never taken. It returns a configuration that trusts nothing.
fn unreachable_config() -> ClientConfig {
    ClientConfig::builder()
        .with_root_certificates(rustls::RootCertStore::empty())
        .with_no_client_auth()
}

#[derive(Debug)]
struct Pinned {
    cert: CertificateDer<'static>,
    provider: Arc<CryptoProvider>,
}

impl ServerCertVerifier for Pinned {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        if end_entity.as_ref() == self.cert.as_ref() {
            Ok(ServerCertVerified::assertion())
        } else {
            Err(rustls::Error::General(
                "the board certificate does not match the pinned one".to_owned(),
            ))
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(
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
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(
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
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn names() -> Vec<String> {
        vec!["127.0.0.1".to_owned(), "localhost".to_owned()]
    }

    #[test]
    fn the_self_signed_certificate_is_kept_until_the_names_change() {
        let dir = tempfile::tempdir().unwrap();
        let a = BoardTls::load_or_create(dir.path(), &names()).unwrap();
        assert!(a.self_signed);
        let mode = {
            use std::os::unix::fs::MetadataExt;
            fs::metadata(dir.path().join(SELF_KEY)).unwrap().mode() & 0o777
        };
        assert_eq!(mode, 0o600);
        let b = BoardTls::load_or_create(dir.path(), &names()).unwrap();
        assert_eq!(a.sha256, b.sha256);
        let mut more = names();
        more.push("deck.tail1234.ts.net".to_owned());
        let c = BoardTls::load_or_create(dir.path(), &more).unwrap();
        assert_ne!(a.sha256, c.sha256);
    }

    #[test]
    fn the_owner_certificate_wins_and_a_readable_key_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let (c, k) = BoardTls::generate(&names()).unwrap();
        fs::write(dir.path().join(OWNER_CERT), &c).unwrap();
        write_file(&dir.path().join(OWNER_KEY), &k, 0o644).unwrap();
        let e = BoardTls::load_or_create(dir.path(), &names()).unwrap_err();
        assert!(e.to_string().contains("chmod 600"), "{e}");
        write_file(&dir.path().join(OWNER_KEY), &k, 0o600).unwrap();
        let t = BoardTls::load_or_create(dir.path(), &names()).unwrap();
        assert!(!t.self_signed);
        assert_eq!(t.sha256, BoardTls::from_pem(&c, &k, false).unwrap().sha256);
    }
}
