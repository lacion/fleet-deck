//! The writes that change a grant that already ended note its station, so
//! that the link cache reads the end texts again (G6, design
//! `grant-idle.md` §3.6, R2-3).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;
use std::time::Duration;

use fleetdeck_core::identity::Identity;
use fleetdeck_proto::event::{Event, EventBody};
use fleetdeck_proto::link::Scope;
use fleetdeck_proto::mutation::CoordinatorBinding;
use fleetdeck_proto::{
    CoordinatorBindingPhase as Phase, CoordinatorBindingState, EventEnvelope, IncarnationId,
    StationId,
};
use fleetdeck_testkit::Det;

use super::super::grants::{GrantClass, NewGrant};
use super::super::stations::NewStation;
use super::super::worker_grant::revoke_tx;
use super::super::{DeckStore, call};

struct World {
    det: Det,
    s: DeckStore,
    station: StationId,
    inc: IncarnationId,
}

async fn world() -> World {
    let det = Det::new();
    let s = DeckStore::open_in_memory(det.clock(), det.ids())
        .await
        .unwrap();
    let station = s
        .pin_station(NewStation {
            key: Identity::generate().unwrap().public_key(),
            name: "alpha".to_owned(),
            scopes: BTreeSet::from([Scope::Admin]),
            dial_url: None,
        })
        .await
        .unwrap()
        .station_id;
    let inc: IncarnationId = "inc".to_owned().into();
    s.apply_snapshot(station.clone(), inc.clone(), 0, Vec::new())
        .await
        .unwrap();
    World {
        det,
        s,
        station,
        inc,
    }
}

/// A grant of `coord-1` with the given times; `bind` makes it bound.
async fn grant(w: &World, ttl_ms: i64, idle_ttl_ms: i64, bind: bool) -> String {
    let mut g = NewGrant::new(w.station.clone(), "coord-1".into());
    g.classes = BTreeSet::from([GrantClass::Mail]);
    g.ttl_ms = ttl_ms;
    g.idle_ttl_ms = idle_ttl_ms;
    let grant = w.s.create_grant(g).await.unwrap();
    if bind {
        let binding = CoordinatorBinding {
            binding_id: "b-1".to_owned(),
            grant_id: grant.grant_id.clone(),
            generation: 1,
            provider_instance: "claude".into(),
            native_id: "native-1".to_owned(),
        };
        assert!(w.s.bind_grant(binding).await.unwrap());
    }
    grant.grant_id
}

/// An idle-expired bound grant; the noted stations are drained.
async fn idle_expired(w: &World) -> String {
    let id = grant(w, 600_000, 1_000, true).await;
    w.det.advance(Duration::from_secs(2));
    w.s.sweep_grants().await.unwrap();
    drop(w.s.take_scope_changes());
    id
}

async fn revoke(w: &World, grant_id: &str) -> (bool, BTreeSet<StationId>) {
    let now = w.s.now_ms();
    let id = grant_id.to_owned();
    call(&w.s.inner.db, move |conn| {
        let tx = conn.transaction()?;
        let out = revoke_tx(&tx, &id, "owner", now)?;
        tx.commit()?;
        Ok(out)
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn a_revoke_of_an_ended_grant_notes_its_station() {
    let w = world().await;
    let id = idle_expired(&w).await;
    let (changed, stations) = revoke(&w, &id).await;
    assert!(changed);
    assert!(stations.contains(&w.station), "{stations:?}");
    let (changed, stations) = revoke(&w, &id).await;
    assert!(!changed);
    assert!(stations.is_empty(), "{stations:?}");
}

#[tokio::test]
async fn a_coordinator_ended_for_an_ended_grant_notes_its_station() {
    let w = world().await;
    idle_expired(&w).await;
    let event = EventEnvelope {
        station_seq: 1,
        session_id: "coord-1".into(),
        session_seq: 1,
        at: w.s.now_ms(),
        proto_version: 1,
        raw_ref: None,
        event: EventBody::Known(Event::CoordinatorEnded {
            binding_id: "b-1".to_owned(),
            generation: 1,
            reason: "revoked".to_owned(),
        }),
    };
    w.s.apply_events(w.station.clone(), w.inc.clone(), vec![event])
        .await
        .unwrap();
    assert!(w.s.take_scope_changes().contains(&w.station));
}

#[tokio::test]
async fn a_snapshot_that_revokes_an_ended_unbound_grant_notes_its_station() {
    let w = world().await;
    // An unbound grant that ended for its lifetime: the station has no
    // `coordinate` scope any more.
    let id = grant(&w, 1_000, 600_000, false).await;
    w.det.advance(Duration::from_secs(2));
    w.s.sweep_grants().await.unwrap();
    drop(w.s.take_scope_changes());
    let projection = CoordinatorBindingState {
        binding_id: "b-1".to_owned(),
        grant_id: id,
        session_id: "coord-1".into(),
        generation: 1,
        provider_instance: "claude".to_owned(),
        native_id: None,
        state: Phase::Revoked,
    };
    w.s.apply_snapshot_with(
        w.station.clone(),
        w.inc.clone(),
        1,
        Vec::new(),
        Some(vec![projection]),
        None,
    )
    .await
    .unwrap();
    assert!(w.s.take_scope_changes().contains(&w.station));
}
