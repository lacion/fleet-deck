//! Link handshake v1, Revision 2 (PLAN §8.2, ADR-004 §2.5 and §9.1),
//! sans-IO part.
//!
//! After TLS, both sides send a [`Hello`] at the same time. Each side then:
//! 1. checks the peer `Hello` against the TLS peer key and the expected role,
//! 2. negotiates the protocol version (highest common) and the feature set
//!    (intersection, L16),
//! 3. signs the transcript: both raw `Hello` texts (both identities, roles,
//!    nonces, epochs, channels), both public keys, and the TLS exporter value
//!    of this TLS session, plus its own side tag (no reflection),
//! 4. checks the peer signature with the key that the peer proved in TLS.
//!
//! The transcript uses the exact `Hello` text that went over the wire, so no
//! JSON canonicalization is needed, and unknown fields of a newer peer are
//! signed and kept (they are only ignored by the parser, L16).
//!
//! Revision 2 (G1): the dialer's `Hello` of a control or data channel
//! carries its persistent per-peer `link_epoch` (at least 1) and the random
//! `attempt_id` of the dial attempt; the listener sends epoch 0 and no
//! attempt. Both sides send the `incarnation` of their database (§9.3).

use std::collections::BTreeSet;

use fleetdeck_core::identity::{Identity, PeerKey};
use fleetdeck_proto::IncarnationId;
use fleetdeck_proto::version::{self, NegotiationError, VersionOffer};
use serde::{Deserialize, Serialize};

pub use fleetdeck_proto::version::{Negotiated, PROTO_MAX, PROTO_MIN, feature};

use crate::tls::Role;

/// The TLS exporter label (RFC 8446 §7.5) that binds the link to the session.
pub const EXPORTER_LABEL: &[u8] = b"EXPORTER-fleet-deck-link-v1";

/// The domain separation prefix of the transcript.
pub const TRANSCRIPT_TAG: &[u8] = b"fleet-deck link transcript v1\0";

/// The two WebSocket channels of a link (§8.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Channel {
    Control,
    Data,
    /// Invitation pairing (§8.5): handshake, proof, answer, close.
    Pair,
}

impl Channel {
    /// The WebSocket path of the channel.
    pub fn path(self) -> &'static str {
        match self {
            Self::Control => "/fdlink/v1/control",
            Self::Data => "/fdlink/v1/data",
            Self::Pair => "/fdlink/v1/pair",
        }
    }

    pub fn from_path(path: &str) -> Option<Self> {
        [Self::Control, Self::Data, Self::Pair]
            .into_iter()
            .find(|c| c.path() == path)
    }
}

/// Which side opened the connection. Part of the signed message, so a
/// signature from one side can never be replayed as the other side's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Dialer,
    Listener,
}

impl Side {
    fn tag(self) -> &'static [u8] {
        match self {
            Self::Dialer => b"dialer",
            Self::Listener => b"listener",
        }
    }

    pub fn other(self) -> Self {
        match self {
            Self::Dialer => Self::Listener,
            Self::Listener => Self::Dialer,
        }
    }
}

/// `Hello` (§8.2). Unknown fields from newer peers are ignored by the parser.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hello {
    pub proto_min: u32,
    pub proto_max: u32,
    pub features: Vec<String>,
    pub role: Role,
    /// The full fingerprint (64 hex digits) of the sender's identity key.
    pub node_id: String,
    /// The dialer's persistent per-peer epoch; 0 from the listener (G1).
    pub link_epoch: u64,
    /// The dial attempt (32 hex digits); only from the dialer (G1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt_id: Option<String>,
    /// The incarnation of the sender's database (ADR-004 §9.3).
    pub incarnation: IncarnationId,
    /// 32 random bytes, 64 hex digits.
    pub nonce: String,
    pub channel: Channel,
}

/// Handshake frames, JSON text messages tagged by `type`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HandshakeFrame {
    Hello(Hello),
    /// Ed25519 signature over the transcript and the sender's side, 128 hex.
    Auth {
        signature: String,
    },
    /// The sender refuses the link; both sides report `reason` and `detail`.
    Close {
        reason: String,
        detail: String,
        /// With `stale_link_epoch`: the lowest epoch that the listener
        /// accepts next (G1).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        min_epoch: Option<u64>,
    },
    /// listener → dialer after both `Auth`s: the link epoch is accepted and
    /// the channel is live.
    Ready,
    /// Pair channel, joiner → inviter after both `Auth`s: HMAC of the
    /// invitation secret over the transcript (64 hex), and the non-secret
    /// invitation id (16 hex) that selects the secret (G11, DL-045).
    PairProof {
        invitation: String,
        proof: String,
    },
    /// Pair channel, inviter → joiner: the inviter's HMAC answer. The joiner
    /// pins the inviter only after it checked this answer.
    Paired {
        proof: String,
    },
}

/// Why a handshake failed. `reason()` is the stable code that goes into the
/// `Close` frame; `Display` is the clear message for logs and the board.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HandshakeError {
    #[error(
        "no common link protocol version: this node speaks {local_min}..={local_max}, the peer speaks {peer_min}..={peer_max}"
    )]
    VersionMismatch {
        local_min: u32,
        local_max: u32,
        peer_min: u32,
        peer_max: u32,
    },
    #[error("the peer is a {got:?}, but this link expects a {expected:?}")]
    RoleMismatch { expected: Role, got: Role },
    #[error("the peer Hello names node {claimed}, but TLS proved key {proved}")]
    NodeIdMismatch { claimed: String, proved: String },
    #[error("the peer opened channel {got:?} on the {expected:?} path")]
    ChannelMismatch { expected: Channel, got: Channel },
    #[error("the peer lacks required feature {0}")]
    MissingFeature(String),
    #[error("bad Hello: {0}")]
    BadHello(String),
    #[error("bad transcript signature")]
    BadSignature,
    #[error("the peer refused the link: {reason}: {detail}")]
    PeerClosed { reason: String, detail: String },
    #[error("this node has not paired with the peer's key")]
    NotPaired,
    #[error("link epoch {got} is stale; the listener accepts {min_epoch} or higher")]
    StaleEpoch { got: u64, min_epoch: u64 },
    #[error("link protocol error: {0}")]
    Protocol(String),
}

impl HandshakeError {
    pub fn reason(&self) -> &'static str {
        match self {
            Self::VersionMismatch { .. } => "version_mismatch",
            Self::RoleMismatch { .. } => "role_mismatch",
            Self::NodeIdMismatch { .. } => "node_id_mismatch",
            Self::ChannelMismatch { .. } => "channel_mismatch",
            Self::MissingFeature(_) => "missing_feature",
            Self::BadHello(_) => "bad_hello",
            Self::BadSignature => "bad_signature",
            Self::PeerClosed { .. } => "peer_closed",
            Self::NotPaired => "not_paired",
            Self::StaleEpoch { .. } => "stale_link_epoch",
            Self::Protocol(_) => "protocol_error",
        }
    }

    /// The `Close` frame that tells the peer why.
    pub fn close_frame(&self) -> HandshakeFrame {
        HandshakeFrame::Close {
            reason: self.reason().to_owned(),
            detail: self.to_string(),
            min_epoch: match self {
                Self::StaleEpoch { min_epoch, .. } => Some(*min_epoch),
                _ => None,
            },
        }
    }
}

/// What this node puts into its `Hello` and requires from the peer.
#[derive(Debug, Clone)]
pub struct LocalHello {
    pub role: Role,
    pub incarnation: IncarnationId,
    pub proto_min: u32,
    pub proto_max: u32,
    pub features: Vec<String>,
    /// Features without which this node refuses the link.
    pub required: Vec<String>,
}

impl LocalHello {
    /// This build's defaults for a role and a database incarnation.
    #[must_use]
    pub fn new(role: Role, incarnation: IncarnationId) -> Self {
        let offer = VersionOffer::this_build();
        Self {
            role,
            incarnation,
            proto_min: offer.proto_min,
            proto_max: offer.proto_max,
            features: offer.features,
            required: feature::REQUIRED.iter().map(|f| (*f).to_owned()).collect(),
        }
    }

    /// The dialer's `Hello` of one attempt.
    #[must_use]
    pub fn dialer_hello(
        &self,
        identity: &Identity,
        attempt: &Attempt,
        nonce: String,
        channel: Channel,
    ) -> Hello {
        let mut hello = self.listener_hello(identity, nonce, channel);
        hello.link_epoch = attempt.epoch;
        hello.attempt_id = Some(attempt.attempt_id.clone());
        hello
    }

    /// The listener's `Hello` (and the pair channel's of both sides).
    #[must_use]
    pub fn listener_hello(&self, identity: &Identity, nonce: String, channel: Channel) -> Hello {
        Hello {
            proto_min: self.proto_min,
            proto_max: self.proto_max,
            features: self.features.clone(),
            role: self.role,
            node_id: identity.fingerprint().to_string(),
            link_epoch: 0,
            attempt_id: None,
            incarnation: self.incarnation.clone(),
            nonce,
            channel,
        }
    }
}

/// One dial attempt: the committed per-peer epoch and a random id (G1).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Attempt {
    pub epoch: u64,
    pub attempt_id: String,
}

impl Attempt {
    /// A new attempt with a random 128-bit id.
    pub fn new(epoch: u64) -> Result<Self, HandshakeError> {
        Ok(Self {
            epoch,
            attempt_id: random_hex::<16>()?,
        })
    }
}

/// The role a peer must have: a deck links only to stations and back.
pub fn expected_peer_role(own: Role) -> Role {
    match own {
        Role::Deck => Role::Station,
        Role::Station => Role::Deck,
    }
}

fn is_hex(s: &str, len: usize) -> bool {
    s.len() == len
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// The checks of the Revision 2 fields: the dialer of a control or data
/// channel sends an epoch of at least 1 and a 32-hex `attempt_id`; a
/// listener (and the pair channel) sends epoch 0 and no attempt.
fn check_attempt(peer: &Hello, peer_side: Side) -> Result<(), HandshakeError> {
    let dialed_link = peer_side == Side::Dialer && peer.channel != Channel::Pair;
    match (&peer.attempt_id, dialed_link) {
        (Some(id), true) if is_hex(id, 32) && peer.link_epoch >= 1 => Ok(()),
        (None, false) if peer.link_epoch == 0 => Ok(()),
        (_, true) => Err(HandshakeError::BadHello(
            "a dialer must send link_epoch >= 1 and a 32-hex attempt_id".into(),
        )),
        (_, false) => Err(HandshakeError::BadHello(
            "only the dialer of a link sends an epoch and an attempt_id".into(),
        )),
    }
}

/// Checks the peer `Hello` (sent by `peer_side`) and negotiates version and
/// features.
pub fn negotiate(
    local: &LocalHello,
    own: &Hello,
    peer: &Hello,
    peer_key: &PeerKey,
    channel: Channel,
    peer_side: Side,
) -> Result<Negotiated, HandshakeError> {
    if peer.proto_min > peer.proto_max {
        return Err(HandshakeError::BadHello(format!(
            "proto_min {} > proto_max {}",
            peer.proto_min, peer.proto_max
        )));
    }
    if !is_hex(&peer.nonce, 64) {
        return Err(HandshakeError::BadHello(
            "the nonce is not 32 hex bytes".into(),
        ));
    }
    if peer.nonce == own.nonce {
        return Err(HandshakeError::BadHello(
            "the peer echoed this node's nonce".into(),
        ));
    }
    let proved = peer_key.fingerprint().to_string();
    if peer.node_id != proved {
        return Err(HandshakeError::NodeIdMismatch {
            claimed: peer.node_id.chars().take(64).collect(),
            proved,
        });
    }
    let expected = expected_peer_role(local.role);
    if peer.role != expected {
        return Err(HandshakeError::RoleMismatch {
            expected,
            got: peer.role,
        });
    }
    if peer.channel != channel {
        return Err(HandshakeError::ChannelMismatch {
            expected: channel,
            got: peer.channel,
        });
    }
    check_attempt(peer, peer_side)?;
    let ours = VersionOffer {
        proto_min: local.proto_min,
        proto_max: local.proto_max,
        features: local.features.clone(),
    };
    let theirs = VersionOffer {
        proto_min: peer.proto_min,
        proto_max: peer.proto_max,
        features: peer.features.clone(),
    };
    let negotiated = version::negotiate(&ours, &theirs).map_err(|e| match e {
        NegotiationError::NoCommonVersion { .. } | NegotiationError::InvalidRange { .. } => {
            HandshakeError::VersionMismatch {
                local_min: local.proto_min,
                local_max: local.proto_max,
                peer_min: peer.proto_min,
                peer_max: peer.proto_max,
            }
        }
        NegotiationError::MissingRequiredFeature(f) => HandshakeError::MissingFeature(f),
    })?;
    let have: BTreeSet<&str> = negotiated.features.iter().map(String::as_str).collect();
    if let Some(missing) = local.required.iter().find(|r| !have.contains(r.as_str())) {
        return Err(HandshakeError::MissingFeature(missing.clone()));
    }
    Ok(negotiated)
}

fn push_len_prefixed(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(&u64::try_from(bytes.len()).unwrap_or(u64::MAX).to_be_bytes());
    out.extend_from_slice(bytes);
}

/// The transcript of one handshake. Both sides compute the same bytes.
pub fn transcript(
    exporter: &[u8; 32],
    dialer_hello: &str,
    listener_hello: &str,
    dialer_key: &PeerKey,
    listener_key: &PeerKey,
) -> Vec<u8> {
    let mut out = TRANSCRIPT_TAG.to_vec();
    out.extend_from_slice(exporter);
    push_len_prefixed(&mut out, dialer_hello.as_bytes());
    push_len_prefixed(&mut out, listener_hello.as_bytes());
    out.extend_from_slice(dialer_key.as_bytes());
    out.extend_from_slice(listener_key.as_bytes());
    out
}

fn signed_message(transcript: &[u8], side: Side) -> Vec<u8> {
    let mut m = transcript.to_vec();
    m.extend_from_slice(side.tag());
    m
}

/// The `Auth` frame of `side` for this transcript.
pub fn sign(identity: &Identity, transcript: &[u8], side: Side) -> HandshakeFrame {
    let sig = identity.sign(&signed_message(transcript, side));
    HandshakeFrame::Auth {
        signature: sig.iter().map(|b| format!("{b:02x}")).collect(),
    }
}

/// Checks the peer's `Auth` signature (made as `side`).
pub fn verify(
    peer_key: &PeerKey,
    transcript: &[u8],
    side: Side,
    signature: &str,
) -> Result<(), HandshakeError> {
    if !is_hex(signature, 128) {
        return Err(HandshakeError::BadSignature);
    }
    let mut sig = [0u8; 64];
    for (i, b) in sig.iter_mut().enumerate() {
        *b = u8::from_str_radix(&signature[2 * i..2 * i + 2], 16)
            .map_err(|_| HandshakeError::BadSignature)?;
    }
    if peer_key.verify(&signed_message(transcript, side), &sig) {
        Ok(())
    } else {
        Err(HandshakeError::BadSignature)
    }
}

/// `N` random bytes as lowercase hex.
fn random_hex<const N: usize>() -> Result<String, HandshakeError> {
    use ring::rand::SecureRandom;
    let mut n = [0u8; N];
    ring::rand::SystemRandom::new()
        .fill(&mut n)
        .map_err(|_| HandshakeError::Protocol("the system random generator failed".into()))?;
    Ok(n.iter().map(|b| format!("{b:02x}")).collect())
}

/// 32 random bytes as 64 hex digits.
pub fn new_nonce() -> Result<String, HandshakeError> {
    random_hex::<32>()
}

#[cfg(test)]
#[path = "hello_tests.rs"]
mod tests;
