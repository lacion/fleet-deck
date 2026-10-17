//! Node identity (PLAN §8.2): one Ed25519 key per node. The TLS certificate is
//! self-signed with that key and carries no other trust; a peer trusts the
//! *key* (pinned after pairing), never a name or a certificate authority.
//!
//! - [`PeerKey`]: a public key, read from a peer certificate with a real X.509
//!   parser (rustls-webpki), never by a byte search.
//! - [`Fingerprint`]: SHA-256 of the raw 32-byte public key, 64 hex digits. The
//!   invitation (`fdinv1:...#<fingerprint>...`) carries it.
//! - [`Identity`]: the private key (in [`Secret`]) and its certificate.

use std::fmt;
use std::fs;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::str::FromStr;

use ed25519_dalek::pkcs8::EncodePrivateKey;
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use ring::rand::SecureRandom;
use rustls_pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};

use crate::secret::Secret;

/// The DER prefix of an Ed25519 SubjectPublicKeyInfo (RFC 8410): SEQUENCE {
/// SEQUENCE { OID 1.3.101.112 }, BIT STRING (0 unused bits) }.
const ED25519_SPKI_PREFIX: [u8; 12] = [
    0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
];

/// The name in every node certificate. Peers ignore it: trust is the key.
pub const CERT_NAME: &str = "node.fleet-deck.invalid";

/// The key file inside a node's data directory.
pub const KEY_FILE: &str = "identity.ed25519";

#[derive(Debug, thiserror::Error)]
pub enum IdentityError {
    #[error("the certificate is not valid X.509: {0}")]
    BadCertificate(String),
    #[error("the peer key is not an Ed25519 key")]
    NotEd25519,
    #[error("the Ed25519 public key is not valid")]
    BadKey,
    #[error("bad fingerprint: {0}")]
    BadFingerprint(String),
    #[error("the key file {path} is readable by other users (mode {mode:o}); use mode 600")]
    KeyFileMode { path: PathBuf, mode: u32 },
    #[error("the key file {0} is not a regular file (a symlink or a directory)")]
    KeyFileType(PathBuf),
    #[error("the key file {0} has the wrong length")]
    KeyFileLength(PathBuf),
    #[error("the system random generator failed")]
    Random,
    #[error("certificate generation failed: {0}")]
    Cert(#[from] rcgen::Error),
    #[error("key encoding failed: {0}")]
    Pkcs8(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// A node's Ed25519 public key: the identity that peers pin.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PeerKey([u8; 32]);

impl PeerKey {
    pub fn from_bytes(bytes: [u8; 32]) -> Result<Self, IdentityError> {
        VerifyingKey::from_bytes(&bytes).map_err(|_| IdentityError::BadKey)?;
        Ok(Self(bytes))
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Reads an exact Ed25519 SubjectPublicKeyInfo (44 bytes).
    pub fn from_spki(spki: &[u8]) -> Result<Self, IdentityError> {
        let raw = spki
            .strip_prefix(&ED25519_SPKI_PREFIX[..])
            .ok_or(IdentityError::NotEd25519)?;
        let bytes: [u8; 32] = raw.try_into().map_err(|_| IdentityError::NotEd25519)?;
        Self::from_bytes(bytes)
    }

    /// Reads the subject public key of a certificate with the X.509 parser of
    /// rustls-webpki. Only the real SPKI field counts, so a certificate that
    /// carries a pinned key somewhere else (an extension, a name) is not
    /// mistaken for the pinned node.
    pub fn from_cert(cert: &CertificateDer<'_>) -> Result<Self, IdentityError> {
        let ee = webpki::EndEntityCert::try_from(cert)
            .map_err(|e| IdentityError::BadCertificate(format!("{e:?}")))?;
        Self::from_spki(ee.subject_public_key_info().as_ref())
    }

    pub fn fingerprint(&self) -> Fingerprint {
        let d = ring::digest::digest(&ring::digest::SHA256, &self.0);
        let mut out = [0u8; 32];
        out.copy_from_slice(d.as_ref());
        Fingerprint(out)
    }

    /// Verifies an Ed25519 signature by this key.
    pub fn verify(&self, message: &[u8], signature: &[u8; 64]) -> bool {
        VerifyingKey::from_bytes(&self.0)
            .is_ok_and(|k| k.verify(message, &Signature::from_bytes(signature)).is_ok())
    }
}

impl fmt::Debug for PeerKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "PeerKey({})", self.fingerprint().short())
    }
}

/// SHA-256 of a node's raw public key.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Fingerprint([u8; 32]);

impl Fingerprint {
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// The first 16 hex digits, for logs and the board.
    pub fn short(&self) -> String {
        self.to_string()[..16].to_owned()
    }

    /// Constant-time comparison (the fingerprint gates pairing).
    pub fn matches(&self, other: &Fingerprint) -> bool {
        let diff = self
            .0
            .iter()
            .zip(other.0.iter())
            .fold(0u8, |acc, (a, b)| acc | (a ^ b));
        std::hint::black_box(diff) == 0
    }
}

impl fmt::Display for Fingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for b in self.0 {
            write!(f, "{b:02x}")?;
        }
        Ok(())
    }
}

impl fmt::Debug for Fingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Fingerprint({})", self.short())
    }
}

impl FromStr for Fingerprint {
    type Err = IdentityError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let bad = || IdentityError::BadFingerprint(s.chars().take(80).collect());
        if s.len() != 64
            || !s
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(bad());
        }
        let mut out = [0u8; 32];
        for (i, byte) in out.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&s[2 * i..2 * i + 2], 16).map_err(|_| bad())?;
        }
        Ok(Self(out))
    }
}

/// A node's private identity: the Ed25519 key and the self-signed certificate
/// made from it.
pub struct Identity {
    key: Secret<SigningKey>,
    key_der: Secret<Vec<u8>>,
    cert: CertificateDer<'static>,
    public: PeerKey,
}

impl fmt::Debug for Identity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Identity")
            .field("public", &self.public)
            .field("key", &self.key)
            .finish()
    }
}

impl Identity {
    /// A new random identity.
    pub fn generate() -> Result<Self, IdentityError> {
        let mut seed = [0u8; 32];
        ring::rand::SystemRandom::new()
            .fill(&mut seed)
            .map_err(|_| IdentityError::Random)?;
        Self::from_seed(Secret::new(seed))
    }

    /// The identity of a 32-byte Ed25519 seed.
    pub fn from_seed(seed: Secret<[u8; 32]>) -> Result<Self, IdentityError> {
        let key = SigningKey::from_bytes(seed.expose());
        let public = PeerKey(key.verifying_key().to_bytes());
        let der = key
            .to_pkcs8_der()
            .map_err(|e| IdentityError::Pkcs8(e.to_string()))?;
        let key_der = der.as_bytes().to_vec();
        let pair = rcgen::KeyPair::try_from(key_der.as_slice())?;
        let mut params = rcgen::CertificateParams::new(vec![CERT_NAME.to_owned()])?;
        params.distinguished_name.push(
            rcgen::DnType::CommonName,
            format!("fleet-deck node {}", public.fingerprint().short()),
        );
        let cert = params.self_signed(&pair)?.der().clone();
        Ok(Self {
            key: Secret::new(key),
            key_der: Secret::new(key_der),
            cert,
            public,
        })
    }

    pub fn public_key(&self) -> PeerKey {
        self.public
    }

    pub fn fingerprint(&self) -> Fingerprint {
        self.public.fingerprint()
    }

    pub fn cert(&self) -> &CertificateDer<'static> {
        &self.cert
    }

    /// The PKCS#8 private key for rustls.
    pub fn private_key_der(&self) -> PrivateKeyDer<'static> {
        PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(self.key_der.expose().clone()))
    }

    pub fn sign(&self, message: &[u8]) -> [u8; 64] {
        self.key.expose().sign(message).to_bytes()
    }

    /// Writes the seed to `<dir>/identity.ed25519` with mode 600. It never
    /// overwrites an existing key: a lost key means a new pairing.
    pub fn save(&self, dir: &Path) -> Result<PathBuf, IdentityError> {
        crate::paths::ensure_private_dir(dir)?;
        let path = dir.join(KEY_FILE);
        // Write a complete temporary file, then link it into place: the link
        // fails if a key exists, and a reader never sees a partial key.
        let mut tag = [0u8; 8];
        ring::rand::SystemRandom::new()
            .fill(&mut tag)
            .map_err(|_| IdentityError::Random)?;
        let tmp = dir.join(format!(".{KEY_FILE}.{:016x}.tmp", u64::from_le_bytes(tag)));
        let written = (|| {
            let mut f = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&tmp)?;
            f.write_all(&self.key.expose().to_bytes())?;
            f.sync_all()?;
            fs::hard_link(&tmp, &path)
        })();
        let _ = fs::remove_file(&tmp);
        written?;
        fs::File::open(dir)?.sync_all()?;
        Ok(path)
    }

    /// Reads `<dir>/identity.ed25519`. A key file that other users can read is
    /// refused (like `ssh` does).
    pub fn load(dir: &Path) -> Result<Self, IdentityError> {
        let path = dir.join(KEY_FILE);
        let meta = fs::symlink_metadata(&path)?;
        if !meta.is_file() {
            return Err(IdentityError::KeyFileType(path));
        }
        let mode = meta.permissions().mode() & 0o777;
        if mode & 0o077 != 0 {
            return Err(IdentityError::KeyFileMode { path, mode });
        }
        let bytes = Secret::new(fs::read(&path)?);
        let seed: [u8; 32] = bytes
            .expose()
            .as_slice()
            .try_into()
            .map_err(|_| IdentityError::KeyFileLength(path.clone()))?;
        Self::from_seed(Secret::new(seed))
    }

    /// Loads the identity of a data directory, or creates and saves one.
    pub fn load_or_create(dir: &Path) -> Result<Self, IdentityError> {
        if dir.join(KEY_FILE).exists() {
            return Self::load(dir);
        }
        let id = Self::generate()?;
        match id.save(dir) {
            Ok(_) => Ok(id),
            // Another process created the key first: use that one.
            Err(IdentityError::Io(e)) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                Self::load(dir)
            }
            Err(e) => Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_certificate_carries_the_identity_key() {
        let id = Identity::generate().unwrap();
        assert_eq!(PeerKey::from_cert(id.cert()).unwrap(), id.public_key());
        let other = Identity::generate().unwrap();
        assert_ne!(other.public_key(), id.public_key());
    }

    #[test]
    fn signatures_verify_only_with_the_right_key_and_message() {
        let id = Identity::generate().unwrap();
        let sig = id.sign(b"transcript");
        assert!(id.public_key().verify(b"transcript", &sig));
        assert!(!id.public_key().verify(b"transcripT", &sig));
        let other = Identity::generate().unwrap();
        assert!(!other.public_key().verify(b"transcript", &sig));
    }

    #[test]
    fn fingerprints_round_trip_and_reject_bad_text() {
        let fp = Identity::generate().unwrap().fingerprint();
        let text = fp.to_string();
        assert_eq!(text.len(), 64);
        assert_eq!(text.parse::<Fingerprint>().unwrap(), fp);
        assert!(fp.matches(&text.parse().unwrap()));
        for bad in [
            "",
            "zz",
            &text[..63],
            &text.to_uppercase(),
            &format!("{text}0"),
        ] {
            assert!(bad.parse::<Fingerprint>().is_err(), "{bad}");
        }
    }

    #[test]
    fn from_spki_needs_an_exact_ed25519_key() {
        let id = Identity::generate().unwrap();
        let mut spki = ED25519_SPKI_PREFIX.to_vec();
        spki.extend_from_slice(id.public_key().as_bytes());
        assert_eq!(PeerKey::from_spki(&spki).unwrap(), id.public_key());
        assert!(PeerKey::from_spki(&spki[..43]).is_err());
        let mut longer = spki.clone();
        longer.push(0);
        assert!(PeerKey::from_spki(&longer).is_err());
        let mut other_alg = spki.clone();
        other_alg[8] = 0x71; // 1.3.101.113 = Ed448
        assert!(PeerKey::from_spki(&other_alg).is_err());
    }

    #[test]
    fn save_and_load_keep_the_identity_and_protect_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let id = Identity::load_or_create(dir.path()).unwrap();
        let path = dir.path().join(KEY_FILE);
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let again = Identity::load_or_create(dir.path()).unwrap();
        assert_eq!(again.public_key(), id.public_key());
        assert!(id.save(dir.path()).is_err(), "never overwrite a key");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(matches!(
            Identity::load(dir.path()),
            Err(IdentityError::KeyFileMode { .. })
        ));
        let dir_mode = fs::metadata(dir.path()).unwrap().permissions().mode() & 0o777;
        assert_eq!(dir_mode & 0o077, 0, "the data directory is private");
    }

    #[test]
    fn a_symlinked_key_file_is_refused() {
        let real = tempfile::tempdir().unwrap();
        Identity::load_or_create(real.path()).unwrap();
        let dir = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(real.path().join(KEY_FILE), dir.path().join(KEY_FILE)).unwrap();
        assert!(matches!(
            Identity::load(dir.path()),
            Err(IdentityError::KeyFileType(_))
        ));
    }

    #[test]
    fn concurrent_creation_ends_with_one_key() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_path_buf();
        let handles: Vec<_> = (0..4)
            .map(|_| {
                let path = path.clone();
                std::thread::spawn(move || Identity::load_or_create(&path).unwrap().public_key())
            })
            .collect();
        let keys: Vec<PeerKey> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        assert!(keys.windows(2).all(|w| w[0] == w[1]), "{keys:?}");
    }

    #[test]
    fn debug_output_never_shows_the_private_key() {
        let id = Identity::from_seed(Secret::new([7u8; 32])).unwrap();
        let text = format!("{id:?}");
        assert!(text.contains("<redacted>"));
        assert!(!text.contains("[7, 7"));
    }
}
