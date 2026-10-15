//! The trusted board certificate from Tailscale (DL-190, certificate design).
//!
//! - [`parse_pem`] and [`check`] decide if the output of `tailscale cert` is
//!   usable. The path is checked offline against two embedded trust anchors
//!   (ISRG Root X1 and X2). A root in the output is never an anchor.
//! - [`write_tailscale`], [`load_tailscale`] and [`remove_tailscale`] keep the
//!   chain and the key together in one file (mode 0600).
//! - [`BoardResolver`] chooses the certificate of a new connection by SNI.
//!
//! No text here holds a byte of the private key: every error has a fixed text.

use std::fmt;
use std::fs;
use std::io;
use std::path::Path;
use std::sync::{Arc, OnceLock, RwLock};
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use fleetdeck_core::clock::{SharedClock, SystemClock};
use rustls::pki_types::pem::{SectionKind, from_buf};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName, TrustAnchor, UnixTime};
use rustls::server::{ClientHello, ResolvesServerCert};
use rustls::sign::CertifiedKey;

use super::tls::{fingerprint, provider, read_private, stage};

/// The file with the chain and the key (mode 0600).
pub const TAILSCALE_PEM: &str = "board-tailscale.pem";

/// Public Let's Encrypt roots. Every Tailscale certificate chains to one.
const ISRG_ROOT_X1: &str = include_str!("roots/isrg-root-x1.pem");
const ISRG_ROOT_X2: &str = include_str!("roots/isrg-root-x2.pem");

/// A leaf must be valid this long for a new certificate (admission).
pub const ADMISSION_REMAINING: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// The trust anchors of a path check.
pub type Anchors = Arc<[TrustAnchor<'static>]>;

/// Why the output of `tailscale cert` is refused. The texts are fixed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum CheckError {
    #[error("the certificate output is not one chain and one private key")]
    BadOutput,
    #[error("the certificate is not valid for this board")]
    BadCertificate,
}

impl CheckError {
    /// The reason code on the wire.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::BadOutput => "bad_output",
            Self::BadCertificate => "bad_certificate",
        }
    }
}

/// The two embedded Let's Encrypt roots, parsed once.
///
/// # Panics
/// Never: the embedded files are checked by a test.
#[must_use]
pub fn isrg_anchors() -> Anchors {
    static ANCHORS: OnceLock<Anchors> = OnceLock::new();
    Arc::clone(ANCHORS.get_or_init(|| {
        let anchors: Vec<TrustAnchor<'static>> = [ISRG_ROOT_X1, ISRG_ROOT_X2]
            .iter()
            .filter_map(|pem| {
                let (chain, _) = split_pem(pem.as_bytes()).ok()?;
                let der = chain.into_iter().next()?;
                let anchor = webpki::anchor_from_trusted_cert(&der).ok()?;
                Some(anchor.to_owned())
            })
            .collect();
        Arc::from(anchors)
    }))
}

/// A checked certificate: the chain, its key, and what the board needs.
pub struct TailscaleCert {
    pub chain: Vec<CertificateDer<'static>>,
    key: PrivateKeyDer<'static>,
    pub name: String,
    pub not_after_ms: i64,
    /// SHA-256 of the leaf, as [`fingerprint`] writes it.
    pub sha256: String,
}

impl fmt::Debug for TailscaleCert {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TailscaleCert")
            .field("name", &self.name)
            .field("not_after_ms", &self.not_after_ms)
            .field("sha256", &self.sha256)
            .finish_non_exhaustive()
    }
}

impl TailscaleCert {
    /// The pair for a TLS configuration.
    ///
    /// # Errors
    /// [`CheckError::BadCertificate`]: the key is not usable or does not match.
    pub fn certified_key(&self) -> Result<Arc<CertifiedKey>, CheckError> {
        certified(&self.chain, &self.key).map(Arc::new)
    }

    /// The chain, then the key, as PEM text (the content of the file).
    ///
    /// # Errors
    /// [`CheckError::BadOutput`]: the key has an unknown form.
    pub fn to_pem(&self) -> Result<Vec<u8>, CheckError> {
        let (label, der) = match &self.key {
            PrivateKeyDer::Pkcs8(k) => ("PRIVATE KEY", k.secret_pkcs8_der()),
            PrivateKeyDer::Pkcs1(k) => ("RSA PRIVATE KEY", k.secret_pkcs1_der()),
            PrivateKeyDer::Sec1(k) => ("EC PRIVATE KEY", k.secret_sec1_der()),
            _ => return Err(CheckError::BadOutput),
        };
        let mut out = String::new();
        for cert in &self.chain {
            push_block(&mut out, "CERTIFICATE", cert.as_ref());
        }
        push_block(&mut out, label, der);
        Ok(out.into_bytes())
    }
}

fn push_block(out: &mut String, label: &str, der: &[u8]) {
    out.push_str("-----BEGIN ");
    out.push_str(label);
    out.push_str("-----\n");
    let text = STANDARD.encode(der);
    for line in text.as_bytes().chunks(64) {
        out.push_str(&String::from_utf8_lossy(line));
        out.push('\n');
    }
    out.push_str("-----END ");
    out.push_str(label);
    out.push_str("-----\n");
}

fn certified(
    chain: &[CertificateDer<'static>],
    key: &PrivateKeyDer<'static>,
) -> Result<CertifiedKey, CheckError> {
    let signing = provider()
        .key_provider
        .load_private_key(key.clone_key())
        .map_err(|_| CheckError::BadCertificate)?;
    let pair = CertifiedKey::new(chain.to_vec(), signing);
    pair.keys_match().map_err(|_| CheckError::BadCertificate)?;
    Ok(pair)
}

/// Splits PEM text in its chain and its one key.
fn split_pem(
    pem: &[u8],
) -> Result<(Vec<CertificateDer<'static>>, Option<PrivateKeyDer<'static>>), CheckError> {
    let mut reader = pem;
    let mut chain = Vec::new();
    let mut key = None;
    loop {
        let section = from_buf(&mut reader).map_err(|_| CheckError::BadOutput)?;
        let Some((kind, der)) = section else {
            break;
        };
        let parsed = match kind {
            SectionKind::Certificate => {
                chain.push(CertificateDer::from(der));
                continue;
            }
            SectionKind::PrivateKey => PrivateKeyDer::Pkcs8(der.into()),
            SectionKind::RsaPrivateKey => PrivateKeyDer::Pkcs1(der.into()),
            SectionKind::EcPrivateKey => PrivateKeyDer::Sec1(der.into()),
            _ => return Err(CheckError::BadOutput),
        };
        if key.replace(parsed).is_some() {
            return Err(CheckError::BadOutput);
        }
    }
    Ok((chain, key))
}

/// The output of `tailscale cert`: one or more `CERTIFICATE` blocks and
/// exactly one private key block. Anything else is refused.
///
/// # Errors
/// [`CheckError::BadOutput`].
pub fn parse_pem(
    pem: &[u8],
) -> Result<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>), CheckError> {
    match split_pem(pem)? {
        (chain, Some(key)) if !chain.is_empty() => Ok((chain, key)),
        _ => Err(CheckError::BadOutput),
    }
}

/// Checks that `chain` is valid for `name` and that `key` is its key.
///
/// The leaf must chain, through `chain[1..]` only, to one of `anchors`. The
/// path must be valid at `now` and at `now + min_remaining`, so the leaf and
/// every certificate above it last that long.
///
/// # Errors
/// [`CheckError::BadCertificate`].
pub fn check(
    chain: Vec<CertificateDer<'static>>,
    key: PrivateKeyDer<'static>,
    name: &str,
    now: UnixTime,
    min_remaining: Duration,
    anchors: &[TrustAnchor<'_>],
) -> Result<TailscaleCert, CheckError> {
    let bad = |_| CheckError::BadCertificate;
    let leaf = chain.first().ok_or(CheckError::BadCertificate)?;
    let end = webpki::EndEntityCert::try_from(leaf).map_err(bad)?;
    let server = ServerName::try_from(name).map_err(|_| CheckError::BadCertificate)?;
    end.verify_is_valid_for_subject_name(&server).map_err(bad)?;
    let later = UnixTime::since_unix_epoch(Duration::from_secs(now.as_secs()) + min_remaining);
    for at in [now, later] {
        end.verify_for_usage(
            webpki::ALL_VERIFICATION_ALGS,
            anchors,
            &chain[1..],
            at,
            webpki::KeyUsage::server_auth(),
            None,
            None,
        )
        .map_err(bad)?;
    }
    certified(&chain, &key)?;
    let not_after_ms = not_after_ms(leaf)?;
    Ok(TailscaleCert {
        sha256: fingerprint(leaf),
        chain,
        key,
        name: name.to_ascii_lowercase(),
        not_after_ms,
    })
}

fn not_after_ms(leaf: &CertificateDer<'_>) -> Result<i64, CheckError> {
    let (_, cert) = x509_parser::parse_x509_certificate(leaf.as_ref())
        .map_err(|_| CheckError::BadCertificate)?;
    Ok(cert.validity().not_after.timestamp().saturating_mul(1000))
}

// ------------------------------------------------------------------ storage

/// Writes the chain and the key to `board-tailscale.pem` (mode 0600): a
/// temporary file is written and synced, renamed over the old file, and the
/// directory is synced. The old file stays until the rename.
///
/// # Errors
/// An I/O error, or a key of an unknown form.
pub fn write_tailscale(dir: &Path, cert: &TailscaleCert) -> io::Result<()> {
    write_tailscale_with(dir, cert, |from, to| fs::rename(from, to))
}

/// [`write_tailscale`] with its own `rename` (a test makes it fail).
fn write_tailscale_with(
    dir: &Path,
    cert: &TailscaleCert,
    rename: impl FnOnce(&Path, &Path) -> io::Result<()>,
) -> io::Result<()> {
    let pem = cert
        .to_pem()
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    let text = String::from_utf8(pem)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "not text"))?;
    let path = dir.join(TAILSCALE_PEM);
    let tmp = stage(&path, &text, 0o600).map_err(into_io)?;
    if let Err(e) = rename(&tmp, &path) {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
    fs::File::open(dir)?.sync_all()
}

fn into_io(e: super::tls::TlsError) -> io::Error {
    match e {
        super::tls::TlsError::Io(e) => e,
        other => io::Error::other(other.to_string()),
    }
}

/// What the stored file holds, for `name` at `now`.
#[derive(Debug)]
pub enum Stored {
    /// The file passes the checks of use now.
    Valid(TailscaleCert),
    /// The file was valid until its end (Unix ms) and is over now.
    Expired(i64),
    /// No file, a loose mode, bad content, another name or a bad path.
    Missing,
}

/// Reads the stored certificate and checks it for use at `now`.
#[must_use]
pub fn read_stored(dir: &Path, name: &str, now: UnixTime, anchors: &[TrustAnchor<'_>]) -> Stored {
    let Ok(text) = read_private(&dir.join(TAILSCALE_PEM)) else {
        return Stored::Missing;
    };
    let Ok((chain, key)) = parse_pem(text.as_bytes()) else {
        return Stored::Missing;
    };
    let Some(end_ms) = not_after_ms(&chain[0]).ok() else {
        return Stored::Missing;
    };
    let run = |at, chain: &[CertificateDer<'static>], key: &PrivateKeyDer<'static>| {
        check(
            chain.to_vec(),
            key.clone_key(),
            name,
            at,
            Duration::ZERO,
            anchors,
        )
    };
    if let Ok(cert) = run(now, &chain, &key) {
        return Stored::Valid(cert);
    }
    // Over, and valid up to the last second before its end: only expired.
    let before = u64::try_from(end_ms / 1000).unwrap_or(0).saturating_sub(1);
    let over = now.as_secs() > before;
    if over
        && run(
            UnixTime::since_unix_epoch(Duration::from_secs(before)),
            &chain,
            &key,
        )
        .is_ok()
    {
        Stored::Expired(end_ms)
    } else {
        Stored::Missing
    }
}

/// The stored certificate when it passes the checks of use for `name` at
/// `now`. Any failure is `None`: the caller has no certificate then.
#[must_use]
pub fn load_tailscale(
    dir: &Path,
    name: &str,
    now: UnixTime,
    anchors: &[TrustAnchor<'_>],
) -> Option<TailscaleCert> {
    match read_stored(dir, name, now, anchors) {
        Stored::Valid(cert) => Some(cert),
        Stored::Expired(_) | Stored::Missing => None,
    }
}

/// Deletes the stored certificate. A missing file is no error.
///
/// # Errors
/// An I/O error other than "not found".
pub fn remove_tailscale(dir: &Path) -> io::Result<()> {
    match fs::remove_file(dir.join(TAILSCALE_PEM)) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    }
    fs::File::open(dir)?.sync_all()
}

// ----------------------------------------------------------------- resolver

/// What a new connection gets: the Tailscale certificate when it is for the
/// SNI name and not past its end, else the fallback.
pub struct BoardResolver {
    /// The owner's pair, or the self-signed one.
    fallback: Arc<CertifiedKey>,
    /// True when the fallback is the owner's pair: it wins for every name.
    owner: bool,
    /// The certificate name (lower case).
    name: Option<String>,
    tailscale: RwLock<Option<(Arc<CertifiedKey>, i64)>>,
    clock: SharedClock,
}

impl fmt::Debug for BoardResolver {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BoardResolver")
            .field("owner", &self.owner)
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

impl BoardResolver {
    #[must_use]
    pub fn new(fallback: Arc<CertifiedKey>, owner: bool, name: Option<String>) -> Self {
        Self {
            fallback,
            owner,
            name: name.map(|n| n.to_ascii_lowercase()),
            tailscale: RwLock::new(None),
            clock: Arc::new(SystemClock),
        }
    }

    /// The same resolver with `clock` as its time (a test moves it).
    #[must_use]
    pub fn with_clock(mut self, clock: SharedClock) -> Self {
        self.clock = clock;
        self
    }

    /// The pair that is served when Tailscale gives none.
    #[must_use]
    pub fn fallback(&self) -> Arc<CertifiedKey> {
        Arc::clone(&self.fallback)
    }

    /// True when the owner's files give the certificate for every name.
    #[must_use]
    pub fn owner(&self) -> bool {
        self.owner
    }

    /// Swaps the Tailscale pair and its end time (Unix ms). New connections
    /// see it; open ones keep theirs.
    pub fn set_tailscale(&self, pair: Option<(Arc<CertifiedKey>, i64)>) {
        *self
            .tailscale
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = pair;
    }

    /// The choice for `sni` at `now_ms`.
    #[must_use]
    pub fn pick(&self, sni: Option<&str>, now_ms: i64) -> Arc<CertifiedKey> {
        let wanted = !self.owner
            && matches!((sni, &self.name), (Some(s), Some(n)) if s.eq_ignore_ascii_case(n));
        if wanted
            && let Some((pair, end)) = self
                .tailscale
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .as_ref()
            && *end > now_ms
        {
            return Arc::clone(pair);
        }
        Arc::clone(&self.fallback)
    }
}

impl ResolvesServerCert for BoardResolver {
    fn resolve(&self, client_hello: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        let now = i64::try_from(self.clock.now_ms()).unwrap_or(i64::MAX);
        Some(self.pick(client_hello.server_name(), now))
    }
}

#[cfg(test)]
#[path = "tls_tailscale_tests.rs"]
mod tests;
