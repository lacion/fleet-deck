//! The wake key has the session (G5, design `fleet-gaps.md` §3): request ids
//! are per session (DL-402), so two sessions of one station can ask with the
//! same id and each request wakes a wall once. A row of an earlier deck (a
//! legacy tombstone of V49) still blocks its request id.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
mod desktop_support;
mod share_support;

use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::time::Duration;

use common::board::{BoardClient, board};
use common::*;
use desktop_support::*;
use fleetdeck_deck::store::wake_seen::WakeReserve;
use fleetdeck_deck::wake::{Limits, WakeCounters, spawn_with};
use fleetdeck_link::LiveLink;
use fleetdeck_link::control::InboundRequest;
use fleetdeck_proto::desktop::DesktopInfo;
use fleetdeck_proto::display::DisplayBinding;
use fleetdeck_proto::event::{Event, EventBody, RequestKind, ResponseChannel};
use fleetdeck_proto::link::Outcome;
use fleetdeck_proto::{EventEnvelope, IncarnationId};
use share_support::*;
use tokio::sync::watch;
use tokio::task::JoinHandle;

struct Wake {
    counters: Arc<WakeCounters>,
    stop: watch::Sender<bool>,
    task: JoinHandle<()>,
}

fn n(counter: &AtomicU64) -> u64 {
    WakeCounters::get(counter)
}

fn now(f: &Fixture) -> i64 {
    i64::try_from(f.det.now_ms()).unwrap()
}

fn advance(f: &Fixture, secs: u64) {
    f.det.advance(Duration::from_secs(secs));
}

async fn until(what: &str, ready: impl Fn() -> bool) {
    let deadline = tokio::time::Instant::now() + T;
    while !ready() {
        assert!(tokio::time::Instant::now() < deadline, "timed out: {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Starts the wake task and waits for its first scan (the baseline).
async fn start(f: &Fixture, c: &BoardClient) -> Wake {
    let counters = Arc::new(WakeCounters::default());
    let (stop, rx) = watch::channel(false);
    let task = spawn_with(
        f.store.clone(),
        Arc::clone(&f.ctx),
        c.state.live.clone(),
        rx,
        Limits::default(),
        Arc::clone(&counters),
    );
    until("the first scan", || n(&counters.scans) >= 1).await;
    Wake {
        counters,
        stop,
        task,
    }
}

impl Wake {
    async fn stop(self) {
        self.stop.send(true).unwrap();
        tokio::time::timeout(Duration::from_secs(10), self.task)
            .await
            .expect("the wake task stops")
            .unwrap();
    }
}

fn requested(seq: u64, session: &str, at: i64, id: &str) -> EventEnvelope {
    EventEnvelope {
        station_seq: seq,
        session_id: session.into(),
        session_seq: seq,
        at,
        proto_version: 1,
        raw_ref: None,
        event: EventBody::Known(Event::ApprovalRequested {
            request_id: id.into(),
            node_id: None,
            request_kind: RequestKind::Command,
            title: "a title".to_owned(),
            details: String::new(),
            input: None,
            options: vec![],
            response: ResponseChannel::RpcReply,
            provider_refs: serde_json::Map::new(),
        }),
    }
}

/// A monitor `DP-1` that is off, with a fresh offset of 0.
fn off(f: &Fixture) -> DesktopInfo {
    let mut i = info();
    i.monitors[0].dpms_on = Some(false);
    i.utc_offset_minutes = Some(0);
    i.offset_sampled_at = Some(now(f));
    i
}

async fn answer_info(station: &mut LiveLink, a: &DesktopInfo) -> InboundRequest {
    let req = next_request(station).await;
    assert_eq!(req.method, "desktop.info");
    station
        .control
        .respond(req.id, Outcome::Ok(serde_json::to_value(a).unwrap()))
        .await
        .unwrap();
    req
}

/// One complete wake: the fresh info, then the action.
async fn serve_wake(f: &Fixture, station: &mut LiveLink) {
    answer_info(station, &off(f)).await;
    let req = next_request(station).await;
    assert_eq!(req.method, "desktop.act");
    assert_eq!(req.params["action"], "screen_power");
    station.control.respond(req.id, ok(None)).await.unwrap();
}

async fn wake_rows(c: &BoardClient) -> usize {
    c.store
        .audit_log(200)
        .await
        .unwrap()
        .into_iter()
        .filter(|e| e.action == "desktop.wake")
        .count()
}

/// A wall display that is bound to `DP-1`.
async fn bind_wall(f: &Fixture, c: &BoardClient) {
    enable(c, true);
    let id = display(f, c, "Wall").await;
    c.store
        .display_set_binding(
            id,
            Some(DisplayBinding {
                station_id: f.station.clone(),
                monitor: "DP-1".to_owned(),
            }),
        )
        .await
        .unwrap();
}

async fn bound() -> (Fixture, LiveLink, BoardClient) {
    let (f, station, c, _h) = setup().await;
    bind_wall(&f, &c).await;
    (f, station, c)
}

/// The same as `bound`, with the deck database in `dir`.
async fn bound_file(dir: &std::path::Path) -> (Fixture, LiveLink, BoardClient) {
    let mut f = fixture_file(dir).await;
    let station = f.connect().await;
    f.store
        .apply_snapshot(
            f.station.clone(),
            f.inc.clone(),
            0,
            vec![card("s1"), card("s2")],
        )
        .await
        .unwrap();
    let c = board(Arc::clone(&f.ctx)).await;
    bind_wall(&f, &c).await;
    (f, station, c)
}

#[tokio::test]
async fn one_request_id_in_two_sessions_is_two_requests_for_the_wake() {
    let (f, mut station, c) = bound().await;
    let wake = start(&f, &c).await;
    let t = now(&f);
    f.store
        .apply_events(
            f.station.clone(),
            f.inc.clone(),
            vec![requested(1, "s1", t, "r1"), requested(2, "s2", t, "r1")],
        )
        .await
        .unwrap();
    until("two keys reserved", || n(&wake.counters.reserved) == 2).await;
    // The two requests join into one job and one wake.
    serve_wake(&f, &mut station).await;
    until("one wake", || n(&wake.counters.sent_ok) == 1).await;
    // A third session with the same id wakes again after the cooldown.
    advance(&f, 31);
    f.store
        .apply_events(
            f.station.clone(),
            f.inc.clone(),
            vec![requested(3, "s3", now(&f), "r1")],
        )
        .await
        .unwrap();
    until("three keys reserved", || n(&wake.counters.reserved) == 3).await;
    serve_wake(&f, &mut station).await;
    until("the second wake", || n(&wake.counters.sent_ok) == 2).await;
    wake.stop().await;
}

#[tokio::test]
async fn a_reserved_key_of_one_session_does_not_block_another_session_after_a_restart() {
    let (f, mut station, c) = bound().await;
    // An earlier process reserved the key of session `s1`.
    let t = now(&f);
    assert_eq!(
        f.store
            .wake_reserve(f.station.as_str(), "s1", "r1", t)
            .await,
        WakeReserve::Reserved
    );
    let wake = start(&f, &c).await;
    f.store
        .apply_events(
            f.station.clone(),
            f.inc.clone(),
            vec![requested(1, "s2", now(&f), "r1")],
        )
        .await
        .unwrap();
    serve_wake(&f, &mut station).await;
    until("the wake of s2", || n(&wake.counters.sent_ok) == 1).await;
    // The same request of `s1` is seen: no wake.
    advance(&f, 31);
    f.store
        .apply_events(
            f.station.clone(),
            f.inc.clone(),
            vec![requested(2, "s1", now(&f), "r1")],
        )
        .await
        .unwrap();
    until("the key of s1 is seen", || n(&wake.counters.seen) == 1).await;
    no_request(&mut station, Duration::from_millis(300)).await;
    assert_eq!(n(&wake.counters.sent_ok), 1);
    wake.stop().await;
}

#[tokio::test]
async fn a_legacy_tombstone_blocks_a_request_restored_after_an_empty_baseline() {
    let dir = tempfile::tempdir().unwrap();
    let (f, mut station, c) = bound_file(dir.path()).await;
    let t = now(&f);
    // The row that V49 makes of a V48 row (the station id is made by the
    // fixture, so the V48 seed itself is in the store tests).
    {
        let conn = rusqlite::Connection::open(dir.path().join("deck.db")).unwrap();
        conn.busy_timeout(Duration::from_secs(5)).unwrap();
        conn.execute(
            "INSERT INTO wake_seen (station_id, session_id, request_id, first_seen_ms)
             VALUES (?1, '', 'r1', ?2)",
            rusqlite::params![f.station.as_str(), t],
        )
        .unwrap();
    }
    let wake = start(&f, &c).await;
    assert_eq!(n(&wake.counters.baseline), 0);
    // The station restores the request under a new incarnation, after the
    // empty baseline.
    let inc2 = IncarnationId::from("fake-station-db-2");
    advance(&f, 2);
    f.store
        .apply_snapshot(f.station.clone(), inc2.clone(), 0, vec![card("s1")])
        .await
        .unwrap();
    f.store
        .apply_events(
            f.station.clone(),
            inc2.clone(),
            vec![requested(1, "s1", now(&f), "r1")],
        )
        .await
        .unwrap();
    until("the key is seen", || n(&wake.counters.seen) == 1).await;
    quiet(&mut station).await;
    assert_eq!(n(&wake.counters.sent_ok) + n(&wake.counters.reserved), 0);
    assert_eq!(wake_rows(&c).await, 0);
    // Another request id of the same session wakes.
    f.store
        .apply_events(
            f.station.clone(),
            inc2,
            vec![requested(2, "s1", now(&f), "r2")],
        )
        .await
        .unwrap();
    serve_wake(&f, &mut station).await;
    until("the wake of r2", || n(&wake.counters.sent_ok) == 1).await;
    wake.stop().await;
}
