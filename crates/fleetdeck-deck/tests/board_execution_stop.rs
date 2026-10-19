//! Stopping the processes of a lost run (BG-4, `board-gaps.md` §5) over
//! loopback HTTPS: `GET .../execution/stop-plan?shim_id=` reads the plan
//! (owner only, feature `execution_stop.v1`), and `session.stop_execution`
//! goes through the generic command route (owner, feature and elevation
//! gates). The audit rows of `session.execution_stop` hold the stage, the
//! shim and the counts. The station is a fake that answers by hand.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
mod settings_support;

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use common::board::{BoardClient, Extras, board_with_extras};
use common::*;
use fleetdeck_deck::http::execution_routes::StopWatch;
use fleetdeck_deck::store::{AuditEntry, AuditResult};
use fleetdeck_link::LiveLink;
use fleetdeck_link::control::InboundRequest;
use fleetdeck_proto::SessionSummary;
use fleetdeck_proto::link::{Outcome, Scope, WireError};
use serde_json::{Value, json};
use settings_support::*;

const DAY: i64 = 24 * 60 * 60 * 1000;
const PLAN: &str = "/api/sessions/fake/s1/execution/stop-plan?shim_id=shim-1";
const STOP: &str = "/api/commands/session.stop_execution";
const UNSUPPORTED: &str =
    "This station's build cannot stop the processes of a lost run from the board.";
const ELEVATION: &str = "This action needs the owner's elevation.";

struct T {
    f: Fixture,
    station: LiveLink,
    c: BoardClient,
    h: Headers,
}

async fn setup() -> T {
    let mut f = fixture().await;
    let station = f.connect().await;
    f.store
        .apply_snapshot(
            f.station.clone(),
            f.inc.clone(),
            0,
            vec![SessionSummary::new("s1".into())],
        )
        .await
        .unwrap();
    // A fast watcher for the outcome of an accepted stop.
    let extras = Extras {
        stop_watch: Some(StopWatch {
            poll: Duration::from_millis(20),
            limit: Duration::from_millis(400),
        }),
        ..Extras::default()
    };
    let c = board_with_extras(Arc::clone(&f.ctx), extras).await;
    set_password(&c).await;
    let h = owner(&c).await;
    T { f, station, c, h }
}

async fn token(c: &BoardClient, scopes: &[Scope]) -> Headers {
    let (_, t) = c
        .store
        .create_token(
            "ops".into(),
            scopes.iter().copied().collect::<BTreeSet<_>>(),
            DAY,
        )
        .await
        .unwrap();
    vec![("Authorization".to_owned(), format!("Bearer {}", t.expose()))]
}

/// A member of a share on session `s1`.
async fn member(c: &BoardClient, h: &Headers) -> Headers {
    let r = post(
        c,
        h,
        "/api/shares",
        json!({ "kind": "session", "target": "fake/s1", "role": "collaborator" }),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    let url = r.body["url"].as_str().unwrap();
    let join = url.split("#join=").nth(1).unwrap();
    let j = c
        .post(
            "/api/auth/join",
            &[],
            json!({ "token": join, "display_name": "Ann" }),
        )
        .await;
    assert_eq!(j.status, 200, "{}", j.body);
    let cookie = j
        .header("set-cookie")
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    vec![
        ("Cookie".to_owned(), cookie),
        (
            "X-CSRF-Token".to_owned(),
            j.body["csrf"].as_str().unwrap().to_owned(),
        ),
    ]
}

fn check_json(last_stop: Value) -> Value {
    let mut v = json!({
        "shim_id": "shim-1",
        "reason": "lost: the link broke",
        "same_boot": true,
        "identity_alive": false,
        "group_members": [{ "pid": 4242, "command": "node" }],
        "session_members": [],
        "escaped": [],
        "complete": true,
        "error": null,
    });
    if !last_stop.is_null() {
        v["last_stop"] = last_stop;
    }
    v
}

fn plan_json() -> Value {
    json!({
        "plan_id": "plan-1",
        "expires_at": 1_792_400_030_000_i64,
        "eligible": true,
        "code": null,
        "targets": [{ "pid": 4242, "command": "node" }],
        "escaped": [{ "pid": 77, "command": "sleep" }],
        "check": check_json(Value::Null),
    })
}

fn stop_body(id: &str, elevation: Option<&str>) -> Value {
    let mut body = json!({
        "station": "fake",
        "command_id": id,
        "command": {
            "op": "stop_execution",
            "session_id": "s1",
            "shim_id": "shim-1",
            "plan_id": "plan-1",
        },
    });
    if let Some(e) = elevation {
        body["elevation"] = json!(e);
    }
    body
}

/// The `last_stop` of a check: the outcome of the command `id`.
fn last_json(id: &str) -> Value {
    json!({
        "command_id": id,
        "outcome": "done",
        "code": null,
        "term_sent": 2,
        "kill_sent": 1,
        "gone": 2,
        "skipped": 0,
    })
}

/// Answers the requests of the fake station until the stop command has its
/// receipt. The checks of the watcher that come before it get an answer
/// without `last_stop`. Returns the stop command.
async fn answer_stop(station: &mut LiveLink) -> InboundRequest {
    loop {
        let req = next_request(station).await;
        if req.method == "session.execution_check" {
            let answer = Outcome::Ok(check_json(Value::Null));
            station.control.respond(req.id, answer).await.unwrap();
            continue;
        }
        let id = req.command_id.clone().expect("a command id");
        station
            .control
            .respond(req.id, accepted(&id))
            .await
            .unwrap();
        return req;
    }
}

/// Answers the next request, which must be a check of the watcher.
async fn answer_check(station: &mut LiveLink, check: &Value) {
    let req = next_request(station).await;
    assert_eq!(req.method, "session.execution_check");
    station
        .control
        .respond(req.id, Outcome::Ok(check.clone()))
        .await
        .unwrap();
}

/// Answers every check of the watcher with `check` for `window`.
async fn answer_checks_for(station: &mut LiveLink, check: &Value, window: Duration) {
    let until = tokio::time::Instant::now() + window;
    while let Some(left) = until.checked_duration_since(tokio::time::Instant::now()) {
        let Ok(req) = tokio::time::timeout(left, next_request(station)).await else {
            break;
        };
        assert_eq!(req.method, "session.execution_check");
        station
            .control
            .respond(req.id, Outcome::Ok(check.clone()))
            .await
            .unwrap();
    }
}

/// The detail of the stop rows that start with `stage`, newest first.
async fn stage_rows(c: &BoardClient, stage: &str) -> Vec<AuditEntry> {
    stop_rows(c)
        .await
        .into_iter()
        .filter(|e| {
            e.detail
                .as_deref()
                .is_some_and(|d| d.split(' ').next() == Some(&format!("stage={stage}")))
        })
        .collect()
}

/// Waits (at most 2 s) until a row of `stage` exists.
async fn wait_stage(c: &BoardClient, stage: &str) {
    let until = std::time::Instant::now() + Duration::from_secs(2);
    while stage_rows(c, stage).await.is_empty() {
        assert!(
            std::time::Instant::now() < until,
            "no `{stage}` row in 2 s: {:?}",
            stop_rows(c).await
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// The `session.execution_stop` rows, newest first.
async fn stop_rows(c: &BoardClient) -> Vec<AuditEntry> {
    c.store
        .audit_log(200)
        .await
        .unwrap()
        .into_iter()
        .filter(|e| e.action == "session.execution_stop")
        .collect()
}

#[tokio::test]
async fn the_owner_reads_the_stop_plan_and_the_method_is_sent() {
    let mut t = setup().await;
    let (r, req) = with_station(
        &mut t.station,
        get(&t.c, &t.h, PLAN),
        Outcome::Ok(plan_json()),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body, plan_json());
    assert_eq!(req.method, "session.execution_stop_plan");
    assert_eq!(
        req.params,
        json!({ "session_id": "s1", "shim_id": "shim-1" })
    );
    // The read of a plan writes no audit row.
    assert!(
        t.c.store
            .audit_log(50)
            .await
            .unwrap()
            .iter()
            .all(|e| !e.action.contains("stop"))
    );
}

#[tokio::test]
async fn the_stop_plan_is_for_the_owner_only_and_needs_the_feature() {
    let mut t = setup().await;
    let m = member(&t.c, &t.h).await;
    let admin = token(&t.c, &[Scope::Read, Scope::Drive, Scope::Admin]).await;
    for who in [&m, &admin] {
        expect(&get(&t.c, who, PLAN).await, 403, "owner_only");
    }
    // A request without `shim_id` is refused by the route itself.
    let r = get(&t.c, &t.h, "/api/sessions/fake/s1/execution/stop-plan").await;
    assert_eq!(r.status, 400, "{}", r.body);
    expect(
        &get(
            &t.c,
            &t.h,
            "/api/sessions/nope/s1/execution/stop-plan?shim_id=x",
        )
        .await,
        404,
        "unknown_station",
    );
    // A station that did not negotiate the feature.
    t.f.store.set_link_features(&t.f.station, 98, vec![]);
    let r = get(&t.c, &t.h, PLAN).await;
    expect(&r, 501, "unsupported");
    assert_eq!(r.body["message"], UNSUPPORTED);
    no_request(&mut t.station, Duration::from_millis(300)).await;
}

#[tokio::test]
async fn the_station_answers_of_the_stop_plan_have_fixed_meanings() {
    let mut t = setup().await;
    for (code, status, name) in [
        ("unsupported", 501, "unsupported"),
        ("feature_not_negotiated", 501, "unsupported"),
        ("unknown_method", 501, "unsupported"),
        ("unknown_session", 404, "unknown_session"),
        ("invalid_params", 502, "station_error"),
    ] {
        let out = Outcome::Err(WireError::new(code, "no"));
        let (r, _) = with_station(&mut t.station, get(&t.c, &t.h, PLAN), out).await;
        expect(&r, status, name);
        if status == 501 {
            assert_eq!(r.body["message"], UNSUPPORTED, "{code}");
        }
    }
    let (r, _) = with_station(
        &mut t.station,
        get(&t.c, &t.h, PLAN),
        Outcome::Ok(json!({ "x": 1 })),
    )
    .await;
    expect(&r, 502, "station_error");
    // No live link.
    let station = t.station;
    t.f.drop_link(station).await;
    expect(&get(&t.c, &t.h, PLAN).await, 409, "station_unreachable");
}

#[tokio::test]
async fn the_stop_command_needs_the_owner_the_feature_and_an_elevation() {
    let mut t = setup().await;
    let m = member(&t.c, &t.h).await;
    let admin = token(&t.c, &[Scope::Read, Scope::Drive, Scope::Admin]).await;
    let e = elevate(&t.c, &t.h).await;
    // A member and a token get `owner_only`, also with a (foreign) elevation.
    for (n, who) in [&m, &admin].into_iter().enumerate() {
        let r = post(&t.c, who, STOP, stop_body(&format!("st-o{n}"), Some(&e))).await;
        expect(&r, 403, "owner_only");
    }
    // No elevation, and an elevation that is not one: 400 and nothing sent.
    for (n, bad) in [None, Some("not-an-elevation")].into_iter().enumerate() {
        let r = post(&t.c, &t.h, STOP, stop_body(&format!("st-e{n}"), bad)).await;
        expect(&r, 400, "elevation_required");
        assert_eq!(r.body["message"], ELEVATION);
    }
    // A station without the feature: 501, before the elevation is checked.
    t.f.store.set_link_features(&t.f.station, 98, vec![]);
    let r = post(&t.c, &t.h, STOP, stop_body("st-f0", None)).await;
    expect(&r, 501, "unsupported");
    assert_eq!(r.body["message"], UNSUPPORTED);
    let r = post(&t.c, &t.h, STOP, stop_body("st-f1", Some(&e))).await;
    expect(&r, 501, "unsupported");
    assert!(t.f.store.commands().await.unwrap().is_empty());
    no_request(&mut t.station, Duration::from_millis(300)).await;
}

#[tokio::test]
async fn an_elevated_owner_stop_command_reaches_the_station() {
    let mut t = setup().await;
    let e = elevate(&t.c, &t.h).await;
    let call = post(&t.c, &t.h, STOP, stop_body("st-ok", Some(&e)));
    let (r, req) = tokio::join!(call, answer_stop(&mut t.station));
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body["receipt"]["state"], "accepted");
    assert_eq!(req.method, "session.stop_execution");
    assert_eq!(
        req.params["command"],
        json!({
            "op": "stop_execution",
            "session_id": "s1",
            "shim_id": "shim-1",
            "plan_id": "plan-1",
        })
    );
    assert_eq!(t.f.store.commands().await.unwrap().len(), 1);
}

#[tokio::test]
async fn the_stop_command_writes_accepted_and_refused_rows_without_a_second_accepted() {
    let mut t = setup().await;
    let e = elevate(&t.c, &t.h).await;
    let target = format!("{}/s1", t.f.station.0);

    // Refused: no elevation.
    let r = post(&t.c, &t.h, STOP, stop_body("st-a-ref", None)).await;
    expect(&r, 400, "elevation_required");
    let rows = stop_rows(&t.c).await;
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].result, AuditResult::Refused);
    assert_eq!(
        rows[0].detail.as_deref(),
        Some("stage=refused code=elevation_required shim=shim-1")
    );
    assert_eq!(rows[0].target.as_deref(), Some(target.as_str()));
    assert_eq!(rows[0].command_id.as_ref().unwrap().as_str(), "st-a-ref");
    assert_eq!(rows[0].actor, "owner");

    // Accepted: one row; a resend of the same command ID adds none.
    let call = post(&t.c, &t.h, STOP, stop_body("st-a-ok", Some(&e)));
    let (r, _) = tokio::join!(call, answer_stop(&mut t.station));
    assert_eq!(r.status, 200, "{}", r.body);
    let again = post(&t.c, &t.h, STOP, stop_body("st-a-ok", Some(&e)));
    assert_eq!(again.await.status, 200);
    let accepted_rows: Vec<_> = stop_rows(&t.c)
        .await
        .into_iter()
        .filter(|r| r.result == AuditResult::Ok)
        .collect();
    assert_eq!(accepted_rows.len(), 1, "{accepted_rows:?}");
    assert_eq!(
        accepted_rows[0].detail.as_deref(),
        Some("stage=accepted shim=shim-1")
    );
    assert_eq!(accepted_rows[0].target.as_deref(), Some(target.as_str()));
    assert_eq!(
        accepted_rows[0].command_id.as_ref().unwrap().as_str(),
        "st-a-ok"
    );
}

#[tokio::test]
async fn a_last_stop_of_a_check_writes_one_outcome_row() {
    let mut t = setup().await;
    let path = "/api/sessions/fake/s1/execution";
    let last = json!({
        "command_id": "st-out-1",
        "outcome": "done",
        "code": null,
        "term_sent": 2,
        "kill_sent": 1,
        "gone": 2,
        "skipped": 0,
    });
    // A check without `last_stop` writes no outcome row.
    let (r, _) = with_station(
        &mut t.station,
        get(&t.c, &t.h, path),
        Outcome::Ok(check_json(Value::Null)),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert!(stop_rows(&t.c).await.is_empty());
    // The same `last_stop` in two checks: one row.
    for _ in 0..2 {
        let (r, _) = with_station(
            &mut t.station,
            get(&t.c, &t.h, path),
            Outcome::Ok(check_json(last.clone())),
        )
        .await;
        assert_eq!(r.status, 200, "{}", r.body);
        assert_eq!(r.body["last_stop"], last);
    }
    let rows = stop_rows(&t.c).await;
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].result, AuditResult::Ok);
    assert_eq!(
        rows[0].detail.as_deref(),
        Some("stage=outcome outcome=done code=- term=2 kill=1 gone=2 skipped=0 shim=shim-1")
    );
    assert_eq!(rows[0].command_id.as_ref().unwrap().as_str(), "st-out-1");
    // A failed stop with a code is a `failed` row of its own.
    let failed = json!({
        "command_id": "st-out-2",
        "outcome": "failed",
        "code": "permission_denied",
        "term_sent": 1,
        "kill_sent": 0,
        "gone": 0,
        "skipped": 1,
    });
    let (_, _) = with_station(
        &mut t.station,
        get(&t.c, &t.h, path),
        Outcome::Ok(check_json(failed)),
    )
    .await;
    let rows = stop_rows(&t.c).await;
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert_eq!(rows[0].result, AuditResult::Failed);
    assert_eq!(
        rows[0].detail.as_deref(),
        Some(
            "stage=outcome outcome=failed code=permission_denied term=1 kill=0 gone=0 skipped=1 shim=shim-1"
        )
    );
}

#[tokio::test]
async fn an_accepted_stop_writes_its_outcome_row_without_a_board_check() {
    let mut t = setup().await;
    let e = elevate(&t.c, &t.h).await;
    let call = post(&t.c, &t.h, STOP, stop_body("st-w-1", Some(&e)));
    let (r, _) = tokio::join!(call, answer_stop(&mut t.station));
    assert_eq!(r.status, 200, "{}", r.body);
    // The watcher asks for the check; the station has the outcome.
    let check = check_json(last_json("st-w-1"));
    answer_check(&mut t.station, &check).await;
    wait_stage(&t.c, "outcome").await;
    let rows = stage_rows(&t.c, "outcome").await;
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].result, AuditResult::Ok);
    assert_eq!(rows[0].command_id.as_ref().unwrap().as_str(), "st-w-1");
    assert_eq!(
        rows[0].detail.as_deref(),
        Some("stage=outcome outcome=done code=- term=2 kill=1 gone=2 skipped=0 shim=shim-1")
    );
    // A board check with the same `last_stop` adds no second row.
    let (r, _) = with_station(
        &mut t.station,
        get(&t.c, &t.h, "/api/sessions/fake/s1/execution"),
        Outcome::Ok(check),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(stage_rows(&t.c, "outcome").await.len(), 1);
    assert!(stage_rows(&t.c, "outcome_unobserved").await.is_empty());
}

#[tokio::test]
async fn a_stop_whose_outcome_is_not_seen_writes_unobserved_and_a_later_check_the_outcome() {
    let mut t = setup().await;
    let e = elevate(&t.c, &t.h).await;
    let call = post(&t.c, &t.h, STOP, stop_body("st-w-2", Some(&e)));
    let (r, _) = tokio::join!(call, answer_stop(&mut t.station));
    assert_eq!(r.status, 200, "{}", r.body);
    // Every check of the watcher has no `last_stop`.
    answer_checks_for(
        &mut t.station,
        &check_json(Value::Null),
        Duration::from_millis(1000),
    )
    .await;
    wait_stage(&t.c, "outcome_unobserved").await;
    let rows = stage_rows(&t.c, "outcome_unobserved").await;
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].result, AuditResult::Failed);
    assert_eq!(rows[0].command_id.as_ref().unwrap().as_str(), "st-w-2");
    assert_eq!(
        rows[0].detail.as_deref(),
        Some("stage=outcome_unobserved shim=shim-1")
    );
    assert!(stage_rows(&t.c, "outcome").await.is_empty());
    // The unobserved row does not claim the command ID: a later check with
    // the outcome writes it.
    let (r, _) = with_station(
        &mut t.station,
        get(&t.c, &t.h, "/api/sessions/fake/s1/execution"),
        Outcome::Ok(check_json(last_json("st-w-2"))),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    let rows = stage_rows(&t.c, "outcome").await;
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].command_id.as_ref().unwrap().as_str(), "st-w-2");
    assert_eq!(stage_rows(&t.c, "outcome_unobserved").await.len(), 1);
}
