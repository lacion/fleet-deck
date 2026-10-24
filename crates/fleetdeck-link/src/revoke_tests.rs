#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use fleetdeck_core::identity::Identity;
use fleetdeck_proto::link::Scope;
use fleetdeck_proto::{CommandId, IncarnationId};

use super::*;
use crate::control::{GrantSource, Grants};
use crate::epochs::Epochs;
use crate::hello::LocalHello;
use crate::tls::Role;

fn station() -> (NodeConfig, Arc<Grants>) {
    let id = Arc::new(Identity::generate().unwrap());
    let grants = Grants::new();
    let node = NodeConfig::new(
        id,
        LocalHello::new(Role::Station, IncarnationId::from("inc-s".to_owned())),
        Arc::new(Epochs::in_memory()),
    )
    .with_store(Arc::clone(&grants));
    (node, grants)
}

fn cmd(s: &str) -> CommandId {
    CommandId::from(s.to_owned())
}

#[tokio::test]
async fn revoke_unpins_and_removes_every_grant() {
    let (node, grants) = station();
    let deck = Identity::generate().unwrap();
    node.pins.pin(deck.public_key(), Role::Deck);
    grants.grant(deck.fingerprint(), [Scope::Read, Scope::Drive]);
    assert_eq!(revoke(&node, &deck.public_key()).await, Ok(true));
    assert_eq!(node.pins.role_of(&deck.public_key()), None);
    assert!(grants.scopes(&deck.fingerprint()).is_empty());
    assert_eq!(
        revoke(&node, &deck.public_key()).await,
        Ok(false),
        "already revoked"
    );
}

#[tokio::test]
async fn a_signed_rotation_moves_role_and_grants_and_revokes_the_old_key() {
    let (node, grants) = station();
    let old = Identity::generate().unwrap();
    let new = Identity::generate().unwrap();
    node.pins.pin(old.public_key(), Role::Deck);
    grants.grant(old.fingerprint(), [Scope::Read]);
    let me = node.identity.public_key();
    let r = sign_rotation(&old, &new.public_key(), &me, &cmd("c1"));
    assert_eq!(
        rotate(&node, &old.public_key(), &r, &cmd("c1")).await,
        Ok(new.public_key())
    );
    assert_eq!(node.pins.role_of(&new.public_key()), Some(Role::Deck));
    assert_eq!(node.pins.role_of(&old.public_key()), None);
    assert_eq!(
        grants
            .scopes(&new.fingerprint())
            .into_iter()
            .collect::<Vec<_>>(),
        vec![Scope::Read],
        "only the grants of the old key"
    );
    assert!(grants.scopes(&old.fingerprint()).is_empty());
}

#[tokio::test]
async fn a_rotation_is_refused_unless_every_check_passes() {
    let (node, _) = station();
    let old = Identity::generate().unwrap();
    let other = Identity::generate().unwrap();
    let new = Identity::generate().unwrap();
    node.pins.pin(old.public_key(), Role::Deck);
    node.pins.pin(other.public_key(), Role::Deck);
    let me = node.identity.public_key();
    let good = sign_rotation(&old, &new.public_key(), &me, &cmd("c1"));

    // Another command id, another receiver: the signature does not bind.
    assert_eq!(
        rotate(&node, &old.public_key(), &good, &cmd("c2")).await,
        Err(RotateError::BadSignature)
    );
    let elsewhere = sign_rotation(&old, &new.public_key(), &other.public_key(), &cmd("c1"));
    assert_eq!(
        rotate(&node, &old.public_key(), &elsewhere, &cmd("c1")).await,
        Err(RotateError::BadSignature)
    );
    // The rotation must arrive on the old key's own link.
    assert_eq!(
        rotate(&node, &other.public_key(), &good, &cmd("c1")).await,
        Err(RotateError::NotLinkPeer)
    );
    // Signed by another key.
    let mut forged = sign_rotation(&other, &new.public_key(), &me, &cmd("c1"));
    forged.old_key = old.fingerprint().to_string();
    assert_eq!(
        rotate(&node, &old.public_key(), &forged, &cmd("c1")).await,
        Err(RotateError::BadSignature)
    );
    let mut bad_key = good.clone();
    bad_key.new_key = "zz".into();
    assert_eq!(
        rotate(&node, &old.public_key(), &bad_key, &cmd("c1")).await,
        Err(RotateError::BadNewKey)
    );
    let same = sign_rotation(&old, &old.public_key(), &me, &cmd("c1"));
    assert_eq!(
        rotate(&node, &old.public_key(), &same, &cmd("c1")).await,
        Err(RotateError::SameKey)
    );
    let taken = sign_rotation(&old, &other.public_key(), &me, &cmd("c1"));
    assert_eq!(
        rotate(&node, &old.public_key(), &taken, &cmd("c1")).await,
        Err(RotateError::NewKeyPaired)
    );
    // Nothing changed.
    assert_eq!(node.pins.role_of(&old.public_key()), Some(Role::Deck));
    assert_eq!(node.pins.role_of(&new.public_key()), None);

    // An unpinned old key cannot rotate.
    node.pins.unpin(&old.public_key());
    assert_eq!(
        rotate(&node, &old.public_key(), &good, &cmd("c1")).await,
        Err(RotateError::NotPaired)
    );
}
