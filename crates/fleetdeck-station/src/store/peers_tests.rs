use std::collections::BTreeSet;

use fleetdeck_core::identity::{Identity, PeerKey};
use fleetdeck_link::tls::Role;
use fleetdeck_proto::command::{
    CommandEnvelope, CommandOrigin, CommandResult, ReceiptState, SessionCommand, UserInput,
};
use fleetdeck_proto::link::Scope;
use fleetdeck_proto::{CommandId, RejectCode};
use fleetdeck_testkit::Det;

use fleetdeck_proto::command::RETRY_WINDOW_MS;

use super::super::{Caller, StoreConfig, call};
use super::*;

async fn store(det: &Det) -> Store {
    let s = Store::open_in_memory(det.clock(), det.ids(), StoreConfig::default())
        .await
        .unwrap();
    s.recover(Vec::new()).await.unwrap();
    s
}

fn key() -> PeerKey {
    Identity::generate().unwrap().public_key()
}

fn deck(key: PeerKey, scopes: &[Scope]) -> PairedKey {
    PairedKey {
        key,
        role: Role::Deck,
        scopes: scopes.iter().copied().collect(),
        dial_url: Some("wss://127.0.0.1:7000".to_owned()),
    }
}

fn envelope(det: &Det, id: &str) -> CommandEnvelope {
    CommandEnvelope {
        authority: None,
        command_id: CommandId::from(id),
        target_incarnation: None,
        retry_until: i64::try_from(det.now_ms()).unwrap() + RETRY_WINDOW_MS,
        origin: CommandOrigin::Board,
        command: SessionCommand::Prompt {
            session_id: "s1".into(),
            input: UserInput::new("hi".to_owned()),
        },
    }
}

async fn decide_as(s: &Store, det: &Det, id: &str, caller: Caller) -> super::super::Decided {
    s.decide(envelope(det, id), caller, |_| Ok(CommandResult::default()))
        .await
        .unwrap()
}

async fn ack_rows(s: &Store) -> Vec<String> {
    call(&s.inner.db, |conn| {
        let mut stmt = conn.prepare("SELECT deck_key FROM deck_acks ORDER BY deck_key")?;
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    })
    .await
    .unwrap()
}

#[test]
fn keys_round_trip_through_hex() {
    let k = key();
    assert_eq!(key_from_hex(&key_hex(&k)), Some(k));
    assert_eq!(key_from_hex("zz"), None);
    assert_eq!(key_from_hex(&"g".repeat(64)), None);
}

#[tokio::test]
async fn a_pin_and_its_grants_are_stored_and_listed() {
    let det = Det::new();
    let s = store(&det).await;
    let k = key();
    s.pin_peer(deck(k, &[Scope::Read, Scope::Drive]))
        .await
        .unwrap();
    assert_eq!(
        s.paired_keys().await.unwrap(),
        vec![deck(k, &[Scope::Read, Scope::Drive])]
    );
    // Pinning again replaces the grants; it adds no second row.
    s.pin_peer(deck(k, &[Scope::Read])).await.unwrap();
    let keys = s.paired_keys().await.unwrap();
    assert_eq!(keys.len(), 1);
    assert_eq!(keys[0].scopes, BTreeSet::from([Scope::Read]));
    assert_eq!(s.revocation_generation().await.unwrap(), 0);
}

#[tokio::test]
async fn the_decision_transaction_checks_the_pin_and_the_grant() {
    let det = Det::new();
    let s = store(&det).await;
    let paired = key();
    let stranger = key();
    s.pin_peer(deck(paired, &[Scope::Read, Scope::Drive]))
        .await
        .unwrap();

    let ok = decide_as(
        &s,
        &det,
        "c1",
        Caller::peer(paired.fingerprint(), Scope::Drive),
    )
    .await;
    assert_eq!(ok.receipt.state, ReceiptState::Accepted);

    for (id, caller) in [
        ("c2", Caller::peer(paired.fingerprint(), Scope::Approve)),
        ("c3", Caller::peer(stranger.fingerprint(), Scope::Drive)),
    ] {
        let d = decide_as(&s, &det, id, caller).await;
        assert_eq!(d.receipt.state, ReceiptState::Rejected, "{id}");
        assert_eq!(
            d.receipt.reject.unwrap().code,
            RejectCode::Forbidden,
            "{id}"
        );
        assert_eq!(
            s.receipt(id.into()).await.unwrap(),
            None,
            "{id}: not stored"
        );
    }
}

#[tokio::test]
async fn a_revocation_removes_pin_grants_and_ack_and_grows_the_generation() {
    let det = Det::new();
    let s = store(&det).await;
    let k = key();
    s.pin_peer(deck(k, &[Scope::Read, Scope::Drive]))
        .await
        .unwrap();
    s.ack(key_hex(&k), 0).await.unwrap();
    assert_eq!(ack_rows(&s).await, vec![key_hex(&k)]);

    assert!(s.revoke_peer(k).await.unwrap());
    assert!(s.paired_keys().await.unwrap().is_empty());
    assert!(ack_rows(&s).await.is_empty(), "the ack goes with the pin");
    assert_eq!(s.revocation_generation().await.unwrap(), 1);
    assert!(!s.revoke_peer(k).await.unwrap(), "nothing left to revoke");
    assert_eq!(s.revocation_generation().await.unwrap(), 1);

    // A command queued on the revoked link is refused.
    let d = decide_as(
        &s,
        &det,
        "late",
        Caller::peer(k.fingerprint(), Scope::Drive),
    )
    .await;
    assert_eq!(d.receipt.reject.unwrap().code, RejectCode::Forbidden);
}

#[tokio::test]
async fn a_rotation_moves_role_grants_and_ack_to_the_new_key_only() {
    let det = Det::new();
    let s = store(&det).await;
    let (old, new, other) = (key(), key(), key());
    s.pin_peer(deck(old, &[Scope::Read, Scope::Admin]))
        .await
        .unwrap();
    // A key of another role stays untouched (one deck per station, DL-048).
    let mut other_key = deck(other, &[Scope::Read]);
    other_key.role = Role::Station;
    s.pin_peer(other_key).await.unwrap();
    s.ack(key_hex(&old), 0).await.unwrap();

    let now = s.now_ms();
    call(&s.inner.db, move |conn| {
        let tx = conn.transaction()?;
        rotate_in(&tx, &old, &new, now)?;
        tx.commit()?;
        Ok(())
    })
    .await
    .unwrap();

    let keys = s.paired_keys().await.unwrap();
    let moved: Vec<_> = keys.iter().filter(|p| p.key == new).collect();
    assert_eq!(moved.len(), 1);
    assert_eq!(moved[0].role, Role::Deck);
    assert_eq!(moved[0].scopes, BTreeSet::from([Scope::Read, Scope::Admin]));
    assert_eq!(moved[0].dial_url.as_deref(), Some("wss://127.0.0.1:7000"));
    assert!(keys.iter().all(|p| p.key != old), "the old key is revoked");
    assert_eq!(keys.len(), 2);
    assert_eq!(ack_rows(&s).await, vec![key_hex(&new)]);
    assert_eq!(s.revocation_generation().await.unwrap(), 1);

    // Rotating an unknown key changes nothing.
    let err = call(&s.inner.db, move |conn| rotate_in(conn, &old, &key(), now))
        .await
        .unwrap_err();
    assert!(matches!(err, StoreError::UnknownDeck(_)), "{err}");
    assert_eq!(s.revocation_generation().await.unwrap(), 1);
}

#[tokio::test]
async fn a_second_deck_key_is_refused_in_the_pin_transaction_until_the_first_is_revoked() {
    let det = Det::new();
    let s = store(&det).await;
    let (first, second) = (key(), key());
    s.pin_peer(deck(first, &[Scope::Read])).await.unwrap();

    let err = s.pin_peer(deck(second, &[Scope::Read])).await.unwrap_err();
    assert!(
        matches!(&err, StoreError::DeckPaired(fp) if *fp == first.fingerprint().to_string()),
        "{err}"
    );
    // The paired deck may pin again (new grants or address).
    s.pin_peer(deck(first, &[Scope::Read, Scope::Drive]))
        .await
        .unwrap();
    let keys = s.paired_keys().await.unwrap();
    assert_eq!(keys.len(), 1);
    assert_eq!(keys[0].key, first);
    assert_eq!(keys[0].scopes.len(), 2);

    assert!(s.revoke_peer(first).await.unwrap());
    s.pin_peer(deck(second, &[Scope::Read])).await.unwrap();
    let keys = s.paired_keys().await.unwrap();
    assert_eq!(keys.len(), 1);
    assert_eq!(keys[0].key, second);
}
