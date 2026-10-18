//! The P7 deck side of the coordinator input tools (R13 P7 task K1):
//! `key_press`, `type_text` and `pointer_move` with their authorization, their
//! admission and charges, the run with the board sign-out, the station
//! failures and the unknown outcome, and the audit rows that hold no text.
//! The checks, the recovery and the delivery pass are in
//! `coordinator_desktop_input/lifecycle.rs`, the sign-out in `fence.rs` and the
//! input limits of the owner caps route in `caps.rs`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "coordinator_desktop_input/caps.rs"]
mod caps;
mod common;
mod coordinator_desktop_support;
#[path = "coordinator_desktop_input/fence.rs"]
mod fence;
#[path = "coordinator_desktop_input/lifecycle.rs"]
mod lifecycle;
mod settings_support;

use std::time::Duration;

use coordinator_desktop_support::*;
use fleetdeck_deck::store::GrantClass;
use fleetdeck_deck::store::coordinator_desktop::{OpClass, OpState, OpTool};
use fleetdeck_proto::version::feature;
use serde_json::json;

pub const UNKNOWN_TEXT: &str =
    "The input outcome is unknown. Inspect the preview before issuing another input request.";

/// The sentinels of the exposure proof: they must never leave the station
/// request, the reply to the caller aside.
pub const SENTINEL_TEXT: &str = "SENTINEL-typed-text-ünï";
pub const SENTINEL_CHORD: &str = "Ctrl+Shift+q";

/// The target link features of the input set without `desktop_input_mcp.v1`.
fn target_without_input() -> Vec<String> {
    [
        feature::COORDINATOR_V2,
        feature::DESKTOP,
        feature::DESKTOP_AUTHORITY,
        feature::DESKTOP_MCP,
        feature::DESKTOP_CAPTURE,
        feature::ARTIFACTS_DELETE,
        feature::CAPTURE_MCP,
    ]
    .map(str::to_owned)
    .to_vec()
}

fn assert_no_request(w: &World) {
    assert!(w.fake.log().is_empty(), "{:?}", w.fake.log());
}

#[tokio::test]
async fn every_missing_class_and_a_missing_allowance_are_refused_before_anything_is_stored() {
    // Each class of the three, missing, with the allowance off.
    for classes in [
        vec![GrantClass::Desktop, GrantClass::Capture],
        vec![GrantClass::Desktop, GrantClass::Input],
        vec![GrantClass::Capture, GrantClass::Input],
    ] {
        let w = world_with(&classes, |_| {}).await;
        w.fake.set(input_station());
        let r = w.call("c1", key_call("k1", "a", None)).await;
        assert_eq!(code(&r), "not_granted", "{classes:?}");
        assert_eq!(input_counters(&w).await, (0, 0, 0, 0));
        assert_no_request(&w);
    }
    // The three classes without `allow_input`: the class is there, the
    // allowance is not.
    let w = world_with(&INPUT3, |_| {}).await;
    w.fake.set(input_station());
    for (i, call) in [
        key_call("k1", "a", None),
        text_call("t1", "hi", None),
        move_call("m1", 1, 2, None),
    ]
    .into_iter()
    .enumerate()
    {
        let r = w.call(&format!("c{i}"), call).await;
        assert_eq!(code(&r), "input_not_allowed");
        assert_eq!(r["message"], "This grant does not allow desktop input.");
    }
    assert_eq!(input_counters(&w).await, (0, 0, 0, 0));
    assert_no_request(&w);
    assert!(
        w.f.store
            .desktop_ops_recent(&w.b.grant_id, 5)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn a_missing_input_feature_on_the_caller_or_on_the_target_is_desktop_unsupported() {
    // The caller link lacks `desktop_input_mcp.v1`.
    let w = input_world(10, 100).await;
    w.f.store
        .set_link_features(&w.f.station, 98, target_without_input());
    let r = w.call("c1", key_call("k1", "a", None)).await;
    assert_eq!(code(&r), "desktop_unsupported");
    assert_eq!(input_counters(&w).await, (0, 0, 0, 0));
    assert_no_request(&w);
    // The target link lacks it (the caller has it).
    let mut w = input_world(10, 100).await;
    let beta_fake = w.link_beta().await;
    w.f.store
        .set_link_features(&w.beta, 98, target_without_input());
    let beta = w.beta.clone();
    let r = w
        .call("c1", text_call("t1", "hi", Some(beta.as_str())))
        .await;
    assert_eq!(code(&r), "desktop_unsupported");
    assert_eq!(beta_fake.count("desktop.act"), 0);
    assert_eq!(input_counters(&w).await, (0, 0, 0, 0));
    // With the feature on that second link the call goes through.
    let mut full = target_without_input();
    full.push(feature::DESKTOP_INPUT_MCP.to_owned());
    w.f.store.set_link_features(&w.beta, 98, full);
    beta_fake.set(input_station());
    let r = w
        .call("c2", text_call("t2", "hi", Some(beta.as_str())))
        .await;
    assert_eq!(r["status"], "ok", "{r}");
    assert_eq!(beta_fake.count("desktop.act"), 1);
}

#[tokio::test]
async fn the_input_policy_is_asked_with_the_input_purpose_and_every_flag_must_be_true() {
    let w = input_world(10, 100).await;
    let r = w.call("c1", key_call("k1", "a", None)).await;
    assert_eq!(r["status"], "ok", "{r}");
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
        json!({"purpose": "input", "monitor": null})
    );
    // Each flag off refuses before the admission: nothing is charged.
    for (i, flag) in [
        "allow_fleet_control",
        "allow_desktop",
        "allow_desktop_capture",
        "allow_desktop_input",
        "monitor_allowed",
    ]
    .into_iter()
    .enumerate()
    {
        w.fake.set(input_except("desktop.policy_now", move |_| {
            Some(ok(input_policy(Some(flag))))
        }));
        let r = w
            .call(&format!("c{i}"), key_call(&format!("kx{i}"), "a", None))
            .await;
        assert_eq!(code(&r), "not_granted", "{flag}");
    }
    // The P6 answer (four flags) is not the closed input answer.
    w.fake.set(input_except("desktop.policy_now", |_| {
        Some(ok(json!({
            "allow_fleet_control": true, "allow_desktop": true,
            "allow_desktop_capture": true, "monitor_allowed": true
        })))
    }));
    let r = w.call("cp", key_call("kp", "a", None)).await;
    assert_eq!(code(&r), "authority_unknown");
    assert_eq!(input_counters(&w).await, (1, 1, 0, 1));
    assert_eq!(w.fake.count("desktop.act"), 1);
}

#[tokio::test]
async fn a_done_input_is_charged_sent_once_and_leaves_no_text_in_the_rows() {
    let w = input_world(10, 100).await;
    let chars = u32::try_from(SENTINEL_TEXT.chars().count()).unwrap();
    assert_ne!(
        SENTINEL_TEXT.chars().count(),
        SENTINEL_TEXT.len(),
        "the text has multi-byte scalars"
    );
    let calls = [
        (
            key_call("req-key-1", SENTINEL_CHORD, None),
            "input_key_press",
        ),
        (
            text_call("req-text-1", SENTINEL_TEXT, None),
            "input_type_text",
        ),
        (
            move_call("req-move-1", 7351, 2468, None),
            "input_pointer_move",
        ),
    ];
    for (i, (call, action)) in calls.iter().enumerate() {
        let r = w.call(&format!("c{i}"), call.clone()).await;
        assert_eq!(r, json!({"status": "ok", "result": {}}));
        let act = w.fake.params_of("desktop.act").pop().unwrap();
        assert_eq!(act["action"], *action);
        assert_eq!(act["window_id"], WINDOW);
        assert!(act["authority"]["action_id"].is_string());
    }
    // The station got the full payload; the deck kept none of it.
    let acts = w.fake.params_of("desktop.act");
    assert_eq!(acts[0]["chord"], SENTINEL_CHORD);
    assert_eq!(acts[1]["text"], SENTINEL_TEXT);
    assert_eq!(
        (acts[2]["x"].as_u64(), acts[2]["y"].as_u64()),
        (Some(7351), Some(2468))
    );
    assert_eq!(input_counters(&w).await, (3, 3, chars, 3));
    for (id, tool, length) in [
        ("req-key-1", OpTool::KeyPress, 1),
        ("req-text-1", OpTool::TypeText, chars),
        ("req-move-1", OpTool::PointerMove, 0),
    ] {
        let o = op_of(&w, id).await;
        assert_eq!(
            (o.tool, o.class, o.state, o.input_length),
            (tool, OpClass::Input, OpState::Done, Some(length)),
            "{id}"
        );
        assert_eq!(o.resource, json!({"window_id": WINDOW}));
        assert_eq!(o.result, Some(json!({})));
        let (attempted, finished, addrs) = op_times(&w, &o.operation_id).await;
        assert!(attempted.is_some() && finished.is_some() && addrs.is_some());
    }
    let kinds = [
        ("desktop.key_press", 1, 0),
        ("desktop.type_text", chars, chars),
        ("desktop.pointer_move", 0, 0),
    ];
    for (action, length, characters) in kinds {
        let rows = audit_details(&w, action).await;
        assert_eq!(rows.len(), 2, "{action}: {rows:?}");
        let (attempt, outcome) = (&rows[0], &rows[1]);
        assert_eq!(
            (attempt["stage"].as_str(), outcome["stage"].as_str()),
            (Some("attempt"), Some("outcome"))
        );
        assert_eq!(attempt["input_length"], length);
        assert_eq!(outcome["input_length"], length);
        assert_eq!(
            attempt["charged"],
            json!({"desktop": 1, "capture": 0, "input": 1, "input_characters": characters})
        );
        // No handle in the attempt row; the handle in the outcome of a done.
        assert!(attempt.get("window_id").is_none(), "{attempt}");
        assert_eq!(outcome["window_id"], WINDOW);
        assert_eq!(outcome["code"], json!(null));
    }
    // The sentinels are nowhere in the audit rows or the stored rows.
    let everything = format!("{}\n{}", w.audit_text().await, stored_text(&w).await);
    for sentinel in [
        SENTINEL_TEXT,
        SENTINEL_CHORD,
        "7351",
        "2468",
        "req-key-1",
        "req-text-1",
        "req-move-1",
    ] {
        assert!(
            !everything.contains(sentinel),
            "{sentinel} leaked: {everything}"
        );
    }
    // A duplicate replays the state and sends nothing, charging nothing.
    let again = w
        .call("cd", text_call("req-text-1", SENTINEL_TEXT, None))
        .await;
    assert_eq!(again["status"], "ok");
    assert_eq!(again["result"]["state"], "done");
    assert_eq!(w.fake.count("desktop.act"), 3);
    assert_eq!(input_counters(&w).await, (3, 3, chars, 3));
    // The same request ID with another payload conflicts.
    let other = w.call("ce", text_call("req-text-1", "another", None)).await;
    assert_eq!(code(&other), "request_id_conflict");
}

#[tokio::test]
async fn both_caps_refuse_with_a_stored_code_and_nothing_is_refunded() {
    let w = input_world(3, 10).await;
    let ok_text = w.call("c1", text_call("t1", "123456", None)).await;
    assert_eq!(ok_text["status"], "ok");
    // Five characters are over the four that are left.
    let r = w.call("c2", text_call("t2", "12345", None)).await;
    assert_eq!(code(&r), "input_characters_reached");
    assert_eq!(
        r["message"],
        "This grant has used its typed-character limit."
    );
    assert_eq!(input_counters(&w).await, (1, 1, 6, 2));
    let again = w.call("c3", text_call("t2", "12345", None)).await;
    assert_eq!(code(&again), "input_characters_reached");
    // A chord and a short text still fit; then the count is used.
    assert_eq!(
        w.call("c4", key_call("k1", "a", None)).await["status"],
        "ok"
    );
    assert_eq!(
        w.call("c5", text_call("t3", "1234", None)).await["status"],
        "ok"
    );
    let r = w.call("c6", key_call("k2", "b", None)).await;
    assert_eq!(code(&r), "input_cap_reached");
    assert_eq!(r["message"], "This grant has used its input action limit.");
    assert_eq!(input_counters(&w).await, (3, 3, 10, 5));
    assert_eq!(w.fake.count("desktop.act"), 3);
    // The refusal rows name no window handle.
    let refused: Vec<_> = audit_details(&w, "desktop.key_press")
        .await
        .into_iter()
        .filter(|d| d["stage"] == "refused")
        .collect();
    assert_eq!(refused.len(), 1, "{refused:?}");
    assert_eq!(refused[0]["code"], "input_cap_reached");
    assert!(refused[0].get("window_id").is_none(), "{}", refused[0]);
    // The status tool tells the stored refusal.
    let s = w.query(status_query("k2")).await;
    assert_eq!(s["result"]["state"], "failed");
    assert_eq!(s["result"]["error"], "input_cap_reached");
    // No refund after a station failure either.
    let w = input_world(5, 100).await;
    w.fake.set(input_except("desktop.act", |_| {
        Some(fail("input_failed", "x"))
    }));
    let r = w.call("c1", text_call("t1", "abc", None)).await;
    assert_eq!(code(&r), "input_failed");
    assert_eq!(input_counters(&w).await, (1, 1, 3, 1));
}

#[tokio::test]
async fn the_input_rate_is_20_a_minute_and_the_20_rows_come_with_the_shared_bucket() {
    let w = input_world(100, 1000).await;
    for i in 0..20 {
        let r = w
            .call(&format!("c{i}"), key_call(&format!("k{i}"), "a", None))
            .await;
        assert_eq!(r["status"], "ok", "{i}: {r}");
    }
    let r = w.call("c-over", key_call("k-over", "a", None)).await;
    assert_eq!(code(&r), "rate_limited");
    assert_eq!(input_counters(&w).await, (20, 20, 0, 20));
    assert_eq!(w.fake.count("desktop.act"), 20);
    w.f.det.advance(Duration::from_secs(61));
    let r = w.call("c-later", key_call("k-later", "a", None)).await;
    assert_eq!(r["status"], "ok", "{r}");
}

#[tokio::test]
async fn a_station_answer_passes_through_or_maps_and_the_operation_fails_with_its_code() {
    let w = input_world(50, 1000).await;
    let locked = "The screen is locked or its lock state is unknown.";
    let cases = [
        ("input_tool_missing", "x", "input_tool_missing", None),
        (
            "input_tool_unsupported",
            "x",
            "input_tool_unsupported",
            None,
        ),
        ("input_board_forbidden", "x", "input_board_forbidden", None),
        (
            "input_destination_unavailable",
            "x",
            "input_destination_unavailable",
            None,
        ),
        ("input_focus_unknown", "x", "input_focus_unknown", None),
        ("focus_changed", "x", "focus_changed", None),
        ("input_interrupted", "x", "input_interrupted", None),
        ("input_failed", "x", "input_failed", None),
        ("input_timeout", "x", "input_timeout", None),
        ("screen_locked", "x", "screen_locked", Some(locked)),
        (
            "desktop_window_not_found",
            "x",
            "desktop_window_not_found",
            None,
        ),
        ("authority_lost", "STATION-TEXT", "authority_lost", None),
        (
            "authority_unknown",
            "STATION-TEXT",
            "authority_unknown",
            None,
        ),
        ("forbidden", "x", "desktop_forbidden", None),
        ("unsupported", "STATION-TEXT", "desktop_unsupported", None),
        ("something_new", "STATION-TEXT", "desktop_failed", None),
    ];
    for (i, (wire, message, want, text)) in cases.into_iter().enumerate() {
        w.fake.set(input_except("desktop.act", move |_| {
            Some(fail(wire, message))
        }));
        let rid = format!("r{i}");
        let r = w.call(&format!("c{i}"), key_call(&rid, "a", None)).await;
        assert_eq!(code(&r), want, "{wire}");
        if let Some(text) = text {
            assert_eq!(r["message"], text);
        }
        assert!(!r.to_string().contains("STATION-TEXT"), "{r}");
        let o = op_of(&w, &rid).await;
        assert_eq!(r["operation_id"], o.operation_id.as_str());
        assert_eq!((o.state, o.code.as_deref()), (OpState::Failed, Some(want)));
        // The status tool tells the same, with the fixed text.
        let s = w.query(status_query(&rid)).await;
        assert_eq!(s["result"]["state"], "failed");
        assert_eq!(s["result"]["error"], want);
        assert!(s["result"]["message"].is_string(), "{wire}: {s}");
        if let Some(text) = text {
            assert_eq!(s["result"]["message"], text);
        }
        assert!(!s.to_string().contains("STATION-TEXT"), "{s}");
        let (attempted, finished, _) = op_times(&w, &o.operation_id).await;
        assert!(attempted.is_some() && finished.is_some(), "{wire}");
    }
    let n = u32::try_from(cases.len()).unwrap();
    assert_eq!(input_counters(&w).await, (n, n, 0, n));
}

#[tokio::test]
async fn a_lost_link_after_the_send_is_unknown_and_never_sent_again() {
    let mut w = input_world(10, 100).await;
    let beta_fake = w.link_beta().await;
    beta_fake.set(input_except("desktop.act", |req| {
        req.control.close("test", "the link drops");
        None
    }));
    let beta = w.beta.clone();
    let r = w
        .call("c1", text_call("t1", "abcd", Some(beta.as_str())))
        .await;
    assert_eq!(code(&r), "desktop_timeout");
    assert_eq!(r["message"], UNKNOWN_TEXT);
    let o = op_of(&w, "t1").await;
    assert_eq!(r["operation_id"], o.operation_id.as_str());
    assert_eq!(o.state, OpState::Unknown);
    assert_eq!(input_counters(&w).await, (1, 1, 4, 1));
    // The status tool tells the unknown outcome with the same text; nothing
    // is sent again and nothing is charged again.
    let s = w.query(status_query("t1")).await;
    assert_eq!(s["result"]["state"], "unknown");
    assert_eq!(s["result"]["error"], "desktop_timeout");
    assert_eq!(s["result"]["message"], UNKNOWN_TEXT);
    assert_eq!(beta_fake.count("desktop.act"), 1);
    assert_eq!(input_counters(&w).await, (1, 1, 4, 1));
    // The outcome row of the audit has the timeout code and no handle.
    let rows = audit_details(&w, "desktop.type_text").await;
    assert_eq!(rows[1]["stage"], "outcome");
    assert_eq!(rows[1]["code"], "desktop_timeout");
    assert!(rows[1].get("window_id").is_none(), "{}", rows[1]);
}
