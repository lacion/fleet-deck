//! The click and the input tools under both scopes (R13 P7b §3.1, §4, §9,
//! §11.3, §11.5, task K1): the charges, the refusals that store nothing, the
//! features, and the exact purpose and action for each tool and scope.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use fleetdeck_deck::store::coordinator_desktop::{OpState, OpTool};
use fleetdeck_proto::coordinator_spec::InputScope;
use fleetdeck_proto::version::feature;
use serde_json::{Value, json};

use crate::coordinator_desktop_support::*;
use crate::{assert_no_request, has_token};

/// The params of a `desktop.act` without the authority.
fn act_bytes(w: &World, index: usize) -> Value {
    let mut params = w.fake.params_of("desktop.act")[index].clone();
    params.as_object_mut().unwrap().remove("authority");
    params
}

#[tokio::test]
async fn a_click_charges_once_and_sends_the_click_action() {
    let w = p7b_world(InputScope::OwnWindows, true).await;
    let r = w.call("c1", click_call("k1", 7351, 8462, None)).await;
    assert_eq!(r, json!({"status": "ok", "result": {}}), "{r}");
    // One desktop action, one input action, no character, one attempt.
    assert_eq!(input_counters(&w).await, (1, 1, 0, 1));
    let o = op_of(&w, "k1").await;
    assert_eq!(
        (o.tool, o.state, o.input_length),
        (OpTool::PointerClick, OpState::Done, Some(0))
    );
    let methods: Vec<String> = w.fake.log().into_iter().map(|(m, _)| m).collect();
    assert_eq!(
        methods,
        [
            "coordinator.policy_now",
            "desktop.policy_now",
            "desktop.act"
        ]
    );
    assert_eq!(
        w.fake.params_of("desktop.policy_now")[0],
        json!({"purpose": "input_click", "monitor": null})
    );
    assert_eq!(
        act_bytes(&w, 0),
        json!({"action": "input_pointer_click", "window_id": WINDOW, "x": 7351, "y": 8462})
    );
    // Two rows: the attempt with the length 0 and the charges, and the
    // outcome. No coordinate is stored or audited.
    let rows = audit_details(&w, "desktop.pointer_click").await;
    let stages: Vec<_> = rows.iter().map(|d| d["stage"].as_str().unwrap()).collect();
    assert_eq!(stages, ["attempt", "outcome"], "{rows:?}");
    assert_eq!(rows[0]["input_length"], 0);
    assert_eq!(
        rows[0]["charged"],
        json!({"desktop": 1, "capture": 0, "input": 1, "input_characters": 0})
    );
    assert!(rows[0].get("window_id").is_none(), "{}", rows[0]);
    assert_eq!(rows[1]["window_id"], WINDOW);
    let dump = format!("{}{}", w.audit_text().await, stored_text(&w).await);
    for secret in ["7351", "8462"] {
        assert!(!has_token(&dump, secret), "{secret} in {dump}");
    }
    // The click is a mutation of the input rate: the 20 of the grant stay
    // shared with the other input tools.
    let s = w.query(status_query("k1")).await;
    assert_eq!(s["result"]["state"], "done");
}

#[tokio::test]
async fn each_tool_and_scope_has_its_purpose_and_its_action() {
    type Case = (Value, &'static str, Value, &'static str);
    let own: [Case; 4] = [
        (
            key_call("a1", "Ctrl+a", None),
            "input",
            json!({"action": "input_key_press", "window_id": WINDOW, "chord": "Ctrl+a"}),
            "desktop.key_press",
        ),
        (
            text_call("a2", "héllo", None),
            "input",
            json!({"action": "input_type_text", "window_id": WINDOW, "text": "héllo"}),
            "desktop.type_text",
        ),
        (
            move_call("a3", 3, 4, None),
            "input",
            json!({"action": "input_pointer_move", "window_id": WINDOW, "x": 3, "y": 4}),
            "desktop.pointer_move",
        ),
        (
            click_call("a4", 5, 6, None),
            "input_click",
            json!({"action": "input_pointer_click", "window_id": WINDOW, "x": 5, "y": 6}),
            "desktop.pointer_click",
        ),
    ];
    let any: [Case; 4] = [
        (
            key_call("a1", "Ctrl+a", None),
            "input_any",
            json!({"action": "input_any_key_press", "window_id": WINDOW, "chord": "Ctrl+a"}),
            "desktop.key_press",
        ),
        (
            text_call("a2", "héllo", None),
            "input_any",
            json!({"action": "input_any_type_text", "window_id": WINDOW, "text": "héllo"}),
            "desktop.type_text",
        ),
        (
            move_call("a3", 3, 4, None),
            "input_any",
            json!({"action": "input_any_pointer_move", "window_id": WINDOW, "x": 3, "y": 4}),
            "desktop.pointer_move",
        ),
        (
            click_call("a4", 5, 6, None),
            "input_any_click",
            json!({"action": "input_any_pointer_click", "window_id": WINDOW, "x": 5, "y": 6}),
            "desktop.pointer_click",
        ),
    ];
    for (scope, cases) in [(InputScope::OwnWindows, own), (InputScope::AnyWindow, any)] {
        let w = p7b_world(scope, true).await;
        for (i, (call, purpose, action, audit)) in cases.into_iter().enumerate() {
            let r = w.call(&format!("c{i}"), call).await;
            assert_eq!(r["status"], "ok", "{scope:?} {purpose}: {r}");
            assert_eq!(
                w.fake.params_of("desktop.policy_now")[i],
                json!({"purpose": purpose, "monitor": null}),
                "{scope:?} {i}"
            );
            assert_eq!(act_bytes(&w, i), action, "{scope:?} {i}");
            // The audit name of each tool is the same in both scopes.
            assert_eq!(audit_details(&w, audit).await.len(), 2, "{audit}");
        }
        // Each call charged one desktop and one input action.
        assert_eq!(input_counters(&w).await, (4, 4, 5, 4), "{scope:?}");
    }
}

#[tokio::test]
async fn a_click_without_the_allowance_is_refused_before_anything_is_stored() {
    for scope in [InputScope::OwnWindows, InputScope::AnyWindow] {
        let w = p7b_world(scope, false).await;
        let r = w.call("c1", click_call("k1", 1, 2, None)).await;
        assert_eq!(code(&r), "input_click_not_allowed", "{scope:?}");
        assert_eq!(r["message"], "This grant does not allow pointer clicks.");
        assert_eq!(input_counters(&w).await, (0, 0, 0, 0));
        assert_no_request(&w);
        assert!(
            w.f.store
                .desktop_ops_recent(&w.b.grant_id, 5)
                .await
                .unwrap()
                .is_empty()
        );
        // The other input tools of the same grant work in both scopes.
        let r = w.call("c2", key_call("k2", "a", None)).await;
        assert_eq!(r["status"], "ok", "{scope:?}: {r}");
    }
}

#[tokio::test]
async fn window_list_needs_the_input_allowance_and_the_any_window_scope() {
    // The scope is `own_windows`.
    let w = p7b_world(InputScope::OwnWindows, true).await;
    let r = w.query(window_list_query(None)).await;
    assert_eq!(code(&r), "input_scope_not_allowed");
    assert_eq!(
        r["message"],
        "This grant does not allow input in other application windows."
    );
    assert_no_request(&w);
    // No input allowance at all.
    let w = world_with(&INPUT3, |_| {}).await;
    w.fake.set(p7b_station());
    let r = w.query(window_list_query(None)).await;
    assert_eq!(code(&r), "input_not_allowed");
    assert_no_request(&w);
    // A class is missing.
    let w = world_with(&BOTH, |_| {}).await;
    let r = w.query(window_list_query(None)).await;
    assert_eq!(code(&r), "not_granted");
    assert_no_request(&w);
}

#[tokio::test]
async fn a_missing_feature_refuses_before_anything_is_stored() {
    // (the missing feature, the world, the call).
    for (missing, scope, tool) in [
        (feature::DESKTOP_CLICK_MCP, InputScope::OwnWindows, "click"),
        (feature::DESKTOP_CLICK_MCP, InputScope::AnyWindow, "click"),
        (feature::DESKTOP_INPUT_ANY, InputScope::AnyWindow, "key"),
        (feature::DESKTOP_INPUT_ANY, InputScope::AnyWindow, "click"),
        (feature::DESKTOP_INPUT_MCP, InputScope::OwnWindows, "click"),
    ] {
        let w = p7b_world(scope, true).await;
        w.f.store
            .set_link_features(&w.f.station, 98, without_feature(missing));
        let call = if tool == "click" {
            click_call("k1", 1, 2, None)
        } else {
            key_call("k1", "a", None)
        };
        let r = w.call("c1", call).await;
        assert_eq!(code(&r), "desktop_unsupported", "{missing} {tool}");
        assert_eq!(input_counters(&w).await, (0, 0, 0, 0), "{missing} {tool}");
        assert_no_request(&w);
    }
    // `window_list` needs both `desktop_input_mcp.v1` and
    // `desktop_input_any.v1`.
    for missing in [feature::DESKTOP_INPUT_MCP, feature::DESKTOP_INPUT_ANY] {
        let w = p7b_world(InputScope::AnyWindow, false).await;
        w.f.store
            .set_link_features(&w.f.station, 98, without_feature(missing));
        let r = w.query(window_list_query(None)).await;
        assert_eq!(code(&r), "desktop_unsupported", "{missing}");
        assert_no_request(&w);
    }
}

#[tokio::test]
async fn the_features_that_a_scope_does_not_need_are_not_required() {
    // An own-window grant needs neither the click feature (for a key) nor
    // the broader-scope feature.
    let w = p7b_world(InputScope::OwnWindows, true).await;
    let mut wanted = without_feature(feature::DESKTOP_INPUT_ANY);
    wanted.retain(|f| f != feature::DESKTOP_CLICK_MCP);
    w.f.store.set_link_features(&w.f.station, 98, wanted);
    let r = w.call("c1", key_call("k1", "a", None)).await;
    assert_eq!(r["status"], "ok", "{r}");
    let r = w.call("c2", move_call("m1", 1, 2, None)).await;
    assert_eq!(r["status"], "ok", "{r}");
    // The click does need its feature.
    let r = w.call("c3", click_call("k3", 1, 2, None)).await;
    assert_eq!(code(&r), "desktop_unsupported");
}

#[tokio::test]
async fn a_target_link_without_the_click_feature_is_unsupported_and_nothing_is_sent() {
    let mut w = p7b_world(InputScope::OwnWindows, true).await;
    let beta_fake = w.link_beta().await;
    w.f.store
        .set_link_features(&w.beta, 98, without_feature(feature::DESKTOP_CLICK_MCP));
    let beta = w.beta.clone();
    let r = w
        .call("c1", click_call("k1", 1, 2, Some(beta.as_str())))
        .await;
    assert_eq!(code(&r), "desktop_unsupported");
    assert_eq!(beta_fake.count("desktop.act"), 0);
    assert_eq!(input_counters(&w).await, (0, 0, 0, 0));
    // The caller has the feature: the same link, with it, goes through.
    w.f.store.clear_link_features(&w.beta, 98);
    beta_fake.set(p7b_station());
    let r = w
        .call("c2", click_call("k2", 1, 2, Some(beta.as_str())))
        .await;
    assert_eq!(r["status"], "ok", "{r}");
    assert_eq!(beta_fake.count("desktop.act"), 1);
}
