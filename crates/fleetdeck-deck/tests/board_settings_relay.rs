//! The credential relay of a settings secret and the restart (DL-172,
//! DL-173): a secret crosses the deck once and is stored nowhere on it; a
//! restart needs an elevation and never runs a real exec in a test.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
mod settings_support;

use std::sync::Arc;
use std::time::Duration;

use common::board::{Extras, board_with_extras};
use common::*;
use fleetdeck_deck::store::AuditResult;
use fleetdeck_proto::link::{Outcome, WireError};
use serde_json::{Value, json};
use settings_support::*;

const CANARY: &str = "sk-canary-0123456789-DO-NOT-STORE";
const SECRET_PATH: &str = "/api/secrets/stations/fake/settings";
const KEY: &str = "provider_instance.work.env.ANTHROPIC_API_KEY";

fn secret_body(elevation: Option<&str>) -> Value {
    let mut b = json!({ "key": KEY, "value": CANARY, "revision": "r1" });
    if let Some(e) = elevation {
        b["elevation"] = json!(e);
    }
    b
}

#[tokio::test]
async fn a_secret_crosses_the_deck_once_and_no_deck_table_or_log_holds_it() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("deck.db");
    let mut f = file_fixture(&db).await;
    let mut station = f.connect().await;
    let c = board_with_extras(Arc::clone(&f.ctx), Extras::default()).await;
    set_password(&c).await;
    let h = owner(&c).await;

    // Without an elevation nothing leaves the deck.
    let r = post(&c, &h, SECRET_PATH, secret_body(None)).await;
    expect(&r, 400, "elevation_required");
    assert_eq!(r.body["keys"], json!(["provider_instance"]));
    no_request(&mut station, Duration::from_millis(200)).await;

    // With one, the station gets the value, in `settings.secret_set`.
    let elevation = elevate(&c, &h).await;
    let (r, req) = with_station(
        &mut station,
        post(&c, &h, SECRET_PATH, secret_body(Some(&elevation))),
        Outcome::Ok(saved("r2")),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(req.method, "settings.secret_set");
    assert_eq!(req.params["secret"]["value"], CANARY);
    assert_eq!(req.params["secret"]["key"], KEY);
    assert_eq!(req.params["revision"], "r1");
    assert_eq!(req.params["elevated"], true);
    assert!(!r.text.contains(CANARY), "{}", r.text);

    // A station error is a fixed text: neither the station's message nor the
    // value comes back.
    for (code, status) in [
        ("settings_changed", 409),
        ("secret_store_unavailable", 500),
        ("invalid", 400),
    ] {
        let (r, _) = with_station(
            &mut station,
            post(&c, &h, SECRET_PATH, secret_body(Some(&elevation))),
            Outcome::Err(WireError::new(code, format!("the station says {CANARY}"))),
        )
        .await;
        expect(&r, status, code);
        assert!(!r.text.contains(CANARY), "{}", r.text);
        assert!(!r.text.contains("the station says"), "{}", r.text);
    }
    let (r, _) = with_station(
        &mut station,
        post(&c, &h, SECRET_PATH, secret_body(Some(&elevation))),
        Outcome::Err(WireError::new("odd", CANARY)),
    )
    .await;
    expect(&r, 502, "station_error");
    assert!(!r.text.contains(CANARY), "{}", r.text);

    // A body that is not valid, too large, or has an unknown field.
    let r = c
        .req(
            "POST",
            SECRET_PATH,
            &refs(&h),
            Some(json!({ "key": KEY, "value": 5, "revision": CANARY })),
        )
        .await;
    expect(&r, 400, "invalid_body");
    assert!(!r.text.contains(CANARY), "{}", r.text);
    // A value of the wrong type that holds the canary: serde would quote it.
    let r = c
        .req(
            "POST",
            SECRET_PATH,
            &refs(&h),
            Some(json!({ "key": KEY, "value": [CANARY], "revision": "r1" })),
        )
        .await;
    expect(&r, 400, "invalid_body");
    assert_eq!(
        r.body["message"],
        "the request body is not a valid secret request"
    );
    assert!(!r.text.contains(CANARY), "{}", r.text);
    let r = c
        .req(
            "POST",
            SECRET_PATH,
            &refs(&h),
            Some(json!({ "key": KEY, "value": CANARY, "revision": "r1", CANARY: 1 })),
        )
        .await;
    expect(&r, 400, "invalid_body");
    assert!(!r.text.contains(CANARY), "{}", r.text);
    let big = json!({ "key": KEY, "value": "x".repeat(17 * 1024), "revision": "r1" });
    let r = c.req("POST", SECRET_PATH, &refs(&h), Some(big)).await;
    assert_eq!(r.status, 413, "{}", r.text);
    let r = post(
        &c,
        &h,
        "/api/secrets/stations/nobody/settings",
        secret_body(Some(&elevation)),
    )
    .await;
    expect(&r, 404, "unknown_station");

    // The canary is in no text or blob column of deck.db, in no file of the
    // database, and in no audit row. The elevation is in none either.
    // The scan finds a value that the database does hold.
    assert!(database_holds(&db, "fake"));
    assert!(!database_holds(&db, CANARY));
    assert!(!database_holds(&db, &elevation));
    assert!(!database_holds(&db, PASSWORD));
    let rows = c.store.audit_log(200).await.unwrap();
    let relay: Vec<_> = rows
        .iter()
        .filter(|e| e.action == "settings.station_secret_set")
        .collect();
    assert!(relay.iter().any(|e| e.result == AuditResult::Ok));
    assert!(
        relay
            .iter()
            .all(|e| matches!(e.target.as_deref(), Some("fake" | "nobody")))
    );
    // The detail is the key or a code, never a value.
    let ok = relay.iter().find(|e| e.result == AuditResult::Ok).unwrap();
    assert_eq!(ok.detail.as_deref(), Some(KEY));
    for e in &rows {
        assert!(!format!("{e:?}").contains(CANARY), "{e:?}");
    }
    // No command, outbox item or receipt was made.
    assert!(c.store.commands().await.unwrap().is_empty());
}

#[tokio::test]
async fn a_restart_needs_an_elevation_and_a_deck_restart_runs_after_the_answer() {
    let mut f = fixture().await;
    let mut station = f.connect().await;
    let fake = Arc::new(CountingRestart::default());
    let c = board_with_extras(
        Arc::clone(&f.ctx),
        Extras {
            restart: Some(fake.clone()),
            ..Extras::default()
        },
    )
    .await;
    set_password(&c).await;
    let h = owner(&c).await;

    // Without an elevation: refused, nothing restarts, nothing is sent.
    let r = post(&c, &h, "/api/settings/restart", json!({ "target": "deck" })).await;
    expect(&r, 400, "elevation_required");
    assert_eq!(r.body["keys"], json!(["restart"]));
    let r = post(
        &c,
        &h,
        "/api/settings/restart",
        json!({ "target": "fake", "elevation": "nope" }),
    )
    .await;
    expect(&r, 400, "elevation_required");
    no_request(&mut station, Duration::from_millis(200)).await;
    assert_eq!(fake.count(), 0);

    // A standalone deck: the answer first, then the restart request.
    let elevation = elevate(&c, &h).await;
    let r = post(
        &c,
        &h,
        "/api/settings/restart",
        json!({ "target": "deck", "elevation": elevation }),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body, json!({ "restarting": true, "would_stop": [] }));
    let deadline = tokio::time::Instant::now() + T;
    while fake.count() == 0 {
        assert!(tokio::time::Instant::now() < deadline, "no restart request");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(fake.count(), 1);

    // A station: the list of sessions that a restart stops gives a 409
    // with the list; `stop_direct_sessions` is passed on.
    let would_stop = json!({ "restarting": false, "would_stop": [
        { "kind": "session", "id": "s1", "title": "Fix the build" } ] });
    let body = |stop: bool| json!({ "target": "fake", "stop_direct_sessions": stop, "elevation": elevation });
    let (r, req) = with_station(
        &mut station,
        post(&c, &h, "/api/settings/restart", body(false)),
        Outcome::Ok(would_stop),
    )
    .await;
    expect(&r, 409, "restart_would_stop_sessions");
    assert_eq!(r.body["would_stop"][0]["id"], "s1");
    assert_eq!(r.body["would_stop"][0]["kind"], "session");
    assert_eq!(req.method, "station.restart");
    assert_eq!(req.params["stop_direct_sessions"], false);
    assert_eq!(req.params["elevated"], true);
    let (r, req) = with_station(
        &mut station,
        post(&c, &h, "/api/settings/restart", body(true)),
        Outcome::Ok(json!({ "restarting": true, "would_stop": [] })),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(req.params["stop_direct_sessions"], true);

    // A station without the handler: 501.
    let (r, _) = with_station(
        &mut station,
        post(&c, &h, "/api/settings/restart", body(true)),
        Outcome::Err(WireError::new("unsupported", "not yet")),
    )
    .await;
    expect(&r, 501, "restart_unsupported");
    expect(
        &post(
            &c,
            &h,
            "/api/settings/restart",
            json!({ "target": "nobody", "elevation": elevation }),
        )
        .await,
        404,
        "unknown_station",
    );
    // The deck was asked once only.
    assert_eq!(fake.count(), 1);
    let rows = c.store.audit_log(100).await.unwrap();
    assert!(
        rows.iter()
            .any(|e| e.action == "settings.restart" && e.result == AuditResult::Ok)
    );
}

#[tokio::test]
async fn in_up_mode_a_deck_restart_goes_to_the_local_station() {
    let mut f = fixture().await;
    let mut station = f.connect().await;
    let fake = Arc::new(CountingRestart::default());
    let c = board_with_extras(
        Arc::clone(&f.ctx),
        Extras {
            restart: Some(fake.clone()),
            local_station: Some(f.station.clone()),
            ..Extras::default()
        },
    )
    .await;
    set_password(&c).await;
    let h = owner(&c).await;
    let elevation = elevate(&c, &h).await;
    let (r, req) = with_station(
        &mut station,
        post(
            &c,
            &h,
            "/api/settings/restart",
            json!({ "target": "deck", "stop_direct_sessions": true, "elevation": elevation }),
        ),
        Outcome::Ok(json!({ "restarting": true, "would_stop": [] })),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(req.method, "station.restart");
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(fake.count(), 0, "the station restarts the process");
}

#[tokio::test]
async fn a_deck_without_a_restart_handle_and_without_a_local_station_answers_409() {
    let f = fixture().await;
    let c = board_with_extras(Arc::clone(&f.ctx), Extras::default()).await;
    set_password(&c).await;
    let h = owner(&c).await;
    let elevation = elevate(&c, &h).await;
    let r = post(
        &c,
        &h,
        "/api/settings/restart",
        json!({ "target": "deck", "elevation": elevation }),
    )
    .await;
    expect(&r, 409, "station_not_paired");
}
