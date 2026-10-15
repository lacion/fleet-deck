//! The earlier history of a session over loopback HTTPS (DL-189): the deck
//! route, the per-share `history` option, the owner's elevation, the link
//! feature check, the error mapping and the audit rows. The station is a
//! fake that answers `session.history` itself.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
mod settings_support;
mod share_support;

use std::collections::BTreeSet;
use std::time::Duration;

use common::board::{BoardClient, Http};
use common::*;
use fleetdeck_deck::store::AuditResult;
use fleetdeck_link::LiveLink;
use fleetdeck_link::control::InboundRequest;
use fleetdeck_proto::history::{
    HistoryItem, HistoryKind, HistoryState, HistoryTool, SessionHistory, ToolResult,
};
use fleetdeck_proto::link::{Outcome, Scope, WireError};
use serde_json::{Value, json};
use settings_support::{elevate, set_password};
use share_support::*;

/// The page that the fake station answers: a prompt and a tool line with an
/// empty summary (what the station sends for a member).
fn page() -> SessionHistory {
    SessionHistory {
        items: vec![
            HistoryItem {
                id: "i1".to_owned(),
                at: 1_000,
                kind: HistoryKind::User,
                text: "fix the build".to_owned(),
                tool: None,
            },
            HistoryItem {
                id: "i2".to_owned(),
                at: 2_000,
                kind: HistoryKind::Tool,
                text: String::new(),
                tool: Some(HistoryTool {
                    name: "Bash".to_owned(),
                    summary: String::new(),
                    status: ToolResult::Ok,
                }),
            },
        ],
        before: Some("abcd:1".to_owned()),
        state: HistoryState::Complete,
    }
}

fn page_json() -> Value {
    serde_json::to_value(page()).unwrap()
}

/// A deck whose owner has a password: the owner can elevate.
async fn setup_elevating() -> (Fixture, LiveLink, BoardClient, Headers) {
    let (f, station, c, _) = setup().await;
    set_password(&c).await;
    let h = owner(&c).await;
    (f, station, c, h)
}

/// One GET that the fake station answers with `outcome`. Returns the HTTP
/// answer and the request that the station got.
async fn read_with(
    c: &BoardClient,
    h: &Headers,
    station: &mut LiveLink,
    path: &str,
    outcome: Outcome,
) -> (Http, InboundRequest) {
    let call = get(c, h, path);
    let answer = async {
        let req = next_request(station).await;
        station.control.respond(req.id, outcome).await.unwrap();
        req
    };
    tokio::join!(call, answer)
}

fn path(st: &str, session: &str) -> String {
    format!("/api/sessions/{st}/{session}/history")
}

async fn quiet(station: &mut LiveLink) {
    no_request(station, Duration::from_millis(300)).await;
}

/// A viewer of `s1`; `history` is the option of the share.
async fn viewer(
    c: &BoardClient,
    h: &Headers,
    st: &str,
    name: &str,
    history: Option<&str>,
) -> (Headers, String) {
    let mut share = json!({ "kind": "session", "target": format!("{st}/s1"), "role": "viewer" });
    if let Some(elevation) = history {
        share["history"] = json!(true);
        share["elevation"] = json!(elevation);
    }
    let (m, share_id, _) = join(c, h, share, name).await;
    (m, share_id)
}

#[tokio::test]
async fn the_owner_reads_a_page_and_the_station_gets_member_false() {
    let (f, mut station, c, h) = setup().await;
    let st = f.station.0.clone();
    let uri = format!("{}?before=abcd:2&limit=7", path(&st, "s1"));
    let (r, req) = read_with(&c, &h, &mut station, &uri, Outcome::Ok(page_json())).await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body, page_json());
    assert_eq!(req.method, "session.history");
    assert_eq!(
        req.params,
        json!({ "session_id": "s1", "before": "abcd:2", "limit": 7, "member": false })
    );
    // No query: the cursor and the limit are absent.
    let (r, req) = read_with(
        &c,
        &h,
        &mut station,
        &path(&st, "s2"),
        Outcome::Ok(page_json()),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(req.params["session_id"], "s2");
    assert_eq!(req.params["before"], Value::Null);
    assert_eq!(req.params["limit"], Value::Null);
}

#[tokio::test]
async fn a_member_reads_only_through_its_own_share_with_history_on() {
    let (f, mut station, c, h) = setup_elevating().await;
    let st = f.station.0.clone();
    let (alice, _) = viewer(&c, &h, &st, "Alice", None).await;
    let e = elevate(&c, &h).await;
    let (bob, _) = viewer(&c, &h, &st, "Bob", Some(&e)).await;
    // Both shares cover `s1`, but only the share of Bob has the option.
    let r = get(&c, &alice, &path(&st, "s1")).await;
    expect(&r, 403, "history_not_shared");
    quiet(&mut station).await;
    let (r, req) = read_with(
        &c,
        &bob,
        &mut station,
        &path(&st, "s1"),
        Outcome::Ok(page_json()),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(req.method, "session.history");
    assert_eq!(req.params["member"], true);
    // The deck passes the station answer as it is: the summary stays empty.
    assert_eq!(r.body, page_json());
    assert_eq!(r.body["items"][1]["tool"]["summary"], "");
}

#[tokio::test]
async fn a_token_is_refused_with_owner_only() {
    let (f, mut station, c, _h) = setup().await;
    let st = f.station.0.clone();
    let (_, token) = f
        .store
        .create_token("reader".into(), BTreeSet::from([Scope::Read]), 86_400_000)
        .await
        .unwrap();
    let auth = format!("Bearer {}", token.expose());
    let r = c
        .get(&path(&st, "s1"), &[("Authorization", auth.as_str())])
        .await;
    expect(&r, 403, "owner_only");
    quiet(&mut station).await;
}

#[tokio::test]
async fn a_member_outside_the_share_gets_unknown_session() {
    let (f, mut station, c, h) = setup_elevating().await;
    let st = f.station.0.clone();
    let e = elevate(&c, &h).await;
    let (bob, _) = viewer(&c, &h, &st, "Bob", Some(&e)).await;
    let r = get(&c, &bob, &path(&st, "s2")).await;
    expect(&r, 404, "unknown_session");
    quiet(&mut station).await;
}

#[tokio::test]
async fn turning_history_off_or_revoking_the_share_stops_the_next_read() {
    let (f, mut station, c, h) = setup_elevating().await;
    let st = f.station.0.clone();
    let e = elevate(&c, &h).await;
    let (bob, share_id) = viewer(&c, &h, &st, "Bob", Some(&e)).await;
    let uri = path(&st, "s1");
    let (r, _) = read_with(&c, &bob, &mut station, &uri, Outcome::Ok(page_json())).await;
    assert_eq!(r.status, 200, "{}", r.body);

    let route = format!("/api/shares/{share_id}/history");
    let r = post(&c, &h, &route, json!({ "enabled": false })).await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body["history"], false);
    let r = get(&c, &bob, &uri).await;
    expect(&r, 403, "history_not_shared");
    quiet(&mut station).await;

    let e = elevate(&c, &h).await;
    let r = post(&c, &h, &route, json!({ "enabled": true, "elevation": e })).await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body["history"], true);
    let (r, _) = read_with(&c, &bob, &mut station, &uri, Outcome::Ok(page_json())).await;
    assert_eq!(r.status, 200, "{}", r.body);

    let r = post(&c, &h, &format!("/api/shares/{share_id}/revoke"), json!({})).await;
    assert_eq!(r.status, 200, "{}", r.body);
    let r = get(&c, &bob, &uri).await;
    // The guard ends the login of a member when its share is revoked.
    assert_eq!(r.status, 401, "{}", r.body);
    quiet(&mut station).await;
}

#[tokio::test]
async fn only_the_owner_sets_history_and_turning_it_on_needs_elevation() {
    let (f, mut station, c, h) = setup_elevating().await;
    let st = f.station.0.clone();
    let (alice, share_id) = viewer(&c, &h, &st, "Alice", None).await;
    let route = format!("/api/shares/{share_id}/history");

    let r = post(&c, &alice, &route, json!({ "enabled": true })).await;
    expect(&r, 403, "owner_only");
    let (_, token) = f
        .store
        .create_token("admin".into(), BTreeSet::from([Scope::Admin]), 86_400_000)
        .await
        .unwrap();
    let auth = format!("Bearer {}", token.expose());
    let r = c
        .post(
            &route,
            &[("Authorization", auth.as_str())],
            json!({ "enabled": true }),
        )
        .await;
    expect(&r, 403, "owner_only");

    let r = post(&c, &h, &route, json!({ "enabled": true })).await;
    expect(&r, 400, "elevation_required");
    assert_eq!(r.body["keys"], json!(["history"]));
    let r = post(
        &c,
        &h,
        &route,
        json!({ "enabled": true, "elevation": "not-an-elevation" }),
    )
    .await;
    expect(&r, 400, "elevation_required");
    let r = post(&c, &h, &route, json!({ "enabled": false })).await;
    assert_eq!(r.status, 200, "{}", r.body);
    let r = post(&c, &h, &route, json!({ "enabled": "yes" })).await;
    expect(&r, 400, "invalid_body");

    // A share that is made with the option needs the elevation too.
    let share = json!({ "kind": "session", "target": format!("{st}/s1"),
                        "role": "viewer", "history": true });
    let r = post(&c, &h, "/api/shares", share).await;
    expect(&r, 400, "elevation_required");
    assert_eq!(r.body["keys"], json!(["history"]));

    let r = post(
        &c,
        &h,
        "/api/shares/nope/history",
        json!({ "enabled": false }),
    )
    .await;
    expect(&r, 404, "unknown_share");

    // The share list shows the option of each share.
    let e = elevate(&c, &h).await;
    let (_, on_id) = viewer(&c, &h, &st, "Bob", Some(&e)).await;
    let list = get(&c, &h, "/api/shares").await;
    assert_eq!(list.status, 200, "{}", list.body);
    for share in list.body.as_array().unwrap() {
        let want = share["share_id"] == on_id.as_str();
        assert_eq!(share["history"], want, "{share}");
    }
    assert_eq!(list.body.as_array().unwrap().len(), 2);
    quiet(&mut station).await;
}

#[tokio::test]
async fn a_station_without_history_v1_gets_409_without_a_forward() {
    let (f, mut station, c, h) = setup().await;
    let st = f.station.0.clone();
    // A second live link that did not negotiate the feature.
    f.store.set_link_features(&f.station, 98, vec![]);
    let r = get(&c, &h, &path(&st, "s1")).await;
    expect(&r, 409, "history_unsupported");
    assert_eq!(
        r.body["message"],
        "This station runs an older Fleet Deck. On that computer, install the new version, then run `fleetdeck service install`."
    );
    quiet(&mut station).await;
}

#[tokio::test]
async fn station_errors_map_to_fixed_codes() {
    let (f, mut station, c, h) = setup().await;
    let st = f.station.0.clone();
    let cases = [
        ("unknown_session", 404, "unknown_session"),
        ("invalid_params", 400, "invalid_params"),
        ("stale_cursor", 409, "stale_cursor"),
        ("busy", 503, "busy"),
        ("boom", 502, "station_error"),
    ];
    for (wire, status, code) in cases {
        let err = WireError::new(wire, "STATION-TEXT-MUST-NOT-LEAK /secret/path");
        let (r, _) = read_with(&c, &h, &mut station, &path(&st, "s1"), Outcome::Err(err)).await;
        assert_eq!((r.status, r.code()), (status, code), "{wire}: {}", r.body);
        assert!(!r.text.contains("STATION-TEXT-MUST-NOT-LEAK"), "{}", r.text);
    }
    // An answer that is not a history is a 502, never the station's text.
    let bad = Outcome::Ok(json!({ "items": "STATION-TEXT-MUST-NOT-LEAK" }));
    let (r, _) = read_with(&c, &h, &mut station, &path(&st, "s1"), bad).await;
    expect(&r, 502, "station_error");
    assert!(!r.text.contains("STATION-TEXT-MUST-NOT-LEAK"), "{}", r.text);
}

#[tokio::test]
async fn bad_queries_are_refused_before_the_station() {
    let (f, mut station, c, h) = setup().await;
    let st = f.station.0.clone();
    let base = path(&st, "s1");
    let long = "a".repeat(65);
    let uris = [
        format!("{base}?limit=0"),
        format!("{base}?limit=101"),
        format!("{base}?limit=abc"),
        format!("{base}?before={long}"),
        format!("{base}?before=a%20b"),
        format!("{base}?extra=1"),
        path(&st, &"s".repeat(129)),
    ];
    for uri in uris {
        let r = get(&c, &h, &uri).await;
        expect(&r, 400, "invalid_params");
    }
    quiet(&mut station).await;
}

#[tokio::test]
async fn the_audit_has_a_row_for_a_member_read_and_for_failures_only() {
    let (f, mut station, c, h) = setup_elevating().await;
    let st = f.station.0.clone();
    let (alice, _) = viewer(&c, &h, &st, "Alice", None).await;
    let e = elevate(&c, &h).await;
    let (bob, bob_share) = viewer(&c, &h, &st, "Bob", Some(&e)).await;
    let rows = |f: &Fixture| {
        let store = f.store.clone();
        async move {
            let mut rows = store.audit_log(100).await.unwrap();
            rows.retain(|r| r.action == "sessions.history");
            rows
        }
    };

    // An owner read writes no row.
    let uri = path("fake", "s1");
    let (r, _) = read_with(&c, &h, &mut station, &uri, Outcome::Ok(page_json())).await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert!(rows(&f).await.is_empty());

    // A member read writes one row with the share.
    let (r, _) = read_with(&c, &bob, &mut station, &uri, Outcome::Ok(page_json())).await;
    assert_eq!(r.status, 200, "{}", r.body);
    let all = rows(&f).await;
    assert_eq!(all.len(), 1, "{all:?}");
    assert_eq!(all[0].actor, "member:Bob");
    assert_eq!(all[0].result, AuditResult::Ok);
    assert_eq!(
        all[0].detail.as_deref(),
        Some(format!("share:{bob_share}").as_str())
    );
    assert_eq!(all[0].target.as_deref(), Some("fake/s1"));

    // A refused read writes a row with the error code.
    let r = get(&c, &alice, &uri).await;
    expect(&r, 403, "history_not_shared");
    let all = rows(&f).await;
    assert_eq!(all.len(), 2, "{all:?}");
    assert_eq!(all[0].actor, "member:Alice");
    assert_eq!(all[0].result, AuditResult::Refused);
    assert_eq!(all[0].detail.as_deref(), Some("history_not_shared"));

    // A failure of the station is a `Failed` row, and the owner's too.
    let err = WireError::new("boom", "STATION-TEXT-MUST-NOT-LEAK");
    let (r, _) = read_with(&c, &h, &mut station, &uri, Outcome::Err(err)).await;
    expect(&r, 502, "station_error");
    let all = rows(&f).await;
    assert_eq!(all.len(), 3, "{all:?}");
    assert_eq!(all[0].actor, "owner");
    assert_eq!(all[0].result, AuditResult::Failed);
    assert_eq!(all[0].detail.as_deref(), Some("station_error"));
    assert!(!format!("{all:?}").contains("STATION-TEXT-MUST-NOT-LEAK"));
}
