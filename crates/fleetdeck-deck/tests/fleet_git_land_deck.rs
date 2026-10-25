//! The land flow on the deck (T3.3a, design §11.2, §11.8, §11.12; FG-8): the
//! routes, the outbox with a lost answer, a rejected receipt, "Stop checking"
//! and its fence, "Send to session", and a coordinator land with its
//! authority. A fake station answers `session.land`, `fleet_git.land_status`
//! and `fleet_git.land_abandon`; the land tasks run as in the deck.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;

use common::fleet::bound;
use common::fleet_git_fake::{FakeGit, Setup, bearer, expect, synced};
use common::fleet_git_land::{LandReply, spawn_land};
use fleetdeck_deck::store::GrantClass;
use fleetdeck_deck::store::land_queue::{LandRequest, LandRequested};
use fleetdeck_proto::event::{Event, EventBody, SessionState};
use fleetdeck_proto::fleet_git::{LandInfo, LandPhase, LandState};
use fleetdeck_proto::link::Scope;
use fleetdeck_proto::mutation::{AuthorityCheck, CommandAuthority};
use fleetdeck_proto::{EventEnvelope, SessionSummary};
use serde_json::{Value, json};

fn session(id: &str, state: SessionState) -> SessionSummary {
    let mut summary = SessionSummary::new(id.into());
    summary.state = state;
    summary.cwd = Some("/work/app".to_owned());
    summary.worktree = Some(format!("/work/app--fd-{id}"));
    summary.branch = Some(format!("fd/fake/{id}"));
    summary.callsign = Some(id.to_owned());
    summary
}

/// A synced project with two ended sessions `s1` and `s2`, the reserved Git
/// name `fake`, and the land tasks running.
async fn world() -> (Setup, Arc<FakeGit>, tokio::sync::watch::Sender<bool>) {
    let (s, fake) = synced().await;
    let station = s.f.station.0.clone();
    s.f.store
        .db()
        .call(move |conn| {
            conn.execute(
                "INSERT INTO slug_reservations (repo, slug, station_id, first_used_at)
                 VALUES ('github.com/acme/app', 'fake', ?1, 1)",
                [station],
            )
            .map_err(Into::into)
        })
        .await
        .unwrap();
    show(
        &s,
        vec![
            session("s1", SessionState::Exited),
            session("s2", SessionState::Exited),
        ],
    )
    .await;
    let stop = spawn_land(&s.f);
    (s, fake, stop)
}

async fn show(s: &Setup, sessions: Vec<SessionSummary>) {
    show_at(s, 0, sessions).await;
}

async fn show_at(s: &Setup, watermark: u64, sessions: Vec<SessionSummary>) {
    s.f.store
        .apply_snapshot(s.f.station.clone(), s.f.inc.clone(), watermark, sessions)
        .await
        .unwrap();
}

async fn event(s: &Setup, seq: u64, session: &str, info: LandInfo) {
    let env = EventEnvelope {
        station_seq: seq,
        session_id: session.into(),
        session_seq: seq,
        at: 1_000,
        proto_version: 1,
        raw_ref: None,
        event: EventBody::Known(Event::SessionLand(Box::new(info))),
    };
    s.f.store
        .apply_events(s.f.station.clone(), s.f.inc.clone(), vec![env])
        .await
        .unwrap();
}

fn info(land: &str, state: LandState, phase: LandPhase) -> LandInfo {
    LandInfo {
        land_id: land.to_owned(),
        attempt: 1,
        state,
        phase,
        reconciling: false,
        code: None,
        text: None,
        files: Vec::new(),
        source_oid: None,
        base_oid: None,
        rebased_oid: None,
        branch_published_oid: None,
        default_published_oid: None,
        pr_url: None,
    }
}

async fn land(s: &Setup, session: &str, command: &str) -> common::board::Http {
    s.c.post(
        &format!("/api/sessions/fake/{session}/land"),
        &s.h(),
        json!({ "command_id": command }),
    )
    .await
}

async fn post(s: &Setup, path: &str, body: Value) -> common::board::Http {
    s.c.post(path, &s.h(), body).await
}

/// The state of a land, waiting until `pred` holds for its row.
async fn wait_row(
    s: &Setup,
    land: &str,
    what: &str,
    pred: fn(&fleetdeck_deck::store::land_queue::LandRow) -> bool,
) {
    let land = land.to_owned();
    s.f.wait(what, move |f| {
        let (store, land) = (f.store.clone(), land.clone());
        async move { pred(&store.land_row(land).await.unwrap().unwrap()) }
    })
    .await;
}

/// Waits until the outbox has nothing to send (the station took the command).
async fn wait_outbox_idle(s: &Setup) {
    s.f.wait("an idle outbox", |f| {
        let store = f.store.clone();
        async move { store.land_outbox_next_due().await.unwrap().is_none() }
    })
    .await;
}

/// Moves the deck clock and wakes the land tasks.
fn tick(s: &Setup, secs: u64) {
    s.f.det.advance(Duration::from_secs(secs));
    s.f.ctx.land_kick.kick();
}

#[tokio::test]
async fn a_land_goes_from_the_route_through_the_outbox_and_the_events_to_landed() {
    let (s, fake, _stop) = world().await;
    let r = land(&s, "s1", "c1").await;
    assert_eq!(r.status, 202, "{}", r.body);
    assert_eq!(r.body["position"], 0);
    let land_id = r.body["land_id"].as_str().unwrap().to_owned();
    let sent = fake.wait_for("session.land", 1).await;
    let env = &sent[0];
    assert!(env["command_id"].as_str().unwrap().starts_with("landcmd-"));
    assert_eq!(env["target_incarnation"], "fake-station-db");
    assert_eq!(env["origin"], "board");
    assert_eq!(env["command"]["op"], "land");
    assert_eq!(env["command"]["land_id"], land_id.as_str());
    assert_eq!(env["command"]["plan"]["publish_ref"], "fd/fake/s1");
    assert_eq!(env["command"]["plan"]["mode"], "pr");
    wait_outbox_idle(&s).await;
    let mut step = info(&land_id, LandState::Running, LandPhase::Captured);
    step.base_oid = Some("b".repeat(40));
    event(&s, 1, "s1", step).await;
    let mut end = info(&land_id, LandState::Landed, LandPhase::Done);
    end.pr_url = Some("https://github.com/acme/app/pull/7".to_owned());
    event(&s, 2, "s1", end).await;
    let id = s.app.project_id.clone();
    let lands = s.c.get(&format!("/api/projects/{id}/lands"), &s.h()).await;
    assert_eq!(lands.status, 200, "{}", lands.body);
    assert_eq!(lands.body["queue"], json!([]));
    let recent = &lands.body["recent"][0];
    assert_eq!(recent["state"], "landed");
    assert_eq!(recent["pr_url"], "https://github.com/acme/app/pull/7");
    assert_eq!(recent["callsign"], "s1");
    assert_eq!(recent["branch"], "fd/fake/s1");
}

#[tokio::test]
async fn the_land_route_is_owner_only_and_refuses_what_cannot_land() {
    let (s, _fake, _stop) = world().await;
    let (name, value) = bearer(&s.c, "t", &[Scope::Admin]).await;
    let token =
        s.c.req(
            "POST",
            "/api/sessions/fake/s1/land",
            &[(name.as_str(), value.as_str())],
            Some(json!({ "command_id": "c0" })),
        )
        .await;
    expect(&token, 403, "owner_only");
    expect(&land(&s, "nope", "c1").await, 404, "unknown_session");
    let bad = post(
        &s,
        "/api/sessions/fake/s1/land",
        json!({ "command_id": "bad id" }),
    )
    .await;
    expect(&bad, 400, "invalid_command_id");
    let station =
        s.c.post(
            "/api/sessions/nowhere/s1/land",
            &s.h(),
            json!({ "command_id": "c2" }),
        )
        .await;
    expect(&station, 404, "unknown_station");
    let first = land(&s, "s1", "c3").await;
    assert_eq!(first.status, 202, "{}", first.body);
    // The same command id gives the first answer; another one is `land_open`.
    let again = land(&s, "s1", "c3").await;
    assert_eq!(again.body["land_id"], first.body["land_id"]);
    expect(&land(&s, "s1", "c4").await, 409, "land_open");
}

#[tokio::test]
async fn cancel_ends_a_queued_land_and_refuses_a_running_one() {
    let (s, _fake, _stop) = world().await;
    let a = land(&s, "s1", "c1").await.body["land_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let b = land(&s, "s2", "c2").await;
    assert_eq!(b.body["position"], 1, "{}", b.body);
    let b = b.body["land_id"].as_str().unwrap().to_owned();
    wait_row(&s, &a, "the first land runs", |r| r.state == "running").await;
    expect(
        &post(&s, &format!("/api/lands/{a}/cancel"), json!({})).await,
        409,
        "land_running",
    );
    let done = post(&s, &format!("/api/lands/{b}/cancel"), json!({})).await;
    assert_eq!(
        (done.status, done.body["state"].as_str()),
        (200, Some("cancelled"))
    );
    expect(
        &post(&s, "/api/lands/land-none/cancel", json!({})).await,
        404,
        "unknown_land",
    );
}

#[tokio::test]
async fn a_lost_answer_resends_the_same_command_and_the_station_receipt_ends_the_wait() {
    let (s, fake, _stop) = world().await;
    *fake.land.lock().unwrap() = VecDeque::from([LandReply::Error("timeout"), LandReply::Accept]);
    assert_eq!(land(&s, "s1", "c1").await.status, 202);
    let first = fake.wait_for("session.land", 1).await;
    // The backoff of the first send is 2 s on the deck clock.
    tick(&s, 3);
    let sent = fake.wait_for("session.land", 2).await;
    assert_eq!(sent[0]["command_id"], sent[1]["command_id"]);
    assert_eq!(sent[0]["command"], first[0]["command"]);
    wait_outbox_idle(&s).await;
    tick(&s, 120);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(fake.requests("session.land").len(), 2);
}

#[tokio::test]
async fn a_rejected_receipt_ends_the_land_with_the_station_code_and_frees_the_queue() {
    let (s, fake, _stop) = world().await;
    *fake.land.lock().unwrap() = VecDeque::from([LandReply::Reject(
        "invalid_state",
        "held: A land holds this session.",
    )]);
    let a = land(&s, "s1", "c1").await.body["land_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let b = land(&s, "s2", "c2").await.body["land_id"]
        .as_str()
        .unwrap()
        .to_owned();
    wait_row(&s, &a, "the first land fails", |r| r.state == "failed").await;
    let row = s.f.store.land_row(a).await.unwrap().unwrap();
    assert_eq!(
        (row.code.as_deref(), row.message.as_deref()),
        (Some("held"), Some("A land holds this session."))
    );
    // The queue is free: the second land was admitted (and fails the same way).
    wait_row(&s, &b, "the second land was admitted", |r| {
        r.started_at.is_some()
    })
    .await;
}

#[tokio::test]
async fn stop_checking_keeps_the_queue_until_the_station_fences() {
    let (s, fake, _stop) = world().await;
    let a = land(&s, "s1", "c1").await.body["land_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let b = land(&s, "s2", "c2").await.body["land_id"]
        .as_str()
        .unwrap()
        .to_owned();
    fake.wait_for("session.land", 1).await;
    wait_outbox_idle(&s).await;
    // The station lost its record: `unknown` with an accepted outbox.
    s.f.ctx.land_kick.link_started(s.f.station.as_str());
    wait_row(&s, &a, "the land is lost", |r| {
        r.phase.as_deref() == Some("lost")
    })
    .await;
    // Not uncertain landed rows are refused; this one is uncertain.
    let stop = post(&s, &format!("/api/lands/{a}/abandon"), json!({})).await;
    assert_eq!(stop.status, 202, "{}", stop.body);
    fake.wait_for("fleet_git.land_abandon", 1).await;
    // `busy`: the row stays `running`, the queue stays occupied.
    let row = s.f.store.land_row(a.clone()).await.unwrap().unwrap();
    assert_eq!(
        (row.state.as_str(), row.abandon_state.as_deref()),
        ("running", Some("requested"))
    );
    assert_eq!(
        s.f.store.land_row(b.clone()).await.unwrap().unwrap().state,
        "queued"
    );
    expect(
        &post(&s, &format!("/api/lands/{a}/forget"), json!({})).await,
        409,
        "station_paired",
    );
    // The fence: the row is `unresolved`, then the second land runs.
    *fake.land_abandon.lock().unwrap() =
        json!({ "state": "fenced", "write": "unknown", "run_state": "running" });
    tick(&s, 3);
    wait_row(&s, &a, "the land is unresolved", |r| {
        r.state == "unresolved"
    })
    .await;
    let row = s.f.store.land_row(a).await.unwrap().unwrap();
    assert_eq!(
        (
            row.code.as_deref(),
            row.abandon_state.as_deref(),
            row.abandon_write.as_deref()
        ),
        (Some("abandoned"), Some("fenced"), Some("unknown"))
    );
    wait_row(&s, &b, "the second land runs", |r| r.state == "running").await;
}

#[tokio::test]
async fn send_to_session_prompts_a_live_idle_session_with_the_conflict_files() {
    let (s, fake, _stop) = world().await;
    let a = land(&s, "s1", "c1").await.body["land_id"]
        .as_str()
        .unwrap()
        .to_owned();
    fake.wait_for("session.land", 1).await;
    wait_outbox_idle(&s).await;
    let path = format!("/api/lands/{a}/send");
    // A land that is not in conflict is refused.
    expect(
        &post(&s, &path, json!({ "command_id": "p0" })).await,
        409,
        "land_not_conflict",
    );
    let mut end = info(&a, LandState::Conflict, LandPhase::Rebased);
    end.files = vec!["a.rs".to_owned()];
    end.base_oid = Some("c".repeat(40));
    event(&s, 1, "s1", end).await;
    // An ended session: "Resume the session first".
    let r = post(&s, &path, json!({ "command_id": "p1" })).await;
    expect(&r, 409, "session_not_ready");
    assert_eq!(r.body["message"], "Resume the session first.");
    let live = vec![
        session("s1", SessionState::Running),
        session("s2", SessionState::Exited),
    ];
    show_at(&s, 1, live).await;
    let r = post(&s, &path, json!({ "command_id": "p2" })).await;
    assert_eq!(r.status, 200, "{}", r.body);
    let prompt = fake.wait_for("session.prompt", 1).await;
    let text = prompt[0]["command"]["input"]["text"].as_str().unwrap();
    assert!(
        text.contains(&"c".repeat(40)) && text.contains("\"a.rs\""),
        "{text}"
    );
    assert!(text.contains("<untrusted source=\"git:conflicts\">"));
}

async fn grant_land(s: &Setup) -> (CommandAuthority, String) {
    s.f.store.set_fleet_control(true);
    let b = bound(&s.f, "coord-1", &[GrantClass::Manage], true).await;
    let authority = CommandAuthority {
        grant_id: b.grant_id.clone(),
        caller_station: s.f.station.clone(),
        caller_session: "coord-1".into(),
        generation: 1,
    };
    let asked =
        s.f.store
            .land_request(LandRequest {
                requested_by: format!("coordinator:{}", b.grant_id),
                request_command_id: "k1".to_owned(),
                station_id: s.f.station.0.clone(),
                session_id: "s1".to_owned(),
                authority: Some(authority.clone()),
                required_classes: None,
            })
            .await
            .unwrap();
    let LandRequested::Queued { land_id, .. } = asked else {
        panic!("{asked:?}");
    };
    s.f.ctx.land_kick.kick();
    (authority, land_id)
}

#[tokio::test]
async fn a_coordinator_land_sends_its_authority_and_the_station_check_finds_the_command() {
    let (s, fake, _stop) = world().await;
    let (authority, land_id) = grant_land(&s).await;
    let sent = fake.wait_for("session.land", 1).await;
    assert_eq!(sent[0]["origin"], "coordinator");
    assert_eq!(
        sent[0]["authority"]["grant_id"],
        authority.grant_id.as_str()
    );
    let command_id = sent[0]["command_id"].as_str().unwrap().to_owned();
    let check = |target: &str| AuthorityCheck {
        grant_id: authority.grant_id.clone(),
        generation: 1,
        caller_station: authority.caller_station.clone(),
        caller_session: authority.caller_session.clone(),
        target_session: target.into(),
        command_id: command_id.clone().into(),
        launch: None,
    };
    let local =
        s.f.store
            .authority_local(s.f.station.clone(), check("s1"))
            .await
            .unwrap();
    assert!(matches!(local, Ok(l) if !l.target_excluded), "{local:?}");
    // Another target session is not this command.
    let other =
        s.f.store
            .authority_local(s.f.station.clone(), check("s2"))
            .await
            .unwrap();
    assert_eq!(other.unwrap_err(), "unknown_command");
    // A revoked grant answers with its reason.
    s.f.store
        .revoke_grant(authority.grant_id.clone(), "owner".to_owned())
        .await
        .unwrap();
    let gone =
        s.f.store
            .authority_local(s.f.station.clone(), check("s1"))
            .await
            .unwrap();
    assert_eq!(gone.unwrap_err(), "grant_revoked");
    drop(land_id);
}

#[tokio::test]
async fn a_resend_that_lost_its_authority_asks_the_station_and_fences_before_it_ends_the_land() {
    let (s, fake, _stop) = world().await;
    *fake.land.lock().unwrap() = VecDeque::from([LandReply::Error("timeout")]);
    *fake.land_abandon.lock().unwrap() =
        json!({ "state": "fenced", "write": "none", "run_state": "unresolved" });
    let (authority, land_id) = grant_land(&s).await;
    fake.wait_for("session.land", 1).await;
    // The kill switch goes off between the first send and the resend.
    s.f.store.set_fleet_control(false);
    tick(&s, 3);
    wait_row(&s, &land_id, "the land fails", |r| r.state == "failed").await;
    let row = s.f.store.land_row(land_id).await.unwrap().unwrap();
    assert_eq!(row.code.as_deref(), Some("authority_lost"));
    assert!(
        row.message.unwrap().contains("fleet_control_off"),
        "the reason is in the text"
    );
    // The deck asked the station and fenced the land; it sent nothing more.
    assert!(!fake.requests("fleet_git.land_status").is_empty());
    assert_eq!(fake.requests("fleet_git.land_abandon").len(), 1);
    assert_eq!(fake.requests("session.land").len(), 1);
    drop(authority);
}
