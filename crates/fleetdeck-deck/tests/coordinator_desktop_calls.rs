//! The P6 deck handlers of `coordinator.desktop_call` (R13 P6 task T1b):
//! the authorization order, the policies, the admission, the send and each
//! outcome of the station, the duplicate rules, the in-flight slots, the
//! audit rows, and the status tool. The authority check and `desktop_info`
//! are in `coordinator_desktop_check.rs`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
mod coordinator_desktop_support;

use std::sync::Arc;
use std::time::Duration;

use coordinator_desktop_support::*;
use fleetdeck_deck::store::GrantClass;
use fleetdeck_deck::store::coordinator_desktop::{OpState, OpTool, request_key};
use fleetdeck_link::desktop_authority::action_hash_for;
use fleetdeck_proto::desktop::{DesktopAction, Urgency};
use fleetdeck_proto::link::Outcome;
use fleetdeck_proto::version::feature;
use serde_json::{Value, json};

const NO_REQUEST: &[&str] = &[];

async fn op(w: &World, request_id: &str) -> fleetdeck_deck::store::coordinator_desktop::DesktopOp {
    w.f.store
        .desktop_op_by_key(&w.b.grant_id, &request_key(request_id))
        .await
        .unwrap()
        .expect("an operation row")
}

#[tokio::test]
async fn a_notification_reaches_the_station_with_a_coordinator_authority_and_is_done() {
    let w = world().await;
    let reply = w.call("c1", notify("req-1", "Deploy done")).await;
    assert_eq!(reply, json!({"status": "ok", "result": {}}));
    // The two policies, in this order, then one act.
    let methods: Vec<String> = w.fake.log().into_iter().map(|(m, _)| m).collect();
    assert_eq!(
        methods,
        [
            "coordinator.policy_now",
            "desktop.policy_now",
            "desktop.act"
        ]
    );
    // The caller policy carries the full expected binding.
    let policy = &w.fake.params_of("coordinator.policy_now")[0];
    assert_eq!(
        *policy,
        json!({"caller_session": "coord-1", "binding": w.b})
    );
    assert_eq!(
        w.fake.params_of("desktop.policy_now")[0],
        json!({"purpose": "desktop", "monitor": null})
    );
    let act = &w.fake.params_of("desktop.act")[0];
    assert_eq!(act["action"], "notify");
    // The final title: the callsign prefix, built by the deck.
    assert_eq!(act["title"], "swift-fox: Deploy done");
    assert_eq!(act["body"], "body text");
    assert_eq!(act["urgency"], "normal");
    let row = op(&w, "req-1").await;
    assert_eq!(row.state, OpState::Done);
    assert_eq!(row.result, Some(json!({})));
    let authority = &act["authority"];
    assert_eq!(authority["action_id"], row.operation_id.as_str());
    assert_eq!(
        authority["coordinator"],
        serde_json::to_value(w.context()).unwrap()
    );
    let action = DesktopAction::Notify {
        title: "swift-fox: Deploy done".into(),
        body: "body text".into(),
        urgency: Urgency::Normal,
    };
    let hash = action_hash_for(&action, &w.node(), Some(&w.context()));
    assert_eq!(authority["action_hash"], hash.as_str());
    assert_eq!(row.action_hash.as_deref(), Some(hash.as_str()));
    assert!(authority.get("quiet_hours").is_none_or(Value::is_null));
}

#[tokio::test]
async fn the_label_is_the_session_id_when_the_callsign_does_not_fit() {
    let w = world().await;
    let mut coord = common::fleet::card("coord-1");
    coord.callsign = Some("Swift Fox".to_owned());
    w.f.store
        .apply_snapshot(w.station(), w.f.inc.clone(), 0, vec![coord])
        .await
        .unwrap();
    let reply = w.call("c1", notify("req-1", "Hi")).await;
    assert_eq!(reply["status"], "ok", "{reply}");
    let act = &w.fake.params_of("desktop.act")[0];
    assert_eq!(act["title"], "coord-1: Hi");
    assert_eq!(act["authority"]["coordinator"]["caller_label"], "coord-1");
}

#[tokio::test]
async fn a_long_title_is_cut_at_80_scalars_after_the_prefix_and_markup_is_stripped() {
    let w = world().await;
    let long = "x".repeat(80);
    let reply = w.call("c1", notify("req-1", &long)).await;
    assert_eq!(reply["status"], "ok", "{reply}");
    let title = w.fake.params_of("desktop.act")[0]["title"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(title.chars().count(), 80);
    assert!(title.starts_with("swift-fox: xxx"), "{title}");
    let reply = w.call("c2", notify("req-2", "<b>bold</b> news")).await;
    assert_eq!(reply["status"], "ok", "{reply}");
    assert_eq!(
        w.fake.params_of("desktop.act")[1]["title"],
        "swift-fox: bold news"
    );
}

#[tokio::test]
async fn a_preview_returns_the_window_handle_and_a_close_names_it() {
    let w = world().await;
    let reply = w
        .call("c1", preview("req-1", "http://localhost:5173/app"))
        .await;
    assert_eq!(
        reply,
        json!({"status": "ok", "result": {"window_id": WINDOW}})
    );
    let act = &w.fake.params_of("desktop.act")[0];
    assert_eq!(act["action"], "browser_open");
    assert_eq!(act["url"], "http://localhost:5173/app");
    assert_eq!(act["session"], "coord-1");
    assert!(act["monitor"].is_null());
    assert!(act["authority"]["coordinator"].is_object());
    let reply = w.call("c2", close("req-2", WINDOW)).await;
    assert_eq!(reply, json!({"status": "ok", "result": {}}));
    let act = &w.fake.params_of("desktop.act")[1];
    assert_eq!(act["action"], "preview_close");
    assert_eq!(act["window_id"], WINDOW);
    let row = op(&w, "req-2").await;
    assert_eq!(row.resource, json!({"window_id": WINDOW}));
    assert_eq!(row.tool, OpTool::WindowClose);
}

#[tokio::test]
async fn a_preview_answer_without_a_window_handle_is_a_failure() {
    let w = world().await;
    w.fake.set(except("desktop.act", |_| {
        Some(ok(json!({"address": null, "info": info()})))
    }));
    let reply = w.call("c1", preview("req-1", "http://localhost:1")).await;
    assert_eq!(code(&reply), "desktop_failed");
    assert!(reply["operation_id"].is_string());
    assert_eq!(op(&w, "req-1").await.state, OpState::Failed);
}

#[tokio::test]
async fn a_url_that_fails_the_grammar_is_refused_and_nothing_goes_to_the_station() {
    let w = world().await;
    let reply = w
        .call("c1", preview("req-1", "ftp://host/SENTINEL-URL-4711"))
        .await;
    assert_eq!(code(&reply), "invalid_params");
    assert_eq!(
        reply["message"],
        "The desktop tool arguments are not valid."
    );
    w.fake.quiet(Duration::from_millis(150), NO_REQUEST).await;
    assert!(
        w.f.store
            .desktop_ops_recent(&w.b.grant_id, 5)
            .await
            .unwrap()
            .is_empty()
    );
    // The refusal has its own row and it never holds the URL.
    let rows = w.audit_rows(Some("desktop.preview_open")).await;
    assert_eq!(rows.len(), 1);
    let detail: Value = serde_json::from_str(&rows[0].2).unwrap();
    assert_eq!(
        (detail["stage"].as_str(), detail["code"].as_str()),
        (Some("refused"), Some("invalid_params"))
    );
    assert!(!w.audit_text().await.contains("SENTINEL-URL-4711"));
}

#[tokio::test]
async fn every_refusal_before_the_policies_has_its_code_and_asks_no_station() {
    let w = world().await;
    // A caller without a grant.
    let mut env = w.call_env("c1", notify("r1", "t"));
    env["caller"] = json!("ghost");
    assert_eq!(
        code(&reply(w.call_raw(Some("c1"), env).await)),
        "not_granted"
    );
    // A stale binding (the full binding is compared).
    let mut env = w.call_env("c2", notify("r1", "t"));
    env["binding"]["generation"] = json!(2);
    assert_eq!(
        code(&reply(w.call_raw(Some("c2"), env).await)),
        "binding_stale"
    );
    let mut env = w.call_env("c3", notify("r1", "t"));
    env["binding"]["native_id"] = json!("another");
    assert_eq!(
        code(&reply(w.call_raw(Some("c3"), env).await)),
        "binding_stale"
    );
    // The switches.
    w.f.store.set_fleet_control(false);
    assert_eq!(
        code(&w.call("c4", notify("r1", "t")).await),
        "fleet_control_off"
    );
    w.f.store.set_fleet_control(true);
    w.live.publish(deck_live(false));
    assert_eq!(
        code(&w.call("c5", notify("r1", "t")).await),
        "desktop_control_off"
    );
    w.live.publish(deck_live(true));
    // A target outside the grant is refused before any request to a station.
    let outside = json!({"tool": "notify", "arguments": {
        "request_id": "r1", "station": "not-in-the-grant", "title": "t", "body": "b"}});
    assert_eq!(code(&w.call("c6", outside).await), "not_granted");
    w.fake.quiet(Duration::from_millis(200), NO_REQUEST).await;
    // Nothing was stored and no row was written (no authorization passed).
    assert!(
        w.f.store
            .desktop_ops_recent(&w.b.grant_id, 5)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(w.audit_rows(None).await.is_empty());
}

#[tokio::test]
async fn a_grant_without_the_class_an_expired_and_an_idle_grant_are_not_granted() {
    let w = world_with(&[GrantClass::Capture], |_| {}).await;
    assert_eq!(code(&w.call("c1", notify("r1", "t")).await), "not_granted");
    let w = world().await;
    w.f.det.advance(Duration::from_secs(31 * 60));
    assert_eq!(code(&w.call("c1", notify("r1", "t")).await), "not_granted");
    let w = world().await;
    w.f.det.advance(Duration::from_secs(13 * 3600));
    assert_eq!(code(&w.call("c1", notify("r1", "t")).await), "not_granted");
    w.fake.quiet(Duration::from_millis(100), NO_REQUEST).await;
}

#[tokio::test]
async fn the_features_of_the_caller_and_of_the_target_link_are_checked() {
    let mut w = world().await;
    // The caller link lacks the features.
    w.strip_features();
    assert_eq!(
        code(&w.call("c1", notify("r1", "t")).await),
        "desktop_unsupported"
    );
    w.f.store.clear_link_features(&w.station(), 97);
    // A target without a link.
    let beta = w.beta.as_str().to_owned();
    let to_beta = |rid: &str| {
        json!({"tool": "notify", "arguments": {
            "request_id": rid, "station": beta, "title": "t", "body": "b"}})
    };
    assert_eq!(
        code(&w.call("c2", to_beta("r2")).await),
        "station_unreachable"
    );
    // A target link that lacks the desktop features.
    let some = vec![feature::COORDINATOR_V2.to_owned()];
    w.f.store.set_link_features(&w.beta, 1, some);
    assert_eq!(
        code(&w.call("c3", to_beta("r3")).await),
        "desktop_unsupported"
    );
    // A target whose features are all there but whose live link is not (no
    // desktop epoch to bind): unreachable.
    let all = [
        feature::COORDINATOR_V2,
        feature::DESKTOP,
        feature::DESKTOP_AUTHORITY,
        feature::DESKTOP_MCP,
    ]
    .map(str::to_owned)
    .to_vec();
    w.f.store.set_link_features(&w.beta, 1, all);
    assert_eq!(
        code(&w.call("c4", to_beta("r4")).await),
        "station_unreachable"
    );
    w.fake.quiet(Duration::from_millis(100), NO_REQUEST).await;
    let _ = &mut w;
}

#[tokio::test]
async fn the_envelope_checks_have_their_codes_and_the_wire_errors_are_only_malformed_forms() {
    let w = world().await;
    let mut env = w.call_env("c1", notify("r1", "t"));
    env["target_incarnation"] = json!("another-deck");
    assert_eq!(
        code(&reply(w.call_raw(Some("c1"), env).await)),
        "incarnation_mismatch"
    );
    let mut env = w.call_env("c2", notify("r1", "t"));
    env["retry_until"] = json!(w.now() - 1);
    assert_eq!(code(&reply(w.call_raw(Some("c2"), env).await)), "expired");
    let mut env = w.call_env("c3", notify("r1", "t"));
    env["retry_until"] = json!(w.now() + 15_001);
    assert_eq!(
        code(&reply(w.call_raw(Some("c3"), env).await)),
        "invalid_params"
    );
    assert_eq!(
        code(&w.call("c4", notify("bad id!", "t")).await),
        "invalid_params"
    );
    // The wire errors: a frame command ID that differs, and a parse failure.
    for out in [
        w.call_raw(Some("other"), w.call_env("c5", notify("r1", "t")))
            .await,
        w.call_raw(Some("c6"), json!({"junk": true})).await,
        w.call_raw(Some("c7"), {
            let mut env = w.call_env("c7", notify("r1", "t"));
            env["extra"] = json!(1);
            env
        })
        .await,
    ] {
        let Outcome::Err(e) = out else {
            panic!("a wire error");
        };
        assert_eq!(e.code, "invalid_params");
    }
    // The tools of other tasks keep the stub.
    let shot = json!({"tool": "screenshot", "arguments": {"request_id": "r1", "monitor": "DP-1"}});
    let wall = json!({"tool": "wall_show", "arguments": {"request_id": "r1", "display_id": "d",
        "content": {"kind": "session", "session": {"station_id": "s", "session_id": "x"}}}});
    for call in [shot, wall] {
        let Outcome::Err(e) = w.call_raw(Some("c8"), w.call_env("c8", call)).await else {
            panic!("the stub");
        };
        assert_eq!(e.code, "desktop_unsupported");
    }
    w.fake.quiet(Duration::from_millis(100), NO_REQUEST).await;
}

#[tokio::test]
async fn the_caller_policy_decides_and_an_uncertain_answer_is_authority_unknown() {
    let w = world().await;
    let cases = [
        (
            json!({"fleet_control": false, "bound": true, "binding_matches": true}),
            "not_granted",
        ),
        (
            json!({"fleet_control": true, "bound": false, "binding_matches": true}),
            "not_granted",
        ),
        (
            json!({"fleet_control": true, "bound": true, "binding_matches": false}),
            "not_granted",
        ),
        // A missing `binding_matches`, and a malformed answer.
        (
            json!({"fleet_control": true, "bound": true}),
            "authority_unknown",
        ),
        (json!({"nonsense": 1}), "authority_unknown"),
    ];
    for (i, (answer, want)) in cases.into_iter().enumerate() {
        w.fake.set(except("coordinator.policy_now", move |_| {
            Some(ok(answer.clone()))
        }));
        let reply = w
            .call(&format!("c{i}"), notify(&format!("r{i}"), "t"))
            .await;
        assert_eq!(code(&reply), want, "{i}");
    }
    // A refusal of the station and a lost answer.
    w.fake.set(except("coordinator.policy_now", |_| {
        Some(fail("forbidden", "no"))
    }));
    assert_eq!(
        code(&w.call("e1", notify("e1", "t")).await),
        "authority_unknown"
    );
    // No target policy and no act ever ran.
    assert_eq!(w.fake.count("desktop.policy_now"), 0);
    assert_eq!(w.fake.count("desktop.act"), 0);
    assert!(
        w.f.store
            .desktop_ops_recent(&w.b.grant_id, 5)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn a_caller_policy_that_never_answers_is_authority_unknown_after_three_seconds() {
    let w = world().await;
    w.fake.set(except("coordinator.policy_now", |_| None));
    let started = std::time::Instant::now();
    assert_eq!(
        code(&w.call("c1", notify("r1", "t")).await),
        "authority_unknown"
    );
    let took = started.elapsed();
    assert!(
        took >= Duration::from_millis(2_900) && took < Duration::from_secs(6),
        "{took:?}"
    );
    assert_eq!(w.fake.count("desktop.act"), 0);
}

#[tokio::test]
async fn the_target_policy_decides_and_an_uncertain_answer_is_authority_unknown() {
    let w = world().await;
    let cases = [
        (
            json!({"allow_fleet_control": false, "allow_desktop": true,
            "allow_desktop_capture": true, "monitor_allowed": true}),
            "not_granted",
        ),
        (
            json!({"allow_fleet_control": true, "allow_desktop": false,
            "allow_desktop_capture": true, "monitor_allowed": true}),
            "not_granted",
        ),
        (json!({"allow_fleet_control": true}), "authority_unknown"),
    ];
    for (i, (answer, want)) in cases.into_iter().enumerate() {
        w.fake.set(except("desktop.policy_now", move |_| {
            Some(ok(answer.clone()))
        }));
        let reply = w
            .call(&format!("c{i}"), notify(&format!("r{i}"), "t"))
            .await;
        assert_eq!(code(&reply), want, "{i}");
    }
    w.fake.set(except("desktop.policy_now", |_| {
        Some(fail("desktop_unsupported", "no"))
    }));
    assert_eq!(
        code(&w.call("e1", notify("e1", "t")).await),
        "authority_unknown"
    );
    assert_eq!(w.fake.count("desktop.act"), 0);
    assert!(
        w.f.store
            .desktop_ops_recent(&w.b.grant_id, 5)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn a_grant_revoked_during_the_policies_is_not_granted_and_nothing_is_sent_or_charged() {
    let w = world().await;
    let (store, gid) = (w.f.store.clone(), w.b.grant_id.clone());
    // Revoke on the target policy request, then answer yes.
    w.fake.set(handler(move |req| {
        let (store, gid) = (store.clone(), gid.clone());
        async move {
            if req.method == "desktop.policy_now" {
                store.revoke_grant(gid, "test".into()).await.unwrap();
            }
            Some(standard_answer(&req))
        }
    }));
    assert_eq!(code(&w.call("c1", notify("r1", "t")).await), "not_granted");
    assert_eq!(w.fake.count("desktop.act"), 0);
    assert!(
        w.f.store
            .desktop_ops_recent(&w.b.grant_id, 5)
            .await
            .unwrap()
            .is_empty()
    );
    let g =
        w.f.store
            .grant(w.b.grant_id.clone())
            .await
            .unwrap()
            .unwrap();
    assert_eq!((g.desktop_used, g.desktop_attempts), (0, 0));
}

#[tokio::test]
async fn a_link_that_loses_its_features_after_admission_sends_nothing_and_fails_the_operation() {
    let w = world().await;
    let store = w.f.store.clone();
    let station = w.station();
    // The target policy answer: the link then has a second connection
    // without any feature (a downgrade).
    w.fake.set(except("desktop.policy_now", move |req| {
        store.set_link_features(&station, 97, Vec::new());
        Some(standard_answer(req))
    }));
    // The caller features are checked first (before the policies) and pass:
    // the downgrade happens after them.
    let reply = w.call("c1", notify("r1", "t")).await;
    assert_eq!(code(&reply), "desktop_unsupported");
    assert!(reply["operation_id"].is_string());
    assert_eq!(w.fake.count("desktop.act"), 0);
    let row = op(&w, "r1").await;
    assert_eq!(
        (row.state, row.code.as_deref()),
        (OpState::Failed, Some("desktop_unsupported"))
    );
    // The attempt row was written, and so was the outcome row.
    let rows = w.audit_rows(Some("desktop.notify")).await;
    assert_eq!(rows.len(), 2, "{rows:?}");
}

#[tokio::test]
async fn a_duplicate_returns_the_recorded_state_and_a_changed_payload_conflicts() {
    let w = world().await;
    let first = w.call("c1", notify("req-1", "Same")).await;
    assert_eq!(first["status"], "ok");
    let row = op(&w, "req-1").await;
    // The same request ID and payload: the recorded state, no second act.
    let again = w.call("c2", notify("req-1", "Same")).await;
    assert_eq!(
        again,
        json!({"status": "ok", "result": {
            "request_id": "req-1", "operation_id": row.operation_id.as_str(),
            "station": w.station().as_str(), "state": "done", "result": {}}})
    );
    assert_eq!(w.fake.count("desktop.act"), 1);
    // Another payload under the same request ID.
    let other = w.call("c3", notify("req-1", "Different")).await;
    assert_eq!(code(&other), "request_id_conflict");
    assert_eq!(
        other["message"],
        "This request ID was used for another desktop request."
    );
    // Another request ID is another action.
    assert_eq!(w.call("c4", notify("req-2", "Same")).await["status"], "ok");
    assert_eq!(w.fake.count("desktop.act"), 2);
    // The duplicate never preserves authority across a revoke.
    w.f.store
        .revoke_grant(w.b.grant_id.clone(), "test".into())
        .await
        .unwrap();
    assert_eq!(
        code(&w.call("c5", notify("req-1", "Same")).await),
        "not_granted"
    );
}

#[tokio::test]
async fn a_stored_budget_refusal_replays_with_its_code_and_the_status_shows_it() {
    let w = world_with(&[GrantClass::Desktop], |g| g.desktop_cap = Some(1)).await;
    assert_eq!(w.call("c1", notify("r1", "t")).await["status"], "ok");
    let refused = w.call("c2", notify("r2", "t")).await;
    assert_eq!(code(&refused), "desktop_cap_reached");
    let replay = w.call("c3", notify("r2", "t")).await;
    assert_eq!(code(&replay), "desktop_cap_reached");
    assert_eq!(w.fake.count("desktop.act"), 1);
    let status = w.query(status_query("r2")).await;
    assert_eq!(
        status,
        json!({"status": "ok", "result": {"request_id": "r2", "state": "failed",
            "error": "desktop_cap_reached",
            "message": "This grant has used its desktop action limit."}})
    );
}

#[tokio::test]
async fn the_station_outcomes_map_to_final_states_and_codes() {
    let w = world().await;
    let cases = [
        ("authority_lost", "authority_lost"),
        ("authority_unknown", "authority_unknown"),
        ("desktop_window_not_found", "desktop_window_not_found"),
        ("forbidden", "desktop_forbidden"),
        ("invalid_params", "invalid_desktop_action"),
        ("not_found", "desktop_not_found"),
        ("conflict", "desktop_conflict"),
        ("too_many_requests", "desktop_overloaded"),
        ("failed", "desktop_failed"),
        ("timeout", "desktop_timeout"),
        ("something_new", "desktop_failed"),
    ];
    for (i, (wire, want)) in cases.into_iter().enumerate() {
        w.fake.set(except("desktop.act", move |_| {
            Some(fail(wire, "STATION-TEXT-SECRET"))
        }));
        let rid = format!("r{i}");
        let reply = w.call(&format!("c{i}"), close(&rid, WINDOW)).await;
        assert_eq!(code(&reply), want, "{wire}");
        assert!(
            !reply.to_string().contains("STATION-TEXT-SECRET"),
            "{reply}"
        );
        let row = op(&w, &rid).await;
        assert_eq!(reply["operation_id"], row.operation_id.as_str());
        assert_eq!(
            (row.state, row.code.as_deref()),
            (OpState::Failed, Some(want)),
            "{wire}"
        );
    }
    assert!(!w.audit_text().await.contains("STATION-TEXT-SECRET"));
}

#[tokio::test]
async fn a_link_that_ends_after_the_send_leaves_the_operation_unknown() {
    let mut w = world().await;
    let beta_fake = w.link_beta().await;
    // The target beta takes the act and drops the link without an answer.
    beta_fake.set(except("desktop.act", |req| {
        req.control.close("test", "the link drops");
        None
    }));
    let beta = w.beta.as_str().to_owned();
    let call = json!({"tool": "notify", "arguments": {
        "request_id": "r1", "station": beta, "title": "t", "body": "b"}});
    let reply = w.call("c1", call.clone()).await;
    assert_eq!(code(&reply), "desktop_timeout");
    assert_eq!(
        reply["message"],
        "The station did not answer in time; the action may have run."
    );
    let row = op(&w, "r1").await;
    assert_eq!(reply["operation_id"], row.operation_id.as_str());
    assert_eq!(row.state, OpState::Unknown);
    // An unknown outcome never runs again: the target link is gone, so the
    // duplicate is refused at the target check, and nothing is sent.
    let again = w.call("c2", call).await;
    assert_eq!(code(&again), "station_unreachable");
    assert_eq!(beta_fake.count("desktop.act"), 1);
    let status = w.query(status_query("r1")).await;
    assert_eq!(status["result"]["state"], "unknown");
    assert_eq!(status["result"]["station"], beta.as_str());
}

#[tokio::test]
async fn a_third_mutation_of_a_grant_in_flight_is_busy_and_the_slots_are_released() {
    let w = world().await;
    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    let held = Arc::clone(&gate);
    w.fake.set(handler(move |req| {
        let gate = Arc::clone(&held);
        async move {
            if req.method == "desktop.act" {
                gate.acquire().await.unwrap().forget();
            }
            Some(standard_answer(&req))
        }
    }));
    let first = w.call("c1", close("r1", WINDOW));
    let second = w.call("c2", close("r2", WINDOW));
    let third = async {
        w.fake.wait_for("desktop.act", 2).await;
        let busy = w.call("c3", close("r3", WINDOW)).await;
        // While two are in flight, the status of a running one is pending.
        let pending = w.query(status_query("r1")).await;
        gate.add_permits(2);
        (busy, pending)
    };
    let (a, b, (busy, pending)) = tokio::join!(first, second, third);
    assert_eq!(
        (a["status"].as_str(), b["status"].as_str()),
        (Some("ok"), Some("ok"))
    );
    assert_eq!(code(&busy), "busy");
    assert_eq!(
        busy["message"],
        "The desktop operation cannot start because its capacity limit is in use."
    );
    assert_eq!(pending["result"]["state"], "pending");
    // The busy operation was accepted (and charged) and ended failed; nothing
    // was sent for it.
    let row = op(&w, "r3").await;
    assert_eq!(
        (row.state, row.code.as_deref()),
        (OpState::Failed, Some("busy"))
    );
    assert_eq!(w.fake.count("desktop.act"), 2);
    // The slots are free again.
    w.fake.set(standard());
    assert_eq!(w.call("c4", close("r4", WINDOW)).await["status"], "ok");
}
