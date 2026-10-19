//! The texts and the codes of the failures of the P7b action variants (R13
//! P7b §13, task K1): the broader texts for the five P7b variants, the P7
//! texts for the legacy ones, the six new station codes that pass through,
//! and the unknown outcome text.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use fleetdeck_proto::coordinator_desktop_text::text;
use fleetdeck_proto::coordinator_spec::InputScope;
use serde_json::Value;

use crate::coordinator_desktop_support::*;
use crate::{P7_UNKNOWN_TEXT, P7B_UNKNOWN_TEXT};

const NOT_FOUND_ANY: &str = "No input window available to this grant matches that handle.";
const DESTINATION_ANY: &str = "The pointer destination is not available inside the target window.";
const FOCUS_UNKNOWN_ANY: &str = "The target window's input focus could not be verified. No further input was started; the pointer may already have moved.";
const FOCUS_CHANGED_ANY: &str =
    "The target window no longer has focus. Input stopped; some input may already have been sent.";

/// The station answers `desktop.act` with the wire error `code`.
fn failing(code: &'static str) -> Handler {
    p7b_except("desktop.act", move |_| {
        Some(fail(code, "SENTINEL-wire-text"))
    })
}

#[tokio::test]
async fn a_p7b_variant_failure_uses_the_broader_text_and_a_legacy_one_keeps_the_p7_text() {
    // (the code, the broader text).
    let codes = [
        ("desktop_window_not_found", NOT_FOUND_ANY),
        ("input_destination_unavailable", DESTINATION_ANY),
        ("input_focus_unknown", FOCUS_UNKNOWN_ANY),
        ("focus_changed", FOCUS_CHANGED_ANY),
    ];
    let own = p7b_world(InputScope::OwnWindows, true).await;
    let any = p7b_world(InputScope::AnyWindow, true).await;
    for (i, (wire, broader)) in codes.into_iter().enumerate() {
        let legacy = text(wire).unwrap();
        assert_ne!(legacy, broader, "{wire}: the texts differ");
        for w in [&own, &any] {
            w.fake.set(failing(wire));
        }
        // An own-window key: the P7 text, as before.
        let r = own
            .call(&format!("o{i}"), key_call(&format!("ok{i}"), "a", None))
            .await;
        assert_eq!(
            (code(&r).as_str(), r["message"].as_str()),
            (wire, Some(legacy))
        );
        // An own-window click is a P7b variant: the broader text.
        let r = own
            .call(&format!("c{i}"), click_call(&format!("ck{i}"), 1, 2, None))
            .await;
        assert_eq!(
            (code(&r).as_str(), r["message"].as_str()),
            (wire, Some(broader))
        );
        // Every call under the broader scope: the broader text.
        let calls: [Value; 4] = [
            key_call(&format!("ak{i}"), "a", None),
            text_call(&format!("at{i}"), "x", None),
            move_call(&format!("am{i}"), 1, 2, None),
            click_call(&format!("ac{i}"), 1, 2, None),
        ];
        for (j, call) in calls.into_iter().enumerate() {
            let r = any.call(&format!("a{i}{j}"), call).await;
            assert_eq!(
                (code(&r).as_str(), r["message"].as_str()),
                (wire, Some(broader))
            );
        }
    }
    // The status tool tells the same texts.
    let s = own.query(status_query("ok3")).await;
    assert_eq!(s["result"]["message"], text("focus_changed").unwrap());
    let s = own.query(status_query("ck3")).await;
    assert_eq!(s["result"]["message"], FOCUS_CHANGED_ANY);
    let s = any.query(status_query("ak3")).await;
    assert_eq!(s["result"]["message"], FOCUS_CHANGED_ANY);
    // No wire text reaches the agent.
    assert!(!own.audit_text().await.contains("SENTINEL"));
}

#[tokio::test]
async fn a_text_that_the_variants_share_does_not_change() {
    // `input_interrupted` has one text for every variant.
    let any = p7b_world(InputScope::AnyWindow, true).await;
    any.fake.set(failing("input_interrupted"));
    let r = any.call("c1", key_call("k1", "a", None)).await;
    assert_eq!(code(&r), "input_interrupted");
    assert_eq!(r["message"], text("input_interrupted").unwrap());
}

#[tokio::test]
async fn the_six_new_station_codes_pass_through_to_the_agent() {
    let w = p7b_world(InputScope::AnyWindow, true).await;
    for (i, wire) in [
        "input_click_not_allowed",
        "input_scope_not_allowed",
        "input_click_disabled",
        "input_any_disabled",
        "input_click_tool_missing",
        "input_click_tool_unsupported",
    ]
    .into_iter()
    .enumerate()
    {
        w.fake.set(failing(wire));
        let r = w
            .call(&format!("c{i}"), click_call(&format!("k{i}"), 1, 2, None))
            .await;
        assert_eq!(code(&r), wire);
        assert_eq!(r["message"], text(wire).unwrap(), "{wire}");
        let o = op_of(&w, &format!("k{i}")).await;
        assert_eq!(r["operation_id"], o.operation_id.as_str());
    }
}

#[tokio::test]
async fn the_unknown_outcome_text_follows_the_variant() {
    assert_eq!(
        P7B_UNKNOWN_TEXT,
        fleetdeck_proto::coordinator_desktop_text::INPUT_ANY_OUTCOME_UNKNOWN_TEXT
    );
    // (the scope, the call, the text).
    type Case = (InputScope, fn(&str, &str) -> Value, &'static str);
    let cases: [Case; 3] = [
        (
            InputScope::OwnWindows,
            |id, s| key_call(id, "a", Some(s)),
            P7_UNKNOWN_TEXT,
        ),
        (
            InputScope::OwnWindows,
            |id, s| click_call(id, 1, 2, Some(s)),
            P7B_UNKNOWN_TEXT,
        ),
        (
            InputScope::AnyWindow,
            |id, s| key_call(id, "a", Some(s)),
            P7B_UNKNOWN_TEXT,
        ),
    ];
    for (n, (scope, call, expected)) in cases.into_iter().enumerate() {
        let mut w = p7b_world(scope, true).await;
        let beta_fake = w.link_beta().await;
        beta_fake.set(p7b_except("desktop.act", |req| {
            req.control.close("test", "the link drops");
            None
        }));
        let beta = w.beta.clone();
        let r = w.call("c1", call("k1", beta.as_str())).await;
        assert_eq!(code(&r), "desktop_timeout", "{n}");
        assert_eq!(r["message"], expected, "{n}");
        // The status tool tells the same text.
        let s = w.query(status_query("k1")).await;
        assert_eq!(s["result"]["state"], "unknown", "{n}");
        assert_eq!(s["result"]["error"], "desktop_timeout");
        assert_eq!(s["result"]["message"], expected, "{n}");
        assert_eq!(beta_fake.count("desktop.act"), 1);
    }
}
