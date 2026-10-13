//! The automations routes (R5, DL-168): owner only, validation, create,
//! update (PUT and the POST alias), delete, run now and the run list.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::BTreeSet;

use common::board::{BoardClient, board};
use common::*;
use fleetdeck_deck::store::AuditResult;
use fleetdeck_proto::link::Scope;
use serde_json::{Value, json};

const DAY: i64 = 24 * 60 * 60 * 1000;

fn body(name: &str) -> Value {
    json!({
        "name": name,
        "enabled": true,
        "cron": "@weekdays",
        "timezone": "+02:00",
        "station": "fake",
        "path": "/work/repo",
        "provider_instance": "claude",
        "prompt": "Triage the new issues.",
        "reuse_session": false,
    })
}

async fn owner(c: &BoardClient) -> (String, String) {
    let (cookie, csrf) = c.login().await;
    (cookie, csrf)
}

#[tokio::test]
async fn a_token_even_with_admin_is_refused_everywhere() {
    let f = fixture().await;
    let c = board(std::sync::Arc::clone(&f.ctx)).await;
    let (_, t) = c
        .store
        .create_token(
            "admin".into(),
            BTreeSet::from([Scope::Read, Scope::Drive, Scope::Admin]),
            DAY,
        )
        .await
        .unwrap();
    let auth = format!("Bearer {}", t.expose());
    let h = [("Authorization", auth.as_str())];
    for (method, path) in [
        ("GET", "/api/schedules"),
        ("POST", "/api/schedules"),
        ("PUT", "/api/schedules/x"),
        ("DELETE", "/api/schedules/x"),
        ("POST", "/api/schedules/x/update"),
        ("POST", "/api/schedules/x/delete"),
        ("POST", "/api/schedules/x/run"),
        ("GET", "/api/schedules/x/runs"),
    ] {
        let b = (method != "GET" && method != "DELETE").then(|| body("x"));
        let r = c.req(method, path, &h, b).await;
        assert_eq!((r.status, r.code()), (403, "owner_only"), "{method} {path}");
    }
    // A token without admin is refused by the scope.
    let (_, t) = c
        .store
        .create_token("reader".into(), BTreeSet::from([Scope::Read]), DAY)
        .await
        .unwrap();
    let auth = format!("Bearer {}", t.expose());
    let r = c
        .get("/api/schedules", &[("Authorization", auth.as_str())])
        .await;
    assert_eq!(r.status, 403);
    assert_eq!(c.get("/api/schedules", &[]).await.status, 401);
}

#[tokio::test]
async fn the_owner_creates_lists_updates_and_deletes_a_schedule() {
    let f = fixture().await;
    let c = board(std::sync::Arc::clone(&f.ctx)).await;
    let (cookie, csrf) = owner(&c).await;
    let h = [("Cookie", cookie.as_str()), ("X-CSRF-Token", csrf.as_str())];

    let r = c.post("/api/schedules", &h, body("Daily triage")).await;
    assert_eq!(r.status, 200, "{}", r.text);
    let id = r.body["schedule_id"].as_str().unwrap().to_owned();
    assert_eq!(r.body["name"], "Daily triage");
    assert_eq!(r.body["station_id"], f.station.as_str());
    assert_eq!(r.body["grace_ms"], 3_600_000);
    assert_eq!(r.body["last_session"], Value::Null);
    assert_eq!(
        c.audited("schedules.create", AuditResult::Ok).await.len(),
        1
    );

    let r = c.get("/api/schedules", &h[..1]).await;
    assert_eq!(r.status, 200, "{}", r.text);
    let list = r.body.as_array().unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0]["schedule"]["schedule_id"], id.as_str());
    assert!(list[0]["next_run_at"].as_i64().unwrap() > f.det.now_ms().try_into().unwrap());
    assert_eq!(list[0]["last_run"], Value::Null);

    // PUT replaces every field; a disabled schedule has no next run.
    let mut changed = body("Renamed");
    changed["enabled"] = json!(false);
    changed["precheck_cmd"] = json!("git fetch && test -n \"$(git log -1)\"");
    let r = c
        .req("PUT", &format!("/api/schedules/{id}"), &h, Some(changed))
        .await;
    assert_eq!(r.status, 200, "{}", r.text);
    assert_eq!(r.body["name"], "Renamed");
    assert_eq!(r.body["enabled"], false);
    let r = c.get("/api/schedules", &h[..1]).await;
    assert_eq!(r.body[0]["next_run_at"], Value::Null);

    // The POST alias does the same.
    let r = c
        .post(&format!("/api/schedules/{id}/update"), &h, body("Again"))
        .await;
    assert_eq!(r.status, 200, "{}", r.text);
    assert_eq!(r.body["name"], "Again");
    assert_eq!(r.body["precheck_cmd"], Value::Null);

    let r = c.get(&format!("/api/schedules/{id}/runs"), &h[..1]).await;
    assert_eq!((r.status, r.body.as_array().unwrap().len()), (200, 0));

    let r = c
        .req("DELETE", &format!("/api/schedules/{id}"), &h, None)
        .await;
    assert_eq!(r.status, 204, "{}", r.text);
    let r = c
        .post(&format!("/api/schedules/{id}/delete"), &h, json!({}))
        .await;
    assert_eq!((r.status, r.code()), (404, "unknown_schedule"));
    assert!(
        c.get("/api/schedules", &h[..1])
            .await
            .body
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn a_bad_body_is_refused_with_its_own_code() {
    let f = fixture().await;
    let c = board(std::sync::Arc::clone(&f.ctx)).await;
    let (cookie, csrf) = owner(&c).await;
    let h = [("Cookie", cookie.as_str()), ("X-CSRF-Token", csrf.as_str())];
    let with = |key: &str, value: Value| {
        let mut b = body("x");
        b[key] = value;
        b
    };
    for (b, status, code) in [
        (with("cron", json!("61 * * * *")), 400, "invalid_cron"),
        (with("cron", json!("@yearly")), 400, "invalid_cron"),
        (
            with("timezone", json!("Europe/Paris")),
            400,
            "invalid_timezone",
        ),
        (with("name", json!(" ")), 400, "invalid_schedule"),
        (with("prompt", json!("")), 400, "invalid_schedule"),
        (with("grace_ms", json!(5)), 400, "invalid_schedule"),
        (with("station", json!("nowhere")), 404, "unknown_station"),
        (with("surprise", json!(1)), 400, "invalid_body"),
    ] {
        let r = c.post("/api/schedules", &h, b.clone()).await;
        assert_eq!((r.status, r.code()), (status, code), "{b}: {}", r.text);
    }
    let r = c
        .req("PUT", "/api/schedules/none", &h, Some(body("x")))
        .await;
    assert_eq!((r.status, r.code()), (404, "unknown_schedule"));
    // A cookie login without the CSRF token is refused.
    let r = c
        .post("/api/schedules", &[("Cookie", cookie.as_str())], body("x"))
        .await;
    assert_eq!(r.status, 403);
    assert!(
        c.get("/api/schedules", &h[..1])
            .await
            .body
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn run_now_needs_a_live_link_and_a_known_schedule() {
    let f = fixture().await;
    let c = board(std::sync::Arc::clone(&f.ctx)).await;
    let (cookie, csrf) = owner(&c).await;
    let h = [("Cookie", cookie.as_str()), ("X-CSRF-Token", csrf.as_str())];
    let r = c.post("/api/schedules/none/run", &h, json!({})).await;
    assert_eq!((r.status, r.code()), (404, "unknown_schedule"));
    let r = c.post("/api/schedules", &h, body("x")).await;
    let id = r.body["schedule_id"].as_str().unwrap().to_owned();
    // The fake station has no live link.
    let r = c
        .post(&format!("/api/schedules/{id}/run"), &h, json!({}))
        .await;
    assert_eq!(
        (r.status, r.code()),
        (409, "station_unreachable"),
        "{}",
        r.text
    );
    assert_eq!(
        c.audited("schedules.run", AuditResult::Failed).await.len(),
        1
    );
    let r = c.get(&format!("/api/schedules/{id}/runs"), &h[..1]).await;
    assert!(r.body.as_array().unwrap().is_empty());
}
