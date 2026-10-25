//! MCP P4b-3c over the board API (design `docs/v2/design/mcp-p4b.md` §6.1 to
//! §6.3, §10, tests 8, 9 and 13, route parts): the owner gives a grant to a
//! session that already runs, changes its classes with the elevation rule for
//! every widening, and ends it; nobody else can. The views carry `live` and
//! `coordinator_live`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
mod settings_support;

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use common::board::{BoardClient, Http, board};
use common::fleet::{bound_edit, card};
use common::*;
use fleetdeck_deck::store::grants::owner_spec::{SpecFeatures, station_spec};
use fleetdeck_deck::store::{AuditResult, GrantClass};
use fleetdeck_proto::CommandEnvelope;
use fleetdeck_proto::command::{CoordinatorSpec, GrantKind, SessionCommand};
use fleetdeck_proto::event::SessionState;
use fleetdeck_proto::link::Scope;
use fleetdeck_proto::version::feature;
use serde_json::{Value, json};
use settings_support::*;

const ELEVATION: &str = "x-fleetdeck-elevation";
const DAY: i64 = 24 * 60 * 60 * 1000;
const LIVE: [&str; 3] = [
    feature::COORDINATOR,
    feature::COORDINATOR_V2,
    feature::COORDINATOR_LIVE,
];

struct Rig {
    f: Fixture,
    c: BoardClient,
    h: Headers,
}

/// A deck whose station negotiated the live features (it has no real link),
/// knows the sessions `run-1` to `run-6` (`run-2` exited) and has fleet
/// control on.
async fn rig() -> Rig {
    let f = fixture().await;
    let sessions = (1..=6)
        .map(|n| {
            let mut s = card(&format!("run-{n}"));
            if n == 2 {
                s.state = SessionState::Exited;
            }
            s
        })
        .collect();
    f.store
        .apply_snapshot(f.station.clone(), f.inc.clone(), 0, sessions)
        .await
        .unwrap();
    f.store
        .set_link_features(&f.station, 98, LIVE.map(str::to_owned).to_vec());
    f.store.set_fleet_control(true);
    let c = board(Arc::clone(&f.ctx)).await;
    set_password(&c).await;
    let h = owner(&c).await;
    Rig { f, c, h }
}

fn spec(classes: Value) -> Value {
    json!({"grant_id": "", "classes": classes, "idle_ttl_ms": 0, "expires_at": 0,
        "control_skill": {"version": "", "text": ""}})
}

fn with(mut body: Value, extra: Value) -> Value {
    body.as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    body
}

fn headers<'a>(h: &'a Headers, elevation: Option<&'a str>) -> Vec<(&'a str, &'a str)> {
    let mut all = refs(h);
    if let Some(token) = elevation {
        all.push((ELEVATION, token));
    }
    all
}

fn message(r: &Http, text: &str) {
    assert_eq!(r.body["message"], text, "{}", r.body);
}

impl Rig {
    fn station(&self) -> String {
        self.f.station.to_string()
    }

    async fn give(&self, session: &str, body: Value, elevation: Option<&str>) -> Http {
        let path = format!(
            "/api/coordinator/sessions/{}/{session}/grant",
            self.station()
        );
        self.c.post(&path, &headers(&self.h, elevation), body).await
    }

    async fn classes(&self, gid: &str, body: Value, elevation: Option<&str>) -> Http {
        let path = format!("/api/coordinator/grants/{gid}/classes");
        self.c.post(&path, &headers(&self.h, elevation), body).await
    }

    async fn revoke(&self, gid: &str) -> Http {
        let path = format!("/api/coordinator/grants/{gid}/revoke");
        self.c.post(&path, &headers(&self.h, None), json!({})).await
    }

    async fn elevation(&self) -> String {
        elevate(&self.c, &self.h).await
    }

    /// The sets of a session, oldest first: the spec (or none).
    async fn sets(&self, session: &str) -> Vec<Option<CoordinatorSpec>> {
        let all = self.f.store.commands().await.unwrap();
        all.into_iter()
            .filter(|c| c.method == "session.coordinator_set")
            .filter_map(|c| {
                let env: CommandEnvelope = serde_json::from_value(c.envelope).unwrap();
                match env.command {
                    SessionCommand::CoordinatorSet {
                        session_id, spec, ..
                    } if session_id.0 == session => Some(spec.map(|s| *s)),
                    _ => None,
                }
            })
            .collect()
    }

    /// The audit rows of an action, newest first: result and detail.
    async fn audit(&self, action: &str) -> Vec<(AuditResult, String)> {
        let rows = self.f.store.audit_log(500).await.unwrap();
        rows.into_iter()
            .filter(|e| e.action == action)
            .map(|e| (e.result, e.detail.unwrap_or_default()))
            .collect()
    }

    async fn grant(&self, gid: &str) -> fleetdeck_deck::store::Grant {
        self.f.store.grant(gid.to_owned()).await.unwrap().unwrap()
    }

    /// An owner grant with a row on `session`.
    async fn given(&self, session: &str, classes: Value) -> String {
        let r = self.give(session, spec(classes), None).await;
        assert_eq!(r.status, 200, "{}", r.body);
        r.body["grant_id"].as_str().unwrap().to_owned()
    }
}

#[tokio::test]
async fn the_owner_gives_a_grant_to_a_running_or_hibernated_session() {
    let r = rig().await;
    // A running session and an exited one get a grant with a row.
    for session in ["run-1", "run-2"] {
        let given = r.give(session, spec(json!(["read", "mail"])), None).await;
        assert_eq!(given.status, 200, "{}", given.body);
        assert_eq!(given.body["live"], true);
        assert_eq!(given.body["session_id"], session);
        assert_eq!(given.body["state"], "active");
        assert!(given.body.get("kind").is_none(), "{}", given.body);
        let gid = given.body["grant_id"].as_str().unwrap();
        assert!(r.f.store.live_grant_ids().await.unwrap().contains(gid));
        let stored = r.grant(gid).await;
        assert_eq!(stored.kind, GrantKind::Coordinator);
        assert_eq!(stored.session_id.0, session);
        let sets = r.sets(session).await;
        assert_eq!(sets.len(), 1, "one set is queued");
        assert_eq!(sets[0].as_ref().unwrap().grant_id, gid);
        let (result, detail) = &r.audit("coordinator.grant_created").await[0];
        assert_eq!(*result, AuditResult::Ok);
        assert!(detail.ends_with(";live=true"), "{detail}");
    }
    // A second give is refused.
    let twice = r.give("run-1", spec(json!(["read"])), None).await;
    expect(&twice, 409, "has_grant");
    message(
        &twice,
        "the session already has a grant; renew or revoke it first",
    );
    assert_eq!(r.sets("run-1").await.len(), 1);
    let refused = r.audit("coordinator.grant_give").await;
    assert_eq!(refused, [(AuditResult::Refused, "has_grant".to_owned())]);
    // An unknown session, a worker kind and a bad body.
    let unknown = r.give("nope", spec(json!(["read"])), None).await;
    expect(&unknown, 404, "unknown_session");
    let kind = with(spec(json!(["read"])), json!({"kind": "worker"}));
    let r3 = r.give("run-3", kind, None).await;
    expect(&r3, 400, "invalid_grant");
    message(&r3, "the grant kind is set by the deck");
    let bad = r.give("run-3", json!({"classes": 3}), None).await;
    expect(&bad, 400, "invalid_body");
    // Held until the feature: both live features are needed.
    let without = LIVE[..2].iter().map(|f| (*f).to_owned()).collect();
    r.f.store.set_link_features(&r.f.station, 98, without);
    let held = r.give("run-3", spec(json!(["read"])), None).await;
    expect(&held, 409, "not_supported");
    message(&held, "this station cannot change grants live");
    r.f.store
        .set_link_features(&r.f.station, 98, LIVE.map(str::to_owned).to_vec());
    assert_eq!(r.f.store.live_grant_ids().await.unwrap().len(), 2);
    start_only_fields_are_refused(&r).await;
    elevation_rules_follow_the_creation_rules(&r).await;
}

/// A desktop, capture, recording, input or click field or class is refused
/// before anything else.
async fn start_only_fields_are_refused(r: &Rig) {
    let text = "desktop, screenshot and input access is given only at start";
    let token = r.elevation().await;
    let bodies = [
        with(spec(json!(["read"])), json!({"desktop_cap": 5})),
        with(spec(json!(["read"])), json!({"capture_cap": 5})),
        with(spec(json!(["read"])), json!({"image_byte_cap": 1_048_576})),
        with(spec(json!(["read"])), json!({"allow_recording": true})),
        with(spec(json!(["read"])), json!({"recording_cap": 1})),
        with(spec(json!(["read"])), json!({"recording_second_cap": 1})),
        with(spec(json!(["read"])), json!({"allow_input": true})),
        with(spec(json!(["read"])), json!({"allow_click": true})),
        with(spec(json!(["read"])), json!({"input_scope": "own_windows"})),
        with(spec(json!(["read"])), json!({"input_cap": 1})),
        with(spec(json!(["read"])), json!({"input_character_cap": 1})),
        spec(json!(["read", "desktop"])),
        spec(json!(["capture"])),
        spec(json!(["input"])),
    ];
    for body in bodies {
        let got = r.give("run-3", body.clone(), Some(&token)).await;
        expect(&got, 400, "invalid_grant");
        message(&got, text);
    }
    assert!(r.sets("run-3").await.is_empty());
    assert_eq!(r.f.store.live_grant_ids().await.unwrap().len(), 2);
}

/// A spawn grant on its own station and a fleet-control grant need the
/// elevation, as at a start; the checks of the spec run before it.
async fn elevation_rules_follow_the_creation_rules(r: &Rig) {
    let spawn = with(spec(json!(["read", "spawn"])), json!({"spawn_cap": 4}));
    let got = r.give("run-3", spawn.clone(), None).await;
    expect(&got, 400, "elevation_required");
    message(
        &got,
        "a grant that starts sessions needs the owner's elevation",
    );
    let bad = with(spec(json!(["read", "spawn"])), json!({"spawn_cap": 51}));
    let got = r.give("run-3", bad, None).await;
    expect(&got, 400, "invalid_grant");
    message(
        &got,
        "spawn_cap is above the grant limit of the deck (Settings, Grant limits)",
    );
    let fleet = spec(json!(["read", "inspect"]));
    let got = r.give("run-3", fleet.clone(), None).await;
    expect(&got, 400, "elevation_required");
    message(&got, "a fleet-control grant needs the owner's elevation");
    let capped = with(spec(json!(["read", "inspect"])), json!({"answer_cap": 5}));
    let got = r.give("run-3", capped, None).await;
    expect(&got, 400, "invalid_grant");
    message(&got, "answer_cap needs the answer class");
    let token = r.elevation().await;
    let got = r.give("run-3", spawn, Some(&token)).await;
    assert_eq!(got.status, 200, "{}", got.body);
    assert_eq!(got.body["spawn_cap"], 4);
    let got = r.give("run-4", fleet.clone(), Some(&token)).await;
    assert_eq!(got.status, 200, "{}", got.body);
    // Fleet control needs `coordinator.v2` on the station's link.
    let v1 = vec![
        feature::COORDINATOR.to_owned(),
        feature::COORDINATOR_LIVE.to_owned(),
    ];
    r.f.store.set_link_features(&r.f.station, 98, v1);
    let got = r.give("run-5", fleet, Some(&token)).await;
    expect(&got, 409, "unsupported");
    let plain = r.give("run-5", spec(json!(["read"])), None).await;
    assert_eq!(plain.status, 200, "{}", plain.body);
    r.f.store
        .set_link_features(&r.f.station, 98, LIVE.map(str::to_owned).to_vec());
}

#[tokio::test]
async fn class_and_cap_changes_need_elevation_for_every_widening() {
    let r = rig().await;
    let gid = r.given("run-1", json!(["read", "mail"])).await;
    let token = r.elevation().await;
    let all = json!(["read", "mail", "drive"]);

    // An added class needs the elevation.
    let got = r.classes(&gid, json!({"classes": all}), None).await;
    expect(&got, 400, "elevation_required");
    message(&got, "adding access needs the owner's elevation");
    assert_eq!(r.grant(&gid).await.classes.len(), 2);
    let got = r.classes(&gid, json!({"classes": all}), Some(&token)).await;
    assert_eq!(got.status, 200, "{}", got.body);
    assert_eq!(got.body["classes"], json!(["read", "mail", "drive"]));
    assert_eq!(got.body["live"], true);
    // A removed class needs none.
    let got = r.classes(&gid, json!({"classes": ["read"]}), None).await;
    assert_eq!(got.status, 200, "{}", got.body);
    assert_eq!(got.body["classes"], json!(["read"]));
    // A newly added `spawn` brings its caps; the caps of other classes are
    // refused, and so are an empty set and a start-only class.
    let spawn = json!({"classes": ["read", "spawn"], "spawn_cap": 5});
    let got = r.classes(&gid, spawn.clone(), None).await;
    expect(&got, 400, "elevation_required");
    let got = r.classes(&gid, spawn, Some(&token)).await;
    assert_eq!(got.status, 200, "{}", got.body);
    assert_eq!(got.body["spawn_cap"], 5);
    let got = r
        .classes(
            &gid,
            json!({"classes": ["read", "spawn"], "spawn_cap": 9}),
            Some(&token),
        )
        .await;
    expect(&got, 400, "invalid_grant");
    message(&got, "a cap is changed with the caps route");
    let got = r.classes(&gid, json!({"classes": []}), Some(&token)).await;
    expect(&got, 400, "invalid_grant");
    message(&got, "a coordinator grant needs a class");
    let got = r
        .classes(&gid, json!({"classes": ["read", "desktop"]}), Some(&token))
        .await;
    expect(&got, 400, "invalid_grant");
    message(
        &got,
        "desktop, screenshot and input access is given only at start",
    );
    let got = r
        .classes(&gid, json!({"classes": ["nope"]}), Some(&token))
        .await;
    expect(&got, 400, "invalid_grant");
    message(&got, "unknown grant class `nope`");
    let got = r
        .classes(&gid, json!({"classes": ["read"], "x": 1}), None)
        .await;
    expect(&got, 400, "invalid_body");
    let got = r.classes("nope", json!({"classes": ["read"]}), None).await;
    expect(&got, 404, "unknown_grant");

    // The fleet-control switch and the link: a new fleet-control class.
    let fleet = json!({"classes": ["read", "spawn", "inspect"]});
    r.f.store.set_fleet_control(false);
    let got = r.classes(&gid, fleet.clone(), Some(&token)).await;
    expect(&got, 409, "fleet_control_off");
    message(&got, "fleet control is off");
    r.f.store.set_fleet_control(true);
    let v1 = vec![
        feature::COORDINATOR.to_owned(),
        feature::COORDINATOR_LIVE.to_owned(),
    ];
    r.f.store.set_link_features(&r.f.station, 98, v1);
    expect(
        &r.classes(&gid, fleet.clone(), Some(&token)).await,
        409,
        "unsupported",
    );
    r.f.store
        .set_link_features(&r.f.station, 98, LIVE.map(str::to_owned).to_vec());
    let got = r.classes(&gid, fleet.clone(), None).await;
    expect(&got, 400, "elevation_required");
    let got = r.classes(&gid, fleet, Some(&token)).await;
    assert_eq!(got.status, 200, "{}", got.body);
    // The switch does not stop a narrowing.
    r.f.store.set_fleet_control(false);
    let got = r.classes(&gid, json!({"classes": ["read"]}), None).await;
    assert_eq!(got.status, 200, "{}", got.body);

    other_grants(&r, &token).await;
    audit_rows(&r, &gid).await;
}

/// A worker grant and a grant without a row never change live.
async fn other_grants(r: &Rig, token: &str) {
    let f = &r.f;
    let worker = bound_edit(
        f,
        &f.station,
        "run-4",
        &[
            GrantClass::Read,
            GrantClass::Mail,
            GrantClass::Drive,
            GrantClass::Spawn,
        ],
        |g| {
            g.kind = GrantKind::Worker;
            g.spawn_cap = 2;
            g.max_modes = [("claude".to_owned(), "plan".to_owned())].into();
        },
    )
    .await
    .grant_id;
    let got = r
        .classes(&worker, json!({"classes": ["read"]}), Some(token))
        .await;
    expect(&got, 409, "worker_grant");
    message(&got, "a worker grant has fixed classes");
    let plain = bound_edit(f, &f.station, "run-5", &[GrantClass::Read], |_| {})
        .await
        .grant_id;
    let got = r
        .classes(&plain, json!({"classes": ["read", "mail"]}), Some(token))
        .await;
    expect(&got, 409, "not_supported");
    message(&got, "this grant cannot change live");
    let got = r.classes(&plain, json!({"classes": ["read"]}), None).await;
    expect(&got, 409, "not_supported");
    assert_eq!(r.grant(&plain).await.classes.len(), 1);
    // A revoked grant is gone.
    let gone = r.given("run-6", json!(["read"])).await;
    assert_eq!(r.revoke(&gone).await.status, 200);
    let got = r
        .classes(&gone, json!({"classes": ["read", "mail"]}), Some(token))
        .await;
    expect(&got, 404, "unknown_grant");
}

/// The audit rows of the changes: old and new classes, and the refusals.
async fn audit_rows(r: &Rig, gid: &str) {
    let rows = r.audit("coordinator.grant_classes").await;
    let ok: Vec<Value> = rows
        .iter()
        .filter(|(result, _)| *result == AuditResult::Ok)
        .map(|(_, detail)| serde_json::from_str(detail).unwrap())
        .collect();
    // Newest first: the narrowing, then the add of `inspect`, then `spawn`.
    assert_eq!(ok[0]["old"]["classes"], json!(["read", "spawn", "inspect"]));
    assert_eq!(ok[0]["new"]["classes"], json!(["read"]));
    assert_eq!(ok[1]["new"]["classes"], json!(["read", "spawn", "inspect"]));
    assert!(ok[1]["new"].get("spawn_cap").is_none(), "{}", ok[1]);
    assert_eq!(ok[2]["old"]["classes"], json!(["read"]));
    assert_eq!(ok[2]["new"]["spawn_cap"], 5);
    assert_eq!(ok[2]["new"]["max_modes"], json!({}));
    assert!(
        rows.iter()
            .any(|(result, detail)| *result == AuditResult::Refused
                && detail == "elevation_required")
    );
    assert!(r.audit("coordinator.grant_classes").await.len() > ok.len());
    let _ = gid;
}

#[tokio::test]
async fn a_member_or_a_token_cannot_give_change_or_end_a_grant() {
    let r = rig().await;
    let gid = r.given("run-1", json!(["read", "mail"])).await;
    // A bearer token with every board scope, and a member of a share.
    let (_, t) =
        r.c.store
            .create_token(
                "ops".into(),
                BTreeSet::from([Scope::Read, Scope::Drive, Scope::Admin]),
                DAY,
            )
            .await
            .unwrap();
    let token = vec![("Authorization".to_owned(), format!("Bearer {}", t.expose()))];
    let member = member(&r).await;
    let give_path = format!("/api/coordinator/sessions/{}/run-3/grant", r.station());
    let paths = [
        (give_path, spec(json!(["read"]))),
        (
            format!("/api/coordinator/grants/{gid}/classes"),
            json!({"classes": ["read"]}),
        ),
        (format!("/api/coordinator/grants/{gid}/revoke"), json!({})),
    ];
    for (who, h) in [("token", &token), ("member", &member)] {
        for (path, body) in &paths {
            let got = r.c.post(path, &refs(h), body.clone()).await;
            assert_eq!(got.status, 403, "{who} {path}: {}", got.body);
            if who == "token" {
                assert_eq!(got.code(), "owner_only", "{path}");
            }
        }
    }
    // Nothing changed: no new grant, the same classes, still active.
    assert_eq!(r.f.store.live_grant_ids().await.unwrap().len(), 1);
    let g = r.grant(&gid).await;
    assert_eq!(
        (g.classes.len(), g.state),
        (2, fleetdeck_deck::store::GrantState::Active)
    );
    assert_eq!(r.sets("run-1").await.len(), 1);
    assert!(r.sets("run-3").await.is_empty());
    // The refusals are audited.
    let refused = r.audit("coordinator.grant_give").await;
    assert!(refused.contains(&(AuditResult::Refused, "owner_only".to_owned())));
}

/// A member of a share on session `run-1`.
async fn member(r: &Rig) -> Headers {
    let target = format!("{}/run-1", r.station());
    let share = post(
        &r.c,
        &r.h,
        "/api/shares",
        json!({ "kind": "session", "target": target, "role": "collaborator" }),
    )
    .await;
    assert_eq!(share.status, 200, "{}", share.body);
    let url = share.body["url"].as_str().unwrap();
    let join = url.split("#join=").nth(1).unwrap();
    let j =
        r.c.post(
            "/api/auth/join",
            &[],
            json!({ "token": join, "display_name": "Ann" }),
        )
        .await;
    assert_eq!(j.status, 200, "{}", j.body);
    let cookie = j
        .header("set-cookie")
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    vec![
        ("Cookie".to_owned(), cookie),
        (
            "X-CSRF-Token".to_owned(),
            j.body["csrf"].as_str().unwrap().to_owned(),
        ),
    ]
}

#[tokio::test]
async fn a_revoke_of_a_live_grant_queues_a_set_and_the_views_carry_the_flags() {
    let r = rig().await;
    let live = r.given("run-1", json!(["read", "mail"])).await;
    let plain = bound_edit(&r.f, &r.f.station, "run-2", &[GrantClass::Read], |_| {})
        .await
        .grant_id;
    // The views: `live` for the grant with a row only, `coordinator_live` for
    // a station whose link negotiated the feature.
    let list = get(&r.c, &r.h, "/api/coordinator/grants").await;
    assert_eq!(list.status, 200, "{}", list.body);
    let by_id = |id: &str| {
        list.body
            .as_array()
            .unwrap()
            .iter()
            .find(|g| g["grant_id"] == id)
            .unwrap()
            .clone()
    };
    assert_eq!(by_id(&live)["live"], true);
    assert!(by_id(&plain).get("live").is_none(), "{}", by_id(&plain));
    let stations = get(&r.c, &r.h, "/api/stations").await;
    assert_eq!(stations.body[0]["coordinator_live"], true);
    let v1 = vec![
        feature::COORDINATOR.to_owned(),
        feature::COORDINATOR_V2.to_owned(),
    ];
    r.f.store.set_link_features(&r.f.station, 98, v1);
    let stations = get(&r.c, &r.h, "/api/stations").await;
    assert!(stations.body[0].get("coordinator_live").is_none());
    r.f.store.set_link_features(&r.f.station, 98, vec![]);

    // The revoke of the grant with a row: a set with no spec, no revoke.
    let revoked = r.revoke(&live).await;
    assert_eq!(revoked.status, 200, "{}", revoked.body);
    assert_eq!(revoked.body["state"], "revoked");
    assert_eq!(revoked.body["live"], true);
    let sets = r.sets("run-1").await;
    assert_eq!(sets.len(), 2, "give and end");
    assert!(sets[0].is_some() && sets[1].is_none());
    assert!(!revoked_commands(&r).await);
    // A second revoke sends nothing more.
    assert_eq!(r.revoke(&live).await.status, 200);
    assert_eq!(r.sets("run-1").await.len(), 2);
    // The grant without a row: today's `session.coordinator_revoke`.
    assert_eq!(r.revoke(&plain).await.status, 200);
    assert!(revoked_commands(&r).await);
    assert!(r.sets("run-2").await.is_empty());
}

/// True when the outbox holds a `session.coordinator_revoke`.
async fn revoked_commands(r: &Rig) -> bool {
    let items = r.f.store.outbox().await.unwrap();
    items
        .iter()
        .any(|i| i.payload.to_string().contains("coordinator_revoke"))
}

/// A board start with `coordinator` and the stored start command that the
/// fake station got: the grant and the spec that the station saw.
async fn board_start(
    f: &mut Fixture,
    station: &mut fleetdeck_link::LiveLink,
    (c, h): (&BoardClient, &Headers),
    (id, coordinator): (&str, Value),
) -> (fleetdeck_deck::store::Grant, CoordinatorSpec) {
    let token = elevate(c, h).await;
    let body = json!({"station": "fake", "command_id": id, "elevation": token,
        "command": {"op": "start", "prompt": null, "spec": {"provider_instance": "claude",
        "cwd": "/w", "mode": null, "model": null, "title": null, "coordinator": coordinator}}});
    let call = post(c, h, "/api/commands/session.start", body);
    let (r, req) = tokio::join!(call, async {
        tokio::time::timeout(Duration::from_secs(2), Fixture::answer(station, accepted)).await
    });
    assert_eq!(r.status, 200, "{}", r.body);
    let req = req.unwrap();
    let sent: CoordinatorSpec =
        serde_json::from_value(req.params["command"]["spec"]["coordinator"].clone()).unwrap();
    let g = f.store.grant(sent.grant_id.clone()).await.unwrap().unwrap();
    (g, sent)
}

/// `station_spec` of a stored grant is what `owner_start` puts into the start
/// command, for the classes and caps that the station sees.
#[tokio::test]
async fn the_station_spec_of_a_grant_equals_the_spec_of_its_board_start() {
    let mut f = fixture().await;
    let mut station = f.connect().await;
    let c = board(Arc::clone(&f.ctx)).await;
    set_password(&c).await;
    let h = owner(&c).await;
    let fleet = with(
        spec(json!(["read", "mail", "drive", "spawn", "answer"])),
        json!({"spawn_cap": 4, "answer_cap": 40, "idle_ttl_ms": 600_000,
            "max_modes": {"claude": "acceptEdits"}, "scope": "fleet"}),
    );
    let (g, sent) = board_start(&mut f, &mut station, (&c, &h), ("st1", fleet)).await;
    assert_eq!(station_spec(&g, SpecFeatures::default()), sent);
    // The link negotiated every allowance: the flags are on the station spec.
    let desktop = with(
        spec(json!(["read", "desktop", "capture", "input"])),
        json!({"allow_recording": true, "allow_input": true, "allow_click": true,
            "input_scope": "any_window", "scope": "fleet"}),
    );
    let (g, sent) = board_start(&mut f, &mut station, (&c, &h), ("st2", desktop)).await;
    assert!(
        sent.allow_recording && sent.allow_input && sent.allow_click,
        "{sent:?}"
    );
    let all = SpecFeatures {
        recording: true,
        input: true,
        click: true,
        input_any: true,
    };
    assert_eq!(station_spec(&g, all), sent);
    // The caps of the deck never go on to a station.
    assert_eq!(sent.desktop_cap, None);
    assert_eq!(sent.recording_cap, None);
}
