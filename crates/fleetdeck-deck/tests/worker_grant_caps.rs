//! MCP P4a deck test 10: the `[worker_grant]` setting and the caps route need
//! the owner's elevation for every widening, and nothing else (design
//! `docs/v2/design/mcp-p4a.md` §2 and §8).

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
mod settings_support;

use std::sync::Arc;

use common::board::{BoardClient, Extras, Http, board, board_with_extras};
use common::fleet::bound_edit;
use common::*;
use fleetdeck_deck::store::{AuditResult, GrantClass};
use fleetdeck_proto::command::{GrantKind, GrantScope};
use serde_json::{Value, json};
use settings_support::*;

const ELEVATION: &str = "x-fleetdeck-elevation";
const DECK_TOML: &str = "name = \"alpha\"\nlisten = [\"127.0.0.1:7420\"]\n";
const CODEX_WRITE: &str = "on-request/workspace-write";

// ---------------------------------------------------------------------------
// The setting
// ---------------------------------------------------------------------------

async fn deck_board(dir: &std::path::Path) -> (Fixture, BoardClient) {
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

/// Saves one key of the deck settings with the current revision.
async fn save(c: &BoardClient, h: &Headers, key: &str, value: Value, elevation: &str) -> Http {
    let revision = get(c, h, "/api/settings/deck").await.body["revision"]
        .as_str()
        .unwrap()
        .to_owned();
    let mut body = json!({"changes": {key: value}, "revision": revision});
    if !elevation.is_empty() {
        body["elevation"] = json!(elevation);
    }
    post(c, h, "/api/settings/deck", body).await
}

async fn stored(c: &BoardClient, h: &Headers, key: &str) -> Value {
    get(c, h, "/api/settings/deck").await.body["values"][key].clone()
}

/// A save without an elevation that must be applied.
async fn narrows(c: &BoardClient, h: &Headers, key: &str, value: Value) {
    let r = save(c, h, key, value.clone(), "").await;
    assert_eq!(r.status, 200, "{key} -> {value}: {}", r.body);
    assert_eq!(stored(c, h, key).await, value, "{key} is saved");
}

/// A save without an elevation that must be refused with exactly that key,
/// and the stored value stays.
async fn widens(c: &BoardClient, h: &Headers, key: &str, value: Value) {
    let before = stored(c, h, key).await;
    let r = save(c, h, key, value.clone(), "").await;
    expect(&r, 400, "elevation_required");
    assert_eq!(r.body["keys"], json!([key]), "{key} -> {value}");
    assert_eq!(stored(c, h, key).await, before, "{key} is unchanged");
}

#[tokio::test]
async fn the_setting_and_the_caps_route_need_elevation_for_every_widening() {
    setting_needs_elevation_for_every_widening().await;
    caps_route_needs_elevation_for_every_widening().await;
}

async fn setting_needs_elevation_for_every_widening() {
    let tmp = tempfile::tempdir().unwrap();
    let (_f, c) = deck_board(tmp.path()).await;
    set_password(&c).await;
    let h = owner(&c).await;
    let defaults = get(&c, &h, "/api/settings/deck").await;
    assert_eq!(defaults.body["values"]["worker_grant.spawn_cap"], 2);

    narrows(&c, &h, "worker_grant.spawn_cap", json!(1)).await;
    widens(&c, &h, "worker_grant.spawn_cap", json!(2)).await;

    narrows(&c, &h, "worker_grant.enabled", json!(false)).await;
    widens(&c, &h, "worker_grant.enabled", json!(true)).await;

    narrows(&c, &h, "worker_grant.stations", json!(["alpha"])).await;
    widens(&c, &h, "worker_grant.stations", json!(["alpha", "beta"])).await;
    widens(&c, &h, "worker_grant.stations", json!(["all"])).await;

    narrows(&c, &h, "worker_grant.lifetime_hours", json!(6)).await;
    widens(&c, &h, "worker_grant.lifetime_hours", json!(7)).await;
    narrows(&c, &h, "worker_grant.idle_minutes", json!(20)).await;
    widens(&c, &h, "worker_grant.idle_minutes", json!(25)).await;

    narrows(&c, &h, "worker_grant.claude_mode", json!("plan")).await;
    widens(&c, &h, "worker_grant.claude_mode", json!("acceptEdits")).await;

    // The Codex ceilings: a lower one is narrowing, an incomparable one is
    // widening in both directions.
    let codex = "worker_grant.codex_mode";
    narrows(&c, &h, codex, json!("on-request/read-only")).await;
    widens(&c, &h, codex, json!("never/read-only")).await;
    let token = elevate(&c, &h).await;
    let r = save(&c, &h, codex, json!("never/read-only"), &token).await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(stored(&c, &h, codex).await, json!("never/read-only"));
    widens(&c, &h, codex, json!(CODEX_WRITE)).await;
}

// ---------------------------------------------------------------------------
// The caps route
// ---------------------------------------------------------------------------

struct Rig {
    f: Fixture,
    c: BoardClient,
    h: Headers,
}

async fn rig() -> Rig {
    let f = fixture().await;
    let c = board(Arc::clone(&f.ctx)).await;
    set_password(&c).await;
    let h = owner(&c).await;
    Rig { f, c, h }
}

impl Rig {
    async fn caps(&self, grant_id: &str, body: Value, elevation: Option<&str>) -> Http {
        let mut headers = refs(&self.h);
        if let Some(token) = elevation {
            headers.push((ELEVATION, token));
        }
        self.c
            .post(
                &format!("/api/coordinator/grants/{grant_id}/caps"),
                &headers,
                body,
            )
            .await
    }

    async fn grant(&self, grant_id: &str) -> fleetdeck_deck::store::Grant {
        self.f
            .store
            .grant(grant_id.to_owned())
            .await
            .unwrap()
            .unwrap()
    }

    /// The details of the successful `coordinator.grant_caps` rows of a
    /// grant, newest first.
    async fn ok_rows(&self, grant_id: &str) -> Vec<Value> {
        self.f
            .store
            .audit_log(200)
            .await
            .unwrap()
            .into_iter()
            .filter(|e| {
                e.action == "coordinator.grant_caps"
                    && e.result == AuditResult::Ok
                    && e.target.as_deref() == Some(grant_id)
            })
            .map(|e| serde_json::from_str(e.detail.as_deref().unwrap()).unwrap())
            .collect()
    }
}

fn modes(claude: Option<&str>, codex: Option<&str>) -> Value {
    let mut m = serde_json::Map::new();
    if let Some(c) = claude {
        m.insert("claude".to_owned(), json!(c));
    }
    if let Some(c) = codex {
        m.insert("codex".to_owned(), json!(c));
    }
    Value::Object(m)
}

fn message(r: &Http, text: &str) {
    assert!(r.body.to_string().contains(text), "{text}: {}", r.body);
}

const SPAWNER: [GrantClass; 4] = [
    GrantClass::Read,
    GrantClass::Mail,
    GrantClass::Drive,
    GrantClass::Spawn,
];

async fn spawner(r: &Rig, session: &str) -> String {
    bound_edit(&r.f, &r.f.station, session, &SPAWNER, |g| {
        g.spawn_cap = 3;
        g.max_modes = [
            ("claude".to_owned(), "acceptEdits".to_owned()),
            ("codex".to_owned(), CODEX_WRITE.to_owned()),
        ]
        .into();
    })
    .await
    .grant_id
}

async fn caps_route_needs_elevation_for_every_widening() {
    let r = rig().await;
    let gid = spawner(&r, "coord-1").await;
    let token = elevate(&r.c, &r.h).await;

    // The spawn cap: lower is narrowing; higher than the stored value needs
    // the elevation (the concurrent-update case).
    let down = r.caps(&gid, json!({"spawn_cap": 1}), None).await;
    assert_eq!(down.status, 200, "{}", down.body);
    assert_eq!(down.body["spawn_cap"], 1);
    let up = r.caps(&gid, json!({"spawn_cap": 2}), None).await;
    expect(&up, 400, "elevation_required");
    message(&up, "raising a limit needs the owner's elevation");
    assert_eq!(r.grant(&gid).await.spawn_cap, 1);
    let up = r.caps(&gid, json!({"spawn_cap": 2}), Some(&token)).await;
    assert_eq!(up.status, 200, "{}", up.body);
    assert_eq!(up.body["spawn_cap"], 2);

    // A lower claude ceiling with the same codex ceiling is narrowing.
    let narrow = modes(Some("plan"), Some(CODEX_WRITE));
    let ok = r.caps(&gid, json!({"max_modes": narrow}), None).await;
    assert_eq!(ok.status, 200, "{}", ok.body);
    assert_eq!(ok.body["max_modes"], narrow);

    // An incomparable Codex ceiling is widening; so is the way back.
    let never = modes(Some("plan"), Some("never/read-only"));
    let body = json!({"max_modes": never});
    expect(
        &r.caps(&gid, body.clone(), None).await,
        400,
        "elevation_required",
    );
    assert_eq!(r.grant(&gid).await.max_modes["codex"], CODEX_WRITE);
    let ok = r.caps(&gid, body, Some(&token)).await;
    assert_eq!(ok.status, 200, "{}", ok.body);
    assert_eq!(ok.body["max_modes"], never);
    let back = json!({"max_modes": narrow});
    expect(&r.caps(&gid, back, None).await, 400, "elevation_required");

    // A removed family is narrowing; a new family is widening.
    let removed = r
        .caps(&gid, json!({"max_modes": modes(Some("plan"), None)}), None)
        .await;
    assert_eq!(removed.status, 200, "{}", removed.body);
    assert_eq!(removed.body["max_modes"], modes(Some("plan"), None));
    let added = json!({"max_modes": modes(Some("plan"), Some("untrusted/read-only"))});
    expect(&r.caps(&gid, added, None).await, 400, "elevation_required");
    assert_eq!(r.grant(&gid).await.max_modes.len(), 1);

    // The audit row of the last success.
    let rows = r.ok_rows(&gid).await;
    let last = &rows[0];
    assert_eq!(last["old"]["spawn_cap"], 2);
    assert_eq!(last["new"]["spawn_cap"], 2);
    assert_eq!(
        last["old"]["max_modes"],
        modes(Some("plan"), Some("never/read-only"))
    );
    assert_eq!(last["new"]["max_modes"], modes(Some("plan"), None));
}

#[tokio::test]
async fn the_caps_route_refuses_bad_spawn_fields() {
    let r = rig().await;
    let token = elevate(&r.c, &r.h).await;
    bad_fields_are_refused(&r, &token).await;
    other_grants(&r).await;
}

/// The values that no elevation allows (rules b, e and f).
async fn bad_fields_are_refused(r: &Rig, token: &str) {
    let gid = spawner(r, "coord-bad").await;
    for (body, text) in [
        (json!({"spawn_cap": 51}), "spawn_cap is 0 to 50"),
        (
            json!({"max_modes": {"gemini": "x"}}),
            "max_modes has only the families claude and codex",
        ),
        (
            json!({"max_modes": {"claude": "bypassPermissions"}}),
            "a mode ceiling is not valid for its family",
        ),
    ] {
        let resp = r.caps(&gid, body, Some(token)).await;
        expect(&resp, 400, "invalid_grant");
        message(&resp, text);
    }
    let g = r.grant(&gid).await;
    assert_eq!(g.spawn_cap, 3, "nothing changed");
    assert_eq!(g.max_modes.len(), 2, "nothing changed");
}

/// A desktop-only grant, a manage-only grant and a worker grant.
async fn other_grants(r: &Rig) {
    let token = elevate(&r.c, &r.h).await;
    let f = &r.f;

    // A desktop-only grant has neither field, in the rules and in the audit.
    let desktop = bound_edit(
        f,
        &f.station,
        "coord-desktop",
        &[GrantClass::Desktop, GrantClass::Capture],
        |_| {},
    )
    .await
    .grant_id;
    let resp = r
        .caps(&desktop, json!({"spawn_cap": 1}), Some(&token))
        .await;
    expect(&resp, 400, "invalid_grant");
    message(&resp, "spawn_cap needs the spawn class");
    let resp = r
        .caps(
            &desktop,
            json!({"max_modes": {"claude": "plan"}}),
            Some(&token),
        )
        .await;
    expect(&resp, 400, "invalid_grant");
    message(&resp, "max_modes needs the spawn or manage class");
    let resp = r.caps(&desktop, json!({"desktop_cap": 10}), None).await;
    assert_eq!(resp.status, 200, "{}", resp.body);
    let rows = r.ok_rows(&desktop).await;
    for side in ["old", "new"] {
        let caps = rows[0][side].as_object().unwrap();
        assert!(!caps.contains_key("spawn_cap"), "{caps:?}");
        assert!(!caps.contains_key("max_modes"), "{caps:?}");
    }

    // A manage-only grant has ceilings and no spawn cap.
    let manage = bound_edit(
        f,
        &f.station,
        "coord-manage",
        &[GrantClass::Read, GrantClass::Manage],
        |g| g.scope = GrantScope::Fleet,
    )
    .await
    .grant_id;
    let resp = r.caps(&manage, json!({"spawn_cap": 1}), Some(&token)).await;
    expect(&resp, 400, "invalid_grant");
    message(&resp, "spawn_cap needs the spawn class");
    let resp = r
        .caps(&manage, json!({"max_modes": {"claude": "plan"}}), None)
        .await;
    expect(&resp, 400, "elevation_required");
    let resp = r
        .caps(
            &manage,
            json!({"max_modes": {"claude": "plan"}}),
            Some(&token),
        )
        .await;
    assert_eq!(resp.status, 200, "{}", resp.body);
    assert_eq!(resp.body["max_modes"], modes(Some("plan"), None));
    let resp = r
        .caps(&manage, json!({"max_modes": {"claude": "default"}}), None)
        .await;
    assert_eq!(resp.status, 200, "{}", resp.body);
    let rows = r.ok_rows(&manage).await;
    let caps = rows[0]["new"].as_object().unwrap();
    assert!(caps.contains_key("max_modes") && !caps.contains_key("spawn_cap"));

    // A worker grant has a spawn cap of at most 10.
    let worker = bound_edit(f, &f.station, "coord-worker", &SPAWNER, |g| {
        g.kind = GrantKind::Worker;
        g.spawn_cap = 2;
        g.max_modes = [
            ("claude".to_owned(), "acceptEdits".to_owned()),
            ("codex".to_owned(), CODEX_WRITE.to_owned()),
        ]
        .into();
    })
    .await
    .grant_id;
    let resp = r
        .caps(&worker, json!({"spawn_cap": 11}), Some(&token))
        .await;
    expect(&resp, 400, "invalid_grant");
    message(&resp, "a worker grant has a spawn cap of at most 10");
    let resp = r.caps(&worker, json!({"spawn_cap": 1}), None).await;
    assert_eq!(resp.status, 200, "{}", resp.body);
    assert_eq!(resp.body["spawn_cap"], 1);
}
