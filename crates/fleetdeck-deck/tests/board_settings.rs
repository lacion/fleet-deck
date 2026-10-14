//! The deck's settings routes and the owner's elevation over loopback HTTPS
//! (DL-172; design 1.2, 1.4, 1.5): view, save, the revision conflict, the
//! elevation for danger changes, the owner-only rule, the forward to a
//! station and the audit rows (keys only).

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
mod settings_support;

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use common::board::{Extras, board_with_extras};
use common::*;
use fleetdeck_deck::store::AuditResult;
use fleetdeck_proto::link::{Outcome, Scope, WireError};
use serde_json::{Value, json};
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

#[tokio::test]
async fn the_deck_view_shows_the_values_the_fields_and_the_flag_override() {
    let tmp = tempfile::tempdir().unwrap();
    let (_f, c) = deck_board(tmp.path()).await;
    let h = owner(&c).await;
    let r = get(&c, &h, "/api/settings/deck").await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body["values"]["name"], "alpha");
    assert_eq!(r.body["values"]["conflict_mail"], true);
    assert_eq!(r.body["overrides"], json!(["listen"]));
    let listen = r.body["fields"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["key"] == "listen")
        .unwrap();
    assert_eq!(listen["cli_flag"], "--listen");
    assert_eq!(r.body["revision"].as_str().unwrap().len(), 64);
    // A board without a settings file answers 503.
    let f = fixture().await;
    let bare = common::board::board(Arc::clone(&f.ctx)).await;
    let bh = owner(&bare).await;
    expect(
        &get(&bare, &bh, "/api/settings/deck").await,
        503,
        "settings_unavailable",
    );
}

#[tokio::test]
async fn a_save_applies_live_fields_at_once_and_a_stale_revision_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let (_f, c) = deck_board(tmp.path()).await;
    let h = owner(&c).await;
    let view = get(&c, &h, "/api/settings/deck").await;
    let revision = view.body["revision"].as_str().unwrap().to_owned();
    assert!(c.state.live.get().conflict_mail);

    let r = post(
        &c,
        &h,
        "/api/settings/deck",
        json!({ "changes": { "conflict_mail": false, "name": "beta" }, "revision": revision }),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body["applied"], json!(["name", "conflict_mail"]));
    assert_eq!(r.body["restart_needed"], json!([]));
    assert_eq!(r.body["values"]["name"], "beta");
    // The live fields and the store flag changed in the running deck.
    let live = c.state.live.get();
    assert_eq!((live.name.as_str(), live.conflict_mail), ("beta", false));
    let text = std::fs::read_to_string(tmp.path().join("deck.toml")).unwrap();
    assert!(text.contains("name = \"beta\""), "{text}");

    // The old revision is stale now.
    let stale = post(
        &c,
        &h,
        "/api/settings/deck",
        json!({ "changes": { "name": "gamma" }, "revision": revision }),
    )
    .await;
    expect(&stale, 409, "settings_changed");
    assert_eq!(c.state.live.get().name, "beta");

    // A bad value is a 400 with the key, not the value.
    let new_revision = r.body["revision"].as_str().unwrap();
    let bad = post(
        &c,
        &h,
        "/api/settings/deck",
        json!({ "changes": { "conflict_mail": "SECRET-LOOKING-TEXT" }, "revision": new_revision }),
    )
    .await;
    expect(&bad, 400, "invalid");
    assert!(!bad.text.contains("SECRET-LOOKING-TEXT"), "{}", bad.text);
    expect(
        &post(&c, &h, "/api/settings/deck", json!({ "nope": 1 })).await,
        400,
        "invalid_body",
    );
}

#[tokio::test]
async fn a_danger_change_needs_an_elevation_and_the_audit_row_has_keys_only() {
    let tmp = tempfile::tempdir().unwrap();
    let (_f, c) = deck_board(tmp.path()).await;
    set_password(&c).await;
    let h = owner(&c).await;
    let revision = get(&c, &h, "/api/settings/deck").await.body["revision"]
        .as_str()
        .unwrap()
        .to_owned();
    let change = |elevation: Option<&str>| {
        let mut b = json!({
            "changes": { "link_listen": ["192.168.77.9:7421"] },
            "revision": revision,
        });
        if let Some(e) = elevation {
            b["elevation"] = json!(e);
        }
        b
    };
    let r = post(&c, &h, "/api/settings/deck", change(None)).await;
    expect(&r, 400, "elevation_required");
    assert_eq!(r.body["keys"], json!(["link_listen"]));
    // A token that is not an elevation counts as none.
    let r = post(
        &c,
        &h,
        "/api/settings/deck",
        change(Some("not-an-elevation")),
    )
    .await;
    expect(&r, 400, "elevation_required");
    // The file is unchanged.
    let text = std::fs::read_to_string(tmp.path().join("deck.toml")).unwrap();
    assert!(!text.contains("192.168.77.9"), "{text}");

    let elevation = elevate(&c, &h).await;
    let r = post(&c, &h, "/api/settings/deck", change(Some(&elevation))).await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body["restart_needed"], json!(["link_listen"]));
    let text = std::fs::read_to_string(tmp.path().join("deck.toml")).unwrap();
    assert!(text.contains("192.168.77.9:7421"), "{text}");

    // The audit rows hold the key names, never a value or an elevation.
    let rows = c.store.audit_log(100).await.unwrap();
    let saves: Vec<_> = rows
        .iter()
        .filter(|e| e.action == "settings.deck_set")
        .collect();
    assert!(saves.iter().any(|e| e.result == AuditResult::Ok));
    assert!(saves.iter().any(|e| e.result == AuditResult::Failed));
    for e in &rows {
        let text = format!("{e:?}");
        assert!(!text.contains("192.168.77.9"), "{text}");
        assert!(!text.contains(&elevation), "{text}");
        assert!(!text.contains(PASSWORD), "{text}");
    }
    let ok = saves.iter().find(|e| e.result == AuditResult::Ok).unwrap();
    assert_eq!(ok.detail.as_deref(), Some("link_listen"));
}

const OWNER_ONLY: [(&str, &str); 7] = [
    ("GET", "/api/settings/deck"),
    ("POST", "/api/settings/deck"),
    ("GET", "/api/stations/fake/settings"),
    ("POST", "/api/stations/fake/settings"),
    ("POST", "/api/secrets/stations/fake/settings"),
    ("POST", "/api/settings/restart"),
    ("POST", "/api/auth/elevate"),
];

#[tokio::test]
async fn a_token_with_admin_is_refused_on_every_settings_route() {
    let tmp = tempfile::tempdir().unwrap();
    let (_f, c) = deck_board(tmp.path()).await;
    let (_, t) = c
        .store
        .create_token(
            "ops".into(),
            BTreeSet::from([Scope::Read, Scope::Drive, Scope::Admin]),
            86_400_000,
        )
        .await
        .unwrap();
    let auth = format!("Bearer {}", t.expose());
    let h = [("Authorization", auth.as_str())];
    for (method, path) in OWNER_ONLY {
        let body = (method == "POST").then(|| json!({}));
        let r = c.req(method, path, &h, body).await;
        assert_eq!((r.status, r.code()), (403, "owner_only"), "{method} {path}");
    }
    // The refusals are audited, and nothing changed.
    let text = std::fs::read_to_string(tmp.path().join("deck.toml")).unwrap();
    assert_eq!(text, DECK_TOML);
    assert!(
        c.audited("settings.deck_set", AuditResult::Refused)
            .await
            .contains(&Some("owner_only".to_owned()))
    );
    // A request without a login is a 401.
    for (method, path) in OWNER_ONLY {
        let r = c.req(method, path, &[], Some(json!({}))).await;
        assert_eq!(r.status, 401, "{method} {path}");
    }
}

#[tokio::test]
async fn an_elevation_by_password_or_by_code_has_the_documented_limits() {
    let tmp = tempfile::tempdir().unwrap();
    let (f, c) = deck_board(tmp.path()).await;
    set_password(&c).await;
    let h = owner(&c).await;
    let ask = |body: Value| post(&c, &h, "/api/auth/elevate", body);

    // Both fields, or none, is a bad request that the limiter does not count.
    expect(
        &ask(json!({ "password": PASSWORD, "code": "AAAAA-AAAAA" })).await,
        400,
        "invalid_request",
    );
    expect(&ask(json!({})).await, 400, "invalid_request");

    // A code is single use, also with the case and the dash changed.
    let (code, _) = c.state.elevations.new_code(c.store.now_ms()).unwrap();
    let typed = code.expose().to_lowercase().replace('-', "");
    let r = ask(json!({ "code": typed })).await;
    assert_eq!(r.status, 200, "{}", r.body);
    let first = r.body["elevation"].as_str().unwrap().to_owned();
    assert_eq!(
        r.body["expires_at"].as_i64().unwrap(),
        c.store.now_ms() + 10 * 60 * 1000
    );
    expect(
        &ask(json!({ "code": code.expose() })).await,
        401,
        "invalid_credentials",
    );

    // An elevation of another login does not work.
    let other = owner(&c).await;
    let revision = get(&c, &h, "/api/settings/deck").await.body["revision"]
        .as_str()
        .unwrap()
        .to_owned();
    let danger = |e: &str| {
        json!({
            "changes": { "link_listen": ["192.168.77.9:7421"] },
            "revision": revision,
            "elevation": e,
        })
    };
    let r = post(&c, &other, "/api/settings/deck", danger(&first)).await;
    expect(&r, 400, "elevation_required");

    // An elevation expires after ten minutes.
    f.det.advance(Duration::from_secs(10 * 60 + 1));
    let r = post(&c, &h, "/api/settings/deck", danger(&first)).await;
    expect(&r, 400, "elevation_required");
    let fresh = elevate(&c, &h).await;
    let r = post(&c, &h, "/api/settings/deck", danger(&fresh)).await;
    assert_eq!(r.status, 200, "{}", r.body);

    // A code expires after five minutes.
    let (old, _) = c.state.elevations.new_code(c.store.now_ms()).unwrap();
    f.det.advance(Duration::from_secs(5 * 60));
    expect(
        &ask(json!({ "code": old.expose() })).await,
        401,
        "invalid_credentials",
    );

    // Failures count in the limiter of the elevation: the fifth locks.
    for _ in 0..3 {
        expect(
            &ask(json!({ "password": "wrong" })).await,
            401,
            "invalid_credentials",
        );
    }
    expect(
        &ask(json!({ "password": "wrong" })).await,
        401,
        "invalid_credentials",
    );
    expect(
        &ask(json!({ "password": PASSWORD })).await,
        429,
        "too_many_attempts",
    );
    // The login has its own limiter: failed elevations do not lock it (NV-13).
    let login = c
        .post("/api/auth/login", &[], json!({ "password": PASSWORD }))
        .await;
    assert_eq!(login.status, 200, "{}", login.body);
    let refused = c.audited("auth.elevate", AuditResult::Refused).await;
    assert!(refused.contains(&Some("invalid_credentials".to_owned())));
    assert!(refused.contains(&Some("too_many_attempts".to_owned())));
}

#[tokio::test]
async fn failed_anonymous_logins_do_not_lock_the_owner_elevation() {
    let tmp = tempfile::tempdir().unwrap();
    let (_f, c) = deck_board(tmp.path()).await;
    set_password(&c).await;
    let h = owner(&c).await;
    // 50 failed password logins from 10 sources fill the global window of
    // the login flow (NV-13).
    let now = c.store.now_ms();
    for source in 1..=10u8 {
        let ip = std::net::IpAddr::V4(std::net::Ipv4Addr::new(10, 0, 0, source));
        for _ in 0..5 {
            c.state.limiter.password.begin(ip, now).unwrap().failed(now);
        }
    }
    let login = c
        .post("/api/auth/login", &[], json!({ "password": PASSWORD }))
        .await;
    expect(&login, 429, "too_many_attempts");
    // The owner still elevates.
    let elevation = elevate(&c, &h).await;
    assert!(!elevation.is_empty());
    // Five failed elevations from one source lock the sixth.
    for _ in 0..5 {
        expect(
            &post(&c, &h, "/api/auth/elevate", json!({ "password": "wrong" })).await,
            401,
            "invalid_credentials",
        );
    }
    expect(
        &post(&c, &h, "/api/auth/elevate", json!({ "password": PASSWORD })).await,
        429,
        "too_many_attempts",
    );
}

#[tokio::test]
async fn a_station_save_is_forwarded_with_the_elevation_decision() {
    let mut f = fixture().await;
    let mut station = f.connect().await;
    let c = board_with_extras(Arc::clone(&f.ctx), Extras::default()).await;
    set_password(&c).await;
    let h = owner(&c).await;

    // The view.
    let view =
        json!({ "values": { "max_sessions": 4 }, "fields": [], "revision": "r1", "overrides": [] });
    let (r, req) = with_station(
        &mut station,
        get(&c, &h, "/api/stations/fake/settings"),
        Outcome::Ok(view),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(req.method, "settings.get");
    assert_eq!(r.body["values"]["max_sessions"], 4);

    // A save without an elevation: the station gets `elevated: false` and
    // no token; its `elevation_required` answer keeps the keys.
    let body = json!({ "changes": { "allow_shell": true }, "revision": "r1" });
    let mut refusal = WireError::new("elevation_required", "a recent sign-in is needed");
    refusal.data = Some(json!({ "keys": ["allow_shell"] }));
    let (r, req) = with_station(
        &mut station,
        post(&c, &h, "/api/stations/fake/settings", body.clone()),
        Outcome::Err(refusal),
    )
    .await;
    expect(&r, 400, "elevation_required");
    assert_eq!(r.body["keys"], json!(["allow_shell"]));
    assert_eq!(req.method, "settings.set");
    assert_eq!(req.params["elevated"], false);
    assert_eq!(req.params["change"]["revision"], "r1");
    assert_eq!(req.params["change"]["changes"]["allow_shell"], true);
    assert_eq!(req.params["change"]["elevation"], Value::Null);

    // With an elevation: `elevated: true`, and the token is not sent on.
    let elevation = elevate(&c, &h).await;
    let mut with = body.clone();
    with["elevation"] = json!(elevation);
    let (r, req) = with_station(
        &mut station,
        post(&c, &h, "/api/stations/fake/settings", with),
        Outcome::Ok(saved("r2")),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body["revision"], "r2");
    assert_eq!(req.params["elevated"], true);
    assert_eq!(req.params["change"]["elevation"], Value::Null);
    assert!(!req.params.to_string().contains(&elevation));

    // The station's save codes map to statuses.
    for (code, status) in [
        ("settings_changed", 409),
        ("settings_changed_on_disk", 409),
        ("invalid", 400),
        ("cannot_apply", 400),
        ("secret_via_relay", 400),
        ("settings_write_failed", 500),
    ] {
        let (r, _) = with_station(
            &mut station,
            post(&c, &h, "/api/stations/fake/settings", body.clone()),
            Outcome::Err(WireError::new(code, "`key` has a problem")),
        )
        .await;
        expect(&r, status, code);
    }
    let (r, _) = with_station(
        &mut station,
        post(&c, &h, "/api/stations/fake/settings", body.clone()),
        Outcome::Err(WireError::new("something_else", "x")),
    )
    .await;
    expect(&r, 502, "station_error");

    // An unknown station.
    expect(
        &get(&c, &h, "/api/stations/nobody/settings").await,
        404,
        "unknown_station",
    );

    // The audit rows hold keys only.
    let rows = c.store.audit_log(100).await.unwrap();
    let ok = rows
        .iter()
        .find(|e| e.action == "settings.station_set" && e.result == AuditResult::Ok)
        .unwrap();
    assert_eq!(ok.detail.as_deref(), Some("allow_shell"));
    assert_eq!(ok.target.as_deref(), Some("fake"));
    for e in &rows {
        assert!(!format!("{e:?}").contains(&elevation));
    }
}

fn name_change(name: &str, revision: &str) -> fleetdeck_proto::settings::SettingsChange {
    serde_json::from_value(json!({ "changes": { "name": name }, "revision": revision })).unwrap()
}

/// PRE-PR r1 finding 5: a save waits for the gate, and the saves of one deck
/// publish in the order of their commits.
#[tokio::test]
async fn a_save_waits_for_the_gate_and_the_live_name_follows_the_last_commit() {
    use fleetdeck_deck::http::settings_routes::{deck_view, save_and_publish};

    let tmp = tempfile::tempdir().unwrap();
    let (_f, c) = deck_board(tmp.path()).await;
    let settings = c.state.settings.clone().unwrap();
    let revision = deck_view(&settings).await.unwrap().revision;

    // A held gate holds the save before it writes the file.
    let held = settings.gate.lock().await;
    let save = tokio::spawn({
        let (settings, store, live) = (settings.clone(), c.store.clone(), c.state.live.clone());
        let change = name_change("held", &revision);
        async move { save_and_publish(&settings, &store, &live, change, false).await }
    });
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(!save.is_finished());
    let text = std::fs::read_to_string(tmp.path().join("deck.toml")).unwrap();
    assert!(text.contains("name = \"alpha\""), "{text}");
    drop(held);
    save.await.unwrap().unwrap();
    assert_eq!(c.state.live.get().name, "held");

    // Concurrent saves, each retried on a stale revision: the live name
    // equals the name in the file at the end.
    let mut tasks = Vec::new();
    for i in 0..8 {
        let (settings, store, live) = (settings.clone(), c.store.clone(), c.state.live.clone());
        tasks.push(tokio::spawn(async move {
            loop {
                let revision = deck_view(&settings).await.unwrap().revision;
                let change = name_change(&format!("n{i}"), &revision);
                match save_and_publish(&settings, &store, &live, change, false).await {
                    Ok(_) => return,
                    Err(fleetdeck_core::settings::SaveError::SettingsChanged) => {}
                    Err(e) => panic!("{e}"),
                }
            }
        }));
    }
    for t in tasks {
        t.await.unwrap();
    }
    let text = std::fs::read_to_string(tmp.path().join("deck.toml")).unwrap();
    let live = c.state.live.get();
    assert!(
        text.contains(&format!("name = \"{}\"", live.name)),
        "{} in {text}",
        live.name
    );
}
