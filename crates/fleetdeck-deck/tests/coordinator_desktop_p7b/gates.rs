//! The live station gates of the three P7b policy purposes (R13 P7b §9.2,
//! task K1): each false flag gives its code, and an answer that does not fit
//! the purpose is malformed (`authority_unknown`). Every refusal comes before
//! the admission: nothing is charged and nothing is sent.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use fleetdeck_proto::coordinator_spec::InputScope;
use serde_json::{Value, json};

use crate::coordinator_desktop_support::*;

/// The five flags of the input answer.
const FLAGS: [&str; 5] = [
    "allow_fleet_control",
    "allow_desktop",
    "allow_desktop_capture",
    "allow_desktop_input",
    "monitor_allowed",
];

/// The station answers `desktop.policy_now` with `answer`, whatever the
/// purpose; everything else as the P7b station.
fn answering(answer: Value) -> Handler {
    p7b_except("desktop.policy_now", move |_| Some(ok(answer.clone())))
}

/// One refused call: the code, nothing charged, no `desktop.act`.
async fn refused(w: &World, id: &str, call: Value, expected: &str) {
    let r = w.call(id, call).await;
    assert_eq!(code(&r), expected, "{id}: {r}");
    assert_eq!(w.fake.count("desktop.act"), 0, "{id}");
    assert_eq!(input_counters(w).await, (0, 0, 0, 0), "{id}");
}

#[tokio::test]
async fn the_click_gate_of_a_preview_click() {
    let w = p7b_world(InputScope::OwnWindows, true).await;
    w.fake.set(answering(gates_policy(
        "input_click",
        Some("allow_desktop_click"),
    )));
    refused(
        &w,
        "c1",
        click_call("k1", 1, 2, None),
        "input_click_disabled",
    )
    .await;
    // Each of the five flags: the P7 code.
    for (i, flag) in FLAGS.into_iter().enumerate() {
        w.fake
            .set(answering(gates_policy("input_click", Some(flag))));
        refused(
            &w,
            &format!("f{i}"),
            click_call(&format!("kf{i}"), 1, 2, None),
            "not_granted",
        )
        .await;
    }
    // The answer must have the click gate and no other gate.
    let mut missing = gates_policy("input_click", None);
    missing
        .as_object_mut()
        .unwrap()
        .remove("allow_desktop_click");
    w.fake.set(answering(missing));
    refused(&w, "m1", click_call("km1", 1, 2, None), "authority_unknown").await;
    let mut extra = gates_policy("input_click", None);
    extra["allow_desktop_input_any"] = json!(true);
    w.fake.set(answering(extra));
    refused(&w, "e1", click_call("ke1", 1, 2, None), "authority_unknown").await;
    // The five flags of the P7 answer are not the closed answer.
    w.fake.set(answering(input_policy(None)));
    refused(&w, "p1", click_call("kp1", 1, 2, None), "authority_unknown").await;
    // Everything on: the click goes through.
    w.fake.set(answering(gates_policy("input_click", None)));
    let r = w.call("ok", click_call("kok", 1, 2, None)).await;
    assert_eq!(r["status"], "ok", "{r}");
}

#[tokio::test]
async fn the_broader_scope_gates_of_keys_movement_and_clicks() {
    let w = p7b_world(InputScope::AnyWindow, true).await;
    // Keyboard and movement: the broader-scope gate.
    w.fake.set(answering(gates_policy(
        "input_any",
        Some("allow_desktop_input_any"),
    )));
    refused(&w, "a1", key_call("ka1", "a", None), "input_any_disabled").await;
    refused(&w, "a2", text_call("ka2", "x", None), "input_any_disabled").await;
    refused(&w, "a3", move_call("ka3", 1, 2, None), "input_any_disabled").await;
    for (i, flag) in FLAGS.into_iter().enumerate() {
        w.fake.set(answering(gates_policy("input_any", Some(flag))));
        refused(
            &w,
            &format!("f{i}"),
            key_call(&format!("kf{i}"), "a", None),
            "not_granted",
        )
        .await;
    }
    // The answer must have the broader gate and no click gate.
    let mut missing = gates_policy("input_any", None);
    missing
        .as_object_mut()
        .unwrap()
        .remove("allow_desktop_input_any");
    w.fake.set(answering(missing));
    refused(&w, "m1", key_call("km1", "a", None), "authority_unknown").await;
    let mut extra = gates_policy("input_any", None);
    extra["allow_desktop_click"] = json!(true);
    w.fake.set(answering(extra));
    refused(&w, "e1", key_call("ke1", "a", None), "authority_unknown").await;
    // The click: both gates; the click gate is checked first.
    w.fake.set(answering(gates_policy(
        "input_any_click",
        Some("allow_desktop_click"),
    )));
    refused(
        &w,
        "c1",
        click_call("kc1", 1, 2, None),
        "input_click_disabled",
    )
    .await;
    w.fake.set(answering(gates_policy(
        "input_any_click",
        Some("allow_desktop_input_any"),
    )));
    refused(
        &w,
        "c2",
        click_call("kc2", 1, 2, None),
        "input_any_disabled",
    )
    .await;
    let mut both = gates_policy("input_any_click", Some("allow_desktop_click"));
    both["allow_desktop_input_any"] = json!(false);
    w.fake.set(answering(both));
    refused(
        &w,
        "c3",
        click_call("kc3", 1, 2, None),
        "input_click_disabled",
    )
    .await;
    for (i, flag) in FLAGS.into_iter().enumerate() {
        w.fake
            .set(answering(gates_policy("input_any_click", Some(flag))));
        refused(
            &w,
            &format!("cf{i}"),
            click_call(&format!("kcf{i}"), 1, 2, None),
            "not_granted",
        )
        .await;
    }
    let mut missing = gates_policy("input_any_click", None);
    missing
        .as_object_mut()
        .unwrap()
        .remove("allow_desktop_input_any");
    w.fake.set(answering(missing));
    refused(
        &w,
        "cm1",
        click_call("kcm1", 1, 2, None),
        "authority_unknown",
    )
    .await;
    // The answer of the click purpose of a preview is not this one.
    w.fake.set(answering(gates_policy("input_click", None)));
    refused(
        &w,
        "cp1",
        click_call("kcp1", 1, 2, None),
        "authority_unknown",
    )
    .await;
    // Everything on: both go through.
    w.fake.set(p7b_station());
    for (id, call) in [
        ("ok1", key_call("kok1", "a", None)),
        ("ok2", click_call("kok2", 1, 2, None)),
    ] {
        let r = w.call(id, call).await;
        assert_eq!(r["status"], "ok", "{r}");
    }
}

#[tokio::test]
async fn a_station_error_and_a_lost_answer_are_authority_unknown() {
    let w = p7b_world(InputScope::AnyWindow, true).await;
    w.fake.set(p7b_except("desktop.policy_now", |_| {
        Some(fail("not_found", "x"))
    }));
    refused(&w, "e1", click_call("k1", 1, 2, None), "authority_unknown").await;
    // The P7 purpose and its bytes are unchanged for an own-window grant.
    let own = p7b_world(InputScope::OwnWindows, false).await;
    let r = own.call("o1", key_call("ko1", "a", None)).await;
    assert_eq!(r["status"], "ok", "{r}");
    assert_eq!(
        own.fake.params_of("desktop.policy_now")[0],
        json!({"purpose": "input", "monitor": null})
    );
}
