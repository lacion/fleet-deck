//! The wake rule of the wall (R13 P3b wake, §6.6, §6D items 3 to 5, 9 and 10,
//! §5E items 5 and 6, §5F item 4, §5G item 2): detection, the baseline, the
//! restart-safe deduplication, the filters, the quiet hours, the joining and
//! the limits. The fake station answers `desktop.info` and `desktop.act`; the
//! deck clock is the manual clock of the fixture.

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
use fleetdeck_deck::store::AuditResult;
use fleetdeck_deck::store::display_directives::DirectiveSender;
use fleetdeck_deck::store::displays::DisplayChange;
use fleetdeck_deck::wake::{Limits, WakeCounters, spawn_with};
use fleetdeck_link::LiveLink;
use fleetdeck_link::control::InboundRequest;
use fleetdeck_proto::desktop::DesktopInfo;
use fleetdeck_proto::display::DisplayBinding;
use fleetdeck_proto::display_directive::{DirectiveBody, DirectiveFilter};
use fleetdeck_proto::event::{
    Event, EventBody, RequestKind, ResolvedBy, ResponseChannel, SessionState,
};
use fleetdeck_proto::link::Outcome;
use fleetdeck_proto::session::{OpenRequest, RepoInfo};
use fleetdeck_proto::{EventEnvelope, IncarnationId, SessionSummary};
use serde_json::json;
use share_support::*;
use tokio::sync::watch;
use tokio::task::JoinHandle;

const SENTINEL: &str = "SENTINEL-REQUEST-TITLE";
const ACT_TIMEOUT_TEXT: &str = "The desktop action did not finish in time.";

struct Wake {
    counters: Arc<WakeCounters>,
    stop: watch::Sender<bool>,
    task: JoinHandle<()>,
}

fn n(counter: &AtomicU64) -> u64 {
    WakeCounters::get(counter)
}

/// Starts the wake task and waits for its first scan (the baseline).
async fn start(f: &Fixture, c: &BoardClient, limits: Limits) -> Wake {
    let counters = Arc::new(WakeCounters::default());
    let (stop, rx) = watch::channel(false);
    let task = spawn_with(
        f.store.clone(),
        Arc::clone(&f.ctx),
        c.state.live.clone(),
        rx,
        limits,
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

async fn until(what: &str, ready: impl Fn() -> bool) {
    let deadline = tokio::time::Instant::now() + T;
    while !ready() {
        assert!(tokio::time::Instant::now() < deadline, "timed out: {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn now(f: &Fixture) -> i64 {
    i64::try_from(f.det.now_ms()).unwrap()
}

fn advance(f: &Fixture, secs: u64) {
    f.det.advance(Duration::from_secs(secs));
}

fn request(id: &str, at: i64) -> OpenRequest {
    OpenRequest {
        request_id: id.into(),
        node_id: None,
        request_kind: RequestKind::Command,
        title: SENTINEL.to_owned(),
        details: String::new(),
        input: None,
        options: vec![],
        response: ResponseChannel::RpcReply,
        requested_at: at,
    }
}

fn card_in(id: &str, root: &str, requests: Vec<OpenRequest>) -> SessionSummary {
    let mut s = card(id);
    s.state = SessionState::Running;
    s.repo = Some(RepoInfo {
        root: root.to_owned(),
        origin: None,
    });
    s.open_requests = requests;
    s
}

fn envelope(seq: u64, session: &str, at: i64, event: Event) -> EventEnvelope {
    EventEnvelope {
        station_seq: seq,
        session_id: session.into(),
        session_seq: seq,
        at,
        proto_version: 1,
        raw_ref: None,
        event: EventBody::Known(event),
    }
}

fn requested(seq: u64, session: &str, at: i64, id: &str) -> EventEnvelope {
    envelope(
        seq,
        session,
        at,
        Event::ApprovalRequested {
            request_id: id.into(),
            node_id: None,
            request_kind: RequestKind::Command,
            title: SENTINEL.to_owned(),
            details: String::new(),
            input: None,
            options: vec![],
            response: ResponseChannel::RpcReply,
            provider_refs: serde_json::Map::new(),
        },
    )
}

fn resolved(seq: u64, session: &str, at: i64, id: &str) -> EventEnvelope {
    envelope(
        seq,
        session,
        at,
        Event::ApprovalResolved {
            request_id: id.into(),
            option_id: "allow".to_owned(),
            by: ResolvedBy::Board,
            answered_by: None,
        },
    )
}

async fn push(f: &Fixture, events: Vec<EventEnvelope>) {
    f.store
        .apply_events(f.station.clone(), f.inc.clone(), events)
        .await
        .unwrap();
}

/// The desktop info of the fake station: monitor `DP-1` with `dpms_on`, and
/// the offset sample.
fn desktop(dpms_on: Option<bool>, offset: Option<i32>, sampled: Option<i64>) -> DesktopInfo {
    let mut i = info();
    i.monitors[0].dpms_on = dpms_on;
    i.utc_offset_minutes = offset;
    i.offset_sampled_at = sampled;
    i
}

/// A monitor that is off, with a fresh offset of 0 (local time = UTC).
fn off(f: &Fixture) -> DesktopInfo {
    desktop(Some(false), Some(0), Some(now(f)))
}

async fn serve_info(station: &mut LiveLink, answer: &DesktopInfo) -> InboundRequest {
    let req = next_request(station).await;
    assert_eq!(req.method, "desktop.info");
    assert_eq!(req.params, json!({ "fresh": true }));
    station
        .control
        .respond(req.id, Outcome::Ok(serde_json::to_value(answer).unwrap()))
        .await
        .unwrap();
    req
}

async fn serve_act(station: &mut LiveLink, reply: Outcome) -> InboundRequest {
    let req = next_request(station).await;
    assert_eq!(req.method, "desktop.act");
    station.control.respond(req.id, reply).await.unwrap();
    req
}

/// One complete wake: the fresh info, then the action.
async fn serve_wake(f: &Fixture, station: &mut LiveLink) -> InboundRequest {
    serve_info(station, &off(f)).await;
    serve_act(station, ok(None)).await
}

async fn bind(f: &Fixture, c: &BoardClient, id: &str, monitor: &str) {
    c.store
        .display_set_binding(
            id.to_owned(),
            Some(DisplayBinding {
                station_id: f.station.clone(),
                monitor: monitor.to_owned(),
            }),
        )
        .await
        .unwrap();
}

async fn set_display(c: &BoardClient, id: &str, change: DisplayChange) {
    c.store
        .display_update(id.to_owned(), change)
        .await
        .unwrap()
        .unwrap();
}

async fn wake_rows(c: &BoardClient) -> Vec<fleetdeck_deck::store::AuditEntry> {
    c.store
        .audit_log(200)
        .await
        .unwrap()
        .into_iter()
        .filter(|e| e.action == "desktop.wake")
        .collect()
}

/// A wall display that is bound to `DP-1`, and the running task.
async fn bound() -> (Fixture, LiveLink, BoardClient, String) {
    let (f, station, c, _h) = setup().await;
    enable(&c, true);
    let id = display(&f, &c, "Wall").await;
    bind(&f, &c, &id, "DP-1").await;
    (f, station, c, id)
}

#[tokio::test]
async fn a_new_request_wakes_the_bound_monitor_and_nothing_else_leaks() {
    let (f, mut station, c, id) = bound().await;
    set_display(
        &c,
        &id,
        DisplayChange {
            quiet_hours: Some(Some("01:00-02:00".to_owned())),
            ..DisplayChange::default()
        },
    )
    .await;
    let wake = start(&f, &c, Limits::default()).await;
    push(&f, vec![requested(1, "s1", now(&f), "r1")]).await;
    let req = serve_wake(&f, &mut station).await;
    assert_eq!(req.params["action"], "screen_power");
    assert_eq!(req.params["monitor"], "DP-1");
    assert_eq!(req.params["on"], true);
    // The authority carries the quiet hours of the display for the station.
    assert_eq!(authority(&req).quiet_hours.as_deref(), Some("01:00-02:00"));
    until("the wake counted", || n(&wake.counters.sent_ok) == 1).await;
    let rows = wake_rows(&c).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].actor, "wake");
    assert_eq!(rows[0].result, AuditResult::Ok);
    assert_eq!(
        rows[0].detail.as_deref(),
        Some(format!("display={id} monitor=DP-1 code=ok").as_str())
    );
    // No request text reaches an audit row.
    assert!(!audit_text(&c).await.contains(SENTINEL));
    // The same request does not wake twice, also after more board changes.
    advance(&f, 31);
    push(&f, vec![requested(2, "s2", now(&f), "r2")]).await;
    serve_wake(&f, &mut station).await;
    until("the second wake", || n(&wake.counters.sent_ok) == 2).await;
    assert_eq!(n(&wake.counters.reserved), 2);
    wake.stop().await;
}

#[tokio::test]
async fn a_request_at_the_start_and_in_a_new_snapshot_is_baseline_and_never_wakes() {
    let (f, mut station, c, _id) = bound().await;
    let t = now(&f);
    f.store
        .apply_snapshot(
            f.station.clone(),
            f.inc.clone(),
            1,
            vec![card_in("s1", "/p/app", vec![request("r0", t)])],
        )
        .await
        .unwrap();
    let wake = start(&f, &c, Limits::default()).await;
    until("the start baseline", || n(&wake.counters.baseline) == 1).await;
    // A new snapshot is a new boundary: its requests are baseline too, even
    // when they are fresh.
    advance(&f, 5);
    let t = now(&f);
    f.store
        .apply_snapshot(
            f.station.clone(),
            f.inc.clone(),
            5,
            vec![card_in(
                "s1",
                "/p/app",
                vec![request("r0", t - 5_000), request("r1", t)],
            )],
        )
        .await
        .unwrap();
    until("the snapshot baseline", || n(&wake.counters.baseline) == 2).await;
    quiet(&mut station).await;
    assert_eq!(n(&wake.counters.sent_ok) + n(&wake.counters.skip_age), 0);
    assert!(wake_rows(&c).await.is_empty());
    // A request above the boundary wakes.
    push(&f, vec![requested(6, "s1", now(&f), "r2")]).await;
    serve_wake(&f, &mut station).await;
    until("the live wake", || n(&wake.counters.sent_ok) == 1).await;
    wake.stop().await;
}

#[tokio::test]
async fn a_key_that_was_reserved_before_never_wakes_after_a_restart_or_a_new_incarnation() {
    let (f, mut station, c, _id) = bound().await;
    // The key was committed by an earlier process that crashed before the send.
    let t = now(&f);
    assert_eq!(
        f.store.wake_reserve(f.station.as_str(), "r1", t).await,
        fleetdeck_deck::store::wake_seen::WakeReserve::Reserved
    );
    let wake = start(&f, &c, Limits::default()).await;
    // The station restores the request under a new incarnation after the
    // initial snapshot.
    let inc2 = IncarnationId::from("fake-station-db-2");
    advance(&f, 2);
    f.store
        .apply_snapshot(f.station.clone(), inc2.clone(), 0, vec![card("s1")])
        .await
        .unwrap();
    f.store
        .apply_events(
            f.station.clone(),
            inc2,
            vec![requested(1, "s1", now(&f), "r1")],
        )
        .await
        .unwrap();
    until("the key is seen", || n(&wake.counters.seen) == 1).await;
    quiet(&mut station).await;
    assert_eq!(n(&wake.counters.sent_ok) + n(&wake.counters.reserved), 0);
    assert!(wake_rows(&c).await.is_empty());
    wake.stop().await;
}

#[tokio::test]
async fn only_a_request_inside_the_age_window_wakes() {
    let (f, mut station, c, _id) = bound().await;
    let wake = start(&f, &c, Limits::default()).await;
    let t = now(&f);
    push(
        &f,
        vec![
            requested(1, "s1", t - 60_001, "old"),
            requested(2, "s1", t + 5_001, "future"),
        ],
    )
    .await;
    until("both skipped by age", || n(&wake.counters.skip_age) == 2).await;
    quiet(&mut station).await;
    assert!(wake_rows(&c).await.is_empty());
    // The edges of the window are inside.
    push(&f, vec![requested(3, "s1", t - 60_000, "edge")]).await;
    serve_wake(&f, &mut station).await;
    until("the edge wakes", || n(&wake.counters.sent_ok) == 1).await;
    advance(&f, 31);
    let t = now(&f);
    push(&f, vec![requested(4, "s1", t + 5_000, "edge-future")]).await;
    serve_wake(&f, &mut station).await;
    until("the future edge wakes", || n(&wake.counters.sent_ok) == 2).await;
    wake.stop().await;
}

#[tokio::test]
async fn a_saturated_table_suppresses_the_wake_and_it_recovers_after_120_seconds() {
    let (f, mut station, c, _id) = bound().await;
    let t = now(&f);
    for i in 0..10_000 {
        f.store
            .wake_reserve("seed-station", &format!("seed-{i}"), t)
            .await;
    }
    let wake = start(&f, &c, Limits::default()).await;
    push(&f, vec![requested(1, "s1", t, "r1")]).await;
    until("suppressed", || n(&wake.counters.suppressed) == 1).await;
    quiet(&mut station).await;
    assert!(wake_rows(&c).await.is_empty());
    // The suppressed key is never tried again, but a new request wakes once
    // the suppression ended and the old rows can go.
    advance(&f, 121);
    push(&f, vec![requested(2, "s1", now(&f), "r2")]).await;
    serve_wake(&f, &mut station).await;
    until("the wake recovers", || n(&wake.counters.sent_ok) == 1).await;
    assert_eq!(n(&wake.counters.suppressed), 1);
    wake.stop().await;
}

#[tokio::test]
async fn two_requests_and_two_displays_on_one_monitor_wake_once_with_a_cooldown() {
    let (f, mut station, c, id) = bound().await;
    let second = display(&f, &c, "Second").await;
    bind(&f, &c, &second, "DP-1").await;
    let wake = start(&f, &c, Limits::default()).await;
    let t = now(&f);
    push(
        &f,
        vec![requested(1, "s1", t, "r1"), requested(2, "s2", t, "r2")],
    )
    .await;
    serve_wake(&f, &mut station).await;
    until("one wake", || n(&wake.counters.sent_ok) == 1).await;
    quiet(&mut station).await;
    let rows = wake_rows(&c).await;
    assert_eq!(rows.len(), 1);
    let detail = rows[0].detail.clone().unwrap();
    assert!(
        detail.starts_with(&format!("display={id} "))
            || detail.starts_with(&format!("display={second} ")),
        "{detail}"
    );
    // A request inside the cooldown of the monitor is skipped.
    advance(&f, 1);
    push(&f, vec![requested(3, "s1", now(&f), "r3")]).await;
    until("the cooldown skip", || n(&wake.counters.skip_cooldown) == 1).await;
    quiet(&mut station).await;
    // After the cooldown the next request wakes again.
    advance(&f, 31);
    push(&f, vec![requested(4, "s1", now(&f), "r4")]).await;
    serve_wake(&f, &mut station).await;
    until("the second wake", || n(&wake.counters.sent_ok) == 2).await;
    wake.stop().await;
}

#[tokio::test]
async fn a_monitor_that_is_on_unknown_or_not_named_is_not_woken() {
    let (f, mut station, c, _id) = bound().await;
    let wake = start(&f, &c, Limits::default()).await;
    let mut other = off(&f);
    other.monitors[0].name = "HDMI-A-1".to_owned();
    let cases = [
        desktop(Some(true), Some(0), Some(now(&f))),
        desktop(None, Some(0), Some(now(&f))),
        other,
    ];
    for (i, answer) in cases.iter().enumerate() {
        let seq = u64::try_from(i).unwrap() + 1;
        push(&f, vec![requested(seq, "s1", now(&f), &format!("r{seq}"))]).await;
        serve_info(&mut station, answer).await;
        until("a monitor skip", || n(&wake.counters.skip_monitor) == seq).await;
        quiet(&mut station).await;
        advance(&f, 31);
    }
    assert!(wake_rows(&c).await.is_empty());
    wake.stop().await;
}

#[tokio::test]
async fn quiet_hours_and_an_unknown_or_stale_offset_block_the_wake() {
    let (f, mut station, c, id) = bound().await;
    // The test clock is 00:00 UTC and the offset is 0.
    set_display(
        &c,
        &id,
        DisplayChange {
            quiet_hours: Some(Some("23:00-01:00".to_owned())),
            ..DisplayChange::default()
        },
    )
    .await;
    let wake = start(&f, &c, Limits::default()).await;
    let answers = [
        // Inside the quiet hours.
        off(&f),
        // The offset is unknown.
        desktop(Some(false), None, Some(now(&f))),
        // The offset sample is too old.
        desktop(Some(false), Some(0), Some(now(&f) - 16_000)),
    ];
    for (i, answer) in answers.iter().enumerate() {
        let seq = u64::try_from(i).unwrap() + 1;
        let expect_skip = seq;
        push(&f, vec![requested(seq, "s1", now(&f), &format!("q{seq}"))]).await;
        serve_info(&mut station, answer).await;
        until("a quiet skip", || {
            n(&wake.counters.skip_quiet) == expect_skip
        })
        .await;
        quiet(&mut station).await;
        advance(&f, 31);
    }
    // Outside the quiet hours the wake goes out.
    set_display(
        &c,
        &id,
        DisplayChange {
            quiet_hours: Some(Some("01:00-02:00".to_owned())),
            ..DisplayChange::default()
        },
    )
    .await;
    push(&f, vec![requested(4, "s1", now(&f), "q4")]).await;
    serve_wake(&f, &mut station).await;
    until("the wake", || n(&wake.counters.sent_ok) == 1).await;
    // Without quiet hours the offset does not matter.
    set_display(
        &c,
        &id,
        DisplayChange {
            quiet_hours: Some(None),
            ..DisplayChange::default()
        },
    )
    .await;
    advance(&f, 31);
    push(&f, vec![requested(5, "s1", now(&f), "q5")]).await;
    serve_info(&mut station, &desktop(Some(false), None, None)).await;
    serve_act(&mut station, ok(None)).await;
    until("the wake without quiet hours", || {
        n(&wake.counters.sent_ok) == 2
    })
    .await;
    wake.stop().await;
}

#[tokio::test]
async fn a_display_that_does_not_take_the_wake_and_the_deck_switch_stop_it() {
    let (f, mut station, c, id) = bound().await;
    // Wake off, revoked, and not bound.
    set_display(
        &c,
        &id,
        DisplayChange {
            wake_on_needs_you: Some(false),
            ..DisplayChange::default()
        },
    )
    .await;
    let revoked = display(&f, &c, "Revoked").await;
    bind(&f, &c, &revoked, "DP-1").await;
    assert!(c.store.display_revoke(revoked).await.unwrap());
    display(&f, &c, "Free").await;
    let wake = start(&f, &c, Limits::default()).await;
    push(&f, vec![requested(1, "s1", now(&f), "r1")]).await;
    until("no display takes it", || {
        n(&wake.counters.skip_no_display) == 1
    })
    .await;
    quiet(&mut station).await;
    // The switch off: no display lookup and no wake.
    set_display(
        &c,
        &id,
        DisplayChange {
            wake_on_needs_you: Some(true),
            ..DisplayChange::default()
        },
    )
    .await;
    enable(&c, false);
    advance(&f, 31);
    push(&f, vec![requested(2, "s1", now(&f), "r2")]).await;
    until("the switch is off", || n(&wake.counters.skip_control) == 1).await;
    quiet(&mut station).await;
    // On again: a new request wakes.
    enable(&c, true);
    advance(&f, 31);
    push(&f, vec![requested(3, "s1", now(&f), "r3")]).await;
    serve_wake(&f, &mut station).await;
    until("the wake", || n(&wake.counters.sent_ok) == 1).await;
    // The skipped requests stay reserved: they never wake later.
    assert_eq!(n(&wake.counters.reserved), 3);
    wake.stop().await;
}

#[tokio::test]
async fn the_repository_filter_of_the_directive_applies_to_the_wake() {
    let (f, mut station, c, id) = bound().await;
    f.store
        .apply_snapshot(
            f.station.clone(),
            f.inc.clone(),
            0,
            vec![
                card_in("s1", "/p/other", vec![]),
                card_in("s2", "/p/app", vec![]),
            ],
        )
        .await
        .unwrap();
    let body = DirectiveBody::Mission {
        pin: false,
        filter: Some(DirectiveFilter {
            stations: vec![f.station.clone()],
            repo: Some("app".to_owned()),
        }),
    };
    c.store
        .display_directive_set(id, DirectiveSender::Owner, body)
        .await
        .unwrap();
    let wake = start(&f, &c, Limits::default()).await;
    push(&f, vec![requested(1, "s1", now(&f), "r1")]).await;
    until("outside the filter", || {
        n(&wake.counters.skip_no_display) == 1
    })
    .await;
    quiet(&mut station).await;
    push(&f, vec![requested(2, "s2", now(&f), "r2")]).await;
    serve_wake(&f, &mut station).await;
    until("inside the filter", || n(&wake.counters.sent_ok) == 1).await;
    wake.stop().await;
}

#[tokio::test]
async fn a_request_that_closes_while_the_info_runs_does_not_wake() {
    let (f, mut station, c, _id) = bound().await;
    let wake = start(&f, &c, Limits::default()).await;
    push(&f, vec![requested(1, "s1", now(&f), "r1")]).await;
    let req = next_request(&mut station).await;
    assert_eq!(req.method, "desktop.info");
    // The request is answered before the info comes back.
    push(&f, vec![resolved(2, "s1", now(&f), "r1")]).await;
    station
        .control
        .respond(req.id, Outcome::Ok(serde_json::to_value(off(&f)).unwrap()))
        .await
        .unwrap();
    until("the closed request", || n(&wake.counters.skip_closed) == 1).await;
    quiet(&mut station).await;
    assert!(wake_rows(&c).await.is_empty());
    wake.stop().await;
}

#[tokio::test]
async fn a_failed_info_is_a_skip_and_a_timed_out_action_is_audited_and_not_retried() {
    let (f, mut station, c, _id) = bound().await;
    let wake = start(&f, &c, Limits::default()).await;
    // The station cannot read the desktop: no mutation was tried, no row.
    push(&f, vec![requested(1, "s1", now(&f), "r1")]).await;
    let req = next_request(&mut station).await;
    station
        .control
        .respond(req.id, err("failed", "no desktop"))
        .await
        .unwrap();
    until("the info skip", || n(&wake.counters.skip_info) == 1).await;
    assert!(wake_rows(&c).await.is_empty());
    // A timeout of the action: the outcome is unknown, so no retry.
    advance(&f, 31);
    push(&f, vec![requested(2, "s1", now(&f), "r2")]).await;
    serve_info(&mut station, &off(&f)).await;
    serve_act(&mut station, err("failed", ACT_TIMEOUT_TEXT)).await;
    until("the failed send", || n(&wake.counters.sent_failed) == 1).await;
    quiet(&mut station).await;
    let rows = wake_rows(&c).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].result, AuditResult::Failed);
    assert!(
        rows[0]
            .detail
            .as_deref()
            .unwrap()
            .ends_with("code=desktop_timeout")
    );
    wake.stop().await;
}

#[tokio::test]
async fn a_full_queue_drops_the_job_and_counts_it() {
    let (f, mut station, c, _id) = bound().await;
    let other = display(&f, &c, "Second wall").await;
    bind(&f, &c, &other, "DP-2").await;
    // No worker takes a job, and the queue holds one.
    let limits = Limits {
        workers: 0,
        queue: 1,
        ..Limits::default()
    };
    let wake = start(&f, &c, limits).await;
    push(&f, vec![requested(1, "s1", now(&f), "r1")]).await;
    until("a dropped job", || n(&wake.counters.queue_full) == 1).await;
    quiet(&mut station).await;
    wake.stop().await;
}

#[tokio::test]
async fn the_task_stops_with_the_shutdown_signal() {
    let (f, _station, c, _id) = bound().await;
    let wake = start(&f, &c, Limits::default()).await;
    assert!(!wake.task.is_finished());
    wake.stop().await;
}
