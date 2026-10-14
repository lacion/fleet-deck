//! Invitations for pairing (PLAN §8.5, ADR-004 §2.8, G11, G12).
//!
//! An invitation is
//! `fdinv1:wss://<ip>:<port>#<fingerprint>.<id>.<secret>.<expiry>`: the
//! address of the inviting node, the full fingerprint of its key (64 hex),
//! the invitation id (16 hex, not secret), a 128-bit secret (32 hex) and the
//! expiry in Unix seconds. The secret is in the URL fragment, so a browser
//! never sends it to a server.
//!
//! - An invitation works once, for 15 minutes by default.
//! - Wrong proofs are counted for each invitation id: 5 wrong proofs cancel
//!   only that invitation (G11). Other invitations stay open.
//! - [`PairLimiter`]: each source IP gets at most 10 pairing attempts per
//!   minute, and at most 4 pairing handshakes run at the same time (G11).
//! - The secret is a `Secret<[u8; 16]>`: `Debug` never prints it; only
//!   [`Invitation::reveal`] writes it, for the one place that shows the
//!   invitation to the owner (G12).

use std::collections::{BTreeSet, HashMap, VecDeque};
use std::fmt;
use std::net::{IpAddr, SocketAddr};
use std::str::FromStr;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use fleetdeck_core::identity::{Fingerprint, PeerKey};
use fleetdeck_core::netscope::{self, AddrError};
use fleetdeck_core::secret::Secret;
use fleetdeck_proto::link::Scope;
use ring::hmac;
use ring::rand::{SecureRandom, SystemRandom};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc};
use tokio::time::Instant;

use crate::tls::{InvitationGate, Role};

pub const PREFIX: &str = "fdinv1:";
/// PLAN §8.5: an invitation is valid for 15 minutes.
pub const DEFAULT_TTL: Duration = Duration::from_secs(15 * 60);
/// Wrong proofs that cancel one invitation (G11).
pub const MAX_FAILED_PROOFS: u32 = 5;
/// Pairing attempts of one source IP in [`RATE_WINDOW`] (G11).
pub const MAX_ATTEMPTS_PER_IP: usize = 10;
pub const RATE_WINDOW: Duration = Duration::from_secs(60);
/// Pairing handshakes that run at the same time (G11).
pub const MAX_PAIRING_HANDSHAKES: usize = 4;

const JOINER_TAG: &[u8] = b"fd-pair-v1 joiner\0";
const INVITER_TAG: &[u8] = b"fd-pair-v1 inviter\0";

/// Stable pairing reason codes (ADR-004 §2.11).
pub mod reason {
    pub const BAD_INVITATION: &str = "bad_invitation";
    pub const ADDRESS_REFUSED: &str = "address_refused";
    pub const INVITATION_EXPIRED: &str = "invitation_expired";
    pub const INVALID_INVITATION: &str = "invalid_invitation";
    pub const TOO_MANY_ATTEMPTS: &str = "too_many_attempts";
    pub const INVITER_PROOF_FAILED: &str = "inviter_proof_failed";
    pub const PAIRING_FAILED: &str = "pairing_failed";
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PairError {
    #[error("not a fleet deck invitation: {0}")]
    Format(String),
    #[error(transparent)]
    Address(#[from] AddrError),
    #[error("the invitation expired")]
    Expired,
    #[error("the invitation is unknown or used, or the proof is wrong")]
    InvalidProof,
    #[error("5 wrong proofs: this invitation is cancelled")]
    TooManyFailures,
    #[error("too many pairing attempts from {0}; wait a minute")]
    RateLimited(IpAddr),
    #[error("{MAX_PAIRING_HANDSHAKES} pairing handshakes are running; try again")]
    Busy,
    #[error("the inviter could not prove that it knows the secret")]
    InviterProof,
    #[error("the paired key could not be stored: {0}")]
    Store(String),
    #[error("the system random generator failed")]
    Random,
}

impl PairError {
    /// The stable reason code that both sides report.
    #[must_use]
    pub fn reason(&self) -> &'static str {
        match self {
            Self::Format(_) => reason::BAD_INVITATION,
            Self::Address(_) => reason::ADDRESS_REFUSED,
            Self::Expired => reason::INVITATION_EXPIRED,
            Self::InvalidProof => reason::INVALID_INVITATION,
            Self::TooManyFailures | Self::RateLimited(_) | Self::Busy => reason::TOO_MANY_ATTEMPTS,
            Self::InviterProof => reason::INVITER_PROOF_FAILED,
            Self::Store(_) | Self::Random => reason::PAIRING_FAILED,
        }
    }
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Lowercase hex of exactly `N` bytes.
pub(crate) fn unhex<const N: usize>(s: &str) -> Option<[u8; N]> {
    if s.len() != 2 * N
        || !s
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return None;
    }
    let mut out = [0u8; N];
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(s.get(2 * i..2 * i + 2)?, 16).ok()?;
    }
    Some(out)
}

fn random<const N: usize>() -> Result<[u8; N], PairError> {
    let mut out = [0u8; N];
    SystemRandom::new()
        .fill(&mut out)
        .map_err(|_| PairError::Random)?;
    Ok(out)
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// The non-secret id of an invitation (16 hex digits).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct InvitationId(String);

impl InvitationId {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Accepts exactly 16 lowercase hex digits.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        unhex::<8>(s).map(|_| Self(s.to_owned()))
    }
}

impl fmt::Display for InvitationId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A parsed invitation. `Debug` never shows the secret.
pub struct Invitation {
    pub addr: SocketAddr,
    pub fingerprint: Fingerprint,
    pub id: InvitationId,
    pub secret: Secret<[u8; 16]>,
    /// Unix seconds.
    pub expiry: u64,
}

impl fmt::Debug for Invitation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Invitation")
            .field("addr", &self.addr)
            .field("fingerprint", &self.fingerprint.short())
            .field("id", &self.id)
            .field("secret", &self.secret)
            .field("expiry", &self.expiry)
            .finish()
    }
}

impl Invitation {
    /// The full invitation text, secret included. Show it only to the
    /// owner, once (G12).
    #[must_use]
    pub fn reveal(&self) -> Secret<String> {
        Secret::new(format!(
            "{PREFIX}wss://{}#{}.{}.{}.{}",
            self.addr,
            self.fingerprint,
            self.id,
            hex(self.secret.expose()),
            self.expiry
        ))
    }

    #[must_use]
    pub fn is_expired(&self) -> bool {
        unix_now() >= self.expiry
    }

    /// The joiner's proof for this handshake transcript.
    #[must_use]
    pub fn joiner_proof(&self, transcript: &[u8]) -> String {
        proof(self.secret.expose(), JOINER_TAG, transcript)
    }

    /// Checks the inviter's answer for this handshake transcript.
    pub fn check_inviter_proof(&self, transcript: &[u8], proof_hex: &str) -> Result<(), PairError> {
        verify(self.secret.expose(), INVITER_TAG, transcript, proof_hex)
            .then_some(())
            .ok_or(PairError::InviterProof)
    }

    /// Parses an invitation from a secret text (G12: the text never leaves
    /// its `Secret`).
    pub fn parse(text: &Secret<String>) -> Result<Self, PairError> {
        text.expose().parse()
    }
}

impl FromStr for Invitation {
    type Err = PairError;

    /// Parses and checks the format and the address scope (D13). It does not
    /// check the expiry: [`Invitation::is_expired`] does. Error messages
    /// never contain the input.
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
        netscope::check_dial(addr)?;
        let mut parts = fragment.split('.');
        let (Some(fp), Some(id), Some(secret), Some(expiry), None) = (
            parts.next(),
            parts.next(),
            parts.next(),
            parts.next(),
            parts.next(),
        ) else {
            return Err(bad(
                "the fragment is not <fingerprint>.<id>.<secret>.<expiry>",
            ));
        };
        let fingerprint = fp.parse().map_err(|_| bad("bad fingerprint"))?;
        let id = InvitationId::parse(id).ok_or_else(|| bad("the id is not 16 hex digits"))?;
        let secret = unhex::<16>(secret).ok_or_else(|| bad("the secret is not 32 hex digits"))?;
        let expiry = expiry.parse().map_err(|_| bad("bad expiry"))?;
        Ok(Self {
            addr,
            fingerprint,
            id,
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

/// A node that paired with this node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairedPeer {
    pub key: PeerKey,
    pub role: Role,
    /// The scopes this node grants to the peer.
    pub scopes: BTreeSet<Scope>,
}

/// Where a node stores what pairing, revocation and key rotation change
/// (the station and deck stores; [`crate::Grants`] in memory). The link
/// layer itself keeps the pins of the node's `PinSet`. A database-backed
/// sink makes each call one transaction (G5), so the methods are async.
#[async_trait::async_trait]
pub trait PairingSink: fmt::Debug + Send + Sync {
    /// Stores the pin and the grants of a new peer. An error fails the
    /// pairing.
    async fn paired(&self, peer: &PairedPeer) -> Result<(), String>;
    /// Removes the pin and every grant of `peer`. An error is returned to
    /// the caller of `revoke`; the link layer unpins the key and closes its
    /// link anyway (fail closed).
    async fn revoked(&self, peer: &PeerKey) -> Result<(), String>;
    /// Gives the role and the grants of `old` to `new` (only those; G5) and
    /// revokes `old`.
    async fn rotated(&self, old: &PeerKey, new: &PeerKey) -> Result<(), String>;
}

struct Open {
    secret: Secret<[u8; 16]>,
    expires: Instant,
    scopes: BTreeSet<Scope>,
    failures: u32,
}

/// What a valid proof redeems.
#[derive(Debug)]
pub struct Redeemed {
    pub scopes: BTreeSet<Scope>,
    /// The inviter's answer proof.
    pub answer: String,
}

/// The open invitations of the inviting node.
pub struct Invitations {
    open: Mutex<HashMap<InvitationId, Open>>,
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
            open: Mutex::new(HashMap::new()),
            paired_tx,
            paired_rx: tokio::sync::Mutex::new(rx),
        }
    }
}

impl InvitationGate for Invitations {
    fn has_open(&self) -> bool {
        self.open_count() > 0
    }
}

impl Invitations {
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::default()
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<InvitationId, Open>> {
        self.open.lock().unwrap_or_else(PoisonError::into_inner)
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
        netscope::check_dial(addr)?;
        let secret = random::<16>()?;
        let id = InvitationId(hex(&random::<8>()?));
        // The visible expiry is rounded down, so the joiner never trusts an
        // invitation longer than the inviter does.
        let expiry = unix_now() + ttl.as_secs();
        self.lock().insert(
            id.clone(),
            Open {
                secret: Secret::new(secret),
                expires: Instant::now() + ttl,
                scopes: scopes.into_iter().collect(),
                failures: 0,
            },
        );
        Ok(Invitation {
            addr,
            fingerprint,
            id,
            secret: Secret::new(secret),
            expiry,
        })
    }

    /// Open, unexpired invitations. The TLS verifier accepts an unpinned
    /// client key only while this is not zero.
    #[must_use]
    pub fn open_count(&self) -> usize {
        let now = Instant::now();
        self.lock().values().filter(|i| i.expires > now).count()
    }

    /// Cancels one invitation; `false` when it was not open.
    pub fn cancel(&self, id: &InvitationId) -> bool {
        self.lock().remove(id).is_some()
    }

    /// Consumes invitation `id` when `proof_hex` over `transcript` proves
    /// its secret. Wrong proofs count against that invitation only. An
    /// unknown id and a wrong proof give the same error, so the reason
    /// tells an attacker nothing about which ids exist.
    pub fn redeem(
        &self,
        id: &InvitationId,
        transcript: &[u8],
        proof_hex: &str,
    ) -> Result<Redeemed, PairError> {
        let now = Instant::now();
        let mut open = self.lock();
        // Other expired invitations go away here (this one below, with its
        // own reason); an invitation with 5 wrong proofs was removed when it
        // reached that count.
        open.retain(|k, i| i.expires > now || k == id);
        let Some(inv) = open.get_mut(id) else {
            return Err(PairError::InvalidProof);
        };
        if inv.expires <= now {
            open.remove(id);
            return Err(PairError::Expired);
        }
        if verify(inv.secret.expose(), JOINER_TAG, transcript, proof_hex) {
            let answer = proof(inv.secret.expose(), INVITER_TAG, transcript);
            let scopes = std::mem::take(&mut inv.scopes);
            open.remove(id);
            return Ok(Redeemed { scopes, answer });
        }
        inv.failures += 1;
        if inv.failures >= MAX_FAILED_PROOFS {
            open.remove(id);
            tracing::warn!(invitation = %id, "5 wrong pairing proofs; the invitation is cancelled");
            return Err(PairError::TooManyFailures);
        }
        Err(PairError::InvalidProof)
    }

    pub(crate) fn notify_paired(&self, peer: PairedPeer) {
        let _ = self.paired_tx.send(peer);
    }

    /// Waits for the next node that paired through an invitation.
    pub async fn next_paired(&self) -> Option<PairedPeer> {
        self.paired_rx.lock().await.recv().await
    }
}

/// Admission of pairing handshakes (G11).
#[derive(Debug, Clone)]
pub struct PairLimiter {
    attempts: Arc<Mutex<HashMap<IpAddr, VecDeque<Instant>>>>,
    slots: Arc<Semaphore>,
    window: Duration,
}

impl Default for PairLimiter {
    fn default() -> Self {
        Self::with_window(RATE_WINDOW)
    }
}

impl PairLimiter {
    /// A limiter whose rate window is `window` instead of [`RATE_WINDOW`]
    /// (tests over real sockets cannot wait a minute).
    #[must_use]
    pub fn with_window(window: Duration) -> Self {
        Self {
            attempts: Arc::default(),
            slots: Arc::new(Semaphore::new(MAX_PAIRING_HANDSHAKES)),
            window,
        }
    }

    /// Admits one pairing attempt from `ip`. The permit holds one of the 4
    /// handshake slots until it is dropped. A refused attempt does not
    /// count, so the lockout of an IP ends one window after its tenth
    /// admitted attempt.
    pub fn admit(&self, ip: IpAddr) -> Result<OwnedSemaphorePermit, PairError> {
        let now = Instant::now();
        {
            let mut all = self.attempts.lock().unwrap_or_else(PoisonError::into_inner);
            let window = self.window;
            all.retain(|_, q| q.back().is_some_and(|t| now.duration_since(*t) < window));
            let q = all.entry(ip).or_default();
            while q.front().is_some_and(|t| now.duration_since(*t) >= window) {
                q.pop_front();
            }
            let limited = q.len() >= MAX_ATTEMPTS_PER_IP;
            if !limited {
                q.push_back(now);
            }
            if limited {
                return Err(PairError::RateLimited(ip));
            }
        }
        Arc::clone(&self.slots)
            .try_acquire_owned()
            .map_err(|_| PairError::Busy)
    }
}

#[cfg(test)]
#[path = "pairing_tests.rs"]
mod tests;
