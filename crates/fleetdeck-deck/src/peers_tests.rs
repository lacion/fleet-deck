//! The end texts of the link cache (G6, design `grant-idle.md` §3.6).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;
use std::time::Duration;

use fleetdeck_core::identity::Identity;
use fleetdeck_proto::StationId;
use fleetdeck_proto::mutation::CoordinatorBinding;
use fleetdeck_testkit::Det;
use serde_json::json;

use super::*;
use crate::store::grants::{GrantClass, NewGrant};

const IDLE_MS: i64 = 1_000;
const LIFETIME_MS: i64 = 10_000;
const IDLE_TEXT: &str =
    "Your fleet grant expired (idle for 1 second). The owner can renew it on the board.";
const LIFETIME_TEXT: &str =
    "Your fleet grant expired (its lifetime ended). The owner can give a new grant on the board.";
const REVOKED_TEXT: &str =
    "Your fleet grant was revoked. The owner can give a new grant on the board.";

struct World {
    det: Det,
    store: DeckStore,
    station: StationId,
    key: PeerKey,
    grant_id: String,
}

/// A station whose only grant (`coord-1`, bound) expired idle.
async fn world() -> World {
    let det = Det::new();
    let store = DeckStore::open_in_memory(det.clock(), det.ids())
        .await
        .unwrap();
    let key = Identity::generate().unwrap().public_key();
    let station = store
        .pin_station(NewStation {
            key,
            name: "alpha".to_owned(),
            scopes: BTreeSet::from([Scope::Admin]),
            dial_url: None,
        })
        .await
        .unwrap()
        .station_id;
    let mut g = NewGrant::new(station.clone(), "coord-1".into());
    g.classes = BTreeSet::from([GrantClass::Mail]);
    g.idle_ttl_ms = IDLE_MS;
    g.ttl_ms = LIFETIME_MS;
    let grant = store.create_grant(g).await.unwrap();
    let binding = CoordinatorBinding {
        binding_id: "b-1".to_owned(),
        grant_id: grant.grant_id.clone(),
        generation: 1,
        provider_instance: "claude".into(),
        native_id: "native-1".to_owned(),
    };
    assert!(store.bind_grant(binding).await.unwrap());
    det.advance(Duration::from_secs(2));
    store.sweep_grants().await.unwrap();
    World {
        det,
        store,
        station,
        key,
        grant_id: grant.grant_id,
    }
}

fn query(caller: &str) -> serde_json::Value {
    json!({ "caller": caller })
}

#[tokio::test]
async fn load_fills_the_end_texts_from_the_store() {
    let w = world().await;
    let peers = DeckPeers::load(w.store.clone()).await.unwrap();
    let fp = w.key.fingerprint();
    assert_eq!(
        peers.refusal(&fp, "coordinator.query", &query("coord-1")),
        Some(IDLE_TEXT.to_owned())
    );
}

#[tokio::test]
async fn refusal_answers_only_for_the_calling_station_its_sessions_and_the_two_methods() {
    let w = world().await;
    let peers = DeckPeers::load(w.store.clone()).await.unwrap();
    let fp = w.key.fingerprint();
    let other = Identity::generate().unwrap().fingerprint();
    assert_eq!(
        peers.refusal(&other, "coordinator.query", &query("coord-1")),
        None
    );
    assert_eq!(
        peers.refusal(&fp, "coordinator.query", &query("coord-9")),
        None
    );
    assert_eq!(
        peers.refusal(&fp, "coordinator.authority_check", &query("coord-1")),
        None
    );
    assert_eq!(peers.refusal(&fp, "coordinator.query", &json!({})), None);
    let call = json!({ "call": { "caller": "coord-1" } });
    assert_eq!(
        peers.refusal(&fp, "coordinator.call", &call),
        Some(IDLE_TEXT.to_owned())
    );
    assert_eq!(
        peers.refusal(&fp, "coordinator.call", &query("coord-1")),
        None
    );
}

#[tokio::test]
async fn a_revoked_key_has_no_end_texts_and_a_rotated_key_keeps_them() {
    let w = world().await;
    let peers = DeckPeers::load(w.store.clone()).await.unwrap();
    let old = w.key;
    let new = Identity::generate().unwrap().public_key();
    peers.rotated(&old, &new).await.unwrap();
    assert_eq!(
        peers.refusal(&old.fingerprint(), "coordinator.query", &query("coord-1")),
        None
    );
    assert_eq!(
        peers.refusal(&new.fingerprint(), "coordinator.query", &query("coord-1")),
        Some(IDLE_TEXT.to_owned())
    );
    peers.revoked(&new).await.unwrap();
    assert_eq!(
        peers.refusal(&new.fingerprint(), "coordinator.query", &query("coord-1")),
        None
    );
}

#[tokio::test]
async fn an_idle_end_becomes_the_lifetime_text_when_the_lifetime_passes() {
    let w = world().await;
    let peers = DeckPeers::load(w.store.clone()).await.unwrap();
    let fp = w.key.fingerprint();
    let ask = || peers.refusal(&fp, "coordinator.query", &query("coord-1"));
    assert_eq!(ask(), Some(IDLE_TEXT.to_owned()));
    // No refresh: the cached end is classified again at the refusal time.
    w.det.advance(Duration::from_millis(LIFETIME_MS as u64));
    assert_eq!(ask(), Some(LIFETIME_TEXT.to_owned()));
}

#[tokio::test]
async fn a_revoke_of_an_idle_expired_grant_changes_the_text() {
    let w = world().await;
    let peers = DeckPeers::load(w.store.clone()).await.unwrap();
    let fp = w.key.fingerprint();
    let scopes_before = w
        .store
        .station(w.station.clone())
        .await
        .unwrap()
        .unwrap()
        .scopes;
    assert!(!scopes_before.contains(&Scope::Coordinate));
    assert!(
        w.store
            .revoke_grant(w.grant_id.clone(), "owner".to_owned())
            .await
            .unwrap()
    );
    peers.refresh_changed().await.unwrap();
    assert_eq!(
        peers.refusal(&fp, "coordinator.query", &query("coord-1")),
        Some(REVOKED_TEXT.to_owned())
    );
    let scopes_after = w
        .store
        .station(w.station.clone())
        .await
        .unwrap()
        .unwrap()
        .scopes;
    assert_eq!(scopes_after, scopes_before);
}
