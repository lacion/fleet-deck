//! The click allowance and the scope at each check (R13 P7b §9.2, task K1):
//! the admission, the execute check of the target, the final delivery pass;
//! and the board sign-out of a click (R13 P7b §12).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::{Arc, Mutex};

use fleetdeck_deck::store::LoginMethod;
use fleetdeck_deck::store::coordinator_desktop::{OpState, OpTool};
use fleetdeck_proto::coordinator_spec::InputScope;
use fleetdeck_proto::version::feature;
use serde_json::{Value, json};

use crate::coordinator_desktop_support::*;

fn payload(id: &str, hash: &str, phase: &str) -> Value {
    json!({"operation_id": id, "action_hash": hash, "phase": phase})
}

fn verdict_of(v: &Value) -> (bool, String) {
    (
        v["valid"].as_bool().unwrap(),
        v["reason"].as_str().unwrap_or_default().to_owned(),
    )
}

/// An `attempted` input operation `id` with hash `H`.
async fn attempted(w: &World, id: &str, tool: OpTool) {
    w.attempted(input_new_op(w, id, tool, "H", 0, &w.station()))
        .await;
}

#[tokio::test]
async fn the_execute_check_follows_the_click_allowance_of_the_grant() {
    let w = p7b_world(InputScope::OwnWindows, true).await;
    attempted(&w, "op-c", OpTool::PointerClick).await;
    let lost = (false, "authority_lost".to_owned());
    assert_eq!(
        verdict_of(&w.check(payload("op-c", "H", "execute")).await),
        (true, String::new())
    );
    // The click allowance is gone: the click is lost, the key is not.
    exec_tokens(&w.f.store, &w.b.grant_id, "allow_click = 0").await;
    assert_eq!(
        verdict_of(&w.check(payload("op-c", "H", "execute")).await),
        lost
    );
    attempted(&w, "op-k", OpTool::KeyPress).await;
    assert_eq!(
        verdict_of(&w.check(payload("op-k", "H", "execute")).await),
        (true, String::new())
    );
}

#[tokio::test]
async fn the_execute_check_follows_the_scope_and_its_feature() {
    let w = p7b_world(InputScope::OwnWindows, true).await;
    attempted(&w, "op-k", OpTool::KeyPress).await;
    assert_eq!(
        verdict_of(&w.check(payload("op-k", "H", "execute")).await),
        (true, String::new())
    );
    // The scope of the grant is the broader one now (a test change of the
    // row): the target link needs `desktop_input_any.v1`.
    exec_tokens(&w.f.store, &w.b.grant_id, "input_scope = 'any_window'").await;
    w.f.store.set_link_features(
        &w.f.station,
        97,
        without_feature(feature::DESKTOP_INPUT_ANY),
    );
    assert_eq!(
        verdict_of(&w.check(payload("op-k", "H", "execute")).await),
        (false, "authority_lost".to_owned())
    );
    w.f.store.clear_link_features(&w.f.station, 97);
    assert_eq!(
        verdict_of(&w.check(payload("op-k", "H", "execute")).await),
        (true, String::new())
    );
    // A click needs `desktop_click_mcp.v1` on the target link.
    attempted(&w, "op-c", OpTool::PointerClick).await;
    w.f.store.set_link_features(
        &w.f.station,
        97,
        without_feature(feature::DESKTOP_CLICK_MCP),
    );
    assert_eq!(
        verdict_of(&w.check(payload("op-c", "H", "execute")).await),
        (false, "authority_lost".to_owned())
    );
}

#[tokio::test]
async fn a_grant_that_loses_the_click_allowance_before_the_admission_is_refused_and_stores_nothing()
{
    let w = p7b_world(InputScope::AnyWindow, true).await;
    let (store, gid) = (w.f.store.clone(), w.b.grant_id.clone());
    // The target policy is asked after the authorization and before the
    // admission: the grant changes in between.
    w.fake.set(handler(move |req| {
        let (store, gid) = (store.clone(), gid.clone());
        async move {
            if req.method == "desktop.policy_now" {
                exec_tokens(&store, &gid, "allow_click = 0").await;
            }
            Some(p7b_answer(&req))
        }
    }));
    let r = w.call("c1", click_call("k1", 1, 2, None)).await;
    assert_eq!(code(&r), "input_click_not_allowed");
    assert_eq!(input_counters(&w).await, (0, 0, 0, 0));
    assert_eq!(w.fake.count("desktop.act"), 0);
}

#[tokio::test]
async fn a_click_allowance_withdrawn_during_the_send_keeps_the_result_and_delivers_authority_lost()
{
    for scope in [InputScope::OwnWindows, InputScope::AnyWindow] {
        let w = p7b_world(scope, true).await;
        let (store, gid) = (w.f.store.clone(), w.b.grant_id.clone());
        w.fake.set(handler(move |req| {
            let (store, gid) = (store.clone(), gid.clone());
            async move {
                if req.method == "desktop.act" {
                    exec_tokens(&store, &gid, "allow_click = 0").await;
                }
                Some(p7b_answer(&req))
            }
        }));
        let r = w.call("c1", click_call("k1", 1, 2, None)).await;
        assert_eq!(code(&r), "authority_lost", "{scope:?}");
        let o = op_of(&w, "k1").await;
        assert_eq!(r["operation_id"], o.operation_id.as_str());
        assert_eq!((o.state, o.result), (OpState::Done, Some(json!({}))));
        assert_eq!(input_counters(&w).await, (1, 1, 0, 1));
        let rows = audit_details(&w, "desktop.pointer_click").await;
        let stages: Vec<_> = rows.iter().map(|d| d["stage"].as_str().unwrap()).collect();
        assert_eq!(stages, ["attempt", "outcome", "delivery"], "{rows:?}");
        assert!(rows[2].get("window_id").is_none(), "{}", rows[2]);
    }
}

#[tokio::test]
async fn a_scope_withdrawn_during_the_send_is_authority_lost_for_the_broader_variants() {
    let w = p7b_world(InputScope::AnyWindow, true).await;
    let (store, gid) = (w.f.store.clone(), w.b.grant_id.clone());
    w.fake.set(handler(move |req| {
        let (store, gid) = (store.clone(), gid.clone());
        async move {
            if req.method == "desktop.act" {
                exec_tokens(&store, &gid, "allow_input = 0, input_scope = NULL").await;
            }
            Some(p7b_answer(&req))
        }
    }));
    let r = w.call("c1", key_call("k1", "a", None)).await;
    assert_eq!(code(&r), "authority_lost");
    assert_eq!(op_of(&w, "k1").await.state, OpState::Done);
}

/// The machine address of the fake target.
const TARGET_ADDR: &str = "10.7.7.7";

#[tokio::test]
async fn a_click_signs_the_sessions_of_the_target_machine_out_before_the_send_and_exposes_it() {
    for scope in [InputScope::OwnWindows, InputScope::AnyWindow] {
        let w = p7b_world(scope, true).await;
        w.f.store
            .set_link_peer(&w.station(), 1_000, TARGET_ADDR.parse().unwrap());
        let store = w.f.store.clone();
        let on_target = store
            .create_login_at(LoginMethod::Password, Some(TARGET_ADDR))
            .await
            .unwrap();
        let elsewhere = store
            .create_login_at(LoginMethod::Password, Some("10.8.8.8"))
            .await
            .unwrap();
        // The fake station looks at the sessions at the moment of the send.
        let seen: Arc<Mutex<Vec<(bool, bool)>>> = Arc::default();
        let cookies = Arc::new((on_target.cookie, elsewhere.cookie));
        let (probe, log, held) = (store.clone(), Arc::clone(&seen), Arc::clone(&cookies));
        w.fake.set(handler(move |req| {
            let (probe, log, held) = (probe.clone(), Arc::clone(&log), Arc::clone(&held));
            async move {
                if req.method == "desktop.act" {
                    let state = (
                        probe.login_session(&held.0).await.unwrap().is_some(),
                        probe.login_session(&held.1).await.unwrap().is_some(),
                    );
                    log.lock().unwrap().push(state);
                }
                Some(p7b_answer(&req))
            }
        }));
        let r = w.call("c1", click_call("k1", 1, 2, None)).await;
        assert_eq!(r["status"], "ok", "{scope:?}: {r}");
        assert_eq!(*seen.lock().unwrap(), vec![(false, true)], "{scope:?}");
        // The exposure of the machine is made by the click alone.
        let o = op_of(&w, "k1").await;
        assert_eq!(o.tool, OpTool::PointerClick);
        let (attempted, finished, addrs) = op_times(&w, &o.operation_id).await;
        assert!(attempted.is_some() && finished >= attempted);
        let snapshot: Vec<String> = serde_json::from_str(&addrs.unwrap()).unwrap();
        assert!(snapshot.contains(&TARGET_ADDR.to_owned()), "{snapshot:?}");
        store.invalidate_exposure();
        let exposed = store.exposed_addresses(store.now_ms()).await.unwrap();
        assert!(exposed.contains(TARGET_ADDR), "{scope:?}");
        assert!(!exposed.contains("10.8.8.8"));
    }
}

#[tokio::test]
async fn window_list_does_no_sign_out_and_exposes_nothing() {
    let w = p7b_world(InputScope::AnyWindow, false).await;
    w.f.store
        .set_link_peer(&w.station(), 1_000, TARGET_ADDR.parse().unwrap());
    let on_target =
        w.f.store
            .create_login_at(LoginMethod::Password, Some(TARGET_ADDR))
            .await
            .unwrap();
    let verdicts = new_verdicts();
    w.fake.set(list_station(
        window_list_value(w.station().as_str(), 1),
        Arc::clone(&verdicts),
        Around::default(),
    ));
    let r = w.query(window_list_query(None)).await;
    assert_eq!(r["status"], "ok", "{r}");
    assert!(
        w.f.store
            .login_session(&on_target.cookie)
            .await
            .unwrap()
            .is_some()
    );
    w.f.store.invalidate_exposure();
    let exposed =
        w.f.store
            .exposed_addresses(w.f.store.now_ms())
            .await
            .unwrap();
    assert!(exposed.is_empty(), "{exposed:?}");
}
