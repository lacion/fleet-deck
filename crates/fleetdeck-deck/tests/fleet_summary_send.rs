//! The sender of the fleet summary (R13 P4 task B1, `omarchy-p4.md` §3.2)
//! over a real in-process link: the eligibility, the answer codes, the
//! keepalive and the setting that turns off. The fake station answers
//! `fleet.summary`; the deck clock is the real clock here, so the limits are
//! small. The schedule itself is tested with explicit times in the unit
//! tests of the module.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::time::Duration;

use common::board::{BoardClient, board};
use common::*;
use fleetdeck_deck::fleet_summary::{Limits, SummaryCounters, spawn_with};
use fleetdeck_link::LiveLink;
use fleetdeck_link::control::InboundRequest;
use fleetdeck_proto::SessionSummary;
use fleetdeck_proto::event::{RequestKind, ResponseChannel, SessionState};
use fleetdeck_proto::link::{Outcome, StationInfo, WireError};
use fleetdeck_proto::session::Activity;
use fleetdeck_proto::session::OpenRequest;
use fleetdeck_proto::version::feature;
use serde_json::{Value, json};
use tokio::sync::watch;
use tokio::task::JoinHandle;

const SENTINEL: &str = "SENTINEL-FREE-TEXT-3c7e";

/// Small limits: a push every 100 ms at most, a keepalive of 60 s (a test
/// makes it shorter), 5 s to answer.
const LIMITS: Limits = Limits {
    debounce_ms: 100,
    keepalive_ms: 60_000,
    timeout_ms: 5000,
};

struct Rig {
    f: Fixture,
    station: LiveLink,
    c: BoardClient,
    counters: Arc<SummaryCounters>,
    stop: watch::Sender<bool>,
    task: JoinHandle<()>,
}

fn n(counter: &AtomicU64) -> u64 {
    SummaryCounters::get(counter)
}

/// A `station.info` with the `fleet_summary` setting `setting`; `sessions`
/// changes the info so that the board changes.
fn info(setting: Option<bool>, sessions: u64) -> StationInfo {
    serde_json::from_value(json!({
        "name": "fake", "incarnation": "inc", "head": 1, "live_sessions": sessions,
        "fleet_summary": setting
    }))
    .unwrap()
}

async fn until(what: &str, ready: impl Fn() -> bool) {
    let deadline = tokio::time::Instant::now() + T;
    while !ready() {
        assert!(tokio::time::Instant::now() < deadline, "timed out: {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// A linked fake station with a session that needs the owner (its texts are
/// the sentinel), the board, and the sender with `limits`. The station
/// info holds `setting`.
async fn rig(limits: Limits, setting: Option<bool>) -> Rig {
    let mut f = fixture().await;
    let station = f.connect().await;
    let mut card = SessionSummary::new("s1".into());
    card.state = SessionState::Running;
    card.activity = Activity::NeedsYou;
    card.callsign = Some("lynx-12".to_owned());
    card.title = Some(SENTINEL.to_owned());
    card.cwd = Some(format!("/work/{SENTINEL}"));
    card.open_requests = vec![OpenRequest {
        request_id: "r1".into(),
        node_id: None,
        request_kind: RequestKind::Command,
        title: SENTINEL.to_owned(),
        details: SENTINEL.to_owned(),
        input: Some(json!({ "command": SENTINEL })),
        options: vec![],
        response: ResponseChannel::RpcReply,
        requested_at: i64::try_from(f.det.now_ms()).unwrap() - 5000,
    }];
    f.store
        .apply_snapshot(f.station.clone(), f.inc.clone(), 0, vec![card])
        .await
        .unwrap();
    let c = board(Arc::clone(&f.ctx)).await;
    f.store.set_station_info(&f.station, info(setting, 0));
    let counters = Arc::new(SummaryCounters::default());
    let (stop, rx) = watch::channel(false);
    let task = spawn_with(
        f.store.clone(),
        Arc::clone(&f.ctx),
        Arc::clone(&c.state.endpoints),
        rx,
        limits,
        Arc::clone(&counters),
    );
    Rig {
        f,
        station,
        c,
        counters,
        stop,
        task,
    }
}

impl Rig {
    /// The next `fleet.summary` request of the deck.
    async fn summary(&mut self) -> InboundRequest {
        let req = next_request(&mut self.station).await;
        assert_eq!(req.method, "fleet.summary");
        req
    }

    async fn answer(&self, req: &InboundRequest, outcome: Outcome) {
        self.station.control.respond(req.id, outcome).await.unwrap();
    }

    /// Changes the kept `station.info` (the board changes).
    fn set_info(&self, setting: Option<bool>, sessions: u64) {
        self.f
            .store
            .set_station_info(&self.f.station, info(setting, sessions));
    }

    async fn stop(self) {
        self.stop.send(true).unwrap();
        tokio::time::timeout(Duration::from_secs(10), self.task)
            .await
            .expect("the sender stops")
            .unwrap();
        drop(self.c);
    }
}

#[tokio::test]
async fn an_eligible_station_gets_the_summary_without_free_text() {
    let mut r = rig(LIMITS, Some(true)).await;
    let req = r.summary().await;
    let text = req.params.to_string();
    r.answer(&req, Outcome::Ok(json!({}))).await;
    until("the push is counted", || n(&r.counters.sent) == 1).await;
    assert_eq!((n(&r.counters.failed), n(&r.counters.builds)), (0, 1));
    let p: &Value = &req.params;
    assert_eq!((p["v"].clone(), p["deck"].clone()), (json!(1), json!("up")));
    assert!(p["at"].as_str().unwrap().ends_with('Z'));
    // The test board has loopback addresses only.
    assert!(p["board"].is_null());
    assert_eq!(p["counts"]["needs_you"], json!(1));
    assert_eq!(p["needs_you"][0]["callsign"], json!("lynx-12"));
    assert_eq!(p["needs_you"][0]["station"], json!("fake"));
    assert_eq!(p["needs_you"][0]["kind"], json!("command"));
    assert_eq!(
        p["stations"][0],
        json!({ "name": "fake", "up": true, "cpu_load_pct": null })
    );
    assert!(!text.contains("SENTINEL"), "{text}");
    r.stop().await;
}

#[tokio::test]
async fn a_station_without_the_setting_gets_nothing_until_it_turns_on() {
    for setting in [None, Some(false)] {
        let mut r = rig(LIMITS, setting).await;
        no_request(&mut r.station, Duration::from_millis(500)).await;
        assert_eq!(
            (
                n(&r.counters.builds),
                n(&r.counters.sent),
                n(&r.counters.failed)
            ),
            (0, 0, 0)
        );
        // A `station.info` that turns the setting on sends at once.
        r.set_info(Some(true), 0);
        let req = r.summary().await;
        r.answer(&req, Outcome::Ok(json!({}))).await;
        until("the push is counted", || n(&r.counters.sent) == 1).await;
        r.stop().await;
    }
}

#[tokio::test]
async fn a_setting_that_turns_off_stops_the_next_push() {
    let mut r = rig(LIMITS, Some(true)).await;
    let req = r.summary().await;
    r.answer(&req, Outcome::Ok(json!({}))).await;
    until("the push is counted", || n(&r.counters.sent) == 1).await;
    r.set_info(Some(false), 1);
    // Changes while the setting is off send nothing, after the window too.
    r.set_info(Some(false), 2);
    no_request(&mut r.station, Duration::from_millis(600)).await;
    assert_eq!((n(&r.counters.sent), n(&r.counters.failed)), (1, 0));
    r.stop().await;
}

#[tokio::test]
async fn a_link_without_the_feature_gets_nothing() {
    let mut r = rig(LIMITS, Some(true)).await;
    let req = r.summary().await;
    r.answer(&req, Outcome::Ok(json!({}))).await;
    until("the push is counted", || n(&r.counters.sent) == 1).await;
    // The first link has the token 0. A link that did not negotiate the
    // feature makes the station not eligible.
    r.f.store.set_link_features(&r.f.station, 0, vec![]);
    r.set_info(Some(true), 1);
    no_request(&mut r.station, Duration::from_millis(600)).await;
    assert_eq!(n(&r.counters.sent), 1);
    // The feature is back: the next change sends again.
    r.f.store
        .set_link_features(&r.f.station, 0, vec![feature::DESKTOP_SHELL.to_owned()]);
    r.set_info(Some(true), 2);
    let req = r.summary().await;
    r.answer(&req, Outcome::Ok(json!({}))).await;
    until("the second push", || n(&r.counters.sent) == 2).await;
    r.stop().await;
}

#[tokio::test]
async fn an_unsupported_answer_is_counted_as_failed_and_the_next_change_sends_again() {
    let mut r = rig(LIMITS, Some(true)).await;
    let req = r.summary().await;
    let refusal = Outcome::Err(WireError::new(
        "unsupported",
        "The fleet summary is not served by this station build.",
    ));
    r.answer(&req, refusal).await;
    until("the failure is counted", || n(&r.counters.failed) == 1).await;
    assert_eq!(n(&r.counters.sent), 0);
    // No retry by itself: the keepalive is 60 s. A change sends again.
    no_request(&mut r.station, Duration::from_millis(400)).await;
    r.set_info(Some(true), 1);
    let req = r.summary().await;
    r.answer(&req, Outcome::Ok(json!({}))).await;
    until("the second push", || n(&r.counters.sent) == 1).await;
    assert_eq!(n(&r.counters.failed), 1);
    r.stop().await;
}

#[tokio::test]
async fn a_station_that_does_not_answer_in_time_is_counted_as_failed() {
    let limits = Limits {
        timeout_ms: 200,
        ..LIMITS
    };
    let mut r = rig(limits, Some(true)).await;
    let _unanswered = r.summary().await;
    until("the timeout is counted", || n(&r.counters.failed) == 1).await;
    assert_eq!(n(&r.counters.sent), 0);
    r.stop().await;
}

#[tokio::test]
async fn the_keepalive_sends_without_a_change() {
    let limits = Limits {
        keepalive_ms: 400,
        ..LIMITS
    };
    let mut r = rig(limits, Some(true)).await;
    let first = r.summary().await;
    r.answer(&first, Outcome::Ok(json!({}))).await;
    let second = r.summary().await;
    r.answer(&second, Outcome::Ok(json!({}))).await;
    until("two pushes", || n(&r.counters.sent) == 2).await;
    r.stop().await;
}
