//! The MCP App call and release routes over loopback HTTPS (B15, design
//! `mcp-apps.md` §4.7): CSRF, the error mapping, the audit rows, the link
//! params and the call rate limit. The station is a fake that answers the
//! link methods itself.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod apps_support;
mod common;
mod share_support;

use std::collections::BTreeSet;
use std::time::Duration;

use apps_support::*;
use common::*;
use fleetdeck_deck::store::{AuditEntry, AuditResult};
use fleetdeck_link::LiveLink;
use fleetdeck_proto::link::{Outcome, Scope, WireError};
use serde_json::json;
use share_support::*;

async fn quiet(station: &mut LiveLink) {
    no_request(station, Duration::from_millis(300)).await;
}

async fn rows(f: &Fixture, action: &str) -> Vec<AuditEntry> {
    let mut all = f.store.audit_log(200).await.unwrap();
    all.retain(|r| r.action == action);
    all
}

fn wire(code: &str) -> Outcome {
    Outcome::Err(WireError::new(code, "The station text."))
}

#[tokio::test]
async fn app_call_and_release_need_csrf_with_a_cookie_login() {
    let (f, mut station, c, h) = setup().await;
    let st = f.station.0.clone();
    let no_csrf: Headers = vec![h[0].clone()];
    let r = post(
        &c,
        &no_csrf,
        &call_path(&st, "s1", APP),
        tools_call("bump", true),
    )
    .await;
    expect(&r, 403, "csrf");
    let r = post(&c, &no_csrf, &release_path(&st, "s1", CALL), json!({})).await;
    expect(&r, 403, "csrf");
    quiet(&mut station).await;
    for action in ["sessions.app_call", "sessions.app_release"] {
        let refused = rows(&f, action).await;
        assert_eq!(refused.len(), 1, "{action}: {refused:?}");
        assert_eq!(refused[0].result, AuditResult::Refused);
        assert_eq!(refused[0].detail.as_deref(), Some("csrf"));
    }
    // With the token the handler runs: the station gets the request.
    let (r, req) = one(
        &c,
        &h,
        &mut station,
        "POST",
        &call_path(&st, "s1", APP),
        Some(tools_call("bump", true)),
        done(),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(req.method, "session.app_call");
    let (r, req) = one(
        &c,
        &h,
        &mut station,
        "POST",
        &release_path(&st, "s1", CALL),
        None,
        Outcome::Ok(json!({ "released": true })),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(req.method, "session.app_release");
    // A bearer token has no CSRF token; it is refused as not the owner.
    let (_, token) = f
        .store
        .create_token(
            "writer".into(),
            BTreeSet::from([Scope::Read, Scope::Drive]),
            86_400_000,
        )
        .await
        .unwrap();
    let auth = format!("Bearer {}", token.expose());
    let r = c
        .req(
            "POST",
            &call_path(&st, "s1", APP),
            &[("Authorization", auth.as_str())],
            Some(tools_call("bump", true)),
        )
        .await;
    expect(&r, 403, "owner_only");
    quiet(&mut station).await;
}

#[tokio::test]
async fn call_route_maps_station_errors_to_http() {
    let (f, mut station, c, h) = setup().await;
    let st = f.station.0.clone();
    let table = [
        ("unknown_session", 404),
        ("unknown_app", 404),
        ("unknown_call", 404),
        ("session_stopped", 409),
        ("call_running", 409),
        ("app_gone", 410),
        ("invalid_params", 400),
        ("tool_not_callable", 403),
        ("too_large", 413),
        ("busy", 429),
        ("provider_error", 502),
        ("timeout", 504),
    ];
    let path = call_path(&st, "s1", APP);
    for (code, status) in table {
        let (r, _) = one(
            &c,
            &h,
            &mut station,
            "POST",
            &path,
            Some(tools_call("bump", true)),
            wire(code),
        )
        .await;
        assert_eq!((r.status, r.code()), (status, code), "{}", r.body);
        assert_eq!(r.body["message"], "The station text.", "{code}");
    }
    // Any other code, and an answer that is not the expected shape, give a
    // fixed 502 without the station's text.
    for outcome in [
        Outcome::Err(WireError::new("internal", "STATION-TEXT-MUST-NOT-LEAK")),
        Outcome::Err(WireError::new("boom", "STATION-TEXT-MUST-NOT-LEAK")),
        Outcome::Ok(json!({ "outcome": "STATION-TEXT-MUST-NOT-LEAK" })),
    ] {
        let (r, _) = one(
            &c,
            &h,
            &mut station,
            "POST",
            &path,
            Some(tools_call("bump", true)),
            outcome,
        )
        .await;
        expect(&r, 502, "station_error");
        assert!(!r.text.contains("STATION-TEXT-MUST-NOT-LEAK"), "{}", r.text);
    }
    // A control character of the station's text does not reach the app.
    let ctrl = Outcome::Err(WireError::new("provider_error", "a\nb\u{1b}c"));
    let (r, _) = one(
        &c,
        &h,
        &mut station,
        "POST",
        &path,
        Some(tools_call("bump", true)),
        ctrl,
    )
    .await;
    assert_eq!(r.body["message"], "abc");
}

#[tokio::test]
async fn call_route_writes_audit_for_tools_call_only() {
    let (f, mut station, c, h) = setup().await;
    let st = f.station.0.clone();
    let path = call_path(&st, "s1", APP);

    // A done call, with the exact link params.
    let (r, req) = one(
        &c,
        &h,
        &mut station,
        "POST",
        &path,
        Some(tools_call("bump", true)),
        done(),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body["outcome"], "done");
    assert_eq!(req.method, "session.app_call");
    assert_eq!(
        req.params,
        json!({
            "session_id": "s1", "app_id": APP, "confirmed": true,
            "request": { "method": "tools/call", "name": "bump", "arguments": { "by": 2 } },
        })
    );
    // A confirm answer.
    let confirm = Outcome::Ok(json!({ "outcome": "confirm", "tool": "bump", "title": null }));
    let (r, _) = one(
        &c,
        &h,
        &mut station,
        "POST",
        &path,
        Some(tools_call("bump", false)),
        confirm,
    )
    .await;
    assert_eq!(r.body["outcome"], "confirm");
    // Errors: busy (429, failed) and unknown_app (404, refused).
    let (r, _) = one(
        &c,
        &h,
        &mut station,
        "POST",
        &path,
        Some(tools_call("bump", true)),
        wire("busy"),
    )
    .await;
    assert_eq!(r.status, 429);
    let (r, _) = one(
        &c,
        &h,
        &mut station,
        "POST",
        &path,
        Some(tools_call("bump", true)),
        wire("unknown_app"),
    )
    .await;
    assert_eq!(r.status, 404);
    // A tool name with a control character is cleaned and never logs arguments.
    let (r, _) = one(
        &c,
        &h,
        &mut station,
        "POST",
        &path,
        Some(tools_call("odd\ntool", true)),
        done(),
    )
    .await;
    assert_eq!(r.status, 200);
    // A resources/read gets no row; a body that does not parse gets 400 and no row.
    let read = json!({ "request": { "method": "resources/read", "uri": "ui://demo/x" } });
    let (r, _) = one(&c, &h, &mut station, "POST", &path, Some(read), done()).await;
    assert_eq!(r.status, 200, "{}", r.body);
    let r = post(&c, &h, &path, json!({ "request": { "method": "nope" } })).await;
    expect(&r, 400, "invalid_params");
    quiet(&mut station).await;

    let rows = rows(&f, "session.app_call").await;
    assert_eq!(rows.len(), 5, "{rows:?}");
    let by_detail = |detail: &str| {
        rows.iter()
            .find(|r| r.detail.as_deref() == Some(detail))
            .unwrap_or_else(|| panic!("no row {detail}: {rows:?}"))
    };
    let ok = by_detail(&format!("{APP}/bump ok"));
    assert_eq!(ok.result, AuditResult::Ok);
    assert_eq!(ok.actor, "owner");
    assert_eq!(ok.target, Some(format!("{st}/s1")));
    assert_eq!(
        by_detail(&format!("{APP}/bump confirm")).result,
        AuditResult::Ok
    );
    assert_eq!(
        by_detail(&format!("{APP}/bump busy")).result,
        AuditResult::Failed
    );
    assert_eq!(
        by_detail(&format!("{APP}/bump unknown_app")).result,
        AuditResult::Refused
    );
    assert_eq!(
        by_detail(&format!("{APP}/oddtool ok")).result,
        AuditResult::Ok
    );
    let text = format!("{rows:?}");
    assert!(!text.contains("\"by\""), "the arguments are never logged");
    assert!(!text.contains("ui://"), "{text}");
}

#[tokio::test]
async fn release_route_writes_audit() {
    let (f, mut station, c, h) = setup().await;
    let st = f.station.0.clone();
    let path = release_path(&st, "s1", CALL);
    let released = Outcome::Ok(json!({ "released": true }));
    let not_open = Outcome::Ok(json!({ "released": false }));
    for outcome in [
        released,
        not_open,
        wire("call_running"),
        wire("unknown_call"),
    ] {
        one(&c, &h, &mut station, "POST", &path, None, outcome).await;
    }
    // A call id that is not a ULID never reaches the station or the log.
    let r = post(&c, &h, &release_path(&st, "s1", "..%2Fetc"), json!({})).await;
    expect(&r, 400, "invalid_params");
    quiet(&mut station).await;
    let rows = rows(&f, "session.app_release").await;
    assert_eq!(rows.len(), 5, "{rows:?}");
    let find = |detail: &str| {
        rows.iter()
            .find(|r| r.detail.as_deref() == Some(detail))
            .unwrap_or_else(|| panic!("no row {detail}: {rows:?}"))
    };
    assert_eq!(find(&format!("{CALL} ok")).result, AuditResult::Ok);
    assert_eq!(find(&format!("{CALL} ok")).actor, "owner");
    assert_eq!(find(&format!("{CALL} ok")).target, Some(format!("{st}/s1")));
    assert_eq!(find(&format!("{CALL} not_open")).result, AuditResult::Ok);
    assert_eq!(
        find(&format!("{CALL} call_running")).result,
        AuditResult::Failed
    );
    assert_eq!(
        find(&format!("{CALL} unknown_call")).result,
        AuditResult::Refused
    );
    assert_eq!(find("- invalid_params").result, AuditResult::Refused);
    assert!(!format!("{rows:?}").contains("%2F"));
}

#[tokio::test]
async fn release_route_sends_session_and_call_only() {
    let (f, mut station, c, h) = setup().await;
    let st = f.station.0.clone();
    // The release uses the session of the path, which is the call's own.
    let (r, req) = one(
        &c,
        &h,
        &mut station,
        "POST",
        &release_path(&st, "s2", CALL),
        None,
        Outcome::Ok(json!({ "released": true })),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body, json!({ "released": true }));
    assert_eq!(req.method, "session.app_release");
    assert_eq!(req.params, json!({ "session_id": "s2", "call_id": CALL }));
    // A call id that is not a ULID is 400 and never sent.
    for bad in [
        "x",
        "01hzx3k5m7q9r2s4t6v8w0y1z3",
        "01HZX3K5M7Q9R2S4T6V8W0Y1Z",
        "01HZX3K5M7Q9R2S4T6V8W0Y1ZU",
    ] {
        let r = post(&c, &h, &release_path(&st, "s2", bad), json!({})).await;
        expect(&r, 400, "invalid_params");
    }
    quiet(&mut station).await;
    let (r, _) = one(
        &c,
        &h,
        &mut station,
        "POST",
        &release_path(&st, "s1", CALL),
        None,
        wire("unknown_call"),
    )
    .await;
    expect(&r, 404, "unknown_call");
    let r = post(&c, &h, &release_path(&st, "nope", CALL), json!({})).await;
    expect(&r, 404, "unknown_session");
    quiet(&mut station).await;
}

#[tokio::test]
async fn call_route_rate_limit_429() {
    let (f, mut station, c, h) = setup().await;
    let st = f.station.0.clone();
    let path = call_path(&st, "s1", APP);
    for n in 0..60 {
        let (r, _) = one(
            &c,
            &h,
            &mut station,
            "POST",
            &path,
            Some(tools_call("bump", true)),
            done(),
        )
        .await;
        assert_eq!(r.status, 200, "call {n}: {}", r.body);
    }
    let r = post(&c, &h, &path, tools_call("bump", true)).await;
    expect(&r, 429, "busy");
    quiet(&mut station).await;
    // The refused call has its row.
    let busy = rows(&f, "session.app_call")
        .await
        .into_iter()
        .filter(|r| r.detail.as_deref() == Some(&format!("{APP}/bump busy")))
        .count();
    assert_eq!(busy, 1);
    // A minute later the login may call again.
    f.det.advance(Duration::from_secs(61));
    let (r, _) = one(
        &c,
        &h,
        &mut station,
        "POST",
        &path,
        Some(tools_call("bump", true)),
        done(),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
}
