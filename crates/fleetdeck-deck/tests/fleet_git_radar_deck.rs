//! The radar on the deck (T3.3b, design §10.2-§10.6, §8.6; FG-7): which
//! branches a run sends, the trigger after `session.push`, the rows that each
//! run replaces, the notices of a failed run, and `GET /api/radar`. A fake
//! station answers `project.radar`; the radar task runs as in the deck, with
//! short timers.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::time::Duration;

use common::fleet_git_fake::{FakeGit, Setup, bearer, synced};
use common::fleet_git_land::{RadarReply, spawn_radar};
use fleetdeck_deck::fleet_git_radar::RadarOptions;
use fleetdeck_proto::event::{Event, EventBody, SessionState};
use fleetdeck_proto::fleet_git::{PushInfo, PushState};
use fleetdeck_proto::link::Scope;
use fleetdeck_proto::{EventEnvelope, SessionSummary};
use serde_json::{Value, json};

/// No timer run inside a test; a push run starts 150 ms after the push.
const OPTIONS: RadarOptions = RadarOptions {
    every: Duration::from_secs(3600),
    after_push: Duration::from_millis(150),
};

fn session(
    id: &str,
    state: SessionState,
    push: Option<(PushState, bool)>,
    at: i64,
) -> SessionSummary {
    let mut summary = SessionSummary::new(id.into());
    summary.state = state;
    summary.cwd = Some("/work/app".to_owned());
    summary.callsign = Some(id.to_owned());
    summary.title = Some(format!("Title {id}"));
    summary.updated_at = at;
    summary.push = push.map(|(state, oid)| PushInfo {
        state,
        ref_name: format!("fd/fake/{id}"),
        oid: oid.then(|| "a".repeat(40)),
        desired_oid: None,
        stop_code: None,
        attempts: 0,
        next_attempt_at: None,
        code: None,
        text: None,
    });
    summary
}

async fn show(s: &Setup, sessions: Vec<SessionSummary>) {
    s.f.store
        .apply_snapshot(s.f.station.clone(), s.f.inc.clone(), 0, sessions)
        .await
        .unwrap();
}

/// A `session.push` event with the state `pushed` for `id`.
async fn pushed(s: &Setup, seq: u64, id: &str) {
    let env = EventEnvelope {
        station_seq: seq,
        session_id: id.into(),
        session_seq: seq,
        at: 1_000,
        proto_version: 1,
        raw_ref: None,
        event: EventBody::Known(Event::SessionPush {
            state: PushState::Pushed,
            ref_name: format!("fd/fake/{id}"),
            oid: Some("a".repeat(40)),
            desired_oid: None,
            stop_code: None,
            attempts: 0,
            next_attempt_at: None,
            code: None,
            text: None,
        }),
    };
    s.f.store
        .apply_events(s.f.station.clone(), s.f.inc.clone(), vec![env])
        .await
        .unwrap();
}

async fn radar(s: &Setup) -> Value {
    let r = s.c.get("/api/radar", &s.h()).await;
    assert_eq!(r.status, 200, "{}", r.body);
    r.body
}

/// Waits until `GET /api/radar` has `n` will-conflict rows.
async fn wait_rows(s: &Setup, n: usize) -> Value {
    for _ in 0..250 {
        let body = radar(s).await;
        let rows = body["groups"]
            .as_array()
            .unwrap()
            .iter()
            .map(|g| g["will_conflict"].as_array().unwrap().len())
            .sum::<usize>();
        if rows == n {
            return body;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("timed out: {n} will-conflict rows, got {}", radar(s).await);
}

async fn wait_notice(s: &Setup, text: Option<&str>) -> Value {
    for _ in 0..250 {
        let body = radar(s).await;
        let notices = body["notices"].as_array().unwrap();
        let got = notices.first().and_then(|n| n["text"].as_str());
        if got == text {
            return body;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("timed out: notice {text:?}, got {}", radar(s).await);
}

async fn world() -> (Setup, std::sync::Arc<FakeGit>) {
    let (s, fake) = synced().await;
    show(
        &s,
        vec![
            session(
                "s1",
                SessionState::Running,
                Some((PushState::Pushed, true)),
                10,
            ),
            session(
                "s2",
                SessionState::Running,
                Some((PushState::Failing, true)),
                20,
            ),
        ],
    )
    .await;
    (s, fake)
}

#[tokio::test]
async fn a_run_sends_only_the_live_pushed_branches_newest_first() {
    let (s, fake) = synced().await;
    show(
        &s,
        vec![
            session(
                "s1",
                SessionState::Running,
                Some((PushState::Pushed, true)),
                10,
            ),
            session(
                "s2",
                SessionState::Running,
                Some((PushState::Failing, true)),
                20,
            ),
            session(
                "s3",
                SessionState::Running,
                Some((PushState::Stopped, true)),
                30,
            ),
            // Not sent: no oid, not pushed yet, push off, ended, lost.
            session(
                "s4",
                SessionState::Running,
                Some((PushState::Pushed, false)),
                40,
            ),
            session(
                "s5",
                SessionState::Running,
                Some((PushState::Due, true)),
                50,
            ),
            session("s6", SessionState::Running, None, 60),
            session(
                "s7",
                SessionState::Exited,
                Some((PushState::Pushed, true)),
                70,
            ),
            session(
                "s8",
                SessionState::Lost,
                Some((PushState::Pushed, true)),
                80,
            ),
            // The same time as s1: the lower session id comes first.
            session(
                "s0",
                SessionState::Running,
                Some((PushState::Pushed, true)),
                10,
            ),
        ],
    )
    .await;
    let _stop = spawn_radar(&s.f, OPTIONS);
    let sent = fake.wait_for("project.radar", 1).await;
    assert_eq!(sent[0]["project_id"], s.app.project_id.as_str());
    let branches = sent[0]["branches"].as_array().unwrap();
    let keys: Vec<&str> = branches
        .iter()
        .map(|b| b["key"].as_str().unwrap())
        .collect();
    assert_eq!(keys, ["s3", "s2", "s0", "s1"]);
    assert_eq!(branches[0]["ref_name"], "fd/fake/s3");
}

#[tokio::test]
async fn the_snapshot_names_the_default_radar_home_and_the_run_fills_the_groups() {
    let (s, fake) = world().await;
    // The default radar home (§10.1): the first home, so the station knows it.
    let snapshots = fake.snapshots();
    assert!(snapshots.last().unwrap().projects[0].radar_home);
    *fake.radar.lock().unwrap() = RadarReply::pair("s1", "s2", true, &["a.rs", "b.rs"]);
    let _stop = spawn_radar(&s.f, OPTIONS);
    let body = wait_rows(&s, 1).await;
    let group = &body["groups"][0];
    assert_eq!(group["project_id"], s.app.project_id.as_str());
    assert_eq!(group["group_key"], "origin:github.com/acme/app");
    let row = &group["will_conflict"][0];
    assert_eq!(row["a"]["session_id"], "s1");
    assert_eq!(row["a"]["callsign"], "s1");
    assert_eq!(row["a"]["title"], "Title s1");
    assert_eq!(row["b"]["session_id"], "s2");
    assert_eq!(row["files"], json!(["a.rs", "b.rs"]));
    assert_eq!(row["checked_at"], 5_000);
    assert_eq!(group["same_files"], json!([]));
    assert_eq!(body["notices"], json!([]));
}

#[tokio::test]
async fn a_push_starts_one_run_for_a_burst_and_the_rows_are_replaced() {
    let (s, fake) = world().await;
    *fake.radar.lock().unwrap() = RadarReply::pair("s1", "s2", true, &["a.rs"]);
    let _stop = spawn_radar(&s.f, OPTIONS);
    wait_rows(&s, 1).await;
    assert_eq!(fake.requests("project.radar").len(), 1);
    // The next run: s1 against the default branch (`b = @default`, "default").
    *fake.radar.lock().unwrap() = RadarReply::pair("s1", "@default", true, &["c.rs"]);
    for (seq, id) in [(1, "s1"), (2, "s2"), (3, "s1")] {
        pushed(&s, seq, id).await;
    }
    fake.wait_for("project.radar", 2).await;
    let body = wait_rows(&s, 1).await;
    let row = &body["groups"][0]["will_conflict"][0];
    assert_eq!(row["b"], "default");
    assert_eq!(row["files"], json!(["c.rs"]));
    // One run for the whole burst.
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(fake.requests("project.radar").len(), 2);
    // A pair without a conflict is stored but not shown.
    *fake.radar.lock().unwrap() = RadarReply::pair("s1", "s2", false, &[]);
    pushed(&s, 4, "s1").await;
    fake.wait_for("project.radar", 3).await;
    wait_rows(&s, 0).await;
}

#[tokio::test]
async fn a_failed_run_keeps_the_rows_and_sets_a_notice_that_the_next_good_run_clears() {
    let (s, fake) = world().await;
    *fake.radar.lock().unwrap() = RadarReply::pair("s1", "s2", true, &["a.rs"]);
    let _stop = spawn_radar(&s.f, OPTIONS);
    wait_rows(&s, 1).await;
    let mut seq = 0;
    let mut run = |n: usize| {
        seq += 1;
        let s = &s;
        let fake = &fake;
        async move {
            pushed(s, seq, "s1").await;
            fake.wait_for("project.radar", n).await;
        }
    };
    *fake.radar.lock().unwrap() = RadarReply::failed("fetch_failed", "The fetch failed: boom");
    run(2).await;
    let body = wait_notice(
        &s,
        Some("Radar: the fetch failed on fake: The fetch failed: boom"),
    )
    .await;
    assert_eq!(body["notices"][0]["code"], "fetch_failed");
    assert_eq!(body["notices"][0]["project_id"], s.app.project_id.as_str());
    // The rows of the last good run stay.
    assert_eq!(
        body["groups"][0]["will_conflict"].as_array().unwrap().len(),
        1
    );
    *fake.radar.lock().unwrap() = RadarReply::failed("git_too_old", "Git is older than 2.45.");
    run(3).await;
    wait_notice(&s, Some("Radar: Git on fake is older than 2.45.")).await;
    *fake.radar.lock().unwrap() =
        RadarReply::failed("no_default_branch", "The remote has no default branch.");
    run(4).await;
    wait_notice(&s, Some("Radar: The remote has no default branch.")).await;
    // `radar_busy` and another wire error change nothing.
    *fake.radar.lock().unwrap() = RadarReply::Error("radar_busy");
    run(5).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    wait_notice(&s, Some("Radar: The remote has no default branch.")).await;
    *fake.radar.lock().unwrap() = RadarReply::pair("s1", "s2", true, &["z.rs"]);
    run(6).await;
    let body = wait_notice(&s, None).await;
    assert_eq!(
        body["groups"][0]["will_conflict"][0]["files"],
        json!(["z.rs"])
    );
}

#[tokio::test]
async fn a_project_with_no_pushed_branch_loses_its_rows_and_makes_no_call() {
    let (s, fake) = world().await;
    *fake.radar.lock().unwrap() = RadarReply::pair("s1", "s2", true, &["a.rs"]);
    let _stop = spawn_radar(&s.f, OPTIONS);
    wait_rows(&s, 1).await;
    // Both sessions end: no live pushed branch is left.
    show(
        &s,
        vec![
            session(
                "s1",
                SessionState::Exited,
                Some((PushState::Pushed, true)),
                10,
            ),
            session(
                "s2",
                SessionState::Exited,
                Some((PushState::Pushed, true)),
                20,
            ),
        ],
    )
    .await;
    s.f.ctx.radar_kick.link_started("nowhere");
    wait_rows(&s, 0).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(fake.requests("project.radar").len(), 1);
}

#[tokio::test]
async fn the_route_is_read_scope_and_a_session_without_push_stays_out_of_will_conflict() {
    let (s, fake) = world().await;
    let (name, value) = bearer(&s.c, "r", &[Scope::Read]).await;
    let r =
        s.c.req(
            "GET",
            "/api/radar",
            &[(name.as_str(), value.as_str())],
            None,
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body, json!({ "groups": [], "notices": [] }));
    let none = s.c.req("GET", "/api/radar", &[], None).await;
    assert_eq!(none.status, 401, "{}", none.body);
    // A branch of a session with push off is never sent.
    show(&s, vec![session("s6", SessionState::Running, None, 60)]).await;
    let _stop = spawn_radar(&s.f, OPTIONS);
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert!(fake.requests("project.radar").is_empty());
}
