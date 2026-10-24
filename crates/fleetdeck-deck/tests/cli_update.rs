//! Provider CLI updates on the deck (B28, design `cli-update.md` §3.11): the
//! audit import after `station.info` with a fake station on a real link, and
//! the two board routes with their owner check, elevation, feature check and
//! audit rows.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
mod settings_support;

use std::collections::{BTreeSet, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::board::{Extras, board_with_extras};
use common::*;
use fleetdeck_deck::link;
use fleetdeck_deck::store::{AuditEntry, AuditResult, LinkFeature};
use fleetdeck_link::LiveLink;
use fleetdeck_link::control::Inbound;
use fleetdeck_link::node;
use fleetdeck_proto::cli_update::{
    CliUpdateAttempt, CliUpdateAttemptsPage, CliUpdateAttemptsQuery, CliUpdateReport,
    CliUpdateRunAnswer, InstallMethod, ProviderUpdateState, UpdateOutcome, UpdateTrigger,
};
use fleetdeck_proto::event::ProviderKind;
use fleetdeck_proto::link::{
    ControlFrame, Outcome, Scope, StationInfo, StreamMode, StreamStart, WireError,
};
use fleetdeck_proto::version::feature;
use fleetdeck_proto::{IncarnationId, StationId};
use serde_json::{Value, json};
use settings_support::*;

const PATH: &str = "/api/stations/fake/cli-update";

/// What the fake station answers, and what it was asked.
#[derive(Default)]
struct Script {
    info: Option<StationInfo>,
    report: Option<CliUpdateReport>,
    rows: Vec<CliUpdateAttempt>,
    /// The incarnation of the n-th `cli_update.attempts` answer (the last one
    /// repeats).
    page_incs: Vec<String>,
    run: VecDeque<Outcome>,
    calls: Vec<(String, Value)>,
}

type Shared = Arc<Mutex<Script>>;

fn info(incarnation: &str, report: Option<CliUpdateReport>) -> StationInfo {
    StationInfo {
        name: "fake".to_owned(),
        incarnation: incarnation.to_owned().into(),
        head: 0,
        live_sessions: 0,
        version: None,
        max_sessions: Some(4),
        allow_shell: None,
        allow_setup_cmd: None,
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
        cli_update: report,
    }
}

fn report(last_seq: u64) -> CliUpdateReport {
    CliUpdateReport {
        enabled: false,
        window: "03:00-05:00 local".to_owned(),
        providers: Vec::new(),
        running: None,
        last_seq,
    }
}

fn attempt(seq: u64, id: &str) -> CliUpdateAttempt {
    CliUpdateAttempt {
        seq,
        attempt_id: id.to_owned(),
        provider: ProviderKind::Codex,
        trigger: UpdateTrigger::Schedule,
        method: InstallMethod::Bun,
        channel: "stable".to_owned(),
        from_version: Some("0.50.0".to_owned()),
        target_version: Some("0.51.0".to_owned()),
        to_version: Some("0.51.0".to_owned()),
        outcome: UpdateOutcome::Updated,
        reason: None,
        started_at: 1,
        ended_at: Some(2),
    }
}

fn run_answer(refused: Option<&str>) -> Outcome {
    Outcome::Ok(json!(CliUpdateRunAnswer {
        state: ProviderUpdateState {
            provider: ProviderKind::Claude,
            method: InstallMethod::Native,
            updatable: true,
            blocked: None,
            channel: "stable".to_owned(),
            installed: Some("2.1.300".to_owned()),
            available: None,
            checked_at: None,
            last: None,
            quarantined: None,
        },
        attempt_id: None,
        refused: refused.map(str::to_owned),
        busy: Vec::new(),
    }))
}

fn respond(script: &Shared, method: &str, params: &Value) -> Outcome {
    let mut s = script.lock().unwrap();
    s.calls.push((method.to_owned(), params.clone()));
    match method {
        "station.info" => Outcome::Ok(json!(s.info)),
        "cli_update.status" => Outcome::Ok(json!(s.report)),
        "cli_update.run" => s.run.pop_front().unwrap_or_else(|| run_answer(None)),
        "cli_update.attempts" => {
            let q: CliUpdateAttemptsQuery = serde_json::from_value(params.clone()).unwrap();
            let n = s
                .calls
                .iter()
                .filter(|(m, _)| m == "cli_update.attempts")
                .count();
            let inc = s.page_incs[(n - 1).min(s.page_incs.len() - 1)].clone();
            let rows = s
                .rows
                .iter()
                .filter(|r| r.seq > q.after_seq)
                .take(q.limit as usize)
                .cloned()
                .collect();
            Outcome::Ok(json!(CliUpdateAttemptsPage {
                incarnation: inc.into(),
                rows
            }))
        }
        _ => Outcome::Err(WireError::new("unknown_method", "not in this fake")),
    }
}

/// The fake station: it answers every request from `script`.
async fn drive(mut station: LiveLink, script: Shared, inc: IncarnationId) {
    while let Some(msg) = station.inbound.recv().await {
        let Inbound::Request(req) = msg else { continue };
        if req.method == "events.replay" {
            let start = StreamStart {
                mode: StreamMode::Snapshot,
                incarnation: inc.clone(),
                from: 0,
            };
            let _ = station
                .control
                .respond(req.id, Outcome::Ok(json!(start)))
                .await;
            for frame in [
                ControlFrame::SnapshotBegin {
                    incarnation: inc.clone(),
                    watermark: 0,
                    session_count: 0,
                },
                ControlFrame::SnapshotEnd {
                    incarnation: inc.clone(),
                    watermark: 0,
                    coordinator: None,
                },
            ] {
                let _ = station.control.send(frame).await;
            }
            continue;
        }
        let outcome = respond(&script, &req.method, &req.params);
        let _ = station.control.respond(req.id, outcome).await;
    }
}

/// Links the fake station and waits until the deck negotiated the feature.
async fn connect(f: &mut Fixture, script: &Shared) {
    let (d, a) = tokio::time::timeout(T, async {
        tokio::join!(
            node::dial(&f.station_node, f.listener.local_addr, f.deck_key),
            f.listener.next_link()
        )
    })
    .await
    .expect("link setup");
    let at_deck = fleetdeck_link::start(&f.deck_node, a.expect("listener"));
    tokio::spawn(link::serve(Arc::clone(&f.ctx), at_deck));
    let station = fleetdeck_link::start(&f.station_node, d.unwrap());
    tokio::spawn(drive(station, Arc::clone(script), f.inc.clone()));
    f.wait("the link and its features", |f| {
        let (store, id) = (f.store.clone(), f.station.clone());
        async move {
            matches!(
                store.link_feature(&id, feature::CLI_UPDATE),
                LinkFeature::Negotiated
            )
        }
    })
    .await;
}

fn script() -> Shared {
    let s = Script {
        info: Some(info("S1", None)),
        report: Some(report(0)),
        page_incs: vec!["S1".to_owned()],
        ..Script::default()
    };
    Arc::new(Mutex::new(s))
}

fn calls_of(script: &Shared, method: &str) -> Vec<Value> {
    script
        .lock()
        .unwrap()
        .calls
        .iter()
        .filter(|(m, _)| m == method)
        .map(|(_, p)| p.clone())
        .collect()
}

async fn imported(f: &Fixture) -> Vec<AuditEntry> {
    f.store
        .audit_log(500)
        .await
        .unwrap()
        .into_iter()
        .filter(|e| e.action == "cli_update.attempt")
        .collect()
}

async fn wait_imported(f: &Fixture, n: usize) {
    let deadline = tokio::time::Instant::now() + T;
    while imported(f).await.len() < n {
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out: {n} audit rows"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn settle() {
    tokio::time::sleep(Duration::from_millis(400)).await;
}

fn station_id(f: &Fixture) -> StationId {
    f.station.clone()
}

// ------------------------------------------------------------- the import

#[tokio::test]
async fn an_info_answer_with_last_seq_two_leads_to_a_call_and_two_audit_rows() {
    let mut f = fixture().await;
    let s = script();
    {
        let mut g = s.lock().unwrap();
        g.info = Some(info("S1", Some(report(2))));
        g.rows = vec![attempt(1, "a1"), attempt(2, "a2")];
    }
    connect(&mut f, &s).await;
    wait_imported(&f, 2).await;
    settle().await;
    let calls = calls_of(&s, "cli_update.attempts");
    assert_eq!(calls, vec![json!({ "after_seq": 0, "limit": 100 })]);
    let rows = imported(&f).await;
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].actor, "station:fake");
    assert_eq!(rows[0].target.as_deref(), Some("fake/codex"));
    assert_eq!(rows[0].result, AuditResult::Ok);
    assert_eq!(
        rows[0].detail.as_deref(),
        Some("schedule bun 0.50.0 -> 0.51.0: updated")
    );
    let cursor = f.store.cli_update_cursor(&station_id(&f)).await.unwrap();
    assert_eq!(cursor, Some((IncarnationId::from("S1"), 2)));
}

#[tokio::test]
async fn a_restored_station_starts_again_at_zero_and_does_not_repeat_an_attempt() {
    let mut f = fixture().await;
    // The deck imported ten attempts of incarnation A before the restore.
    let before: Vec<_> = (1..=10).map(|n| attempt(n, &format!("a{n}"))).collect();
    f.store
        .import_cli_update_attempts(station_id(&f), "fake".to_owned(), "A".into(), before)
        .await
        .unwrap();
    assert_eq!(imported(&f).await.len(), 10);
    let s = script();
    {
        let mut g = s.lock().unwrap();
        g.info = Some(info("B", Some(report(3))));
        g.page_incs = vec!["B".to_owned()];
        g.rows = vec![attempt(1, "a1"), attempt(2, "b2"), attempt(3, "b3")];
    }
    connect(&mut f, &s).await;
    wait_imported(&f, 12).await;
    settle().await;
    assert_eq!(imported(&f).await.len(), 12, "a1 was imported before");
    assert_eq!(
        calls_of(&s, "cli_update.attempts"),
        vec![json!({ "after_seq": 0, "limit": 100 })]
    );
    let cursor = f.store.cli_update_cursor(&station_id(&f)).await.unwrap();
    assert_eq!(cursor, Some((IncarnationId::from("B"), 3)));
}

#[tokio::test]
async fn a_page_of_another_incarnation_starts_the_task_again_once_at_zero() {
    let mut f = fixture().await;
    f.store
        .import_cli_update_attempts(
            station_id(&f),
            "fake".to_owned(),
            "C".into(),
            vec![attempt(1, "a1")],
        )
        .await
        .unwrap();
    let s = script();
    {
        let mut g = s.lock().unwrap();
        // `station.info` says B; the station answers with C (it changed
        // between the two calls).
        g.info = Some(info("B", Some(report(2))));
        g.page_incs = vec!["C".to_owned()];
        g.rows = vec![attempt(1, "a1"), attempt(2, "a2")];
    }
    connect(&mut f, &s).await;
    wait_imported(&f, 2).await;
    settle().await;
    assert_eq!(
        calls_of(&s, "cli_update.attempts"),
        vec![
            json!({ "after_seq": 0, "limit": 100 }),
            json!({ "after_seq": 0, "limit": 100 })
        ]
    );
    assert_eq!(imported(&f).await.len(), 2, "a1 is not repeated");
    let cursor = f.store.cli_update_cursor(&station_id(&f)).await.unwrap();
    assert_eq!(cursor, Some((IncarnationId::from("C"), 2)));
}

#[tokio::test]
async fn a_second_incarnation_change_ends_the_task() {
    let mut f = fixture().await;
    let s = script();
    {
        let mut g = s.lock().unwrap();
        g.info = Some(info("B", Some(report(2))));
        g.page_incs = vec!["C".to_owned(), "D".to_owned()];
        g.rows = vec![attempt(1, "a1")];
    }
    connect(&mut f, &s).await;
    settle().await;
    assert_eq!(calls_of(&s, "cli_update.attempts").len(), 2);
    assert!(imported(&f).await.is_empty());
}

#[tokio::test]
async fn an_info_answer_without_news_asks_for_nothing() {
    let mut f = fixture().await;
    // No `cli_update` in the answer.
    let s = script();
    connect(&mut f, &s).await;
    settle().await;
    assert!(calls_of(&s, "cli_update.attempts").is_empty());
    assert!(imported(&f).await.is_empty());
}

#[tokio::test]
async fn a_cursor_that_is_up_to_date_asks_for_nothing() {
    let mut f = fixture().await;
    f.store
        .import_cli_update_attempts(
            station_id(&f),
            "fake".to_owned(),
            "S1".into(),
            vec![attempt(1, "a1"), attempt(2, "a2")],
        )
        .await
        .unwrap();
    let s = script();
    s.lock().unwrap().info = Some(info("S1", Some(report(2))));
    connect(&mut f, &s).await;
    settle().await;
    assert!(calls_of(&s, "cli_update.attempts").is_empty());
}

// --------------------------------------------------------------- the routes

struct Board {
    f: Fixture,
    c: common::board::BoardClient,
    h: Headers,
    s: Shared,
}

async fn board() -> Board {
    let mut f = fixture().await;
    let s = script();
    connect(&mut f, &s).await;
    let c = board_with_extras(Arc::clone(&f.ctx), Extras::default()).await;
    set_password(&c).await;
    let h = owner(&c).await;
    Board { f, c, h, s }
}

#[tokio::test]
async fn the_status_route_forwards_cli_update_status() {
    let b = board().await;
    b.s.lock().unwrap().report = Some(report(7));
    let r = get(&b.c, &b.h, PATH).await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body["last_seq"], 7);
    assert_eq!(r.body["window"], "03:00-05:00 local");
    assert_eq!(calls_of(&b.s, "cli_update.status").len(), 1);
}

#[tokio::test]
async fn a_check_needs_no_elevation_and_is_audited() {
    let b = board().await;
    let r = post(
        &b.c,
        &b.h,
        PATH,
        json!({ "provider": "claude", "check_only": true }),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body["state"]["installed"], "2.1.300");
    assert_eq!(
        calls_of(&b.s, "cli_update.run"),
        vec![json!({ "provider": "claude", "check_only": true, "force": false })]
    );
    let rows = b.c.audited("cli_update.check", AuditResult::Ok).await;
    assert_eq!(rows, vec![None]);
    let all = b.f.store.audit_log(50).await.unwrap();
    let row = all.iter().find(|e| e.action == "cli_update.check").unwrap();
    assert_eq!(row.actor, "owner");
    assert_eq!(row.target.as_deref(), Some("fake/claude"));
}

#[tokio::test]
async fn an_update_needs_an_elevation() {
    let b = board().await;
    let r = post(&b.c, &b.h, PATH, json!({ "provider": "claude" })).await;
    expect(&r, 400, "elevation_required");
    assert_eq!(r.body["keys"], json!(["update"]));
    // A wrong token is no elevation either.
    let r = post(
        &b.c,
        &b.h,
        PATH,
        json!({ "provider": "claude", "elevation": "not-a-token" }),
    )
    .await;
    expect(&r, 400, "elevation_required");
    settle().await;
    assert!(calls_of(&b.s, "cli_update.run").is_empty());
    let rows = b.c.audited("cli_update.update", AuditResult::Refused).await;
    assert_eq!(
        rows,
        vec![
            Some("elevation_required".to_owned()),
            Some("elevation_required".to_owned())
        ]
    );
    // With an elevation the call goes through.
    let e = elevate(&b.c, &b.h).await;
    let r = post(
        &b.c,
        &b.h,
        PATH,
        json!({ "provider": "codex", "elevation": e }),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(
        calls_of(&b.s, "cli_update.run"),
        vec![json!({ "provider": "codex", "check_only": false, "force": false })]
    );
    assert_eq!(
        b.c.audited("cli_update.update", AuditResult::Ok).await,
        vec![None]
    );
}

#[tokio::test]
async fn a_refused_answer_is_audited_with_its_reason_and_force() {
    let b = board().await;
    b.s.lock()
        .unwrap()
        .run
        .push_back(run_answer(Some("sessions_running")));
    let e = elevate(&b.c, &b.h).await;
    let r = post(
        &b.c,
        &b.h,
        PATH,
        json!({ "provider": "claude", "force": true, "elevation": e }),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body["refused"], "sessions_running");
    assert_eq!(
        calls_of(&b.s, "cli_update.run"),
        vec![json!({ "provider": "claude", "check_only": false, "force": true })]
    );
    assert_eq!(
        b.c.audited("cli_update.update", AuditResult::Refused).await,
        vec![Some("sessions_running force".to_owned())]
    );
}

#[tokio::test]
async fn only_the_owner_login_calls_the_routes() {
    let b = board().await;
    let (_, t) =
        b.c.store
            .create_token(
                "ops".into(),
                BTreeSet::from([Scope::Read, Scope::Drive, Scope::Admin]),
                86_400_000,
            )
            .await
            .unwrap();
    let auth = format!("Bearer {}", t.expose());
    let h = [("Authorization", auth.as_str())];
    let r = b.c.req("GET", PATH, &h, None).await;
    assert_eq!((r.status, r.code()), (403, "owner_only"));
    let r =
        b.c.req(
            "POST",
            PATH,
            &h,
            Some(json!({ "provider": "claude", "check_only": true })),
        )
        .await;
    assert_eq!((r.status, r.code()), (403, "owner_only"));
    settle().await;
    assert!(calls_of(&b.s, "cli_update.status").is_empty());
    assert!(calls_of(&b.s, "cli_update.run").is_empty());
}

#[tokio::test]
async fn a_body_that_is_not_valid_is_refused_with_a_fixed_text() {
    let b = board().await;
    for body in [
        json!({}),
        json!({ "provider": "claude", "extra": true }),
        json!({ "provider": ["canary-text"] }),
    ] {
        let r = post(&b.c, &b.h, PATH, body).await;
        expect(&r, 400, "invalid_body");
        assert!(!r.text.contains("canary-text"), "{}", r.text);
    }
    settle().await;
    assert!(calls_of(&b.s, "cli_update.run").is_empty());
}

#[tokio::test]
async fn a_station_without_the_feature_answers_501_and_gets_no_request() {
    let b = board().await;
    b.f.store.set_link_features(&b.f.station, 98, vec![]);
    let r = get(&b.c, &b.h, PATH).await;
    expect(&r, 501, "cli_update_unsupported");
    let r = post(
        &b.c,
        &b.h,
        PATH,
        json!({ "provider": "claude", "check_only": true }),
    )
    .await;
    expect(&r, 501, "cli_update_unsupported");
    settle().await;
    assert!(calls_of(&b.s, "cli_update.status").is_empty());
    assert!(calls_of(&b.s, "cli_update.run").is_empty());
}

#[tokio::test]
async fn a_station_that_answers_unsupported_is_a_501_with_a_fixed_text() {
    let b = board().await;
    b.s.lock()
        .unwrap()
        .run
        .push_back(Outcome::Err(WireError::new(
            "unsupported",
            "station text that must not leak",
        )));
    let r = post(
        &b.c,
        &b.h,
        PATH,
        json!({ "provider": "claude", "check_only": true }),
    )
    .await;
    expect(&r, 501, "cli_update_unsupported");
    assert!(!r.text.contains("must not leak"), "{}", r.text);
    b.s.lock()
        .unwrap()
        .run
        .push_back(Outcome::Err(WireError::new(
            "internal",
            "station text that must not leak",
        )));
    let r = post(
        &b.c,
        &b.h,
        PATH,
        json!({ "provider": "claude", "check_only": true }),
    )
    .await;
    expect(&r, 502, "station_error");
    assert!(!r.text.contains("must not leak"), "{}", r.text);
    assert_eq!(
        b.c.audited("cli_update.check", AuditResult::Failed).await,
        vec![
            Some("station_error".to_owned()),
            Some("cli_update_unsupported".to_owned())
        ]
    );
}

#[tokio::test]
async fn an_unknown_station_and_a_station_without_a_link_are_refused() {
    let f = fixture().await;
    let c = board_with_extras(Arc::clone(&f.ctx), Extras::default()).await;
    set_password(&c).await;
    let h = owner(&c).await;
    let r = get(&c, &h, "/api/stations/nope/cli-update").await;
    expect(&r, 404, "unknown_station");
    let r = post(
        &c,
        &h,
        "/api/stations/nope/cli-update",
        json!({ "provider": "claude", "check_only": true }),
    )
    .await;
    expect(&r, 404, "unknown_station");
    // `fake` is paired but has no live link.
    let r = get(&c, &h, PATH).await;
    expect(&r, 409, "station_unreachable");
    let r = post(
        &c,
        &h,
        PATH,
        json!({ "provider": "claude", "check_only": true }),
    )
    .await;
    expect(&r, 409, "station_unreachable");
}
