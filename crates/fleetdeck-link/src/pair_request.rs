//! The data of a pairing request (DL-178, tailnet design 5.3; ADR-004 §10).
//!
//! A station asks a deck to pair, without an invitation. The request is
//! immutable: `request_id`, name, `nonce` and the scopes that the station
//! offers to the deck. Three canonical encodings use it:
//!
//! - the **request digest**, SHA-256 over the tag `fleetdeck pair-request v1`
//!   and the length-prefixed request fields;
//! - the **pending digest** that the deck shows to the owner and that the
//!   owner sends back with the approval;
//! - the **approval message** that the deck signs with its link key
//!   (`fleetdeck pair-approved v1`) and that binds the answer to one TLS
//!   session (the hash of the handshake transcript).
//!
//! Every field is prefixed with its length as 8 bytes, big endian. Scope
//! lists are sorted by name and joined with `,`.

use std::collections::BTreeSet;
use std::net::IpAddr;

use fleetdeck_core::identity::{Fingerprint, Identity, PeerKey};
use fleetdeck_proto::link::Scope;
use ring::digest::{Context, SHA256};
use ring::rand::{SecureRandom, SystemRandom};

use crate::pairing::{hex, unhex};

/// The tag of the request digest.
pub const REQUEST_TAG: &[u8] = b"fleetdeck pair-request v1";
/// The tag of the signed approval.
pub const APPROVED_TAG: &[u8] = b"fleetdeck pair-approved v1";
/// The tag of the pending-record digest.
pub const PENDING_TAG: &[u8] = b"fleetdeck pair-pending v1";

/// The longest requested name, in characters.
pub const MAX_NAME_CHARS: usize = 64;

/// The scopes a station may offer to a deck (never `coordinate`).
pub const OFFERABLE_SCOPES: [Scope; 4] = [Scope::Read, Scope::Drive, Scope::Approve, Scope::Admin];

/// Stable reason codes of a pairing request (design 5.4, DL-178).
pub mod reason {
    pub const WINDOW_CLOSED: &str = "window_closed";
    pub const BUSY: &str = "busy";
    pub const ALREADY_PAIRED: &str = "already_paired";
    pub const REQUEST_ID_CONFLICT: &str = "request_id_conflict";
    pub const DENIED: &str = "denied";
    pub const EXPIRED: &str = "expired";
    pub const CANCELLED: &str = "cancelled";
    pub const UNKNOWN_REQUEST: &str = "unknown_request";
    pub const REVOKED: &str = "revoked";
    pub const FINGERPRINT_MISMATCH: &str = "fingerprint_mismatch";
    pub const UNSUPPORTED: &str = "unsupported";
}

/// Why a request does not parse.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RequestError {
    #[error("the request id is not 32 hex digits")]
    BadId,
    #[error("the nonce is not 64 hex digits")]
    BadNonce,
    #[error("the requested name must have 1 to {MAX_NAME_CHARS} characters, none of them a control or direction character")]
    BadName,
    #[error("a requested scope is not one of read, drive, approve, admin")]
    BadScope,
    #[error("the system random generator failed")]
    Random,
}

/// True for a control character or a text direction character (a name can
/// not hide its end or reorder the text around it).
fn is_unsafe_char(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            '\u{061C}' | '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}'
        )
}

/// Checks a requested name: 1 to 64 characters, no control or bidi character.
pub fn validate_name(name: &str) -> Result<(), RequestError> {
    let n = name.chars().count();
    if n == 0 || n > MAX_NAME_CHARS || name.chars().any(is_unsafe_char) {
        return Err(RequestError::BadName);
    }
    Ok(())
}

/// Checks that every scope is one that a station may offer.
pub fn validate_scopes(scopes: &BTreeSet<Scope>) -> Result<(), RequestError> {
    if scopes.iter().all(|s| OFFERABLE_SCOPES.contains(s)) {
        Ok(())
    } else {
        Err(RequestError::BadScope)
    }
}

/// The scopes sorted by name and joined with `,`.
#[must_use]
pub fn scopes_text(scopes: &BTreeSet<Scope>) -> String {
    let mut names: Vec<&str> = scopes.iter().map(|s| s.name()).collect();
    names.sort_unstable();
    names.join(",")
}

fn push(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(&u64::try_from(bytes.len()).unwrap_or(u64::MAX).to_be_bytes());
    out.extend_from_slice(bytes);
}

fn sha256(parts: &[&[u8]]) -> [u8; 32] {
    let mut ctx = Context::new(&SHA256);
    for p in parts {
        ctx.update(p);
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(ctx.finish().as_ref());
    out
}

/// SHA-256 of the transcript of one handshake. The approval is bound to it.
#[must_use]
pub fn transcript_hash(transcript: &[u8]) -> [u8; 32] {
    sha256(&[transcript])
}

/// The immutable request of a station.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanonicalRequest {
    pub request_id: [u8; 16],
    pub name: String,
    pub nonce: [u8; 32],
    pub offered_scopes: BTreeSet<Scope>,
}

impl CanonicalRequest {
    /// A new request with a random id and nonce.
    pub fn generate(
        name: &str,
        offered_scopes: BTreeSet<Scope>,
    ) -> Result<Self, RequestError> {
        let mut request_id = [0u8; 16];
        let mut nonce = [0u8; 32];
        let rng = SystemRandom::new();
        rng.fill(&mut request_id).map_err(|_| RequestError::Random)?;
        rng.fill(&mut nonce).map_err(|_| RequestError::Random)?;
        Self::new(request_id, name.to_owned(), nonce, offered_scopes)
    }

    /// A request from its parts; the name and the scopes are checked.
    pub fn new(
        request_id: [u8; 16],
        name: String,
        nonce: [u8; 32],
        offered_scopes: BTreeSet<Scope>,
    ) -> Result<Self, RequestError> {
        validate_name(&name)?;
        validate_scopes(&offered_scopes)?;
        Ok(Self {
            request_id,
            name,
            nonce,
            offered_scopes,
        })
    }

    /// A request from the fields of a `PairRequest` frame.
    pub fn from_frame(
        request_id: &str,
        name: &str,
        nonce: &str,
        offered_scopes: &[Scope],
    ) -> Result<Self, RequestError> {
        let request_id = unhex::<16>(request_id).ok_or(RequestError::BadId)?;
        let nonce = unhex::<32>(nonce).ok_or(RequestError::BadNonce)?;
        Self::new(
            request_id,
            name.to_owned(),
            nonce,
            offered_scopes.iter().copied().collect(),
        )
    }

    #[must_use]
    pub fn id_hex(&self) -> String {
        hex(&self.request_id)
    }

    #[must_use]
    pub fn nonce_hex(&self) -> String {
        hex(&self.nonce)
    }

    /// The scopes in frame order (sorted by the `Scope` order).
    #[must_use]
    pub fn scopes_vec(&self) -> Vec<Scope> {
        self.offered_scopes.iter().copied().collect()
    }

    /// The request digest: SHA-256 of the tag, then the length-prefixed
    /// `request_id`, name, `nonce` and offered scopes.
    #[must_use]
    pub fn digest(&self) -> [u8; 32] {
        let mut body = Vec::new();
        push(&mut body, &self.request_id);
        push(&mut body, self.name.as_bytes());
        push(&mut body, &self.nonce);
        push(&mut body, scopes_text(&self.offered_scopes).as_bytes());
        sha256(&[REQUEST_TAG, &body])
    }
}

/// The digest of a pending record: the request digest, the proven key of
/// the station, the source address (canonical form) and the admission time
/// in Unix milliseconds (decimal text). The owner sends it back to approve.
#[must_use]
pub fn pending_digest(
    request_digest: &[u8; 32],
    station: &Fingerprint,
    source: IpAddr,
    admitted_at_ms: i64,
) -> [u8; 32] {
    let mut body = Vec::new();
    push(&mut body, request_digest);
    push(&mut body, station.as_bytes());
    push(&mut body, source.to_canonical().to_string().as_bytes());
    push(&mut body, admitted_at_ms.to_string().as_bytes());
    sha256(&[PENDING_TAG, &body])
}

/// What a deck signs when it approves a request.
#[derive(Debug, Clone)]
pub struct Approval<'a> {
    /// [`transcript_hash`] of the handshake of this connection.
    pub transcript_hash: [u8; 32],
    pub station: Fingerprint,
    pub deck: Fingerprint,
    pub request: &'a CanonicalRequest,
    /// What the deck grants to the station on the deck.
    pub granted_scopes: &'a BTreeSet<Scope>,
}

impl Approval<'_> {
    /// The signed message: the tag, then the length-prefixed transcript
    /// hash, station key fingerprint, deck key fingerprint, `request_id`,
    /// `nonce`, request digest, offered scopes and granted scopes.
    #[must_use]
    pub fn message(&self) -> Vec<u8> {
        let mut out = APPROVED_TAG.to_vec();
        push(&mut out, &self.transcript_hash);
        push(&mut out, self.station.as_bytes());
        push(&mut out, self.deck.as_bytes());
        push(&mut out, &self.request.request_id);
        push(&mut out, &self.request.nonce);
        push(&mut out, &self.request.digest());
        push(&mut out, scopes_text(&self.request.offered_scopes).as_bytes());
        push(&mut out, scopes_text(self.granted_scopes).as_bytes());
        out
    }

    /// The Ed25519 signature of the deck, 128 hex digits.
    #[must_use]
    pub fn sign(&self, deck: &Identity) -> String {
        hex(&deck.sign(&self.message()))
    }

    /// Checks `signature_hex` against the key of the deck.
    #[must_use]
    pub fn verify(&self, deck: &PeerKey, signature_hex: &str) -> bool {
        unhex::<64>(signature_hex).is_some_and(|sig| deck.verify(&self.message(), &sig))
    }
}

#[cfg(test)]
#[path = "pair_request_tests.rs"]
mod tests;
