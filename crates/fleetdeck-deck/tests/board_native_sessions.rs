//! Browse the native sessions of a station over loopback HTTPS (B7): the
//! owner reads, a token and a member get 403 `owner_only`, the feature check,
//! the checks before the station, the error mapping and the audit rows. The
//! station is a fake that answers the link methods itself.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
mod share_support;

use std::collections::BTreeSet;
use std::time::Duration;

use common::board::{BoardClient, Http};
use common::*;
use fleetdeck_deck::store::AuditResult;
use fleetdeck_link::LiveLink;
use fleetdeck_link::control::InboundRequest;
use fleetdeck_proto::link::{Outcome, Scope, WireError};
use serde_json::{Value, json};
use share_support::*;

const SECRET: &str = "STATION-TEXT-MUST-NOT-LEAK /secret/path";

fn list_json() -> Value {
    json!({
        "homes": [{"provider": "claude", "instances": ["claude"], "state": {"state": "ok"}}],
        "sessions": [{
            "provider": "claude", "native_id": "0b6c1d2e-aaaa-4bbb-8ccc-123456789abc",
            "instances": ["claude"],
            "pin": {"home": "/home/u/.claude", "dev": 7, "ino": 99},
            "cwd": "/home/u/work", "title": "A title", "first_prompt": null,
            "modified_at": 1_700_000_000_000_i64, "size": 2048, "parent_native_id": null,
            "known": null, "import": {"state": "ok"}, "verified": true,
        }],
        "truncated": false, "timed_out": false,
    })
}

fn preview_json() -> Value {
    json!({
        "items": [{"id": "i1", "at": 1000, "kind": "user", "text": "work", "tool": null}],
        "state": {"kind": "complete"},
        "cwd": "/home/u/work", "size": 2048, "modified_at": 1_700_000_000_000_i64,
        "earlier_omitted": false,
    })
}

fn preview_body() -> Value {
    json!({
        "provider_instance": "claude",
        "native_id": "0b6c1d2e-aaaa-4bbb-8ccc-123456789abc",
        "pin": {"home": "/home/u/.claude", "dev": 7, "ino": 99},
    })
}

fn list_path(st: &str) -> String {
    format!("/api/stations/{st}/native-sessions")
}

fn preview_path(st: &str) -> String {
    format!("/api/stations/{st}/native-sessions/preview")
}

/// One GET that the fake station answers with `outcome`.
async fn list_with(
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

/// One POST of the preview that the fake station answers with `outcome`.
async fn preview_with(
    c: &BoardClient,
    h: &Headers,
    station: &mut LiveLink,
    st: &str,
    body: Value,
    outcome: Outcome,
) -> (Http, InboundRequest) {
    let path = preview_path(st);
    let call = post(c, h, &path, body);
    let answer = async {
        let req = next_request(station).await;
        station.control.respond(req.id, outcome).await.unwrap();
        req
    };
    tokio::join!(call, answer)
}

async fn quiet(station: &mut LiveLink) {
    no_request(station, Duration::from_millis(300)).await;
}

#[tokio::test]
async fn the_owner_lists_and_previews_and_the_answer_passes_through() {
    let (f, mut station, c, h) = setup().await;
    let st = f.station.0.clone();

    let (r, req) = list_with(
        &c,
        &h,
        &mut station,
        &list_path(&st),
        Outcome::Ok(list_json()),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body, list_json());
    assert_eq!(req.method, "station.native_sessions");
    assert_eq!(
        req.params,
        json!({ "provider_instance": null, "limit": null })
    );

    let (r, req) = list_with(
        &c,
        &h,
        &mut station,
        &format!("{}?instance=claude-2&limit=200", list_path(&st)),
        Outcome::Ok(list_json()),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(
        req.params,
        json!({ "provider_instance": "claude-2", "limit": 200 })
    );

    let (r, req) = preview_with(
        &c,
        &h,
        &mut station,
        &st,
        preview_body(),
        Outcome::Ok(preview_json()),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body, preview_json());
    assert_eq!(req.method, "station.native_preview");
    assert_eq!(req.params, preview_body());
}

#[tokio::test]
async fn a_token_gets_owner_only_also_with_the_drive_scope() {
    let (f, mut station, c, _h) = setup().await;
    let st = f.station.0.clone();
    let (_, token) = f
        .store
        .create_token(
            "reader".into(),
            BTreeSet::from([Scope::Read, Scope::Drive, Scope::Admin]),
            86_400_000,
        )
        .await
        .unwrap();
    let auth = format!("Bearer {}", token.expose());
    let r = c
        .get(&list_path(&st), &[("Authorization", auth.as_str())])
        .await;
    expect(&r, 403, "owner_only");
    let r = c
        .post(
            &preview_path(&st),
            &[("Authorization", auth.as_str())],
            preview_body(),
        )
        .await;
    expect(&r, 403, "owner_only");
    quiet(&mut station).await;
}

#[tokio::test]
async fn a_member_gets_owner_only() {
    let (f, mut station, c, h) = setup().await;
    let st = f.station.0.clone();
    let share = json!({ "kind": "session", "target": format!("{st}/s1"), "role": "viewer" });
    let (member, _, _) = join(&c, &h, share, "Alice").await;
    let r = get(&c, &member, &list_path(&st)).await;
    expect(&r, 403, "owner_only");
    let r = post(&c, &member, &preview_path(&st), preview_body()).await;
    expect(&r, 403, "owner_only");
    quiet(&mut station).await;
}

#[tokio::test]
async fn an_unknown_station_is_a_404_without_a_forward() {
    let (_f, mut station, c, h) = setup().await;
    let r = get(&c, &h, &list_path("no-such-station")).await;
    expect(&r, 404, "unknown_station");
    let r = post(&c, &h, &preview_path("no-such-station"), preview_body()).await;
    expect(&r, 404, "unknown_station");
    quiet(&mut station).await;
}

#[tokio::test]
async fn a_station_without_the_feature_gets_409_without_a_forward() {
    let (f, mut station, c, h) = setup().await;
    let st = f.station.0.clone();
    f.store.set_link_features(&f.station, 98, vec![]);
    let text = "This station does not list its sessions yet. Update it.";
    let r = get(&c, &h, &list_path(&st)).await;
    expect(&r, 409, "native_sessions_unsupported");
    assert_eq!(r.body["message"], text);
    let r = post(&c, &h, &preview_path(&st), preview_body()).await;
    expect(&r, 409, "native_sessions_unsupported");
    assert_eq!(r.body["message"], text);
    quiet(&mut station).await;
}

#[tokio::test]
async fn bad_values_are_refused_before_the_station() {
    let (f, mut station, c, h) = setup().await;
    let st = f.station.0.clone();
    for query in [
        "limit=0",
        "limit=201",
        "limit=abc",
        "limit=-1",
        "instance=Bad",
        "instance=a%20b",
        "instance=",
        "instance=-x",
    ] {
        let r = get(&c, &h, &format!("{}?{query}", list_path(&st))).await;
        expect(&r, 400, "invalid_params");
    }
    let long = "a".repeat(33);
    let with = |key: &str, value: Value| {
        let mut body = preview_body();
        body[key] = value;
        body
    };
    let with_home = |home: Value| {
        let mut body = preview_body();
        body["pin"]["home"] = home;
        body
    };
    let bad_bodies = [
        with("provider_instance", json!(long)),
        with("provider_instance", json!("UPPER")),
        with("native_id", json!("-r")),
        with("native_id", json!("a/../b")),
        with("native_id", json!("a".repeat(129))),
        with_home(json!("relative/home")),
        with_home(json!("/home/\u{0}x")),
        with_home(json!(format!("/{}", "h".repeat(4096)))),
    ];
    for body in bad_bodies {
        let r = post(&c, &h, &preview_path(&st), body).await;
        expect(&r, 400, "invalid_params");
    }
    // A body that does not parse.
    let r = post(&c, &h, &preview_path(&st), json!({ "native_id": "x" })).await;
    expect(&r, 400, "invalid_body");
    assert_eq!(r.body["message"], "The request body is not valid.");
    let mut extra = preview_body();
    extra["extra"] = json!(1);
    let r = post(&c, &h, &preview_path(&st), extra).await;
    expect(&r, 400, "invalid_body");
    quiet(&mut station).await;
}

#[tokio::test]
async fn station_errors_map_to_fixed_texts() {
    let (f, mut station, c, h) = setup().await;
    let st = f.station.0.clone();
    let cases = [
        (
            "busy",
            503,
            "busy",
            "The station is busy. Try again in a moment.",
        ),
        (
            "invalid_params",
            400,
            "invalid_params",
            "The station refused the request.",
        ),
        (
            "unknown_session",
            502,
            "station_error",
            "The station could not answer.",
        ),
        (
            "boom",
            502,
            "station_error",
            "The station could not answer.",
        ),
    ];
    for (wire, status, code, text) in cases {
        let err = WireError::new(wire, SECRET);
        let (r, _) = list_with(&c, &h, &mut station, &list_path(&st), Outcome::Err(err)).await;
        assert_eq!((r.status, r.code()), (status, code), "{wire}: {}", r.body);
        assert_eq!(r.body["message"], text);
        assert!(!r.text.contains("STATION-TEXT-MUST-NOT-LEAK"), "{}", r.text);

        let err = WireError::new(wire, SECRET);
        let (r, _) =
            preview_with(&c, &h, &mut station, &st, preview_body(), Outcome::Err(err)).await;
        assert_eq!((r.status, r.code()), (status, code), "{wire}: {}", r.body);
        assert_eq!(r.body["message"], text);
        assert!(!r.text.contains("STATION-TEXT-MUST-NOT-LEAK"), "{}", r.text);
    }
    // An answer that is not the expected shape is a 502, never the station's text.
    let bad = Outcome::Ok(json!({ "homes": SECRET }));
    let (r, _) = list_with(&c, &h, &mut station, &list_path(&st), bad).await;
    expect(&r, 502, "station_error");
    assert!(!r.text.contains("STATION-TEXT-MUST-NOT-LEAK"), "{}", r.text);
}

#[tokio::test]
async fn the_audit_has_a_row_for_each_failure_and_none_for_an_owner_read() {
    let (f, mut station, c, h) = setup().await;
    let rows = |f: &Fixture| {
        let store = f.store.clone();
        async move {
            let mut rows = store.audit_log(100).await.unwrap();
            rows.retain(|r| r.action.starts_with("stations.native_"));
            rows
        }
    };
    let (r, _) = list_with(
        &c,
        &h,
        &mut station,
        &list_path("fake"),
        Outcome::Ok(list_json()),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert!(rows(&f).await.is_empty());

    let err = WireError::new("boom", SECRET);
    let (r, _) = list_with(&c, &h, &mut station, &list_path("fake"), Outcome::Err(err)).await;
    expect(&r, 502, "station_error");
    let mut bad = preview_body();
    bad["native_id"] = json!("-r");
    let r = post(&c, &h, &preview_path("fake"), bad).await;
    expect(&r, 400, "invalid_params");
    let r = get(&c, &h, &list_path("no-such-station")).await;
    expect(&r, 404, "unknown_station");

    let all = rows(&f).await;
    assert_eq!(all.len(), 3, "{all:?}");
    let failed = all
        .iter()
        .find(|r| r.action == "stations.native_sessions" && r.result == AuditResult::Failed)
        .unwrap();
    assert_eq!(failed.detail.as_deref(), Some("station_error"));
    assert_eq!(failed.actor, "owner");
    assert_eq!(failed.target.as_deref(), Some("fake"));
    let refused = all
        .iter()
        .find(|r| r.action == "stations.native_preview")
        .unwrap();
    assert_eq!(refused.result, AuditResult::Refused);
    assert_eq!(refused.detail.as_deref(), Some("invalid_params"));
    let missing = all
        .iter()
        .find(|r| r.detail.as_deref() == Some("unknown_station"))
        .unwrap();
    assert_eq!(missing.result, AuditResult::Refused);
    assert!(!format!("{all:?}").contains("STATION-TEXT-MUST-NOT-LEAK"));
}
