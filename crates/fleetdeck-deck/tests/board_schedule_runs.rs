//! "Run now" of an automation over a fake station link (R5, DL-168): the
//! precheck goes to the station, a non-zero exit is a `skipped` run, a pass
//! starts a session in a worktree and keeps its id, and `reuse_session`
//! sends a follow-up to the idle session of the last run.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::time::Duration;

use common::board::{BoardClient, Http, board};
use common::*;
use fleetdeck_deck::store::AuditResult;
use fleetdeck_proto::command::{CommandResult, Receipt, ReceiptState};
use fleetdeck_proto::event::{ProviderKind, SessionState};
use fleetdeck_proto::link::{Outcome, PrecheckResult, StationInfo};
use fleetdeck_proto::session::Activity;
use fleetdeck_proto::{CommandId, SessionSummary};
use serde_json::{Value, json};

fn info() -> StationInfo {
    StationInfo {
        name: "fake".to_owned(),
        incarnation: "fake-station-db".to_owned().into(),
        head: 0,
        live_sessions: 0,
        version: None,
        max_sessions: Some(4),
        allow_shell: None,
        allow_setup_cmd: Some(true),
        providers: Vec::new(),
        assist_tokens_today: None,
        assist_cap: None,
        resources: None,
        desktop: None,
        traits: None,
        config: None,
        taildrive: None,
        fleet_git: None,
        fleet_summary: None,
    }
}

fn schedule(reuse: bool, precheck: Option<&str>) -> Value {
    json!({
        "name": "Nightly",
        "enabled": true,
        "cron": "@daily",
        "timezone": "UTC",
        "station": "fake",
        "path": "/work/repo",
        "provider_instance": "claude",
        "mode": "plan",
        "prompt": "Fix the flaky tests.",
        "precheck_cmd": precheck,
        "reuse_session": reuse,
    })
}

fn started(id: &CommandId, session: &str) -> Outcome {
    Outcome::Ok(json!(Receipt {
        command_id: id.clone(),
        state: ReceiptState::Accepted,
        station_seq: Some(1),
        result: Some(CommandResult {
            session_id: Some(session.into()),
            input_id: None,
            seq: None,
        }),
        reject: None,
        at: 0,
    }))
}

fn pre(code: i32, tail: &str) -> Outcome {
    Outcome::Ok(json!(PrecheckResult {
        exit_code: Some(code),
        timed_out: false,
        output_tail: tail.to_owned(),
    }))
}

async fn owner(c: &BoardClient) -> Vec<(String, String)> {
    let (cookie, csrf) = c.login().await;
    vec![
        ("Cookie".to_owned(), cookie),
        ("X-CSRF-Token".to_owned(), csrf),
    ]
}

fn refs(h: &[(String, String)]) -> Vec<(&str, &str)> {
    h.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect()
}

async fn create(c: &BoardClient, h: &[(String, String)], body: Value) -> String {
    let r = c.post("/api/schedules", &refs(h), body).await;
    assert_eq!(r.status, 200, "{}", r.text);
    r.body["schedule_id"].as_str().unwrap().to_owned()
}

async fn run_now(c: &BoardClient, h: &[(String, String)], id: &str) -> Http {
    c.post(&format!("/api/schedules/{id}/run"), &refs(h), json!({}))
        .await
}

/// The runs of a schedule once the newest one left `started` with a reason
/// or a session (the work runs in a task).
async fn settled(c: &BoardClient, h: &[(String, String)], id: &str, want: usize) -> Vec<Value> {
    let deadline = tokio::time::Instant::now() + T;
    loop {
        let r = c
            .get(&format!("/api/schedules/{id}/runs"), &refs(h)[..1])
            .await;
        let runs = r.body.as_array().unwrap().clone();
        let done = runs.len() == want
            && runs
                .iter()
                .all(|x| x["state"] != "started" || !x["session_id"].is_null());
        if done {
            return runs;
        }
        assert!(tokio::time::Instant::now() < deadline, "{runs:?}");
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
}

#[tokio::test]
async fn a_failed_precheck_skips_and_a_passed_one_starts_a_worktree_session() {
    let mut f = fixture().await;
    let mut station = f.connect().await;
    f.store.set_station_info(&f.station, info());
    let c = board(std::sync::Arc::clone(&f.ctx)).await;
    let h = owner(&c).await;
    let id = create(&c, &h, schedule(false, Some("test -s todo.txt"))).await;

    // The precheck exits 1: a skipped run with the last output line; no
    // start is sent.
    let r = run_now(&c, &h, &id).await;
    assert_eq!(r.status, 200, "{}", r.text);
    assert_eq!(
        (r.body["state"].as_str(), r.body["manual"].as_bool()),
        (Some("started"), Some(true))
    );
    let req = next_request(&mut station).await;
    assert_eq!(req.method, "workspace.precheck");
    assert_eq!(req.params["cwd"], "/work/repo");
    assert_eq!(req.params["cmd"], "test -s todo.txt");
    station
        .control
        .respond(req.id, pre(1, "checking\nnothing to do\n"))
        .await
        .unwrap();
    let deadline = tokio::time::Instant::now() + T;
    let runs = loop {
        let r = c
            .get(&format!("/api/schedules/{id}/runs"), &refs(&h)[..1])
            .await;
        let runs = r.body.as_array().unwrap().clone();
        if runs.first().is_some_and(|x| x["state"] == "skipped") {
            break runs;
        }
        assert!(tokio::time::Instant::now() < deadline, "{runs:?}");
        tokio::time::sleep(Duration::from_millis(30)).await;
    };
    assert_eq!(runs[0]["reason"], "nothing to do");
    assert_eq!(runs[0]["session_id"], Value::Null);
    no_request(&mut station, Duration::from_millis(300)).await;
    // A run now is claimed at the clock time; the test clock is manual.
    f.det.advance(Duration::from_secs(1));

    // The precheck exits 0: the station gets a start in a new worktree.
    let r = run_now(&c, &h, &id).await;
    assert_eq!(r.status, 200, "{}", r.text);
    let req = next_request(&mut station).await;
    assert_eq!(req.method, "workspace.precheck");
    station.control.respond(req.id, pre(0, "")).await.unwrap();
    let start = Fixture::answer(&mut station, |id| started(id, "sess-1")).await;
    assert_eq!(start.method, "session.start");
    let cmd = &start.params["command"];
    assert_eq!(cmd["op"], "start");
    assert_eq!(cmd["spec"]["cwd"], "/work/repo");
    assert_eq!(cmd["spec"]["provider_instance"], "claude");
    assert_eq!(cmd["spec"]["mode"], "plan");
    assert!(
        cmd["spec"]["title"]
            .as_str()
            .unwrap()
            .starts_with("Nightly 2026-"),
        "{}",
        cmd["spec"]["title"]
    );
    assert_eq!(cmd["prompt"]["text"], "Fix the flaky tests.");
    assert_eq!(cmd["workspace"]["kind"], "worktree");
    assert!(cmd["setup_cmd"].is_null());
    let runs = settled(&c, &h, &id, 2).await;
    assert_eq!(runs[0]["state"], "started");
    assert_eq!(runs[0]["session_id"], "sess-1");
    let list = c.get("/api/schedules", &refs(&h)[..1]).await;
    assert_eq!(
        list.body[0]["schedule"]["last_session"]["session_id"],
        "sess-1"
    );
    assert_eq!(list.body[0]["last_run"]["state"], "started");
    assert!(
        !c.audited("schedules.result", AuditResult::Ok)
            .await
            .is_empty(),
        "the end of a run is audited"
    );
}

#[tokio::test]
async fn reuse_session_sends_a_follow_up_to_the_idle_session_of_the_last_run() {
    let mut f = fixture().await;
    let mut station = f.connect().await;
    f.store.set_station_info(&f.station, info());
    let c = board(std::sync::Arc::clone(&f.ctx)).await;
    let h = owner(&c).await;
    let id = create(&c, &h, schedule(true, None)).await;

    // The first run starts a session.
    run_now(&c, &h, &id).await;
    let start = Fixture::answer(&mut station, |id| started(id, "sess-1")).await;
    assert_eq!(start.method, "session.start");
    settled(&c, &h, &id, 1).await;

    // The session is working: the next run starts another one.
    let mut card = SessionSummary::new("sess-1".into());
    card.state = SessionState::Running;
    card.activity = Activity::Working;
    card.provider = Some(ProviderKind::Claude);
    f.store
        .apply_snapshot(f.station.clone(), f.inc.clone(), 0, vec![card.clone()])
        .await
        .unwrap();
    // A run now is claimed at the clock time; the test clock is manual.
    f.det.advance(Duration::from_secs(1));
    run_now(&c, &h, &id).await;
    let second = Fixture::answer(&mut station, |id| started(id, "sess-2")).await;
    assert_eq!(second.method, "session.start");
    settled(&c, &h, &id, 2).await;

    // Now the last session (sess-2) is idle: the prompt is a follow-up.
    let mut idle = SessionSummary::new("sess-2".into());
    idle.state = SessionState::Running;
    idle.activity = Activity::Idle;
    idle.provider = Some(ProviderKind::Claude);
    f.store
        .apply_snapshot(f.station.clone(), f.inc.clone(), 0, vec![card, idle])
        .await
        .unwrap();
    // A run now is claimed at the clock time; the test clock is manual.
    f.det.advance(Duration::from_secs(1));
    run_now(&c, &h, &id).await;
    let third = Fixture::answer(&mut station, |id| started(id, "sess-2")).await;
    assert_eq!(third.method, "session.follow_up");
    assert_eq!(third.params["command"]["session_id"], "sess-2");
    assert_eq!(
        third.params["command"]["input"]["text"],
        "Fix the flaky tests."
    );
    settled(&c, &h, &id, 3).await;
}
