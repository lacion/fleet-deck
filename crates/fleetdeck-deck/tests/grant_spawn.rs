//! MCP P0 on the deck over the board API (design §4 tests 1 to 5, 7, 8 and
//! 11): the spawn cap and the mode ceilings of `owner_start`, the elevation of
//! a grant that starts sessions, a coordinator grant on a board import, and
//! the first spawn of a grant that the board made.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
mod settings_support;

use std::sync::Arc;
use std::time::Duration;

use common::board::board;
use common::fleet::claude_info;
use common::*;
use fleetdeck_deck::store::GrantClass;
use fleetdeck_proto::command::ReceiptState;
use fleetdeck_proto::mutation::CoordinatorOp;
use fleetdeck_proto::mutation::{CoordinatorBinding, CoordinatorCall, CoordinatorCallEnvelope};
use serde_json::{Value, json};
use settings_support::*;

const START: &str = "/api/commands/session.start";
const IMPORT: &str = "/api/commands/session.import";

fn session(coordinator: Option<Value>) -> Value {
    let mut spec = json!({"provider_instance": "claude", "cwd": "/w", "mode": null,
        "model": null, "title": null});
    if let Some(c) = coordinator {
        spec["coordinator"] = c;
    }
    spec
}

fn coordinator(extra: Value) -> Value {
    let mut spec = json!({"grant_id": "", "classes": ["read"], "idle_ttl_ms": 0, "expires_at": 0,
        "control_skill": {"version": "", "text": ""}});
    spec.as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    spec
}

fn start_body(id: &str, extra: Value, elevation: Option<&str>) -> Value {
    let mut body = json!({"station": "fake", "command_id": id, "command": {"op": "start",
        "prompt": null, "spec": session(Some(coordinator(extra)))}});
    if let Some(e) = elevation {
        body["elevation"] = json!(e);
    }
    body
}

fn invalid_grant(r: &common::board::Http, message: &str) {
    expect(r, 400, "invalid_grant");
    assert_eq!(r.body["message"], message, "{}", r.body);
}

/// Test 1: each refusal of §2.2 rules 1 to 5, with its exact message; the
/// checks run before the elevation, and nothing is stored or sent.
#[tokio::test]
async fn each_spawn_field_rule_refuses_with_its_message_and_stores_nothing() {
    let mut f = fixture().await;
    let mut station = f.connect().await;
    let c = board(Arc::clone(&f.ctx)).await;
    set_password(&c).await;
    let h = owner(&c).await;
    let cases = [
        (
            json!({"classes": ["read"], "spawn_cap": 3}),
            "spawn_cap needs the spawn class",
        ),
        (
            json!({"classes": ["spawn"], "spawn_cap": 51}),
            "spawn_cap is 0 to 50",
        ),
        (
            json!({"classes": ["read"], "max_modes": {"claude": "plan"}}),
            "max_modes needs the spawn or manage class",
        ),
        (
            json!({"classes": ["spawn"], "max_modes": {"gemini": "plan"}}),
            "unknown provider family `gemini`",
        ),
        (
            json!({"classes": ["spawn"], "max_modes": {"claude": "bypassPermissions"}}),
            "the mode ceiling `bypassPermissions` is not valid for `claude`",
        ),
        (
            json!({"classes": ["manage"], "max_modes": {"codex": "never/danger-full-access"}}),
            "the mode ceiling `never/danger-full-access` is not valid for `codex`",
        ),
    ];
    for (i, (extra, message)) in cases.into_iter().enumerate() {
        let r = post(&c, &h, START, start_body(&format!("r{i}"), extra, None)).await;
        invalid_grant(&r, message);
    }
    no_request(&mut station, Duration::from_millis(200)).await;
    assert!(c.store.grants(10).await.unwrap().is_empty());
}

/// Test 2: a grant with the class `spawn` needs the owner's elevation.
#[tokio::test]
async fn a_spawn_grant_needs_the_owners_elevation() {
    let mut f = fixture().await;
    let mut station = f.connect().await;
    let c = board(Arc::clone(&f.ctx)).await;
    set_password(&c).await;
    let h = owner(&c).await;
    let extra = json!({"classes": ["read", "spawn"]});
    let r = post(&c, &h, START, start_body("e1", extra.clone(), None)).await;
    expect(&r, 400, "elevation_required");
    assert_eq!(
        r.body["message"],
        "a grant that starts sessions needs the owner's elevation"
    );
    let r = post(&c, &h, START, start_body("e1", extra, Some("nope"))).await;
    expect(&r, 400, "elevation_required");
    no_request(&mut station, Duration::from_millis(200)).await;
    assert!(c.store.grants(10).await.unwrap().is_empty());
}

/// Tests 3 and 4: the stored cap and ceilings, the default cap, and the
/// start command without the two board-only fields.
#[tokio::test]
async fn a_spawn_grant_is_stored_with_its_cap_and_ceilings_and_the_station_gets_neither() {
    let mut f = fixture().await;
    let mut station = f.connect().await;
    let c = board(Arc::clone(&f.ctx)).await;
    set_password(&c).await;
    let h = owner(&c).await;
    let token = elevate(&c, &h).await;
    let extra = json!({"classes": ["read", "spawn"], "spawn_cap": 7,
        "max_modes": {"claude": "acceptEdits", "codex": "on-request/workspace-write"}});
    let call = post(&c, &h, START, start_body("s1", extra, Some(&token)));
    let (r, req) = tokio::join!(call, Fixture::answer(&mut station, accepted));
    assert_eq!(r.status, 200, "{}", r.body);
    let sent = req.params["command"]["spec"]["coordinator"].clone();
    let keys: Vec<&str> = sent
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert!(!keys.contains(&"spawn_cap"), "{keys:?}");
    assert!(!keys.contains(&"max_modes"), "{keys:?}");
    assert!(!req.params.to_string().contains("max_modes"));
    let g = c.store.grants(10).await.unwrap().remove(0);
    assert_eq!(g.spawn_cap, 7);
    assert_eq!(g.max_modes["claude"], "acceptEdits");
    assert_eq!(g.max_modes["codex"], "on-request/workspace-write");
    // The audit row names the cap and the ceilings.
    let details = c
        .audited(
            "coordinator.grant_created",
            fleetdeck_deck::store::AuditResult::Ok,
        )
        .await;
    let detail = details[0].clone().unwrap();
    assert!(
        detail.ends_with(
            ";spawn_cap=7;max_modes=claude:acceptEdits,codex:on-request/workspace-write"
        )
    );
    // Test 4: no cap in the request, the stored cap is 3.
    let extra = json!({"classes": ["spawn"]});
    let call = post(&c, &h, START, start_body("s2", extra, Some(&token)));
    let (r, _) = tokio::join!(call, Fixture::answer(&mut station, accepted));
    assert_eq!(r.status, 200, "{}", r.body);
    let grants = c.store.grants(10).await.unwrap();
    assert_eq!(grants[0].spawn_cap, 3);
    assert!(grants[0].max_modes.is_empty());
}

/// Test 5: `manage` with `max_modes` and without `spawn` is accepted with
/// the elevation.
#[tokio::test]
async fn a_manage_grant_may_carry_mode_ceilings() {
    let mut f = fixture().await;
    let mut station = f.connect().await;
    f.store
        .set_link_features(&f.station, 1, vec!["coordinator.v2".to_owned()]);
    let c = board(Arc::clone(&f.ctx)).await;
    set_password(&c).await;
    let h = owner(&c).await;
    let token = elevate(&c, &h).await;
    let extra = json!({"classes": ["manage"], "max_modes": {"claude": "plan"}});
    let call = post(&c, &h, START, start_body("m1", extra, Some(&token)));
    let (r, _) = tokio::join!(call, Fixture::answer(&mut station, accepted));
    assert_eq!(r.status, 200, "{}", r.body);
    let g = c.store.grants(10).await.unwrap().remove(0);
    assert_eq!(g.max_modes["claude"], "plan");
    assert_eq!(g.max_modes.len(), 1);
}

/// Test 7: a lineage `spawn` grant on the own station needs the elevation but
/// no `coordinator.v2` link.
#[tokio::test]
async fn a_lineage_spawn_grant_needs_elevation_but_no_v2_link() {
    let mut f = fixture().await;
    let mut station = f.connect().await;
    let c = board(Arc::clone(&f.ctx)).await;
    set_password(&c).await;
    let h = owner(&c).await;
    // A live link of the station that negotiated no `coordinator.v2`.
    f.store.set_link_features(&f.station, 99, vec![]);
    let extra = json!({"classes": ["read", "mail", "spawn"]});
    let r = post(&c, &h, START, start_body("v1", extra.clone(), None)).await;
    expect(&r, 400, "elevation_required");
    let token = elevate(&c, &h).await;
    let call = post(&c, &h, START, start_body("v2", extra, Some(&token)));
    let (r, _) = tokio::join!(call, Fixture::answer(&mut station, accepted));
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(c.store.grants(10).await.unwrap().len(), 1);
}

/// Test 8 (import): a board `session.import` with a coordinator grant is a
/// `400 invalid_command`, and nothing is sent.
#[tokio::test]
async fn a_board_import_with_a_coordinator_grant_is_refused_and_not_sent() {
    let mut f = fixture().await;
    let mut station = f.connect().await;
    let c = board(Arc::clone(&f.ctx)).await;
    set_password(&c).await;
    let h = owner(&c).await;
    let body = |coordinator: Option<Value>| {
        json!({"station": "fake", "command_id": "i1", "command": {"op": "import",
            "prompt": null, "native_session_id": "7c1f7e44-5d52-4a53-8a8c-0e6b7b7f1d11",
            "spec": session(coordinator)}})
    };
    let with = Some(coordinator(json!({"classes": ["spawn"], "spawn_cap": 2})));
    let r = post(&c, &h, IMPORT, body(with)).await;
    expect(&r, 400, "invalid_command");
    assert_eq!(
        r.body["message"],
        "an import never carries a coordinator grant"
    );
    no_request(&mut station, Duration::from_millis(200)).await;
    assert!(c.store.grants(10).await.unwrap().is_empty());
}

/// Test 11: a grant that the board made with a Claude ceiling `acceptEdits`
/// accepts a `spawn` in mode `plan` and refuses one in mode `dontAsk`.
#[tokio::test]
async fn a_grant_from_the_board_spawns_within_its_ceiling_only() {
    let mut f = fixture_with(fleet::coordinate()).await;
    let mut station = f.connect().await;
    f.store.set_station_info(&f.station, claude_info());
    let c = board(Arc::clone(&f.ctx)).await;
    set_password(&c).await;
    let h = owner(&c).await;
    let token = elevate(&c, &h).await;
    let extra = json!({"classes": ["read", "spawn"], "spawn_cap": 2,
        "max_modes": {"claude": "acceptEdits"}});
    let call = post(&c, &h, START, start_body("g1", extra, Some(&token)));
    let (r, _) = tokio::join!(call, Fixture::answer(&mut station, accepted));
    assert_eq!(r.status, 200, "{}", r.body);
    let stored = c.store.grants(10).await.unwrap().remove(0);
    assert!(stored.classes.contains(&GrantClass::Spawn));
    // The station has not named the session yet, so the grant holds the empty
    // session id; the caller of the calls below is that session.
    let b = CoordinatorBinding {
        binding_id: "b-1".to_owned(),
        grant_id: stored.grant_id.clone(),
        generation: 1,
        provider_instance: "claude".into(),
        native_id: "n-1".to_owned(),
    };
    assert!(f.store.bind_grant(b.clone()).await.unwrap());
    let spawn = |mode: &str| {
        let mut spec = session(None);
        spec["mode"] = json!(mode);
        CoordinatorOp::Spawn {
            station: f.station.clone(),
            spec: serde_json::from_value(spec).unwrap(),
            prompt: None,
        }
    };
    let call = |id: &str, op: CoordinatorOp| CoordinatorCallEnvelope {
        command_id: id.into(),
        target_incarnation: f.store.incarnation().clone(),
        retry_until: f.store.now_ms() + fleetdeck_proto::command::RETRY_WINDOW_MS,
        call: CoordinatorCall {
            caller: "".into(),
            binding: b.clone(),
            idempotency_key: None,
            op,
        },
    };
    let ok = f
        .store
        .decide_coordinator(f.station.clone(), call("c1", spawn("plan")))
        .await
        .unwrap();
    assert_eq!(ok.state, ReceiptState::Accepted, "{ok:?}");
    let refused = f
        .store
        .decide_coordinator(f.station.clone(), call("c2", spawn("dontAsk")))
        .await
        .unwrap();
    assert_eq!(refused.state, ReceiptState::Rejected, "{refused:?}");
    assert_eq!(
        refused.reject.unwrap().code,
        fleetdeck_proto::RejectCode::Forbidden
    );
}
