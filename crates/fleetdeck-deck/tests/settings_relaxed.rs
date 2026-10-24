//! The second audit row of a settings save that relaxed a control (B27
//! §3.6): the row `settings.relaxed` holds the keys, never the values, and
//! only a save that relaxed something writes it.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
mod settings_support;

use std::sync::Arc;

use common::board::{Extras, board_with_extras};
use common::*;
use fleetdeck_deck::store::AuditResult;
use fleetdeck_proto::link::Outcome;
use serde_json::json;
use settings_support::*;

const DECK_TOML: &str = "name = \"alpha\"\nlisten = [\"127.0.0.1:7420\"]\n";

async fn deck_board(dir: &std::path::Path) -> (Fixture, common::board::BoardClient) {
    let f = fixture().await;
    let c = board_with_extras(
        Arc::clone(&f.ctx),
        Extras {
            settings: Some(deck_settings(dir, DECK_TOML)),
            ..Extras::default()
        },
    )
    .await;
    (f, c)
}

async fn revision(c: &common::board::BoardClient, h: &Headers) -> String {
    get(c, h, "/api/settings/deck").await.body["revision"]
        .as_str()
        .unwrap()
        .to_owned()
}

#[tokio::test]
async fn an_elevated_deck_save_that_relaxes_writes_the_relaxed_row() {
    let tmp = tempfile::tempdir().unwrap();
    let (_f, c) = deck_board(tmp.path()).await;
    set_password(&c).await;
    let h = owner(&c).await;
    let elevation = elevate(&c, &h).await;
    let r = post(
        &c,
        &h,
        "/api/settings/deck",
        json!({
            "changes": { "worker_grant.allow_bypass": true },
            "revision": revision(&c, &h).await,
            "elevation": elevation,
        }),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body["relaxed"], json!(["worker_grant.allow_bypass"]));
    let rows = c.store.audit_log(100).await.unwrap();
    let save = rows
        .iter()
        .find(|e| e.action == "settings.deck_set" && e.result == AuditResult::Ok)
        .unwrap();
    assert_eq!(save.detail.as_deref(), Some("worker_grant.allow_bypass"));
    let relaxed = rows
        .iter()
        .find(|e| e.action == "settings.relaxed")
        .unwrap();
    assert_eq!(relaxed.detail.as_deref(), Some("worker_grant.allow_bypass"));
    assert_eq!(relaxed.target.as_deref(), Some("deck"));
    assert_eq!(relaxed.actor, "owner");
    // The view shows the control as relaxed.
    let view = get(&c, &h, "/api/settings/deck").await;
    let items = view.body["relaxed"].as_array().unwrap();
    assert!(
        items
            .iter()
            .any(|i| i["key"] == "worker_grant.allow_bypass" && i["state"] == "active"),
        "{items:?}"
    );
}

#[tokio::test]
async fn a_deck_save_that_only_lowers_a_value_writes_no_relaxed_row() {
    let tmp = tempfile::tempdir().unwrap();
    let (_f, c) = deck_board(tmp.path()).await;
    set_password(&c).await;
    let h = owner(&c).await;
    let r = post(
        &c,
        &h,
        "/api/settings/deck",
        json!({
            "changes": { "security.elevation_minutes": 5 },
            "revision": revision(&c, &h).await,
        }),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert!(r.body.get("relaxed").is_none(), "{}", r.body);
    let rows = c.store.audit_log(100).await.unwrap();
    assert!(rows.iter().any(|e| e.action == "settings.deck_set"));
    assert!(rows.iter().all(|e| e.action != "settings.relaxed"));
}

#[tokio::test]
async fn a_station_save_that_relaxes_writes_the_relaxed_row_with_the_station_name() {
    let mut f = fixture().await;
    let mut station = f.connect().await;
    let c = board_with_extras(Arc::clone(&f.ctx), Extras::default()).await;
    set_password(&c).await;
    let h = owner(&c).await;
    let elevation = elevate(&c, &h).await;
    let answer = json!({
        "values": {},
        "applied": ["allow_shell"],
        "restart_needed": [],
        "unapplied": [],
        "revision": "r2",
        "durability": "confirmed",
        "relaxed": ["allow_shell"],
    });
    let (r, _) = with_station(
        &mut station,
        post(
            &c,
            &h,
            "/api/stations/fake/settings",
            json!({
                "changes": { "allow_shell": true },
                "revision": "r1",
                "elevation": elevation,
            }),
        ),
        Outcome::Ok(answer),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    let rows = c.store.audit_log(100).await.unwrap();
    let relaxed = rows
        .iter()
        .find(|e| e.action == "settings.relaxed")
        .unwrap();
    assert_eq!(relaxed.detail.as_deref(), Some("allow_shell"));
    assert_eq!(relaxed.target.as_deref(), Some("fake"));
    // A station that does not report `relaxed` (an older station) gets no row.
    let before = rows
        .iter()
        .filter(|e| e.action == "settings.relaxed")
        .count();
    let (r, _) = with_station(
        &mut station,
        post(
            &c,
            &h,
            "/api/stations/fake/settings",
            json!({ "changes": { "allow_shell": false }, "revision": "r2" }),
        ),
        Outcome::Ok(saved("r3")),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    let rows = c.store.audit_log(100).await.unwrap();
    assert_eq!(
        rows.iter()
            .filter(|e| e.action == "settings.relaxed")
            .count(),
        before
    );
}
