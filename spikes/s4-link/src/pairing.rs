//! Invitation pairing (PLAN §8.5).
//!
//! An invitation is `fdinv1:wss://<ip>:<port>#<fingerprint>.<secret>.<expiry>`:
//! the address of the inviting node, the full fingerprint of its key (64 hex),
//! a 128-bit secret (32 hex) and the expiry in Unix seconds. The secret is in
//! the URL fragment, so a browser never sends it to a server.
//!
//! The joining node dials the address, accepts only the key with that
//! fingerprint, runs the normal link handshake on the pair channel, and then
//! proves that it knows the secret with an HMAC over the handshake transcript.
//! The transcript holds the TLS exporter, so the proof is bound to this TLS
//! session and cannot be relayed. The inviter answers with its own HMAC, then
//! pins the joiner's key with the joiner's role and grants the invitation's
//! scopes. An invitation works once, for 15 minutes by default, and five wrong
//! proofs close every open invitation of the node.

use std::collections::BTreeSet;
use std::fmt;
use std::net::SocketAddr;
use std::str::FromStr;
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ring::hmac;
use ring::rand::{SecureRandom, SystemRandom};
use tokio::sync::mpsc;

use crate::addr::{self, AddrError};
use crate::frames::Scope;
use crate::identity::{Fingerprint, PeerKey};
use crate::secret::Secret;
use crate::tls::Role;

pub const PREFIX: &str = "fdinv1:";
/// PLAN §8.5: an invitation is valid for 15 minutes.
pub const DEFAULT_TTL: Duration = Duration::from_secs(15 * 60);
/// Wrong proofs before the node closes all open invitations.
pub const MAX_FAILED_PROOFS: u32 = 5;

const JOINER_TAG: &[u8] = b"fd-pair-v1 joiner\0";
const INVITER_TAG: &[u8] = b"fd-pair-v1 inviter\0";

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PairError {
    #[error("not a fleet deck invitation: {0}")]
    Format(String),
    #[error(transparent)]
    Address(#[from] AddrError),
    #[error("the invitation expired")]
    Expired,
    #[error("the invitation is unknown, used, or the proof is wrong")]
    InvalidProof,
    #[error("too many wrong pairing proofs: every open invitation is closed")]
    TooManyFailures,
    #[error("the inviter could not prove that it knows the secret")]
    InviterProof,
    #[error("the system random generator failed")]
    Random,
}

impl PairError {
    /// The stable reason code that both sides report.
    pub fn reason(&self) -> &'static str {
        match self {
            Self::Format(_) => "bad_invitation",
            Self::Address(_) => "address_refused",
            Self::Expired => "invitation_expired",
            Self::InvalidProof => "invalid_invitation",
            Self::TooManyFailures => "too_many_attempts",
            Self::InviterProof => "inviter_proof_failed",
            Self::Random => "random_failed",
        }
    }
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Lowercase hex of exactly `N` bytes.
pub fn unhex<const N: usize>(s: &str) -> Option<[u8; N]> {
    if s.len() != 2 * N
        || !s
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return None;
    }
    let mut out = [0u8; N];
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(&s[2 * i..2 * i + 2], 16).ok()?;
    }
    Some(out)
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// A parsed invitation. `Debug` and `Display` never show the secret; only
/// [`Invitation::to_uri`] writes it, for the one place that shows it to the
/// owner.
pub struct Invitation {
    pub addr: SocketAddr,
    pub fingerprint: Fingerprint,
    pub secret: Secret<[u8; 16]>,
    /// Unix seconds.
    pub expiry: u64,
}

impl fmt::Debug for Invitation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Invitation")
            .field("addr", &self.addr)
            .field("fingerprint", &self.fingerprint)
            .field("secret", &self.secret)
            .field("expiry", &self.expiry)
            .finish()
    }
}

impl Invitation {
    /// The full invitation text, secret included.
    pub fn to_uri(&self) -> String {
        format!(
            "{PREFIX}wss://{}#{}.{}.{}",
            self.addr,
            self.fingerprint,
            hex(self.secret.expose()),
            self.expiry
        )
    }

    pub fn is_expired(&self) -> bool {
        unix_now() >= self.expiry
    }

    /// The joiner's proof for this handshake transcript.
    pub fn joiner_proof(&self, transcript: &[u8]) -> String {
        proof(self.secret.expose(), JOINER_TAG, transcript)
    }

    /// Checks the inviter's answer for this handshake transcript.
    pub fn check_inviter_proof(&self, transcript: &[u8], proof_hex: &str) -> Result<(), PairError> {
        verify(self.secret.expose(), INVITER_TAG, transcript, proof_hex)
            .then_some(())
            .ok_or(PairError::InviterProof)
    }
}

impl FromStr for Invitation {
    type Err = PairError;

    /// Parses and checks the format and the address scope (D13). It does not
    /// check the expiry: [`Invitation::is_expired`] does.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let bad = |why: &str| PairError::Format(why.to_owned());
        let rest = s
            .trim()
            .strip_prefix(PREFIX)
            .ok_or_else(|| bad("no fdinv1: prefix"))?;
        let (url, fragment) = rest.split_once('#').ok_or_else(|| bad("no # fragment"))?;
        let host = url
            .strip_prefix("wss://")
            .ok_or_else(|| bad("the address is not wss://"))?
            .trim_end_matches('/');
        let addr: SocketAddr = host
            .parse()
            .map_err(|_| bad("the address is not an IP address with a port"))?;
        addr::check_dial(addr)?;
        let mut parts = fragment.split('.');
        let (Some(fp), Some(secret), Some(expiry), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return Err(bad("the fragment is not <fingerprint>.<secret>.<expiry>"));
        };
        let fingerprint = fp.parse().map_err(|_| bad("bad fingerprint"))?;
        let secret = unhex::<16>(secret).ok_or_else(|| bad("the secret is not 32 hex digits"))?;
        let expiry = expiry.parse().map_err(|_| bad("bad expiry"))?;
        Ok(Self {
            addr,
            fingerprint,
            secret: Secret::new(secret),
            expiry,
        })
    }
}

fn proof(secret: &[u8; 16], tag: &[u8], transcript: &[u8]) -> String {
    let key = hmac::Key::new(hmac::HMAC_SHA256, secret);
    let mut ctx = hmac::Context::with_key(&key);
    ctx.update(tag);
    ctx.update(transcript);
    hex(ctx.sign().as_ref())
}

/// Constant-time check (ring's HMAC verify).
fn verify(secret: &[u8; 16], tag: &[u8], transcript: &[u8], proof_hex: &str) -> bool {
    let Some(tag_bytes) = unhex::<32>(proof_hex) else {
        return false;
    };
    let key = hmac::Key::new(hmac::HMAC_SHA256, secret);
    let mut msg = tag.to_vec();
    msg.extend_from_slice(transcript);
    hmac::verify(&key, &msg, &tag_bytes).is_ok()
}

/// A node that paired through one of this node's invitations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairedPeer {
    pub key: PeerKey,
    pub role: Role,
    pub scopes: BTreeSet<Scope>,
}

struct Open {
    secret: Secret<[u8; 16]>,
    expires: SystemTime,
    scopes: BTreeSet<Scope>,
}

/// What a valid proof redeems.
pub struct Redeemed {
    pub scopes: BTreeSet<Scope>,
    /// The inviter's answer proof.
    pub answer: String,
}

/// The open invitations of the inviting node.
pub struct Invitations {
    open: Mutex<Vec<Open>>,
    failures: Mutex<u32>,
    paired_tx: mpsc::UnboundedSender<PairedPeer>,
    paired_rx: tokio::sync::Mutex<mpsc::UnboundedReceiver<PairedPeer>>,
}

impl fmt::Debug for Invitations {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Invitations")
            .field("open", &self.open_count())
            .finish()
    }
}

impl Default for Invitations {
    fn default() -> Self {
        let (paired_tx, rx) = mpsc::unbounded_channel();
        Self {
            open: Mutex::new(Vec::new()),
            failures: Mutex::new(0),
            paired_tx,
            paired_rx: tokio::sync::Mutex::new(rx),
        }
    }
}

impl Invitations {
    pub fn new() -> std::sync::Arc<Self> {
        std::sync::Arc::default()
    }

    /// Opens an invitation to the node at `addr` with key `fingerprint`. The
    /// joiner gets `scopes` on this node.
    pub fn create(
        &self,
        addr: SocketAddr,
        fingerprint: Fingerprint,
        ttl: Duration,
        scopes: impl IntoIterator<Item = Scope>,
    ) -> Result<Invitation, PairError> {
        addr::check_dial(addr)?;
        let mut secret = [0u8; 16];
        SystemRandom::new()
            .fill(&mut secret)
            .map_err(|_| PairError::Random)?;
        let expires = SystemTime::now() + ttl;
        // Round the visible expiry down, so the joiner never trusts an
        // invitation longer than the inviter does.
        let expiry = expires
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let mut open = self.open.lock().map_err(|_| PairError::InvalidProof)?;
        open.push(Open {
            secret: Secret::new(secret),
            expires,
            scopes: scopes.into_iter().collect(),
        });
        Ok(Invitation {
            addr,
            fingerprint,
            secret: Secret::new(secret),
            expiry,
        })
    }

    /// Open, unexpired invitations. The TLS verifier accepts an unpinned
    /// client key only while this is not zero.
    pub fn open_count(&self) -> usize {
        let now = SystemTime::now();
        self.open
            .lock()
            .map(|o| o.iter().filter(|i| i.expires > now).count())
            .unwrap_or(0)
    }

    /// Closes every open invitation.
    pub fn close_all(&self) {
        if let Ok(mut o) = self.open.lock() {
            o.clear();
        }
    }

    /// Consumes the invitation whose secret made `proof_hex` over `transcript`.
    pub fn redeem(&self, transcript: &[u8], proof_hex: &str) -> Result<Redeemed, PairError> {
        let now = SystemTime::now();
        let mut open = self.open.lock().map_err(|_| PairError::InvalidProof)?;
        let had_expired = open.iter().any(|i| i.expires <= now);
        open.retain(|i| i.expires > now);
        let hit = open
            .iter()
            .position(|i| verify(i.secret.expose(), JOINER_TAG, transcript, proof_hex));
        if let Some(pos) = hit {
            let inv = open.swap_remove(pos);
            return Ok(Redeemed {
                answer: proof(inv.secret.expose(), INVITER_TAG, transcript),
                scopes: inv.scopes,
            });
        }
        let mut failures = self.failures.lock().map_err(|_| PairError::InvalidProof)?;
        *failures += 1;
        if *failures >= MAX_FAILED_PROOFS {
            open.clear();
            return Err(PairError::TooManyFailures);
        }
        Err(if had_expired && open.is_empty() {
            PairError::Expired
        } else {
            PairError::InvalidProof
        })
    }

    pub(crate) fn notify_paired(&self, peer: PairedPeer) {
        let _ = self.paired_tx.send(peer);
    }

    /// Waits for the next node that pairs through an invitation.
    pub async fn next_paired(&self) -> Option<PairedPeer> {
        self.paired_rx.lock().await.recv().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::Identity;

    fn fp() -> Fingerprint {
        Identity::generate().unwrap().fingerprint()
    }

    fn lan() -> SocketAddr {
        "192.168.1.20:7443".parse().unwrap()
    }

    #[test]
    fn invitations_round_trip_and_never_print_the_secret() {
        let store = Invitations::default();
        let inv = store
            .create(lan(), fp(), DEFAULT_TTL, [Scope::Read])
            .unwrap();
        let uri = inv.to_uri();
        assert!(uri.starts_with("fdinv1:wss://192.168.1.20:7443#"));
        let back: Invitation = uri.parse().unwrap();
        assert_eq!(back.addr, inv.addr);
        assert_eq!(back.fingerprint, inv.fingerprint);
        assert_eq!(back.secret.expose(), inv.secret.expose());
        assert_eq!(back.expiry, inv.expiry);
        let secret_hex = hex(inv.secret.expose());
        assert!(!format!("{inv:?}").contains(&secret_hex));
        assert!(!format!("{store:?}").contains(&secret_hex));
        assert!(!inv.is_expired());
    }

    #[test]
    fn bad_invitations_are_refused() {
        let good = Invitations::default()
            .create(lan(), fp(), DEFAULT_TTL, [])
            .unwrap()
            .to_uri();
        let (_, frag) = good.split_once('#').unwrap();
        for bad in [
            format!("fdinv2:wss://192.168.1.20:7443#{frag}"),
            format!("fdinv1:https://192.168.1.20:7443#{frag}"),
            format!("fdinv1:wss://deck.example:7443#{frag}"),
            "fdinv1:wss://192.168.1.20:7443".to_owned(),
            format!("fdinv1:wss://192.168.1.20:7443#{frag}.extra"),
            good.replace(&frag[65..97], "zz"),
        ] {
            assert!(
                matches!(bad.parse::<Invitation>(), Err(PairError::Format(_))),
                "{bad}"
            );
        }
        let public = good.replace("192.168.1.20", "8.8.8.8");
        assert!(matches!(
            public.parse::<Invitation>(),
            Err(PairError::Address(_))
        ));
        assert!(
            Invitations::default()
                .create("8.8.8.8:7443".parse().unwrap(), fp(), DEFAULT_TTL, [])
                .is_err()
        );
    }

    #[test]
    fn a_proof_redeems_its_invitation_once() {
        let store = Invitations::default();
        let inv = store
            .create(lan(), fp(), DEFAULT_TTL, [Scope::Read])
            .unwrap();
        let other = store
            .create(lan(), fp(), DEFAULT_TTL, [Scope::Admin])
            .unwrap();
        let tr = b"transcript of one TLS session";
        let r = store.redeem(tr, &inv.joiner_proof(tr)).unwrap();
        assert_eq!(r.scopes, BTreeSet::from([Scope::Read]));
        inv.check_inviter_proof(tr, &r.answer).unwrap();
        assert_eq!(
            inv.check_inviter_proof(b"another session", &r.answer),
            Err(PairError::InviterProof)
        );
        assert_eq!(
            store.redeem(tr, &inv.joiner_proof(tr)).err(),
            Some(PairError::InvalidProof)
        );
        // A proof made for another session does not redeem the other invitation.
        assert!(store.redeem(tr, &other.joiner_proof(b"x")).is_err());
        assert_eq!(store.open_count(), 1);
    }

    #[test]
    fn expired_invitations_are_refused() {
        let store = Invitations::default();
        let inv = store.create(lan(), fp(), Duration::ZERO, []).unwrap();
        assert!(inv.is_expired());
        assert_eq!(store.open_count(), 0);
        assert_eq!(
            store.redeem(b"t", &inv.joiner_proof(b"t")).err(),
            Some(PairError::Expired)
        );
    }

    #[test]
    fn wrong_proofs_close_every_invitation() {
        let store = Invitations::default();
        let inv = store.create(lan(), fp(), DEFAULT_TTL, []).unwrap();
        for _ in 1..MAX_FAILED_PROOFS {
            assert_eq!(
                store.redeem(b"t", &"0".repeat(64)).err(),
                Some(PairError::InvalidProof)
            );
        }
        assert_eq!(
            store.redeem(b"t", "nothex").err(),
            Some(PairError::TooManyFailures)
        );
        assert_eq!(store.open_count(), 0);
        assert!(store.redeem(b"t", &inv.joiner_proof(b"t")).is_err());
    }
}
