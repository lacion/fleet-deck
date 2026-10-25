//! A running turn is use of a bound grant (G6, design `grant-idle.md` §3
//! and §6 T1). The in-memory store has a test clock; a link is `up(token)`
//! and `down(token)`; the stream is recorded with `record_link_stream`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;
use std::time::Duration;

use fleetdeck_core::identity::Identity;
use fleetdeck_proto::command::GrantKind;
use fleetdeck_proto::event::{Event, EventBody, RunOrigin, SessionState, TurnStatus};
use fleetdeck_proto::link::Scope;
use fleetdeck_proto::mutation::{CoordinatorBinding, CoordinatorQuery, CoordinatorQueryEnvelope};
use fleetdeck_proto::{EventEnvelope, IncarnationId, RunId, SessionSummary, StationId};
use fleetdeck_testkit::Det;

use super::*;
use crate::store::grants::{GrantClass, GrantState, NewGrant};
use crate::store::stations::NewStation;

const INC: &str = "inc";

struct World {
    det: Det,
    s: DeckStore,
    a: StationId,
    b: StationId,
}

async fn pin(s: &DeckStore, name: &str) -> StationId {
    s.pin_station(NewStation {
        key: Identity::generate().unwrap().public_key(),
        name: name.to_owned(),
        scopes: BTreeSet::from([Scope::Admin]),
        dial_url: None,
    })
    .await
    .unwrap()
    .station_id
}

async fn world() -> World {
    let det = Det::new();
    let s = DeckStore::open_in_memory(det.clock(), det.ids())
        .await
        .unwrap();
    let a = pin(&s, "alpha").await;
    let b = pin(&s, "beta").await;
    World { det, s, a, b }
}

fn session(id: &str, running: bool) -> SessionSummary {
    let mut summary = SessionSummary::new(id.into());
    summary.state = SessionState::Running;
    if running {
        summary.current_run = Some(RunId::from("r1".to_owned()));
    }
    summary
}

/// A snapshot of `station` at `watermark` (the cursor).
async fn seed(w: &World, station: &StationId, sessions: Vec<SessionSummary>, watermark: u64) {
    w.s.apply_snapshot(station.clone(), INC.into(), watermark, sessions)
        .await
        .unwrap();
}

fn up(w: &World, station: &StationId, token: u64) {
    w.s.set_reachable(station, true);
    w.s.set_link_features(station, token, vec![]);
}

fn down(w: &World, station: &StationId, token: u64) {
    w.s.clear_link_features(station, token);
    w.s.set_reachable(station, false);
}

async fn answer(w: &World, station: &StationId, token: u64, head: u64) -> usize {
    w.s.record_link_stream(station.clone(), token, (INC.into(), head))
        .await
        .unwrap()
}

fn advance(w: &World, secs: u64) {
    w.det.advance(Duration::from_secs(secs));
}

fn envelope(w: &World, seq: u64, session: &str, event: Event) -> EventEnvelope {
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

fn run_started() -> Event {
    Event::RunStarted {
        run_id: RunId::from("r2".to_owned()),
        input_summary: "go".to_owned(),
        origin: RunOrigin::Board,
        author: None,
        input_id: None,
        input_text: None,
    }
}

fn run_completed(run: &str) -> Event {
    Event::RunCompleted {
        run_id: RunId::from(run.to_owned()),
        status: TurnStatus::Done,
        background_pending: false,
    }
}

async fn events(w: &World, station: &StationId, list: Vec<EventEnvelope>) {
    w.s.apply_events(station.clone(), INC.into(), list)
        .await
        .unwrap();
}

/// A grant of `session` on `station` with the idle time `idle_ms`, bound.
async fn bound(w: &World, station: &StationId, session: &str, idle_ms: i64) -> Grant {
    let mut new = NewGrant::new(station.clone(), session.into());
    new.idle_ttl_ms = idle_ms;
    let g = w.s.create_grant(new).await.unwrap();
    bind(w, &g).await;
    g
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

async fn bind(w: &World, g: &Grant) {
    assert!(w.s.bind_grant(binding(g)).await.unwrap());
}

async fn get(w: &World, g: &Grant) -> Grant {
    w.s.grant(g.grant_id.clone()).await.unwrap().unwrap()
}

async fn sweep(w: &World) {
    w.s.sweep_grants().await.unwrap();
}

async fn audit_idle_rows(w: &World) -> usize {
    w.s.audit_log(50)
        .await
        .unwrap()
        .iter()
        .filter(|r| r.action == "coordinator.grant_expired" && r.detail.as_deref() == Some("idle"))
        .count()
}

async fn query_accepted(w: &World, g: &Grant, session: &str) -> bool {
    let query = CoordinatorQueryEnvelope {
        query_id: "q".to_owned(),
        target_incarnation: w.s.incarnation().clone(),
        caller: session.into(),
        binding: binding(g),
        deadline: w.s.now_ms() + 10_000,
        query: CoordinatorQuery::ListSessions,
    };
    w.s.coordinator_query(g.station_id.clone(), query)
        .await
        .unwrap()
        .is_ok()
}

/// A world with a running `coord-1` at cursor 5, a live link 1 and the
/// answer of link 1 at head 5, and a bound grant with idle time `idle_ms`.
async fn running_world(idle_ms: i64) -> (World, Grant) {
    let w = world().await;
    seed(&w, &w.a, vec![session("coord-1", true)], 5).await;
    let g = bound(&w, &w.a, "coord-1", idle_ms).await;
    up(&w, &w.a, 1);
    answer(&w, &w.a, 1, 5).await;
    (w, g)
}

#[tokio::test]
async fn a_running_turn_keeps_a_bound_grant_in_effect_past_its_idle_time() {
    let (w, g) = running_world(60_000).await;
    advance(&w, 50);
    sweep(&w).await;
    assert_eq!(get(&w, &g).await.last_used_at, Some(w.s.now_ms()));
    advance(&w, 50);
    sweep(&w).await;
    let got = get(&w, &g).await;
    assert_eq!(got.state, GrantState::Active);
    assert_eq!(got.last_used_at, Some(w.s.now_ms()));
}

#[tokio::test]
async fn the_idle_clock_starts_at_the_last_moment_the_turn_was_seen() {
    let (w, g) = running_world(60_000).await;
    advance(&w, 50);
    sweep(&w).await;
    let seen = get(&w, &g).await.last_used_at;
    events(
        &w,
        &w.a,
        vec![envelope(&w, 6, "coord-1", run_completed("r1"))],
    )
    .await;
    assert_eq!(get(&w, &g).await.last_used_at, seen);
    advance(&w, 59);
    sweep(&w).await;
    assert_eq!(get(&w, &g).await.state, GrantState::Active);
    advance(&w, 2);
    sweep(&w).await;
    let got = get(&w, &g).await;
    assert_eq!(
        (got.state, got.revoked_reason.as_deref()),
        (GrantState::Expired, Some("idle"))
    );
    assert_eq!(audit_idle_rows(&w).await, 1);
}

#[tokio::test]
async fn a_run_started_event_touches_the_grant_in_its_own_batch() {
    let w = world().await;
    seed(&w, &w.a, vec![session("coord-1", false)], 5).await;
    let g = bound(&w, &w.a, "coord-1", 60_000).await;
    up(&w, &w.a, 1);
    answer(&w, &w.a, 1, 5).await;
    advance(&w, 59);
    events(&w, &w.a, vec![envelope(&w, 6, "coord-1", run_started())]).await;
    assert_eq!(get(&w, &g).await.last_used_at, Some(w.s.now_ms()));
}

#[tokio::test]
async fn no_extension_without_a_live_link() {
    let (w, g) = running_world(60_000).await;
    down(&w, &w.a, 1);
    assert!(w.s.inner.current_stream(&w.a).is_none());
    advance(&w, 61);
    sweep(&w).await;
    let got = get(&w, &g).await;
    assert_eq!(
        (got.state, got.revoked_reason.as_deref()),
        (GrantState::Expired, Some("idle"))
    );
}

#[tokio::test]
async fn a_delayed_answer_of_an_ended_link_does_not_make_the_new_link_fresh() {
    let w = world().await;
    seed(&w, &w.a, vec![session("coord-1", true)], 5).await;
    let g = bound(&w, &w.a, "coord-1", 60_000).await;
    up(&w, &w.a, 1);
    down(&w, &w.a, 1);
    up(&w, &w.a, 2);
    assert_eq!(answer(&w, &w.a, 1, 5).await, 0);
    assert!(w.s.inner.current_stream(&w.a).is_none());
    advance(&w, 61);
    sweep(&w).await;
    assert_eq!(get(&w, &g).await.state, GrantState::Expired);

    // Second case: the live link answers a head that the cursor has not reached.
    let w = world().await;
    seed(&w, &w.a, vec![session("coord-1", true)], 5).await;
    let g = bound(&w, &w.a, "coord-1", 60_000).await;
    up(&w, &w.a, 2);
    assert_eq!(answer(&w, &w.a, 2, 7).await, 0);
    advance(&w, 30);
    events(
        &w,
        &w.a,
        vec![
            envelope(
                &w,
                6,
                "coord-1",
                Event::ModeChanged {
                    mode: "x".to_owned(),
                },
            ),
            envelope(&w, 7, "coord-1", run_completed("r1")),
        ],
    )
    .await;
    assert_ne!(get(&w, &g).await.last_used_at, Some(w.s.now_ms()));
    advance(&w, 31);
    sweep(&w).await;
    assert_eq!(get(&w, &g).await.state, GrantState::Expired);
}

#[tokio::test]
async fn no_extension_before_the_read_model_reaches_the_reported_head_or_with_another_incarnation()
{
    let w = world().await;
    seed(&w, &w.a, vec![session("coord-1", true)], 5).await;
    let g = bound(&w, &w.a, "coord-1", 60_000).await;
    up(&w, &w.a, 1);
    assert_eq!(answer(&w, &w.a, 1, 9).await, 0);
    advance(&w, 30);
    sweep(&w).await;
    assert_ne!(get(&w, &g).await.last_used_at, Some(w.s.now_ms()));
    let n =
        w.s.record_link_stream(w.a.clone(), 1, ("inc-2".into(), 5))
            .await
            .unwrap();
    assert_eq!(n, 0);
    sweep(&w).await;
    assert_ne!(get(&w, &g).await.last_used_at, Some(w.s.now_ms()));
}

#[tokio::test]
async fn no_extension_for_an_exited_or_lost_session() {
    let w = world().await;
    let mut exited = session("c-exited", true);
    exited.state = SessionState::Exited;
    let mut lost = session("c-lost", true);
    lost.state = SessionState::Lost;
    seed(&w, &w.a, vec![exited, lost], 5).await;
    let mut grants = Vec::new();
    for id in ["c-exited", "c-lost"] {
        grants.push(bound(&w, &w.a, id, 600_000).await);
    }
    up(&w, &w.a, 1);
    answer(&w, &w.a, 1, 5).await;
    advance(&w, 30);
    sweep(&w).await;
    for g in &grants {
        assert_ne!(get(&w, g).await.last_used_at, Some(w.s.now_ms()));
    }
}

#[tokio::test]
async fn a_turn_never_revives_a_grant_that_is_idle_past_its_time_past_its_lifetime_or_revoked() {
    // Idle past its time before the first touch.
    let w = world().await;
    seed(&w, &w.a, vec![session("coord-1", true)], 5).await;
    let g = bound(&w, &w.a, "coord-1", 60_000).await;
    advance(&w, 61);
    up(&w, &w.a, 1);
    assert_eq!(answer(&w, &w.a, 1, 5).await, 0);
    sweep(&w).await;
    let got = get(&w, &g).await;
    assert_eq!(
        (got.state, got.revoked_reason.as_deref()),
        (GrantState::Expired, Some("idle"))
    );

    // Past its lifetime.
    let w = world().await;
    seed(&w, &w.a, vec![session("coord-1", true)], 5).await;
    let mut new = NewGrant::new(w.a.clone(), "coord-1".into());
    new.ttl_ms = 60_000;
    let g = w.s.create_grant(new).await.unwrap();
    bind(&w, &g).await;
    up(&w, &w.a, 1);
    advance(&w, 61);
    assert_eq!(answer(&w, &w.a, 1, 5).await, 0);
    sweep(&w).await;
    let got = get(&w, &g).await;
    assert_eq!(
        (got.state, got.revoked_reason.as_deref()),
        (GrantState::Expired, Some("expired"))
    );

    // Revoked.
    let (w, g) = running_world(60_000).await;
    let before = get(&w, &g).await.last_used_at;
    w.s.revoke_grant(g.grant_id.clone(), "owner".to_owned())
        .await
        .unwrap();
    advance(&w, 10);
    assert_eq!(answer(&w, &w.a, 1, 5).await, 0);
    sweep(&w).await;
    let got = get(&w, &g).await;
    assert_eq!(got.state, GrantState::Revoked);
    assert_eq!(got.last_used_at, before);
}

#[tokio::test]
async fn an_unbound_grant_gets_no_extension() {
    let w = world().await;
    seed(
        &w,
        &w.a,
        vec![session("coord-1", true), session("w-1", true)],
        5,
    )
    .await;
    let coord =
        w.s.create_grant(NewGrant::new(w.a.clone(), "coord-1".into()))
            .await
            .unwrap();
    let mut new = NewGrant::new(w.a.clone(), "w-1".into());
    new.kind = GrantKind::Worker;
    new.idle_ttl_ms = 60_000;
    new.classes = BTreeSet::from([
        GrantClass::Read,
        GrantClass::Mail,
        GrantClass::Drive,
        GrantClass::Spawn,
    ]);
    let worker = w.s.create_grant(new).await.unwrap();
    up(&w, &w.a, 1);
    answer(&w, &w.a, 1, 5).await;
    advance(&w, 61);
    sweep(&w).await;
    assert_eq!(get(&w, &coord).await.last_used_at, None);
    assert_eq!(get(&w, &coord).await.state, GrantState::Active);
    let got = get(&w, &worker).await;
    assert_eq!(
        (got.state, got.revoked_reason.as_deref()),
        (GrantState::Expired, Some("idle"))
    );
}

#[tokio::test]
async fn a_bound_worker_grant_is_extended() {
    let w = world().await;
    seed(&w, &w.a, vec![session("w-1", true)], 5).await;
    let mut new = NewGrant::new(w.a.clone(), "w-1".into());
    new.kind = GrantKind::Worker;
    new.idle_ttl_ms = 60_000;
    new.classes = BTreeSet::from([
        GrantClass::Read,
        GrantClass::Mail,
        GrantClass::Drive,
        GrantClass::Spawn,
    ]);
    let worker = w.s.create_grant(new).await.unwrap();
    bind(&w, &worker).await;
    up(&w, &w.a, 1);
    answer(&w, &w.a, 1, 5).await;
    advance(&w, 50);
    sweep(&w).await;
    assert_eq!(get(&w, &worker).await.last_used_at, Some(w.s.now_ms()));
    advance(&w, 50);
    sweep(&w).await;
    assert_eq!(get(&w, &worker).await.state, GrantState::Active);
}

#[tokio::test]
async fn the_turn_of_another_session_or_another_station_does_not_extend_a_grant() {
    let w = world().await;
    seed(
        &w,
        &w.a,
        vec![session("coord-1", false), session("w-1", true)],
        5,
    )
    .await;
    seed(&w, &w.b, vec![session("coord-1", true)], 5).await;
    let g = bound(&w, &w.a, "coord-1", 60_000).await;
    up(&w, &w.a, 1);
    up(&w, &w.b, 1);
    answer(&w, &w.a, 1, 5).await;
    answer(&w, &w.b, 1, 5).await;
    advance(&w, 30);
    sweep(&w).await;
    assert_ne!(get(&w, &g).await.last_used_at, Some(w.s.now_ms()));
}

#[tokio::test]
async fn a_query_after_a_long_silent_turn_is_admitted() {
    let (w, g) = running_world(60_000).await;
    advance(&w, 50);
    sweep(&w).await;
    advance(&w, 50);
    sweep(&w).await;
    assert!(query_accepted(&w, &g, "coord-1").await);
}

#[tokio::test]
async fn a_reconnect_just_before_the_idle_deadline_keeps_the_grant() {
    let w = world().await;
    seed(&w, &w.a, vec![session("coord-1", true)], 5).await;
    let g = bound(&w, &w.a, "coord-1", 60_000).await;
    advance(&w, 59);
    up(&w, &w.a, 1);
    assert_eq!(answer(&w, &w.a, 1, 5).await, 1);
    assert_eq!(get(&w, &g).await.last_used_at, Some(w.s.now_ms()));
    advance(&w, 2);
    assert!(query_accepted(&w, &g, "coord-1").await);
}

#[tokio::test]
async fn a_snapshot_that_makes_the_station_fresh_touches_at_once() {
    let w = world().await;
    seed(&w, &w.a, vec![session("coord-1", true)], 5).await;
    let g = bound(&w, &w.a, "coord-1", 60_000).await;
    up(&w, &w.a, 1);
    assert_eq!(answer(&w, &w.a, 1, 9).await, 0);
    advance(&w, 59);
    seed(&w, &w.a, vec![session("coord-1", true)], 9).await;
    assert_eq!(get(&w, &g).await.last_used_at, Some(w.s.now_ms()));
    advance(&w, 2);
    assert!(query_accepted(&w, &g, "coord-1").await);
}

#[tokio::test]
async fn a_batch_touches_a_running_session_without_an_event_in_it() {
    let w = world().await;
    seed(
        &w,
        &w.a,
        vec![session("coord-1", true), session("w-1", false)],
        5,
    )
    .await;
    let g = bound(&w, &w.a, "coord-1", 60_000).await;
    up(&w, &w.a, 1);
    assert_eq!(answer(&w, &w.a, 1, 6).await, 0);
    advance(&w, 59);
    events(&w, &w.a, vec![envelope(&w, 6, "w-1", run_started())]).await;
    assert_eq!(get(&w, &g).await.last_used_at, Some(w.s.now_ms()));
}

#[tokio::test]
async fn the_newest_live_link_that_answered_decides() {
    let w = world().await;
    seed(&w, &w.a, vec![session("coord-1", true)], 5).await;
    let g = bound(&w, &w.a, "coord-1", 60_000).await;
    let stream = |head: u64| Some((IncarnationId::from(INC), head));
    up(&w, &w.a, 1);
    assert_eq!(answer(&w, &w.a, 1, 5).await, 1);
    w.s.set_link_features(&w.a, 2, vec![]);
    assert_eq!(w.s.inner.current_stream(&w.a), stream(5));
    advance(&w, 10);
    assert_eq!(answer(&w, &w.a, 2, 9).await, 0);
    assert_eq!(w.s.inner.current_stream(&w.a), stream(9));
    assert_ne!(get(&w, &g).await.last_used_at, Some(w.s.now_ms()));
    w.s.clear_link_features(&w.a, 2);
    assert_eq!(w.s.inner.current_stream(&w.a), stream(5));
    w.s.clear_link_features(&w.a, 1);
    assert_eq!(w.s.inner.current_stream(&w.a), None);
}

#[tokio::test]
async fn background_work_without_a_turn_keeps_the_grant_in_effect() {
    // Owner decision 2026-10-24: background tasks or running agents count as work.
    let w = world().await;
    let mut tasks = session("c-bg", false);
    tasks.background_tasks = vec!["t1".to_owned()];
    let mut agents = session("c-agents", false);
    agents.running_agents = 2;
    let idle = session("c-idle", false);
    seed(&w, &w.a, vec![tasks, agents, idle], 5).await;
    let g_tasks = bound(&w, &w.a, "c-bg", 600_000).await;
    let g_agents = bound(&w, &w.a, "c-agents", 600_000).await;
    let g_idle = bound(&w, &w.a, "c-idle", 600_000).await;
    up(&w, &w.a, 1);
    answer(&w, &w.a, 1, 5).await;
    advance(&w, 30);
    sweep(&w).await;
    assert_eq!(get(&w, &g_tasks).await.last_used_at, Some(w.s.now_ms()));
    assert_eq!(get(&w, &g_agents).await.last_used_at, Some(w.s.now_ms()));
    assert_ne!(get(&w, &g_idle).await.last_used_at, Some(w.s.now_ms()));
}
