//! The attach proof and the peer check (ADR-005 §3 handshake).
//!
//! `proof = HMAC-SHA256(key, nonce ‖ incarnation ‖ generation)`. The key is
//! 32 random bytes per shim; it travels to the shim through an inherited
//! pipe and lives in the 0600 key file, never in argv, the environment, the
//! database, events or logs. The parts have a length prefix, so two
//! different triples never give the same message.

use fleetdeck_core::Secret;
use ring::hmac;
use ring::rand::{SecureRandom, SystemRandom};

/// The size of the attach key.
pub const KEY_BYTES: usize = 32;
/// The size of the hello nonce.
pub const NONCE_BYTES: usize = 32;

/// The attach key of one shim.
pub struct AttachKey(Secret<[u8; KEY_BYTES]>);

impl std::fmt::Debug for AttachKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AttachKey(<redacted>)")
    }
}

#[derive(Debug, thiserror::Error)]
#[error("the system random source failed")]
pub struct RandomError;

impl AttachKey {
    /// A new random key.
    pub fn generate() -> Result<Self, RandomError> {
        let mut key = [0u8; KEY_BYTES];
        SystemRandom::new()
            .fill(&mut key)
            .map_err(|_| RandomError)?;
        Ok(Self(Secret::new(key)))
    }

    /// The key from its bytes (the key file or the inherited pipe). Any other
    /// length is refused.
    #[must_use]
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        let key: [u8; KEY_BYTES] = bytes.try_into().ok()?;
        Some(Self(Secret::new(key)))
    }

    /// The bytes, for the key file and the pipe only.
    #[must_use]
    pub fn expose(&self) -> &[u8; KEY_BYTES] {
        self.0.expose()
    }

    /// The proof of an attach.
    #[must_use]
    pub fn proof(&self, nonce: &[u8], incarnation: &str, generation: u64) -> Vec<u8> {
        let key = hmac::Key::new(hmac::HMAC_SHA256, self.0.expose());
        hmac::sign(&key, &message(nonce, incarnation, generation))
            .as_ref()
            .to_vec()
    }

    /// True if `proof` is the proof of the triple (constant time).
    #[must_use]
    pub fn verify(&self, nonce: &[u8], incarnation: &str, generation: u64, proof: &[u8]) -> bool {
        let key = hmac::Key::new(hmac::HMAC_SHA256, self.0.expose());
        hmac::verify(&key, &message(nonce, incarnation, generation), proof).is_ok()
    }
}

/// A new random nonce.
pub fn nonce() -> Result<[u8; NONCE_BYTES], RandomError> {
    let mut nonce = [0u8; NONCE_BYTES];
    SystemRandom::new()
        .fill(&mut nonce)
        .map_err(|_| RandomError)?;
    Ok(nonce)
}

fn message(nonce: &[u8], incarnation: &str, generation: u64) -> Vec<u8> {
    let mut m = Vec::with_capacity(16 + nonce.len() + incarnation.len());
    m.extend_from_slice(&(nonce.len() as u64).to_be_bytes());
    m.extend_from_slice(nonce);
    m.extend_from_slice(&(incarnation.len() as u64).to_be_bytes());
    m.extend_from_slice(incarnation.as_bytes());
    m.extend_from_slice(&generation.to_be_bytes());
    m
}

/// The peer check of handshake step 1: the peer must run as our own user.
/// `peer` is `None` when the platform gave no credentials; that fails
/// closed.
#[must_use]
pub fn peer_allowed(peer: Option<u32>, own: u32) -> bool {
    peer == Some(own)
}

/// What the shim decides about an `attach` frame (handshake step 4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttachDecision {
    Accept,
    Refuse(super::wire::Refusal),
}

/// Handshake step 4: the proof, the incarnation and a rising generation.
/// The proof is checked first, so a station without the key learns nothing
/// about the incarnation or the generation.
#[must_use]
pub fn decide_attach(
    key: &AttachKey,
    nonce: &[u8],
    own_incarnation: &str,
    current_generation: u64,
    incarnation: &str,
    generation: u64,
    proof: &str,
) -> AttachDecision {
    use super::wire::Refusal;
    let Some(proof) = super::wire::unhex(proof) else {
        return AttachDecision::Refuse(Refusal::BadProof);
    };
    if !key.verify(nonce, incarnation, generation, &proof) {
        return AttachDecision::Refuse(Refusal::BadProof);
    }
    if incarnation != own_incarnation {
        return AttachDecision::Refuse(Refusal::ForeignStation);
    }
    if generation <= current_generation {
        return AttachDecision::Refuse(Refusal::StaleOwner);
    }
    AttachDecision::Accept
}
