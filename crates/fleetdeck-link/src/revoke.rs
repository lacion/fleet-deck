//! Revocation and key rotation on the link layer (PLAN §8.5, ADR-004 §9.5,
//! G5).
//!
//! - [`revoke`] unpins a key, removes its grants through the node's
//!   [`PairingSink`] and closes its active link with reason `revoked`. The
//!   control channel ends at once (well within the 1 s of G5), so a request
//!   that is still queued on that link is never answered with `ok`.
//! - [`sign_rotation`] and [`rotate`]: a paired node signs its new key with
//!   its old key (`link.rotate_key`). The receiver checks the signature and
//!   that the old key is the link's peer, gives the new key only the role
//!   and the grants of the old key, and then revokes the old key.
//!
//! The stores make each of these one transaction with a persistent
//! revocation generation; their decision transactions re-check the pin and
//! the grant (the station and deck services).
//!
//! [`PairingSink`]: crate::pairing::PairingSink

use fleetdeck_core::identity::{Identity, PeerKey};
use fleetdeck_proto::CommandId;
use fleetdeck_proto::mutation::KeyRotation;

use crate::node::NodeConfig;
use crate::pairing::{hex, unhex};
use crate::table::Closing;
use crate::tls::Role;

/// The close reason of a link whose key was revoked.
pub const REVOKED: &str = "revoked";

/// Why a key rotation was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RotateError {
    #[error("old_key is not the pinned key of this link")]
    NotLinkPeer,
    #[error("the old key is not paired")]
    NotPaired,
    #[error("new_key is not an Ed25519 public key in 64 hex digits")]
    BadNewKey,
    #[error("the new key is the old key")]
    SameKey,
    #[error("the new key is already paired")]
    NewKeyPaired,
    #[error("the rotation signature is not valid")]
    BadSignature,
    #[error("the key store failed: {0}")]
    Store(String),
}

/// Revokes `key`: removes its pin and grants through the node's
/// [`PairingSink`] (one transaction in a store), unpins it and closes its
/// active link. Returns `false` when the key was not pinned. A sink error
/// is logged; the key is unpinned and its link closed anyway (fail closed).
pub async fn revoke(node: &NodeConfig, key: &PeerKey) -> bool {
    let fp = key.fingerprint();
    if let Err(e) = node.sink.revoked(key).await {
        tracing::error!(peer = %fp.short(), error = %e, "revocation store");
    }
    let was_pinned = node.pins.unpin(key);
    close_revoked(node, key);
    tracing::info!(peer = %fp.short(), "key revoked");
    was_pinned
}

fn close_revoked(node: &NodeConfig, key: &PeerKey) {
    node.table.close_peer(
        &key.fingerprint(),
        Closing {
            reason: REVOKED.into(),
            detail: "the peer key was revoked".into(),
        },
    );
}

/// The old identity signs the rotation to `new_key` for the receiver with
/// fingerprint `receiver` and the command `command_id`.
#[must_use]
pub fn sign_rotation(
    old: &Identity,
    new_key: &PeerKey,
    receiver: &PeerKey,
    command_id: &CommandId,
) -> KeyRotation {
    let mut rotation = KeyRotation {
        old_key: old.fingerprint().to_string(),
        new_key: hex(new_key.as_bytes()),
        signature: String::new(),
    };
    let message = rotation.signed_message(&receiver.fingerprint().to_string(), command_id);
    rotation.signature = hex(&old.sign(&message));
    rotation
}

/// Checks a `link.rotate_key` that arrived on the link of `link_peer`:
/// `old_key` is the link's pinned key, the new key is valid, new and not
/// paired, and the old key signed it for this node and `command_id`.
/// Returns the new key and the role of the old one. Changes nothing.
pub fn check_rotation(
    node: &NodeConfig,
    link_peer: &PeerKey,
    rotation: &KeyRotation,
    command_id: &CommandId,
) -> Result<(PeerKey, Role), RotateError> {
    let old_fp = link_peer.fingerprint();
    if rotation.old_key != old_fp.to_string() {
        return Err(RotateError::NotLinkPeer);
    }
    let role = node.pins.role_of(link_peer).ok_or(RotateError::NotPaired)?;
    let new_key = unhex::<32>(&rotation.new_key)
        .and_then(|b| PeerKey::from_bytes(b).ok())
        .ok_or(RotateError::BadNewKey)?;
    if new_key == *link_peer {
        return Err(RotateError::SameKey);
    }
    if node.pins.role_of(&new_key).is_some() {
        return Err(RotateError::NewKeyPaired);
    }
    let signature = unhex::<64>(&rotation.signature).ok_or(RotateError::BadSignature)?;
    let message = rotation.signed_message(&node.identity.fingerprint().to_string(), command_id);
    if !link_peer.verify(&message, &signature) {
        return Err(RotateError::BadSignature);
    }
    Ok((new_key, role))
}

/// The link-layer part of a rotation that a store already committed: pins
/// the new key with the old key's role, unpins the old key and closes its
/// link with `revoked`.
pub fn finish_rotation(node: &NodeConfig, old: &PeerKey, new: PeerKey, role: Role) {
    node.pins.pin(new, role);
    node.pins.unpin(old);
    close_revoked(node, old);
    tracing::info!(
        old = %old.fingerprint().short(),
        new = %new.fingerprint().short(),
        "peer key rotated; the old key is revoked"
    );
}

/// Checks and applies a `link.rotate_key` through the node's
/// [`PairingSink`]. Returns the new key, which now has the role and grants
/// of the old one. A store that keeps command receipts uses
/// [`check_rotation`], its own transaction and [`finish_rotation`] instead.
pub async fn rotate(
    node: &NodeConfig,
    link_peer: &PeerKey,
    rotation: &KeyRotation,
    command_id: &CommandId,
) -> Result<PeerKey, RotateError> {
    let (new_key, role) = check_rotation(node, link_peer, rotation, command_id)?;
    node.sink
        .rotated(link_peer, &new_key)
        .await
        .map_err(RotateError::Store)?;
    finish_rotation(node, link_peer, new_key, role);
    Ok(new_key)
}

#[cfg(test)]
#[path = "revoke_tests.rs"]
mod tests;
