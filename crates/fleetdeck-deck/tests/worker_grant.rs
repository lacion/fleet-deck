//! MCP P4a deck tests 1 to 5 and 7 over the board API and a fake station
//! link: the default worker grant of an owner start, an issue batch and a
//! schedule, the starts that get none, the server-owned grant kind and the
//! hold of a queued start. (The store tests are in
//! `src/store/worker_grant_tests.rs`.)

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::board::{BoardClient, Http, board};
use common::fleet_git_fake::{bearer, synced};
use common::*;
use fleetdeck_core::config_worker_grant::WorkerGrantConfig;
use fleetdeck_deck::store::worker_grant::{Provenance, WorkerStart};
use fleetdeck_deck::store::{Grant, GrantState};
use fleetdeck_proto::CommandId;
use fleetdeck_proto::command::{
    CommandResult, DeckCommandState, GrantKind, RETRY_WINDOW_MS, Receipt, ReceiptState,
    SessionCommand, SessionSpec,
};
use fleetdeck_proto::link::{Outcome, Scope, StationInfo};
use serde_json::{Value, json};

const START: &str = "/api/commands/session.start";

fn enable(c: &BoardClient) {
    let mut live = (*c.state.live.get()).clone();
    live.worker_grant = WorkerGrantConfig::default();
    c.state.live.publish(live);
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

fn start_body(id: &str, extra: Value) -> Value {
    let mut spec = json!({"provider_instance": "claude", "cwd": "/w", "mode": null,
        "model": null, "title": null});
    if let Some(extra) = extra.as_object() {
        spec.as_object_mut().unwrap().extend(extra.clone());
    }
    json!({"station": "fake", "command_id": id, "command": {"op": "start",
        "prompt": null, "spec": spec}})
}

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

fn started(id: &CommandId) -> Outcome {
    Outcome::Ok(json!(Receipt {
        command_id: id.clone(),
        state: ReceiptState::Accepted,
        station_seq: Some(1),
        result: Some(CommandResult {
            session_id: Some("sess-1".into()),
            input_id: None,
            seq: None,
        }),
        reject: None,
        at: 0,
    }))
}

fn grants_of(all: Vec<Grant>, kind: GrantKind) -> Vec<Grant> {
    all.into_iter().filter(|g| g.kind == kind).collect()
}

/// Posts a start and answers it at the station; returns the HTTP answer and
/// the `coordinator` object of the command that the station received.
async fn post_start(
    c: &BoardClient,
    station: &mut fleetdeck_link::LiveLink,
    headers: &[(&str, &str)],
    body: Value,
) -> (Http, Value) {
    let call = c.post(START, headers, body);
    let (r, req) = tokio::join!(call, Fixture::answer(station, started));
    (r, req.params["command"]["spec"]["coordinator"].clone())
}

#[tokio::test]
async fn an_owner_board_start_gets_a_worker_grant_with_the_setting_values() {
    let mut f = fixture().await;
    let mut station = f.connect().await;
    let c = board(Arc::clone(&f.ctx)).await;
    let h = owner(&c).await;
    // The tests turn the setting off by default; this one turns it on.
    let (r, sent) = post_start(&c, &mut station, &refs(&h), start_body("o0", json!({}))).await;
    assert_eq!(r.status, 200, "{}", r.text);
    assert!(sent.is_null(), "the setting is off: {sent}");
    enable(&c);
    let (r, sent) = post_start(&c, &mut station, &refs(&h), start_body("o1", json!({}))).await;
    assert_eq!(r.status, 200, "{}", r.text);
    assert_eq!(sent["kind"], "worker");
    assert_eq!(sent["classes"], json!(["read", "mail", "drive", "spawn"]));
    for hidden in ["stations", "spawn_cap", "max_modes", "scope", "answer_cap"] {
        assert!(sent.get(hidden).is_none(), "{hidden} reached the station");
    }
    let all = grants_of(c.store.grants(10).await.unwrap(), GrantKind::Worker);
    assert_eq!(all.len(), 1);
    let g = &all[0];
    assert_eq!(sent["grant_id"], g.grant_id.as_str());
    assert_eq!((g.spawn_cap, g.idle_ttl_ms), (2, 30 * 60_000));
    assert_eq!(g.expires_at - g.created_at, 12 * 3_600_000);
    assert_eq!(g.max_modes["claude"], "acceptEdits");
    assert_eq!(g.max_modes["codex"], "on-request/workspace-write");
    assert_eq!(g.state, GrantState::Active);
    // The list shows the kind and the ceilings.
    let list = c.get("/api/coordinator/grants", &refs(&h)).await;
    assert_eq!(list.status, 200, "{}", list.text);
    let view = &list.body[0];
    assert_eq!(view["kind"], "worker");
    assert_eq!(view["max_modes"]["claude"], "acceptEdits");
    assert_eq!(view["spawn_cap"], 2);
    // A resend of the same command ID returns the stored command and no grant.
    let r = c.post(START, &refs(&h), start_body("o1", json!({}))).await;
    assert_eq!(r.status, 200, "{}", r.text);
    assert_eq!(
        grants_of(c.store.grants(10).await.unwrap(), GrantKind::Worker).len(),
        1
    );
    let again = c
        .store
        .audit_log(50)
        .await
        .unwrap()
        .into_iter()
        .filter(|a| a.action == "coordinator.worker_grant_created")
        .count();
    assert_eq!(again, 1);
}

#[tokio::test]
async fn issue_batches_by_the_owner_login_get_one_and_by_a_drive_token_get_none() {
    let (s, fake) = synced().await;
    enable(&s.c);
    let batch = |id: &str| {
        json!({"batch_id": id, "issues": ["#7"], "provider_instance": "claude",
            "count_per_issue": 1})
    };
    let path = format!("/api/projects/{}/sessions", s.app.project_id);
    let r = s.c.post(&path, &s.h(), batch("B-OWNER")).await;
    assert_eq!(r.status, 200, "{}", r.text);
    let spec = |n: usize| match &fake.starts()[n].command {
        SessionCommand::Start { spec, .. } => spec.coordinator.clone(),
        other => panic!("{other:?}"),
    };
    let granted = spec(0).expect("the owner's batch start carries a grant");
    assert_eq!(granted.kind, GrantKind::Worker);
    // A drive token starts a batch: the plain path, no grant.
    let token = bearer(&s.c, "ci", &[Scope::Read, Scope::Drive]).await;
    let headers = [(token.0.as_str(), token.1.as_str())];
    let r = s.c.post(&path, &headers, batch("B-TOKEN")).await;
    assert_eq!(r.status, 200, "{}", r.text);
    assert!(spec(1).is_none());
    // A forged origin or provenance in the body changes nothing: the body is
    // refused or ignored, and no grant is made.
    let mut forged = batch("B-FORGED");
    forged["origin"] = json!("board");
    forged["provenance"] = json!("issue_batch");
    let r = s.c.post(&path, &headers, forged).await;
    assert!(r.status == 400 || r.status == 200, "{}", r.text);
    if r.status == 200 {
        assert!(fake.starts().iter().skip(2).all(|e| matches!(
            &e.command,
            SessionCommand::Start { spec, .. } if spec.coordinator.is_none()
        )));
    }
    assert_eq!(
        grants_of(s.c.store.grants(10).await.unwrap(), GrantKind::Worker).len(),
        1
    );
    let created =
        s.c.store
            .audit_log(50)
            .await
            .unwrap()
            .into_iter()
            .find(|a| a.action == "coordinator.worker_grant_created")
            .expect("the audit row");
    assert_eq!(created.actor, "issue_batch");
}

fn schedule_body() -> Value {
    json!({
        "name": "Nightly", "enabled": true, "cron": "@daily", "timezone": "UTC",
        "station": "fake", "path": "/work/repo", "provider_instance": "claude",
        "mode": "plan", "prompt": "Fix the flaky tests.", "precheck_cmd": null,
        "reuse_session": false,
    })
}

#[tokio::test]
async fn a_schedule_gets_one_only_when_the_owner_login_saved_it_last() {
    let mut f = fixture().await;
    let mut station = f.connect().await;
    f.store.set_station_info(&f.station, info());
    let c = board(Arc::clone(&f.ctx)).await;
    enable(&c);
    // The runner of the deck reads the live settings through the link.
    f.ctx.desktop_live.set(c.state.live.clone()).ok();
    let h = owner(&c).await;
    let r = c.post("/api/schedules", &refs(&h), schedule_body()).await;
    assert_eq!(r.status, 200, "{}", r.text);
    let id = r.body["schedule_id"].as_str().unwrap().to_owned();
    assert_eq!(
        c.store
            .schedule_author(id.clone())
            .await
            .unwrap()
            .as_deref(),
        Some("owner")
    );
    // "Run now": the schedule's author is the owner.
    let r = c
        .post(&format!("/api/schedules/{id}/run"), &refs(&h), json!({}))
        .await;
    assert_eq!(r.status, 200, "{}", r.text);
    let req = next_request(&mut station).await;
    assert_eq!(req.method, "session.start");
    assert_eq!(
        req.params["command"]["spec"]["coordinator"]["kind"],
        "worker"
    );
    let id_cmd = req.command_id.clone().unwrap();
    station
        .control
        .respond(req.id, started(&id_cmd))
        .await
        .unwrap();
    let audit = c.store.audit_log(50).await.unwrap();
    let row = audit
        .iter()
        .find(|a| a.action == "coordinator.worker_grant_created")
        .expect("the audit row");
    assert_eq!(row.actor, "schedule");
    // A token saved it last: no grant. An old row (`unknown`) gets none either.
    for (author, n) in [("token", 1), ("unknown", 2)] {
        f.det.advance(Duration::from_secs(2));
        set_author(&c, &id, author).await;
        let r = c
            .post(&format!("/api/schedules/{id}/run"), &refs(&h), json!({}))
            .await;
        assert_eq!(r.status, 200, "{}", r.text);
        let req = next_request(&mut station).await;
        assert!(
            req.params["command"]["spec"]["coordinator"].is_null(),
            "{author}: {}",
            req.params
        );
        let cmd = req.command_id.clone().unwrap();
        station
            .control
            .respond(req.id, started(&cmd))
            .await
            .unwrap();
        assert_eq!(
            grants_of(c.store.grants(10).await.unwrap(), GrantKind::Worker).len(),
            1,
            "run {n}"
        );
    }
    // The owner saves it again: the author is `owner` and a run gets a grant.
    f.det.advance(Duration::from_secs(2));
    let r = c
        .req(
            "PUT",
            &format!("/api/schedules/{id}"),
            &refs(&h),
            Some(schedule_body()),
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.text);
    assert_eq!(
        c.store
            .schedule_author(id.clone())
            .await
            .unwrap()
            .as_deref(),
        Some("owner")
    );
    let r = c
        .post(&format!("/api/schedules/{id}/run"), &refs(&h), json!({}))
        .await;
    assert_eq!(r.status, 200, "{}", r.text);
    let req = next_request(&mut station).await;
    assert_eq!(
        req.params["command"]["spec"]["coordinator"]["kind"],
        "worker"
    );
    let cmd = req.command_id.clone().unwrap();
    station
        .control
        .respond(req.id, started(&cmd))
        .await
        .unwrap();
}

/// Writes the author of a schedule row directly (a token cannot reach the
/// owner-only routes, and an old row reads `unknown`).
async fn set_author(c: &BoardClient, id: &str, author: &str) {
    let input: fleetdeck_proto::automation::ScheduleInput =
        serde_json::from_value(schedule_body()).unwrap();
    let stored = c.store.schedule(id.to_owned()).await.unwrap().unwrap();
    let station = stored.schedule.station_id.clone();
    c.store
        .update_schedule(id.to_owned(), input, station, author)
        .await
        .unwrap();
}

#[tokio::test]
async fn members_bearer_tokens_imports_resumes_spawns_and_cli_starts_get_none() {
    let mut f = fixture().await;
    let mut station = f.connect().await;
    let c = board(Arc::clone(&f.ctx)).await;
    enable(&c);
    let h = owner(&c).await;
    // A bearer token start: the plain path.
    let token = bearer(&c, "ci", &[Scope::Read, Scope::Drive]).await;
    let headers = [(token.0.as_str(), token.1.as_str())];
    let (r, sent) = post_start(&c, &mut station, &headers, start_body("t1", json!({}))).await;
    assert_eq!(r.status, 200, "{}", r.text);
    assert!(sent.is_null());
    // An import and a resume carry no grant.
    let import = json!({"station": "fake", "command_id": "i1", "command": {"op": "import",
        "spec": {"provider_instance": "claude", "cwd": "/w", "mode": null, "model": null,
        "title": null}, "native_session_id": "0b6c1d2e-aaaa-4bbb-8ccc-123456789abc",
        "prompt": null}});
    let hs = refs(&h);
    let call = c.post("/api/commands/session.import", &hs, import);
    let (r, req) = tokio::join!(call, Fixture::answer(&mut station, started));
    assert_eq!(r.status, 200, "{}", r.text);
    assert!(req.params["command"]["spec"]["coordinator"].is_null());
    let resume = json!({"station": "fake", "command_id": "rs1",
        "command": {"op": "resume", "session_id": "s1"}});
    let call = c.post("/api/commands/session.resume", &hs, resume);
    let (r, req) = tokio::join!(call, Fixture::answer(&mut station, started));
    assert_eq!(r.status, 200, "{}", r.text);
    assert!(req.params["command"].get("spec").is_none());
    // A coordinator spawn goes through the ops, not these routes: its child
    // has no grant (the store tests). Here: nothing was created for any of it.
    assert!(grants_of(c.store.grants(10).await.unwrap(), GrantKind::Worker).is_empty());
    // A start that the owner's login sent with its own coordinator request
    // keeps the owner grant and gets no worker grant.
    let spec = json!({"coordinator": {"grant_id": "", "classes": ["read"], "idle_ttl_ms": 0,
        "expires_at": 0, "control_skill": {"version": "", "text": ""}}});
    let (r, sent) = post_start(&c, &mut station, &refs(&h), start_body("oc1", spec)).await;
    assert_eq!(r.status, 200, "{}", r.text);
    assert!(sent.get("kind").is_none(), "{sent}");
    let all = c.store.grants(10).await.unwrap();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].kind, GrantKind::Coordinator);
}

#[tokio::test]
async fn the_grant_kind_is_server_owned() {
    let mut f = fixture().await;
    let mut station = f.connect().await;
    let c = board(Arc::clone(&f.ctx)).await;
    let h = owner(&c).await;
    let forged = json!({"coordinator": {"grant_id": "", "classes": ["read"], "idle_ttl_ms": 0,
        "expires_at": 0, "kind": "worker",
        "control_skill": {"version": "", "text": ""}}});
    let r = c.post(START, &refs(&h), start_body("k1", forged)).await;
    assert_eq!((r.status, r.code()), (400, "invalid_grant"), "{}", r.text);
    assert_eq!(r.body["message"], "the grant kind is set by the deck");
    no_request(&mut station, Duration::from_millis(200)).await;
    assert!(c.store.grants(10).await.unwrap().is_empty());
    // An owner grant is stored and sent as `coordinator` (no `kind` on the wire).
    let spec = json!({"coordinator": {"grant_id": "", "classes": ["read"], "idle_ttl_ms": 0,
        "expires_at": 0, "kind": "coordinator",
        "control_skill": {"version": "", "text": ""}}});
    let (r, sent) = post_start(&c, &mut station, &refs(&h), start_body("k2", spec)).await;
    assert_eq!(r.status, 200, "{}", r.text);
    assert!(sent.get("kind").is_none(), "{sent}");
    let all = c.store.grants(10).await.unwrap();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].kind, GrantKind::Coordinator);
}

#[tokio::test]
async fn a_queued_worker_start_waits_for_a_compatible_link_and_expires_with_its_grant_revoked() {
    let mut f = fixture().await;
    let mut station = f.connect().await;
    let c = board(Arc::clone(&f.ctx)).await;
    let start = || WorkerStart {
        station: f.station.clone(),
        command: SessionCommand::Start {
            spec: SessionSpec {
                effort: None,
                provider_instance: "claude".into(),
                cwd: "/w".to_owned(),
                mode: None,
                model: None,
                title: None,
                coordinator: None,
                launch: None,
                extra_env: None,
            },
            prompt: None,
            workspace: None,
            setup_cmd: None,
            batch: None,
            issue: None,
            origin_text: false,
            fleet_git: None,
        },
        command_id: CommandId::from("h1"),
        provenance: Provenance::Owner,
        author: Some("owner".to_owned()),
        setting: WorkerGrantConfig::default(),
    };
    // The decision is made while every link has the feature; the store call
    // does not send, so the downgrade below comes before the first send.
    let out = c
        .store
        .submit_start_with_worker_decision(start())
        .await
        .unwrap();
    assert!(out.new);
    // A second link of the station that did not negotiate `worker_grant.v1`.
    c.store
        .set_link_features(&f.station, 99, vec!["coordinator.v1".to_owned()]);
    f.ctx.send(&CommandId::from("h1")).await;
    no_request(&mut station, Duration::from_millis(400)).await;
    let g = grants_of(c.store.grants(10).await.unwrap(), GrantKind::Worker).remove(0);
    assert_eq!(g.state, GrantState::Active);
    let stored = c
        .store
        .command(CommandId::from("h1"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (stored.attempts, stored.state),
        (0, DeckCommandState::Pending)
    );
    // The TTL passes: the command fails as undelivered and the grant is revoked.
    f.det.advance(Duration::from_millis(
        u64::try_from(RETRY_WINDOW_MS).unwrap() + 1,
    ));
    c.store.expire_commands().await.unwrap();
    let g = c.store.grant(g.grant_id).await.unwrap().unwrap();
    assert_eq!(
        (g.state, g.revoked_reason.as_deref()),
        (GrantState::Revoked, Some("undelivered"))
    );
    // With a compatible link again, a new start is sent with its worker spec.
    c.store.clear_link_features(&f.station, 99);
    let mut again = start();
    again.command_id = CommandId::from("h2");
    let out = f.ctx.submit_start_by(again).await.unwrap();
    assert!(out.1);
    let req = next_request(&mut station).await;
    assert_eq!(
        req.params["command"]["spec"]["coordinator"]["kind"],
        "worker"
    );
}
