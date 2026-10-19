//! The desktop tools routes and the maintenance channel of a station
//! (`board-gaps.md` section 2.9, BG-1) over loopback HTTPS with a fake station
//! link: the six routes, the owner rule, the two feature gates, the elevation,
//! the claim of a plan by one request, the fixed texts of the station codes
//! and the audit rows `station.maintenance` (opened, refused, completed) with
//! no password, no terminal byte and no message of the station.

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
use fleetdeck_proto::link::{Outcome, Scope, WireError};
use serde_json::{Value, json};
use settings_support::*;

const TOOLS: &str = "/api/stations/fake/desktop-tools";
const PLAN: &str = "/api/stations/fake/desktop-tools/plan";
const OPEN: &str = "/api/stations/fake/maintenance";
const UNSUPPORTED: &str = "This station's build cannot install desktop tools from the board.";
const ELEVATION: &str = "This action needs the owner's elevation.";
const INVALID: &str = "The request is not valid.";
const PLAN_CHANGED: &str = "The plan changed or expired. Review the desktop tools again.";
const SECRET_TEXT: &str = "SECRET-STATION-MESSAGE sudo password: hunter2";
const DAY: i64 = 24 * 60 * 60 * 1000;

struct World {
    f: Fixture,
    station: LiveLink,
    c: BoardClient,
    h: Headers,
}

async fn setup() -> World {
    let mut f = fixture().await;
    let station = f.connect().await;
    // A fast watcher for the completion of an opened run.
    let extras = Extras {
        maintenance_watch: Some(StopWatch {
            poll: Duration::from_millis(20),
            limit: Duration::from_millis(400),
        }),
        ..Extras::default()
    };
    let c = board_with_extras(Arc::clone(&f.ctx), extras).await;
    set_password(&c).await;
    let h = owner(&c).await;
    World { f, station, c, h }
}

/// An ID of 32 lowercase hex characters, different for each `n`. The plans
/// and the claims of the deck are in maps for the whole process.
fn hex(n: u8) -> String {
    format!("{n:02x}{}", "ab".repeat(15))
}

fn path_of(id: &str, tail: &str) -> String {
    format!("/api/stations/fake/maintenance/{id}/{tail}")
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

async fn elevated_for(c: &BoardClient, h: &Headers) -> Headers {
    let mut h = h.clone();
    let token = elevate(c, &h).await;
    h.push(("X-Fleetdeck-Elevation".to_owned(), token));
    h
}

async fn elevated(w: &World) -> Headers {
    elevated_for(&w.c, &w.h).await
}

fn tools_json() -> Value {
    json!({
        "sampled_at": 1_792_400_000_000_i64, "desktop": "omarchy", "tools": [],
        "keyboard": { "status": "ok", "layouts": ["us"] },
        "plugin": { "status": "valid" }, "passwordless_sudo": "off",
    })
}

fn plan_json(plan_id: Option<&str>) -> Value {
    json!({
        "plan_id": plan_id,
        "expires_at": plan_id.map(|_| 1_792_400_120_000_i64),
        "installable": ["wtype"], "unavailable": [], "already_present": ["grim"],
        "command_display": plan_id.map(|_| "sudo pacman -S --needed extra/wtype"),
        "system_upgrade": false, "blocked": null, "refused": [],
        "transaction": [{ "repo": "extra", "name": "wtype", "version": "0.4-1" }],
    })
}

/// The answer of an open for `request` (the deck checks the request ID).
fn with_request(mut info: Value, request: &str) -> Value {
    info["request_id"] = json!(request);
    info
}

fn run_json(id: &str, state: &str, code: Option<i32>, post: Option<&str>) -> Value {
    json!({
        "maintenance_id": id,
        "request_id": hex(250),
        "state": state,
        "exit_code": code,
        "post_check": post,
        "created_at": 1_792_400_000_000_i64,
        "tools": ["wtype", "grim"],
    })
}

fn list_json(runs: &[Value]) -> Value {
    json!({ "runs": runs })
}

fn open_body(plan: &str, request: &str) -> Value {
    json!({ "plan_id": plan, "request_id": request, "cols": 120, "rows": 40 })
}

/// The `station.maintenance` rows, newest first.
async fn rows(c: &BoardClient) -> Vec<AuditEntry> {
    c.store
        .audit_log(200)
        .await
        .unwrap()
        .into_iter()
        .filter(|e| e.action == "station.maintenance")
        .collect()
}

/// The `station.maintenance` rows whose detail starts with `stage=<stage>`.
async fn stage_rows(c: &BoardClient, stage: &str) -> Vec<AuditEntry> {
    rows(c)
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
            rows(c).await
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Answers every `maintenance.list` of the watcher with `runs` for `window`.
async fn serve_lists(station: &mut LiveLink, runs: &Value, window: Duration) {
    let until = tokio::time::Instant::now() + window;
    while let Some(left) = until.checked_duration_since(tokio::time::Instant::now()) {
        let Ok(req) = tokio::time::timeout(left, next_request(station)).await else {
            break;
        };
        assert_eq!(req.method, "maintenance.list");
        station
            .control
            .respond(req.id, Outcome::Ok(runs.clone()))
            .await
            .unwrap();
    }
}

/// Answers the requests of the fake station until `maintenance.open` has its
/// answer `info`. The lists of the watcher that come before get a `running`
/// run. Returns the open request.
async fn answer_open(station: &mut LiveLink, info: &Value) -> InboundRequest {
    loop {
        let req = next_request(station).await;
        if req.method == "maintenance.list" {
            let runs = list_json(std::slice::from_ref(info));
            station
                .control
                .respond(req.id, Outcome::Ok(runs))
                .await
                .unwrap();
            continue;
        }
        station
            .control
            .respond(req.id, Outcome::Ok(info.clone()))
            .await
            .unwrap();
        return req;
    }
}

/// Reads a plan as the owner login; the station answers with the plan `plan`.
async fn read_plan(w: &mut World, plan: &str) {
    let (r, req) = with_station(
        &mut w.station,
        get(&w.c, &w.h, PLAN),
        Outcome::Ok(plan_json(Some(plan))),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(req.method, "station.desktop_tools_plan");
}

/// Opens a run with `plan` and `request`; the station answers with `info`.
async fn open_ok(w: &mut World, h: &Headers, plan: &str, request: &str, info: &Value) -> Value {
    let call = post(&w.c, h, OPEN, open_body(plan, request));
    let (r, req) = tokio::join!(call, answer_open(&mut w.station, info));
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(req.method, "maintenance.open");
    r.body
}

#[tokio::test]
async fn each_route_sends_its_link_method_and_params() {
    let mut w = setup().await;
    let (plan, request, run) = (hex(1), hex(2), hex(3));
    let h = elevated(&w).await;

    let (r, req) = with_station(
        &mut w.station,
        get(&w.c, &w.h, TOOLS),
        Outcome::Ok(tools_json()),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body, tools_json());
    assert_eq!(
        (req.method.as_str(), req.params),
        ("station.tools", json!({}))
    );

    read_plan(&mut w, &plan).await;
    let (r, _) = with_station(
        &mut w.station,
        get(&w.c, &w.h, PLAN),
        Outcome::Ok(plan_json(None)),
    )
    .await;
    assert_eq!(r.body, plan_json(None), "a plan without an ID is passed on");

    let info = with_request(run_json(&run, "running", None, None), &request);
    let call = post(&w.c, &h, OPEN, open_body(&plan, &request));
    let (r, req) = tokio::join!(call, answer_open(&mut w.station, &info));
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body, info);
    assert_eq!(req.method, "maintenance.open");
    assert_eq!(req.params, open_body(&plan, &request));

    // Let the watcher of the run end before the next call.
    serve_lists(
        &mut w.station,
        &list_json(std::slice::from_ref(&info)),
        Duration::from_millis(700),
    )
    .await;

    let runs = list_json(std::slice::from_ref(&info));
    let (r, req) = with_station(
        &mut w.station,
        get(&w.c, &w.h, OPEN),
        Outcome::Ok(runs.clone()),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body, runs);
    assert_eq!(
        (req.method.as_str(), req.params),
        ("maintenance.list", json!({}))
    );

    let (r, req) = with_station(
        &mut w.station,
        post(
            &w.c,
            &h,
            &path_of(&run, "resize"),
            json!({ "cols": 90, "rows": 30 }),
        ),
        Outcome::Ok(json!({})),
    )
    .await;
    assert_eq!(r.status, 204, "{}", r.body);
    assert_eq!(req.method, "maintenance.resize");
    assert_eq!(
        req.params,
        json!({ "maintenance_id": run, "cols": 90, "rows": 30 })
    );
}

#[tokio::test]
async fn the_routes_are_for_the_owner_login_only() {
    let mut w = setup().await;
    let admin = token(&w.c, &[Scope::Read, Scope::Drive, Scope::Admin]).await;
    let (id, body) = (hex(10), json!({ "cols": 80, "rows": 24 }));
    for r in [
        get(&w.c, &admin, TOOLS).await,
        get(&w.c, &admin, PLAN).await,
        get(&w.c, &admin, OPEN).await,
        post(&w.c, &admin, OPEN, open_body(&hex(11), &hex(12))).await,
        post(&w.c, &admin, &path_of(&id, "resize"), body).await,
    ] {
        expect(&r, 403, "owner_only");
    }
    no_request(&mut w.station, Duration::from_millis(300)).await;
    // The refused row of the open is the owner gate's own row.
    assert!(stage_rows(&w.c, "refused").await.is_empty());
}

#[tokio::test]
async fn each_feature_gates_its_routes_with_501_and_a_fixed_text() {
    let mut w = setup().await;
    let h = elevated(&w).await;
    let id = hex(20);
    let sock_cookie = w.h[0].1.clone();
    let origin = w.c.origin();
    let sock = [
        ("Cookie", sock_cookie.as_str()),
        ("Origin", origin.as_str()),
    ];
    let attach = path_of(&id, "attach");
    let resize = path_of(&id, "resize");

    // Only the tools feature: the five install routes are refused.
    w.f.store
        .set_link_features(&w.f.station, 98, vec!["station_tools.v1".to_owned()]);
    for r in [
        get(&w.c, &w.h, PLAN).await,
        get(&w.c, &w.h, OPEN).await,
        post(&w.c, &h, OPEN, open_body(&hex(21), &hex(22))).await,
        post(&w.c, &h, &resize, json!({ "cols": 80, "rows": 24 })).await,
    ] {
        expect(&r, 501, "unsupported");
        assert_eq!(r.body["message"], UNSUPPORTED);
    }
    assert_eq!(w.c.ws(&attach, &sock).await.err(), Some(501));
    no_request(&mut w.station, Duration::from_millis(300)).await;
    let (r, _) = with_station(
        &mut w.station,
        get(&w.c, &w.h, TOOLS),
        Outcome::Ok(tools_json()),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);

    // Only the install feature: the tools read is refused.
    w.f.store
        .set_link_features(&w.f.station, 98, vec!["desktop_tool_install.v1".to_owned()]);
    let r = get(&w.c, &w.h, TOOLS).await;
    expect(&r, 501, "unsupported");
    assert_eq!(r.body["message"], UNSUPPORTED);
    no_request(&mut w.station, Duration::from_millis(300)).await;
    let (r, _) = with_station(
        &mut w.station,
        get(&w.c, &w.h, PLAN),
        Outcome::Ok(plan_json(None)),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);

    // The refusal of the open is audited (after the owner gate).
    w.f.store.set_link_features(&w.f.station, 98, vec![]);
    let (plan, request) = (hex(23), hex(24));
    let r = post(&w.c, &h, OPEN, open_body(&plan, &request)).await;
    expect(&r, 501, "unsupported");
    let refused = stage_rows(&w.c, "refused").await;
    assert_eq!(refused.len(), 2, "{refused:?}");
    for row in &refused {
        assert_eq!(row.result, AuditResult::Refused);
        assert_eq!(
            row.detail.as_deref(),
            Some("stage=refused code=unsupported request=-")
        );
    }

    // An unknown station is a 404 and a down link is a 409.
    w.f.store.clear_link_features(&w.f.station, 98);
    expect(
        &get(&w.c, &w.h, "/api/stations/nobody/desktop-tools").await,
        404,
        "unknown_station",
    );
    w.f.drop_link(w.station).await;
    expect(&get(&w.c, &w.h, TOOLS).await, 409, "station_unreachable");
}

#[tokio::test]
async fn the_open_needs_an_elevation_and_a_plan_that_the_deck_saw() {
    let mut w = setup().await;
    let (plan, request) = (hex(30), hex(31));
    // No elevation, and an elevation that is not one: 400, nothing sent, one
    // refused row each.
    for bad in [None, Some("not-an-elevation")] {
        let mut h = w.h.clone();
        if let Some(b) = bad {
            h.push(("X-Fleetdeck-Elevation".to_owned(), b.to_owned()));
        }
        let r = post(&w.c, &h, OPEN, open_body(&plan, &request)).await;
        expect(&r, 400, "elevation_required");
        assert_eq!(r.body["message"], ELEVATION);
    }
    let refused = stage_rows(&w.c, "refused").await;
    assert_eq!(refused.len(), 2, "{refused:?}");
    for row in &refused {
        assert_eq!(row.result, AuditResult::Refused);
        assert_eq!(row.target.as_deref(), Some("fake"));
        assert_eq!(
            row.detail.as_deref(),
            Some(format!("stage=refused code=elevation_required request={request}").as_str())
        );
    }
    // A plan that no read gave: 409 `plan_changed`, nothing sent.
    let h = elevated(&w).await;
    let r = post(&w.c, &h, OPEN, open_body(&plan, &request)).await;
    expect(&r, 409, "plan_changed");
    assert_eq!(r.body["message"], PLAN_CHANGED);
    no_request(&mut w.station, Duration::from_millis(300)).await;
    let refused = stage_rows(&w.c, "refused").await;
    assert_eq!(
        refused[0].detail.as_deref(),
        Some(format!("stage=refused code=plan_changed request={request}").as_str())
    );
}

#[tokio::test]
async fn a_plan_belongs_to_one_request_of_one_login_for_two_minutes() {
    let mut w = setup().await;
    let (plan, r1, r2, run) = (hex(40), hex(41), hex(42), hex(43));
    let h = elevated(&w).await;
    read_plan(&mut w, &plan).await;
    let info = with_request(run_json(&run, "running", None, None), &r1);

    // The same request asks twice: both reach the station.
    open_ok(&mut w, &h, &plan, &r1, &info).await;
    open_ok(&mut w, &h, &plan, &r1, &info).await;

    // Another request cannot use the plan.
    let r = post(&w.c, &h, OPEN, open_body(&plan, &r2)).await;
    expect(&r, 409, "plan_changed");
    // Another login cannot use it either.
    let other = owner(&w.c).await;
    let other = elevated_for(&w.c, &other).await;
    let r = post(&w.c, &other, OPEN, open_body(&plan, &r1)).await;
    expect(&r, 409, "plan_changed");
    serve_lists(
        &mut w.station,
        &list_json(std::slice::from_ref(&info)),
        Duration::from_millis(500),
    )
    .await;

    // After two minutes the binding is gone, also for the request that has it.
    w.f.det.advance(Duration::from_secs(121));
    let h = elevated(&w).await;
    let r = post(&w.c, &h, OPEN, open_body(&plan, &r1)).await;
    expect(&r, 409, "plan_changed");
    no_request(&mut w.station, Duration::from_millis(300)).await;
}

#[tokio::test]
async fn a_request_that_is_not_valid_is_refused_before_the_station() {
    let mut w = setup().await;
    let h = elevated(&w).await;
    let (plan, request) = (hex(50), hex(51));
    read_plan(&mut w, &plan).await;
    let upper = "AB".repeat(16);
    let bodies = [
        json!({ "plan_id": plan, "request_id": "short", "cols": 80, "rows": 24 }),
        json!({ "plan_id": plan, "request_id": upper, "cols": 80, "rows": 24 }),
        json!({ "plan_id": "short", "request_id": request, "cols": 80, "rows": 24 }),
        json!({ "plan_id": plan, "request_id": request, "cols": 0, "rows": 24 }),
        json!({ "plan_id": plan, "request_id": request, "cols": 80, "rows": 1001 }),
        json!({ "plan_id": plan, "request_id": request, "cols": 70_000, "rows": 24 }),
        json!({ "plan_id": plan, "request_id": request, "cols": 80, "rows": 24, "extra": 1 }),
        json!({ "plan_id": plan, "request_id": request, "cols": 80 }),
    ];
    for body in bodies {
        let r = post(&w.c, &h, OPEN, body.clone()).await;
        expect(&r, 400, "invalid_request");
        assert_eq!(r.body["message"], INVALID, "{body}");
    }
    no_request(&mut w.station, Duration::from_millis(300)).await;
    let refused = stage_rows(&w.c, "refused").await;
    assert_eq!(refused.len(), 8, "{refused:?}");
    // A request ID that is not valid never reaches the audit.
    assert!(refused.iter().all(|e| {
        e.result == AuditResult::Refused
            && e.detail.as_deref().is_some_and(|d| {
                d.starts_with("stage=refused code=invalid_request request=")
                    && !d.contains("short")
                    && !d.contains(&upper)
            })
    }));
    // The plan was not claimed by a bad request: a good one still opens.
    let info = with_request(run_json(&hex(52), "running", None, None), &request);
    open_ok(&mut w, &h, &plan, &request, &info).await;
}

#[tokio::test]
async fn each_station_code_has_a_fixed_status_name_and_text() {
    let mut w = setup().await;
    let h = elevated(&w).await;
    let (plan, request) = (hex(60), hex(61));
    read_plan(&mut w, &plan).await;
    let conflict = |name: &'static str| (409, name, code_text(name));
    let cases = [
        conflict("transaction_refused"),
        conflict("maintenance_busy"),
        conflict("request_id_conflict"),
        conflict("shell_disabled"),
        conflict("plan_changed"),
        conflict("maintenance_ended"),
        (404, "unknown_maintenance", code_text("unknown_maintenance")),
        (400, "invalid_request", INVALID),
        (400, "invalid_request", INVALID),
        (501, "unsupported", UNSUPPORTED),
        (501, "unsupported", UNSUPPORTED),
        (501, "unsupported", UNSUPPORTED),
        (501, "unsupported", UNSUPPORTED),
        (502, "station_error", "the station answered with an error"),
    ];
    let wire = [
        "transaction_refused",
        "maintenance_busy",
        "request_id_conflict",
        "shell_disabled",
        "plan_changed",
        "maintenance_ended",
        "unknown_maintenance",
        "invalid_params",
        "invalid_request",
        "unavailable",
        "unsupported",
        "feature_not_negotiated",
        "unknown_method",
        "something_else",
    ];
    for (code, (status, name, text)) in wire.into_iter().zip(cases) {
        let out = Outcome::Err(WireError::new(code, SECRET_TEXT));
        let (r, _) = with_station(
            &mut w.station,
            post(&w.c, &h, OPEN, open_body(&plan, &request)),
            out,
        )
        .await;
        expect(&r, status, name);
        assert_eq!(r.body["message"], text, "{code}");
        assert!(!r.text.contains("SECRET"), "{code}: {}", r.text);
    }
    // An answer that the deck does not know.
    let (r, _) = with_station(
        &mut w.station,
        post(&w.c, &h, OPEN, open_body(&plan, &request)),
        Outcome::Ok(json!({ "x": 1 })),
    )
    .await;
    expect(&r, 502, "station_error");
    // The audit holds the code of the deck and never the station's text.
    let refused = stage_rows(&w.c, "refused").await;
    assert_eq!(refused.len(), 15, "{refused:?}");
    assert!(refused.iter().all(|e| {
        let d = e.detail.as_deref().unwrap();
        !d.contains("SECRET") && d.ends_with(&format!("request={request}"))
    }));
    assert!(refused.iter().any(|e| {
        e.detail
            .as_deref()
            .is_some_and(|d| d.starts_with("stage=refused code=maintenance_busy "))
    }));
    // The reads map the same codes.
    let (r, _) = with_station(
        &mut w.station,
        get(&w.c, &w.h, OPEN),
        Outcome::Err(WireError::new("unknown_method", SECRET_TEXT)),
    )
    .await;
    expect(&r, 501, "unsupported");
    let (r, _) = with_station(
        &mut w.station,
        get(&w.c, &w.h, TOOLS),
        Outcome::Err(WireError::new("boom", SECRET_TEXT)),
    )
    .await;
    expect(&r, 502, "station_error");
    assert!(!r.text.contains("SECRET"), "{}", r.text);
}

fn code_text(name: &str) -> &'static str {
    fleetdeck_proto::desktop_tools::code::text(name).unwrap()
}

#[tokio::test]
async fn an_opened_run_has_an_opened_row_and_one_completed_row() {
    let mut w = setup().await;
    let h = elevated(&w).await;
    let (plan, request, run) = (hex(70), hex(71), hex(72));
    read_plan(&mut w, &plan).await;
    let info = with_request(run_json(&run, "running", None, None), &request);
    open_ok(&mut w, &h, &plan, &request, &info).await;
    let opened = stage_rows(&w.c, "opened").await;
    assert_eq!(opened.len(), 1, "{opened:?}");
    assert_eq!(opened[0].result, AuditResult::Ok);
    assert_eq!(opened[0].actor, "owner");
    assert_eq!(opened[0].target.as_deref(), Some("fake"));
    assert_eq!(
        opened[0].detail.as_deref(),
        Some(format!("stage=opened request={request} maintenance={run} tools=wtype,grim").as_str())
    );

    // The watcher and a list read of the owner both see the end of the run.
    let exited = list_json(&[run_json(&run, "exited", Some(0), Some("ok"))]);
    let read = get(&w.c, &w.h, OPEN);
    let serve = serve_lists(&mut w.station, &exited, Duration::from_millis(300));
    let (r, ()) = tokio::join!(read, serve);
    assert_eq!(r.status, 200, "{}", r.body);
    wait_stage(&w.c, "completed").await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    let done = stage_rows(&w.c, "completed").await;
    assert_eq!(done.len(), 1, "{done:?}");
    assert_eq!(done[0].result, AuditResult::Ok);
    assert_eq!(
        done[0].detail.as_deref(),
        Some(format!("stage=completed maintenance={run} outcome=exited code=0 post=ok").as_str())
    );
}

#[tokio::test]
async fn the_watcher_alone_writes_the_completed_row_once() {
    let mut w = setup().await;
    let h = elevated(&w).await;
    let (plan, request, run) = (hex(75), hex(76), hex(77));
    read_plan(&mut w, &plan).await;
    open_ok(
        &mut w,
        &h,
        &plan,
        &request,
        &with_request(run_json(&run, "admitted", None, None), &request),
    )
    .await;
    let ended = list_json(&[run_json(&run, "exited", Some(1), Some("ok"))]);
    serve_lists(&mut w.station, &ended, Duration::from_millis(300)).await;
    wait_stage(&w.c, "completed").await;
    let done = stage_rows(&w.c, "completed").await;
    assert_eq!(done.len(), 1, "{done:?}");
    assert_eq!(done[0].result, AuditResult::Failed);
    assert_eq!(
        done[0].detail.as_deref(),
        Some(format!("stage=completed maintenance={run} outcome=exited code=1 post=ok").as_str())
    );
}

#[tokio::test]
async fn a_list_read_writes_the_completed_row_of_each_ended_run_once() {
    let mut w = setup().await;
    let (ok, failed, changed, lost, running, admitted) =
        (hex(80), hex(81), hex(82), hex(83), hex(84), hex(85));
    let runs = list_json(&[
        run_json(&ok, "exited", Some(0), Some("ok")),
        run_json(&failed, "exited", Some(2), Some("ok")),
        run_json(&changed, "exited", Some(0), Some("transaction_changed")),
        run_json(&lost, "lost", None, None),
        run_json(&running, "running", None, None),
        run_json(&admitted, "admitted", None, None),
    ]);
    for _ in 0..2 {
        let (r, _) = with_station(
            &mut w.station,
            get(&w.c, &w.h, OPEN),
            Outcome::Ok(runs.clone()),
        )
        .await;
        assert_eq!(r.status, 200, "{}", r.body);
        assert_eq!(r.body, runs);
    }
    let done = stage_rows(&w.c, "completed").await;
    assert_eq!(done.len(), 4, "{done:?}");
    let find = |id: &str| {
        done.iter()
            .find(|e| {
                e.detail
                    .as_deref()
                    .unwrap()
                    .contains(&format!("maintenance={id} "))
            })
            .unwrap_or_else(|| panic!("no row for {id}: {done:?}"))
    };
    let expect_row = |id: &str, result: AuditResult, tail: &str| {
        let row = find(id);
        assert_eq!(row.result, result, "{id}");
        assert_eq!(
            row.detail.as_deref(),
            Some(format!("stage=completed maintenance={id} {tail}").as_str())
        );
        assert_eq!(row.target.as_deref(), Some("fake"));
    };
    expect_row(&ok, AuditResult::Ok, "outcome=exited code=0 post=ok");
    expect_row(
        &failed,
        AuditResult::Failed,
        "outcome=exited code=2 post=ok",
    );
    expect_row(
        &changed,
        AuditResult::Failed,
        "outcome=exited code=0 post=transaction_changed",
    );
    expect_row(&lost, AuditResult::Failed, "outcome=lost code=- post=-");
}

#[tokio::test]
async fn the_resize_route_checks_its_input_and_maps_the_station_codes() {
    let mut w = setup().await;
    let h = elevated(&w).await;
    let id = hex(90);
    let path = path_of(&id, "resize");
    for (p, body) in [
        (
            path_of("short", "resize"),
            json!({ "cols": 80, "rows": 24 }),
        ),
        (
            path_of(&"AB".repeat(16), "resize"),
            json!({ "cols": 80, "rows": 24 }),
        ),
        (path.clone(), json!({ "cols": 0, "rows": 24 })),
        (path.clone(), json!({ "cols": 80, "rows": 1001 })),
        (path.clone(), json!({ "cols": 80, "rows": 24, "extra": 1 })),
        (path.clone(), json!({ "cols": 80 })),
    ] {
        let r = post(&w.c, &h, &p, body.clone()).await;
        expect(&r, 400, "invalid_request");
        assert_eq!(r.body["message"], INVALID, "{body}");
    }
    no_request(&mut w.station, Duration::from_millis(300)).await;
    for (code, status, name) in [
        ("unknown_maintenance", 404, "unknown_maintenance"),
        ("maintenance_ended", 409, "maintenance_ended"),
        ("unsupported", 501, "unsupported"),
    ] {
        let (r, _) = with_station(
            &mut w.station,
            post(&w.c, &h, &path, json!({ "cols": 80, "rows": 24 })),
            Outcome::Err(WireError::new(code, SECRET_TEXT)),
        )
        .await;
        expect(&r, status, name);
        assert!(!r.text.contains("SECRET"), "{}", r.text);
    }
}

#[tokio::test]
async fn the_attach_route_refuses_a_bad_id_and_a_missing_feature_before_the_upgrade() {
    let mut w = setup().await;
    let cookie = w.h[0].1.clone();
    let origin = w.c.origin();
    let sock = [("Cookie", cookie.as_str()), ("Origin", origin.as_str())];
    let good = path_of(&hex(95), "attach");
    assert_eq!(
        w.c.ws(&path_of("short", "attach"), &sock).await.err(),
        Some(400)
    );
    assert_eq!(
        w.c.ws("/api/stations/nobody/maintenance/x/attach", &sock)
            .await
            .err(),
        Some(404)
    );
    w.f.store.set_link_features(&w.f.station, 98, vec![]);
    assert_eq!(w.c.ws(&good, &sock).await.err(), Some(501));
    w.f.store.clear_link_features(&w.f.station, 98);
    no_request(&mut w.station, Duration::from_millis(300)).await;
}
