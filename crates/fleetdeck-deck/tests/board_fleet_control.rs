//! ADR-008 on the deck, over the board API and over a fake station link
//! (§9 tests 2, 3, 4, 8, 8a, 8c, 8d and the deck part of 11): the creation of
//! a fleet-control grant needs a fresh elevation and `coordinator.v2` on the
//! link, the deck setting `fleet_control`, the new queries, the rechecks of
//! the authority until the send, and `coordinator.authority_check` with the
//! `coordinator.policy_now` round trip. (The decision of the ops is in
//! `src/store/fleet_control_tests.rs`.)

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
mod settings_support;

use std::sync::Arc;
use std::time::Duration;

use common::board::{Extras, board, board_with_extras};
use common::fleet::*;
use common::*;
use fleetdeck_deck::store::outbox::{Gate, OutboxItem, OutboxState};
use fleetdeck_deck::store::{GrantClass, NewCommand, QueryAnswer};
use fleetdeck_proto::RejectCode;
use fleetdeck_proto::command::{CommandEnvelope, CommandOrigin, GrantScope};
use fleetdeck_proto::command::{ControlSkill, CoordinatorSpec, SessionCommand};
use fleetdeck_proto::mutation::{AuthorityCheck, CoordinatorBinding, CoordinatorQuery};
use serde_json::{Value, json};
use settings_support::*;

const START: &str = "/api/commands/session.start";

fn start_body(id: &str, coordinator: Value, elevation: Option<&str>) -> Value {
    let mut spec = json!({"grant_id": "", "classes": ["read"], "idle_ttl_ms": 0, "expires_at": 0,
        "control_skill": {"version": "", "text": ""}});
    spec.as_object_mut()
        .unwrap()
        .extend(coordinator.as_object().unwrap().clone());
    let mut body = json!({"station": "fake", "command_id": id, "command": {"op": "start",
        "prompt": null, "spec": {"provider_instance": "claude", "cwd": "/w", "mode": null,
        "model": null, "title": null, "coordinator": spec}}});
    if let Some(e) = elevation {
        body["elevation"] = json!(e);
    }
    body
}

/// ADR-008 §9 test 2: the elevation, and the new fields in the stored grant
/// and in the start command.
#[tokio::test]
async fn a_fleet_control_grant_needs_a_fresh_elevation_and_is_stored_with_its_new_fields() {
    let mut f = fixture().await;
    let mut station = f.connect().await;
    let c = board(Arc::clone(&f.ctx)).await;
    set_password(&c).await;
    let h = owner(&c).await;
    let fleet = json!({"classes": ["read", "inspect"], "scope": "fleet"});
    let r = post(&c, &h, START, start_body("e1", fleet.clone(), None)).await;
    expect(&r, 400, "elevation_required");
    let r = post(&c, &h, START, start_body("e1", fleet.clone(), Some("nope"))).await;
    expect(&r, 400, "elevation_required");
    for classes in [["inspect"], ["manage"], ["answer"]] {
        let spec = json!({ "classes": classes });
        let r = post(&c, &h, START, start_body("e2", spec, None)).await;
        expect(&r, 400, "elevation_required");
    }
    no_request(&mut station, Duration::from_millis(200)).await;
    assert!(c.store.grants(10).await.unwrap().is_empty());
    // With the elevation the grant is stored and the station gets the fields.
    let token = elevate(&c, &h).await;
    let spec = json!({"classes": ["read", "answer"], "scope": "fleet", "answer_cap": 7});
    let call = post(&c, &h, START, start_body("e3", spec, Some(&token)));
    let (r, req) = tokio::join!(call, Fixture::answer(&mut station, accepted));
    assert_eq!(r.status, 200, "{}", r.body);
    let sent = &req.params["command"]["spec"]["coordinator"];
    assert_eq!(sent["scope"], "fleet");
    assert_eq!(sent["answer_cap"], 7);
    assert_eq!(sent["classes"], json!(["read", "answer"]));
    assert!(!req.params.to_string().contains(&token));
    let g = c.store.grants(10).await.unwrap().remove(0);
    let got = (g.scope, g.answer_cap, g.answer_used);
    assert_eq!(got, (GrantScope::Fleet, Some(7), 0));
    // The cap defaults to 500 with the class; the board's cap is 0 to 5000
    // and needs the class.
    let spec = json!({"classes": ["answer"]});
    let call = post(&c, &h, START, start_body("e4", spec, Some(&token)));
    let (r, _) = tokio::join!(call, Fixture::answer(&mut station, accepted));
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(c.store.grants(10).await.unwrap()[0].answer_cap, Some(500));
    for spec in [
        json!({"classes": ["answer"], "answer_cap": 5001}),
        json!({"classes": ["inspect"], "answer_cap": 1}),
        json!({"classes": ["read"], "answer_cap": 1}),
    ] {
        let r = post(&c, &h, START, start_body("e5", spec, Some(&token))).await;
        expect(&r, 400, "invalid_grant");
    }
    assert_eq!(c.store.grants(10).await.unwrap().len(), 2);
}

/// ADR-008 §9 test 3: the version gate, and the byte check of a lineage grant.
#[tokio::test]
async fn a_station_without_v2_gets_no_fleet_control_grant_and_a_lineage_start_keeps_its_bytes() {
    let mut f = fixture().await;
    let mut station = f.connect().await;
    let c = board(Arc::clone(&f.ctx)).await;
    set_password(&c).await;
    let h = owner(&c).await;
    let token = elevate(&c, &h).await;
    // A second live link of the station that did not negotiate v2.
    f.store.set_link_features(&f.station, 99, vec![]);
    let fleet = json!({"classes": ["inspect"]});
    let r = post(&c, &h, START, start_body("v1", fleet, Some(&token))).await;
    expect(&r, 409, "unsupported");
    no_request(&mut station, Duration::from_millis(200)).await;
    assert!(c.store.grants(10).await.unwrap().is_empty());
    // A lineage grant is not gated, and its start command has no new field.
    let lineage = json!({"classes": ["read", "mail"]});
    let call = post(&c, &h, START, start_body("v2", lineage, None));
    let (r, req) = tokio::join!(call, Fixture::answer(&mut station, accepted));
    assert_eq!(r.status, 200, "{}", r.body);
    let sent = req.params["command"]["spec"]["coordinator"].clone();
    let mut keys: Vec<&str> = sent
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        [
            "classes",
            "control_skill",
            "expires_at",
            "grant_id",
            "idle_ttl_ms"
        ]
    );
    let spec = CoordinatorSpec {
        grant_id: "g".to_owned(),
        classes: vec!["read".to_owned(), "mail".to_owned()],
        idle_ttl_ms: 1,
        expires_at: 2,
        control_skill: ControlSkill {
            version: "1".to_owned(),
            text: "t".to_owned(),
        },
        scope: GrantScope::Lineage,
        answer_cap: None,
        stations: Vec::new(),
    };
    let recorded = r#"{"grant_id":"g","classes":["read","mail"],"idle_ttl_ms":1,"expires_at":2,"control_skill":{"version":"1","text":"t"}}"#;
    assert_eq!(serde_json::to_string(&spec).unwrap(), recorded);
}

/// The deck setting `fleet_control`: off by default, on needs the elevation,
/// off does not; the store flag follows at once.
#[tokio::test]
async fn the_deck_setting_turns_on_with_an_elevation_and_off_without_one() {
    let tmp = tempfile::tempdir().unwrap();
    let f = fixture().await;
    let settings = deck_settings(tmp.path(), "name = \"alpha\"\n");
    let extras = Extras {
        settings: Some(settings),
        ..Extras::default()
    };
    let c = board_with_extras(Arc::clone(&f.ctx), extras).await;
    set_password(&c).await;
    let h = owner(&c).await;
    let view = get(&c, &h, "/api/settings/deck").await;
    assert_eq!(view.body["values"]["fleet_control"], false);
    let mut field = view.body["fields"].as_array().unwrap().iter();
    let field = field.find(|f| f["key"] == "fleet_control").unwrap();
    assert_eq!(
        (&field["danger"], &field["apply"]),
        (&json!(true), &json!("live"))
    );
    assert!(!f.store.fleet_control());
    let save = |on: bool, revision: &Value, elevation: Option<&str>| {
        let mut body = json!({"changes": {"fleet_control": on}, "revision": revision});
        if let Some(e) = elevation {
            body["elevation"] = json!(e);
        }
        post(&c, &h, "/api/settings/deck", body)
    };
    let r = save(true, &view.body["revision"], None).await;
    expect(&r, 400, "elevation_required");
    assert!(!f.store.fleet_control());
    let token = elevate(&c, &h).await;
    let r = save(true, &view.body["revision"], Some(&token)).await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert!(f.store.fleet_control() && c.state.live.get().fleet_control);
    let r = save(false, &r.body["revision"], None).await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert!(!f.store.fleet_control() && !c.state.live.get().fleet_control);
}

/// ADR-008 §9 test 4.
#[tokio::test]
async fn a_read_grant_gets_none_of_the_new_queries_and_the_setting_stops_them_all() {
    let r = rig().await;
    let f = &r.f;
    let s = fleetdeck_proto::SessionId::from;
    let new = || {
        vec![
            CoordinatorQuery::SessionTail {
                session_id: s("w1"),
                max_items: 5,
            },
            CoordinatorQuery::SessionDiff {
                session_id: s("w1"),
            },
            CoordinatorQuery::NeedsYou,
            CoordinatorQuery::Projects,
        ]
    };
    let read = bound(f, "coord-1", &[GrantClass::Read], false).await;
    for q in new() {
        assert_eq!(ask(f, &read, q).await.unwrap_err(), RejectCode::Forbidden);
    }
    let classes = [GrantClass::Read, GrantClass::Inspect, GrantClass::Answer];
    // A fleet-scope grant: the lineage of `coord-1` holds no session yet.
    let b = bound(f, "coord-1", &classes, true).await;
    // The class gate passes: the store answers (the station read of
    // `session_diff` is served by the link layer; see `fleet_control_reads`).
    for q in new() {
        match ask_any(f, &b, q.clone()).await.unwrap() {
            QueryAnswer::Value(_) => assert!(!matches!(q, CoordinatorQuery::SessionDiff { .. })),
            QueryAnswer::Station(_) => {
                assert!(matches!(q, CoordinatorQuery::SessionDiff { .. }));
            }
        }
    }
    f.store.set_fleet_control(false);
    for q in new() {
        assert_eq!(ask(f, &b, q).await.unwrap_err(), RejectCode::Forbidden);
    }
    // The ADR-006 reads still work with the setting off.
    let sessions = ask(f, &b, CoordinatorQuery::ListSessions).await.unwrap();
    assert_eq!(sessions["sessions"].as_array().unwrap().len(), 3);
}

fn attempt(item: &OutboxItem, id: &str) -> NewCommand {
    let kind = serde_json::from_value(item.payload.clone()).unwrap();
    let fleetdeck_deck::link::CommandKind::Session { command } = kind else {
        panic!("a session command");
    };
    let envelope = CommandEnvelope {
        authority: item.authority.clone(),
        command_id: id.into(),
        target_incarnation: None,
        retry_until: item.expires_at,
        origin: CommandOrigin::Coordinator,
        command,
    };
    NewCommand {
        command_id: id.into(),
        station_id: item.station_id.clone(),
        method: envelope.command.method().to_owned(),
        origin: "coordinator".to_owned(),
        envelope: serde_json::to_value(&envelope).unwrap(),
        retry_until: item.expires_at,
        author: None,
    }
}

/// An accepted call on a rig whose station is not linked (no router work).
async fn accepted_stop(loss_rig: &Fixture) -> (CoordinatorBinding, OutboxItem) {
    let b = bound(loss_rig, "coord-1", &[GrantClass::Drive], true).await;
    let r = call(loss_rig, &b, "k1", stop(loss_rig, "w1")).await;
    assert!(r.reject.is_none(), "{r:?}");
    (b, loss_rig.store.outbox().await.unwrap().remove(0))
}

/// A fixture whose station never links: the router has nothing to send, and
/// the test drives the attempts and the gates by hand.
async fn unlinked() -> Fixture {
    let f = fixture_with(coordinate()).await;
    let sessions = ["coord-1", "w1", "w2"].map(card).to_vec();
    let inc = f.inc.clone();
    f.store
        .apply_snapshot(f.station.clone(), inc, 0, sessions)
        .await
        .unwrap();
    f.store
        .set_link_features(&f.station, 1, vec!["coordinator.v2".to_owned()]);
    f.store.set_fleet_control(true);
    f
}

fn is_lost(item: &OutboxItem) -> bool {
    (item.state, item.reason.as_deref()) == (OutboxState::Dead, Some("authority_lost"))
}

/// ADR-008 §9 tests 8 (a), 8 (b) and 8c: a lost authority is never sent.
#[tokio::test]
async fn a_lost_authority_is_never_sent_before_the_first_attempt_the_first_send_or_a_resend() {
    for loss in LOSSES {
        // (a) between the acceptance and the first attempt.
        let f = unlinked().await;
        let (b, item) = accepted_stop(&f).await;
        lose(&f, &b, &item, loss).await;
        let started = f
            .store
            .start_attempt(item.outbox_id.clone(), attempt(&item, "c1"))
            .await
            .unwrap();
        assert!(started.is_none(), "{loss:?}");
        assert!(
            is_lost(
                &f.store
                    .outbox_item(item.outbox_id.clone())
                    .await
                    .unwrap()
                    .unwrap()
            ),
            "{loss:?}"
        );
        assert!(
            f.store.command("c1".into()).await.unwrap().is_none(),
            "{loss:?}"
        );
        // 8c: after `start_attempt` committed the pending command, before the first send.
        let f = unlinked().await;
        let (b, item) = accepted_stop(&f).await;
        let id = item.outbox_id.clone();
        f.store
            .start_attempt(id.clone(), attempt(&item, "c2"))
            .await
            .unwrap()
            .unwrap();
        let authority = lose(&f, &b, &item, loss).await;
        let station = item.station_id.clone();
        let gate = f
            .store
            .gate_authority("c2".into(), station, authority, Some("w1".into()))
            .await
            .unwrap();
        assert_eq!(gate, Gate::Lost, "{loss:?}");
        assert!(
            is_lost(&f.store.outbox_item(id).await.unwrap().unwrap()),
            "{loss:?}"
        );
        let cmd = f.store.command("c2".into()).await.unwrap().unwrap();
        assert_eq!(cmd.reason.as_deref(), Some("authority_lost"), "{loss:?}");
        // (b) between the first send and a resend.
        let f = unlinked().await;
        let (b, item) = accepted_stop(&f).await;
        let id = item.outbox_id.clone();
        f.store
            .start_attempt(id.clone(), attempt(&item, "c3"))
            .await
            .unwrap()
            .unwrap();
        f.store.mark_sent("c3".into()).await.unwrap().unwrap();
        let (station, authority) = (item.station_id.clone(), item.authority.clone().unwrap());
        let open = f
            .store
            .gate_authority("c3".into(), station.clone(), authority, Some("w1".into()))
            .await
            .unwrap();
        assert_eq!(open, Gate::Open);
        let authority = lose(&f, &b, &item, loss).await;
        let gate = f
            .store
            .gate_authority("c3".into(), station, authority, Some("w1".into()))
            .await
            .unwrap();
        assert_eq!(gate, Gate::Lost, "{loss:?}");
        assert!(
            is_lost(&f.store.outbox_item(id).await.unwrap().unwrap()),
            "{loss:?}"
        );
    }
}

#[tokio::test]
async fn a_station_without_a_live_link_makes_the_item_wait_and_ten_minutes_end_it() {
    let f = unlinked().await;
    let (_, item) = accepted_stop(&f).await;
    f.store.clear_link_features(&f.station, 1);
    let id = item.outbox_id.clone();
    assert!(
        f.store
            .start_attempt(id.clone(), attempt(&item, "c1"))
            .await
            .unwrap()
            .is_none()
    );
    let still = f.store.outbox_item(id.clone()).await.unwrap().unwrap();
    assert_eq!(still.state, OutboxState::Queued);
    f.store
        .set_link_features(&f.station, 2, vec!["coordinator.v2".to_owned()]);
    assert!(
        f.store
            .start_attempt(id.clone(), attempt(&item, "c1"))
            .await
            .unwrap()
            .is_some()
    );
    // An item that no attempt took ends after its 10 minutes.
    let b = bound(&f, "coord-1", &[GrantClass::Drive], true).await;
    assert!(call(&f, &b, "k2", stop(&f, "w2")).await.reject.is_none());
    f.det.advance(Duration::from_secs(601));
    f.store.reconcile_outbox().await.unwrap();
    let last = f.store.outbox().await.unwrap().pop().unwrap();
    assert_eq!(
        (last.state, last.reason.as_deref()),
        (OutboxState::Dead, Some("expired"))
    );
}

/// ADR-008 §9 tests 8a and 8d (the deck part): the authority travels on the
/// wire, and `coordinator.authority_check` reads the caller station's live
/// policy.
#[tokio::test]
async fn the_authority_check_asks_the_caller_station_for_its_live_policy() {
    let mut r = rig().await;
    let b = bound(
        &r.f,
        "coord-1",
        &[GrantClass::Drive, GrantClass::Mail],
        true,
    )
    .await;
    let receipt = call(&r.f, &b, "k1", stop(&r.f, "w1")).await;
    assert!(receipt.reject.is_none(), "{receipt:?}");
    let (id, params) = wire(&mut r).await;
    let want = json!({"grant_id": b.grant_id, "caller_station": r.f.station, "caller_session": "coord-1", "generation": 1});
    assert_eq!(params["authority"], want);
    let check = AuthorityCheck {
        grant_id: b.grant_id.clone(),
        generation: 1,
        caller_station: r.f.station.clone(),
        caller_session: "coord-1".into(),
        target_session: "w1".into(),
        command_id: id.clone(),
        launch: None,
    };
    let live = Some(json!({"fleet_control": true, "bound": true}));
    let valid = authority_check(&mut r, &check, live.clone()).await;
    assert_eq!(valid, json!({"valid": true}));
    // The caller station's switch is off while the deck setting stays on.
    let off = Some(json!({"fleet_control": false, "bound": true}));
    let got = authority_check(&mut r, &check, off).await;
    assert_eq!(got, json!({"valid": false, "reason": "caller_policy_off"}));
    let unbound = Some(json!({"fleet_control": true, "bound": false}));
    let got = authority_check(&mut r, &check, unbound).await;
    assert_eq!(got["reason"], "caller_unbound");
    // Not the command that the deck sent, or another target or generation.
    let other = AuthorityCheck {
        command_id: "nope".into(),
        launch: None,
        ..check.clone()
    };
    assert_eq!(
        authority_check(&mut r, &other, None).await["reason"],
        "unknown_command"
    );
    let other = AuthorityCheck {
        target_session: "w2".into(),
        ..check.clone()
    };
    assert_eq!(
        authority_check(&mut r, &other, None).await["reason"],
        "unknown_command"
    );
    let other = AuthorityCheck {
        generation: 2,
        ..check.clone()
    };
    assert_eq!(
        authority_check(&mut r, &other, None).await["reason"],
        "generation_changed"
    );
    // A target that became a coordinator.
    let w1 = bound(&r.f, "w1", &[GrantClass::Drive], false).await;
    let got = authority_check(&mut r, &check, live.clone()).await;
    assert_eq!(got["reason"], "target_excluded");
    r.f.store
        .revoke_grant(w1.grant_id, "owner".to_owned())
        .await
        .unwrap();
    // The deck setting, then the revocation.
    r.f.store.set_fleet_control(false);
    assert_eq!(
        authority_check(&mut r, &check, None).await["reason"],
        "fleet_control_off"
    );
    r.f.store.set_fleet_control(true);
    assert_eq!(authority_check(&mut r, &check, live).await["valid"], true);
    // Another grant keeps the station covered: the revoked grant answers
    // `grant_revoked`, not the admission refusal of an uncovered station.
    let _keep = bound(&r.f, "keep", &[GrantClass::Read], false).await;
    assert!(revoke(&r.f, &b).await);
    assert_eq!(
        authority_check(&mut r, &check, None).await["reason"],
        "grant_revoked"
    );
}

#[tokio::test]
async fn a_caller_station_that_cannot_answer_makes_the_check_invalid() {
    let mut r = rig().await;
    // The caller lives on another station that has no link to the deck.
    let st2 =
        r.f.store
            .pin_station(fleetdeck_deck::store::stations::NewStation {
                key: fleetdeck_core::identity::Identity::generate()
                    .unwrap()
                    .public_key(),
                name: "caller".to_owned(),
                scopes: coordinate(),
                dial_url: None,
            })
            .await
            .unwrap()
            .station_id;
    r.f.store
        .set_link_features(&st2, 5, vec!["coordinator.v2".to_owned()]);
    let b = bound_on(&r.f, &st2, "boss", &[GrantClass::Drive], true).await;
    let receipt = call_from(&r.f, &st2, &b, "k9", stop(&r.f, "w1")).await;
    assert!(receipt.reject.is_none(), "{receipt:?}");
    let (id, params) = wire(&mut r).await;
    assert_eq!(params["authority"]["caller_session"], "boss");
    let check = AuthorityCheck {
        grant_id: b.grant_id,
        generation: 1,
        caller_station: st2,
        caller_session: "boss".into(),
        target_session: "w1".into(),
        command_id: id,
        launch: None,
    };
    let got = authority_check(&mut r, &check, None).await;
    assert_eq!(got, json!({"valid": false, "reason": "caller_unreachable"}));
}

/// `command_status` lists the deliveries of the call (round 2/3 decisions).
#[tokio::test]
async fn the_status_of_a_call_lists_its_deliveries_and_a_lineage_call_has_no_authority_on_the_wire()
{
    let mut r = rig().await;
    let b = bound(&r.f, "coord-1", &[GrantClass::Drive], true).await;
    let none = ask(
        &r.f,
        &b,
        CoordinatorQuery::CommandStatus {
            command_id: "k1".into(),
        },
    )
    .await
    .unwrap();
    assert_eq!(none, json!({"receipt": null, "deliveries": []}));
    assert!(
        call(&r.f, &b, "k1", stop(&r.f, "w1"))
            .await
            .reject
            .is_none()
    );
    let _ = Fixture::answer(&mut r.station, accepted).await;
    let f = &r.f;
    f.wait("delivered", |f| {
        let (store, b) = (f.store.clone(), b.clone());
        let q = query(
            f,
            &b,
            CoordinatorQuery::CommandStatus {
                command_id: "k1".into(),
            },
        );
        let station = f.station.clone();
        async move {
            let QueryAnswer::Value(got) =
                store.coordinator_query(station, q).await.unwrap().unwrap()
            else {
                panic!("expected a value");
            };
            got["deliveries"][0]["state"] == "delivered"
        }
    })
    .await;
    let got = ask(
        f,
        &b,
        CoordinatorQuery::CommandStatus {
            command_id: "k1".into(),
        },
    )
    .await
    .unwrap();
    assert_eq!(got["receipt"]["state"], "accepted");
    assert_eq!(got["deliveries"].as_array().unwrap().len(), 1);
    assert_eq!(got["deliveries"][0]["reason"], Value::Null);
    // A lineage grant: no authority on the wire.
    let plain = bound(f, "coord-1", &[GrantClass::Drive], false).await;
    f.store
        .add_lineage(
            plain.grant_id.clone(),
            f.station.clone(),
            "w2".into(),
            "s1".to_owned(),
        )
        .await
        .unwrap();
    assert!(call(f, &plain, "k2", stop(f, "w2")).await.reject.is_none());
    let (_, params) = wire(&mut r).await;
    assert!(params.get("authority").is_none(), "{params}");
    let SessionCommand::Stop { .. } = serde_json::from_value(params["command"].clone()).unwrap()
    else {
        panic!("{params}");
    };
}
