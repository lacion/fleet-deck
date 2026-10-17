//! The cooldown of the wake at the send (R13 P3b, PRE-PR round 1, F4): the
//! 30 s between two wakes of one monitor count from the wake attempt, not
//! from the scan. The fake station answers `desktop.info` and
//! `desktop.act`; the deck clock is the manual clock of the fixture.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
mod desktop_support;
mod share_support;

use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::time::Duration;

use common::board::BoardClient;
use common::*;
use desktop_support::*;
use fleetdeck_deck::wake::{Limits, WakeCounters, spawn_with};
use fleetdeck_link::LiveLink;
use fleetdeck_proto::EventEnvelope;
use fleetdeck_proto::desktop::DesktopInfo;
use fleetdeck_proto::display::DisplayBinding;
use fleetdeck_proto::event::{Event, EventBody, RequestKind, ResponseChannel};
use fleetdeck_proto::link::Outcome;
use share_support::*;
use tokio::sync::watch;

fn n(counter: &AtomicU64) -> u64 {
    WakeCounters::get(counter)
}

fn now(f: &Fixture) -> i64 {
    i64::try_from(f.det.now_ms()).unwrap()
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

async fn until(what: &str, ready: impl Fn() -> bool) {
    let deadline = tokio::time::Instant::now() + T;
    while !ready() {
        assert!(tokio::time::Instant::now() < deadline, "timed out: {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn answer_info(
    station: &mut LiveLink,
    req: fleetdeck_link::control::InboundRequest,
    a: &DesktopInfo,
) {
    assert_eq!(req.method, "desktop.info");
    station
        .control
        .respond(req.id, Outcome::Ok(serde_json::to_value(a).unwrap()))
        .await
        .unwrap();
}

async fn push_request(f: &Fixture, seq: u64, session: &str, id: &str) {
    f.store
        .apply_events(
            f.station.clone(),
            f.inc.clone(),
            vec![requested(seq, session, now(f), id)],
        )
        .await
        .unwrap();
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

#[tokio::test]
async fn two_jobs_for_one_monitor_with_sends_two_seconds_apart_wake_once() {
    let (f, mut station, c, _h) = setup().await;
    enable(&c, true);
    let id = display(&f, &c, "Wall").await;
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
    until("the baseline scan", || n(&counters.scans) >= 1).await;
    // Job 1 is queued at t0. Its worker waits for the fresh desktop info.
    push_request(&f, 1, "s1", "r1").await;
    let held = next_request(&mut station).await;
    // The info arrives at t0 + 29 s: job 1 sends its wake at that time.
    f.det.advance(Duration::from_secs(29));
    answer_info(&mut station, held, &off(&f)).await;
    let act = next_request(&mut station).await;
    assert_eq!(act.method, "desktop.act");
    assert_eq!(act.params["action"], "screen_power");
    station.control.respond(act.id, ok(None)).await.unwrap();
    until("the first wake", || n(&counters.sent_ok) == 1).await;
    // Job 2 passes the queue-time cooldown (31 s after the first job), and
    // its send would come 2 s after the first send: the worker refuses it.
    f.det.advance(Duration::from_secs(2));
    push_request(&f, 2, "s2", "r2").await;
    let info_req = next_request(&mut station).await;
    answer_info(&mut station, info_req, &off(&f)).await;
    until("the cooldown skip", || n(&counters.skip_cooldown) == 1).await;
    no_request(&mut station, Duration::from_millis(300)).await;
    assert_eq!(n(&counters.sent_ok), 1);
    assert_eq!(n(&counters.reserved), 2);
    assert_eq!(wake_rows(&c).await, 1);
    // An attempt after the cooldown of the first send wakes again.
    f.det.advance(Duration::from_secs(30));
    push_request(&f, 3, "s3", "r3").await;
    let info_req = next_request(&mut station).await;
    answer_info(&mut station, info_req, &off(&f)).await;
    let act = next_request(&mut station).await;
    assert_eq!(act.method, "desktop.act");
    station.control.respond(act.id, ok(None)).await.unwrap();
    until("the second wake", || n(&counters.sent_ok) == 2).await;
    stop.send(true).unwrap();
    tokio::time::timeout(Duration::from_secs(10), task)
        .await
        .expect("the wake task stops")
        .unwrap();
}
