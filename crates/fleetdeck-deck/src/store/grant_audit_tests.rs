//! The audit rows of grants that a station event keeps or drops, and the
//! drops by station removal (B26, design `grants-update.md` §3.4, §3.5).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;

use fleetdeck_core::identity::{Identity, PeerKey};
use fleetdeck_proto::event::{Event, EventBody};
use fleetdeck_proto::link::Scope;
use fleetdeck_proto::mutation::CoordinatorBinding;
use fleetdeck_proto::{
    CoordinatorBindingPhase as Phase, CoordinatorBindingState, EventEnvelope, StationId,
};
use fleetdeck_testkit::Det;

use super::*;
use crate::store::grants::{Grant, GrantClass, GrantState, NewGrant};
use crate::store::stations::NewStation;
use crate::store::{DeckStore, call};

struct World {
    s: DeckStore,
    a: StationId,
    b: StationId,
    c: StationId,
    key_a: PeerKey,
}

async fn pin(s: &DeckStore, name: &str, key: PeerKey) -> StationId {
    s.pin_station(NewStation {
        key,
        name: name.to_owned(),
        scopes: BTreeSet::from([Scope::Admin]),
        dial_url: None,
    })
    .await
    .unwrap()
    .station_id
}

/// Stations `st-1` and `st-2` (the grant stations) and `st-3` (a target).
async fn world() -> World {
    let det = Det::new();
    let s = DeckStore::open_in_memory(det.clock(), det.ids())
        .await
        .unwrap();
    let key_a = Identity::generate().unwrap().public_key();
    let a = pin(&s, "st-1", key_a).await;
    let b = pin(&s, "st-2", Identity::generate().unwrap().public_key()).await;
    let c = pin(&s, "st-3", Identity::generate().unwrap().public_key()).await;
    s.apply_snapshot(a.clone(), "inc".to_owned().into(), 0, Vec::new())
        .await
        .unwrap();
    World { s, a, b, c, key_a }
}

async fn grant(w: &World, own: &StationId, session: &str, targets: &[&StationId]) -> Grant {
    let mut g = NewGrant::new(own.clone(), session.into());
    g.classes = BTreeSet::from([GrantClass::Mail]);
    g.stations.extend(targets.iter().map(|s| (*s).clone()));
    w.s.create_grant(g).await.unwrap()
}

/// Makes `g` a worker grant of start `cmd`.
async fn make_worker(w: &World, g: &Grant, cmd: &str) {
    let (id, cmd) = (g.grant_id.clone(), cmd.to_owned());
    call(&w.s.inner.db, move |conn| {
        conn.execute(
            "UPDATE tokens SET kind = 'worker', start_command_id = ?2 WHERE grant_id = ?1",
            rusqlite::params![id, cmd],
        )?;
        Ok(())
    })
    .await
    .unwrap();
}

fn binding(g: &Grant) -> CoordinatorBinding {
    CoordinatorBinding {
        binding_id: "b-1".to_owned(),
        grant_id: g.grant_id.clone(),
        generation: 1,
        provider_instance: "claude".into(),
        native_id: "native-1".to_owned(),
    }
}

fn event(w: &World, seq: u64, session: &str, event: Event) -> EventEnvelope {
    EventEnvelope {
        station_seq: seq,
        session_id: session.into(),
        session_seq: seq,
        at: w.s.now_ms(),
        proto_version: 1,
        raw_ref: None,
        event: EventBody::Known(event),
    }
}

async fn apply(w: &World, seq: u64, session: &str, e: Event) {
    let env = event(w, seq, session, e);
    w.s.apply_events(w.a.clone(), "inc".to_owned().into(), vec![env])
        .await
        .unwrap();
}

fn ended(reason: &str) -> Event {
    Event::CoordinatorEnded {
        binding_id: "b-1".to_owned(),
        generation: 1,
        reason: reason.to_owned(),
    }
}

async fn state(w: &World, g: &Grant) -> (GrantState, Option<String>) {
    let got = w.s.grant(g.grant_id.clone()).await.unwrap().unwrap();
    (got.state, got.revoked_reason)
}

/// The `(target, detail)` of the audit rows with `action`, oldest first.
async fn rows(w: &World, action: &str) -> Vec<(String, String)> {
    let mut all: Vec<_> =
        w.s.audit_log(1000)
            .await
            .unwrap()
            .into_iter()
            .filter(|r| r.action == action)
            .map(|r| (r.target.unwrap(), r.detail.unwrap()))
            .collect();
    all.reverse();
    all
}

#[tokio::test]
async fn a_process_exit_that_revokes_a_grant_writes_one_dropped_row() {
    let w = world().await;
    let g = grant(&w, &w.a, "coord-1", &[]).await;
    assert!(w.s.bind_grant(binding(&g)).await.unwrap());
    apply(&w, 1, "coord-1", ended("process_exit")).await;
    assert_eq!(
        state(&w, &g).await,
        (GrantState::Revoked, Some("process_exit".to_owned()))
    );
    assert_eq!(
        rows(&w, DROPPED).await,
        vec![(
            g.grant_id.clone(),
            "reason=process_exit;cause=process_exit;station=st-1".to_owned()
        )]
    );
}

#[tokio::test]
async fn a_session_end_that_revokes_a_worker_grant_writes_one_dropped_row() {
    let w = world().await;
    let g = grant(&w, &w.a, "worker-1", &[]).await;
    make_worker(&w, &g, "cmd-1").await;
    apply(&w, 1, "worker-1", ended_session()).await;
    assert_eq!(
        state(&w, &g).await,
        (GrantState::Revoked, Some("session_ended".to_owned()))
    );
    assert_eq!(
        rows(&w, DROPPED).await,
        vec![(
            g.grant_id.clone(),
            "reason=session_ended;cause=session_ended;station=st-1".to_owned()
        )]
    );
}

fn ended_session() -> Event {
    Event::SessionEnded {
        reason: "start_failed".into(),
        exit_code: None,
    }
}

#[tokio::test]
async fn a_worker_outcome_names_the_station_reason_as_the_cause() {
    let w = world().await;
    let g = grant(&w, &w.a, "worker-1", &[]).await;
    make_worker(&w, &g, "cmd-1").await;
    let outcome = Event::CoordinatorWorkerOutcome {
        grant_id: g.grant_id.clone(),
        start_command_id: "cmd-1".into(),
        outcome: fleetdeck_proto::event::WorkerOutcome::Skipped,
        reason: "resumed".to_owned(),
    };
    apply(&w, 1, "worker-1", outcome).await;
    assert_eq!(
        rows(&w, DROPPED).await,
        vec![(
            g.grant_id.clone(),
            "reason=handoff_skipped;cause=resumed;station=st-1".to_owned()
        )]
    );
}

#[tokio::test]
async fn an_event_that_changes_no_grant_writes_no_row() {
    let w = world().await;
    let g = grant(&w, &w.a, "coord-1", &[]).await;
    // A first `coordinator.bound` binds the grant and ends nothing.
    let bound = Event::CoordinatorBound {
        binding_id: "b-1".to_owned(),
        grant_id: g.grant_id.clone(),
        generation: 1,
        provider_instance: "claude".to_owned(),
        native_id: "native-1".to_owned(),
    };
    apply(&w, 1, "coord-1", bound).await;
    assert!(rows(&w, DROPPED).await.is_empty());
    // An event of another kind.
    let run = Event::SessionEnded {
        reason: "x".into(),
        exit_code: None,
    };
    apply(&w, 2, "other", run).await;
    assert!(rows(&w, DROPPED).await.is_empty());
    // The end writes one row; a duplicate and an end of a revoked grant write none.
    apply(&w, 3, "coord-1", ended("process_exit")).await;
    assert_eq!(rows(&w, DROPPED).await.len(), 1);
    apply(&w, 3, "coord-1", ended("process_exit")).await;
    apply(&w, 4, "coord-1", ended("process_exit")).await;
    assert_eq!(rows(&w, DROPPED).await.len(), 1);
}

#[tokio::test]
async fn a_snapshot_that_ends_a_grant_writes_a_row_with_the_cause_snapshot() {
    let w = world().await;
    let g = grant(&w, &w.a, "coord-1", &[]).await;
    assert!(w.s.bind_grant(binding(&g)).await.unwrap());
    let p = CoordinatorBindingState {
        binding_id: "b-1".to_owned(),
        grant_id: g.grant_id.clone(),
        session_id: "coord-1".into(),
        generation: 1,
        provider_instance: "claude".to_owned(),
        native_id: Some("native-1".to_owned()),
        state: Phase::Expired,
    };
    w.s.apply_snapshot_with(
        w.a.clone(),
        "inc".to_owned().into(),
        1,
        Vec::new(),
        Some(vec![p]),
        None,
    )
    .await
    .unwrap();
    assert_eq!(
        rows(&w, DROPPED).await,
        vec![(
            g.grant_id.clone(),
            "reason=idle;cause=snapshot;station=st-1".to_owned()
        )]
    );
}

#[tokio::test]
async fn removing_a_station_revokes_the_grants_of_its_sessions() {
    let w = world().await;
    let active = grant(&w, &w.a, "s1", &[&w.c]).await;
    let lapsed = grant(&w, &w.a, "s2", &[]).await;
    assert!(w.s.bind_grant(binding(&lapsed)).await.unwrap());
    apply(&w, 1, "s2", ended("idle")).await;
    assert_eq!(state(&w, &lapsed).await.0, GrantState::Expired);
    let other = grant(&w, &w.b, "s3", &[&w.a]).await;
    drop(w.s.take_scope_changes());
    let rec = w.s.revoke_station(w.key_a).await.unwrap().unwrap();
    assert_eq!(rec.station_id, w.a);
    assert_eq!(
        state(&w, &active).await,
        (GrantState::Revoked, Some("station_removed".to_owned()))
    );
    assert_eq!(
        state(&w, &lapsed).await,
        (GrantState::Revoked, Some("station_removed".to_owned()))
    );
    let dropped = rows(&w, DROPPED).await;
    let removed: Vec<_> = dropped
        .iter()
        .filter(|(_, d)| d == "reason=station_removed;station=st-1")
        .collect();
    assert_eq!(removed.len(), 2);
    // A grant of another station that names the removed station stays.
    assert_eq!(state(&w, &other).await.0, GrantState::Active);
    // st-3 lost `coordinate` (only the removed station's grant named it).
    assert!(w.s.take_scope_changes().contains(&w.c));
}

#[tokio::test]
async fn a_key_rotation_keeps_the_grants_and_writes_kept_rows() {
    let w = world().await;
    let g = grant(&w, &w.a, "coord-1", &[]).await;
    let new = Identity::generate().unwrap().public_key();
    w.s.rotate_station(w.key_a, new).await.unwrap();
    assert_eq!(state(&w, &g).await.0, GrantState::Active);
    assert_eq!(
        rows(&w, KEPT).await,
        vec![(
            g.grant_id.clone(),
            "reason=key_rotated;station=st-1".to_owned()
        )]
    );
    assert!(rows(&w, DROPPED).await.is_empty());
}
