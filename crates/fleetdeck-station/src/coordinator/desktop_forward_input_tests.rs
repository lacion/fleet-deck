//! R13 P7 §6.1, §10: the caller station forwarder of the three input tools:
//! the checked arguments, the `desktop_input_mcp.v1` feature check, the 45 s
//! deadline and the text-only result.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use fleetdeck_proto::coordinator_desktop::DesktopCall;
use fleetdeck_proto::coordinator_desktop_text::code;
use fleetdeck_proto::coordinator_desktop_wire::CoordinatorDesktopCallEnvelope;
use fleetdeck_proto::link::Outcome;
use fleetdeck_proto::{CommandId, IncarnationId};
use serde_json::{Value, json};

use super::{DesktopRequest, parse_request};
use crate::coordinator::{Caller, LinkForward, Transport};

const DESKTOP: &str = "desktop_mcp.v1";
const INPUT: &str = "desktop_input_mcp.v1";
const WINDOW: &str = "0123456789abcdef0123456789abcdef";

struct FakeDeck {
    features: Vec<&'static str>,
    sent: Mutex<Vec<(String, Value)>>,
}

#[async_trait]
impl Transport for FakeDeck {
    async fn request(
        &self,
        method: &str,
        _command_id: Option<&CommandId>,
        params: Value,
    ) -> Result<Outcome, String> {
        self.sent.lock().unwrap().push((method.to_owned(), params));
        Ok(Outcome::Ok(json!({"status": "ok", "result": {
            "state": "committed",
            "operation_id": "op-1"
        }})))
    }

    fn has_feature(&self, feature: &str) -> bool {
        self.features.contains(&feature)
    }
}

fn deck(features: &[&'static str]) -> Arc<FakeDeck> {
    Arc::new(FakeDeck {
        features: features.to_vec(),
        sent: Mutex::default(),
    })
}

fn caller() -> Caller {
    Caller {
        binding_id: "b-1".to_owned(),
        grant_id: "g-1".to_owned(),
        session_id: "coord-1".into(),
        provider_instance: "claude".to_owned(),
        native_id: "native-1".to_owned(),
        generation: 2,
    }
}

fn forward_with(deck: &Arc<FakeDeck>) -> LinkForward {
    let forward = LinkForward::with_clock(Arc::new(|| 1_000_000));
    forward.connect_transport(deck.clone(), IncarnationId::from("deck-inc".to_owned()));
    forward
}

fn args(tool: &str) -> Value {
    match tool {
        "key_press" => json!({"request_id": "r1", "window_id": WINDOW, "chord": "Ctrl+a"}),
        "type_text" => json!({"request_id": "r1", "window_id": WINDOW, "text": "hello"}),
        _ => json!({"request_id": "r1", "window_id": WINDOW, "x": 10, "y": 20}),
    }
}

const TOOLS: [&str; 3] = ["key_press", "type_text", "pointer_move"];

fn request(tool: &str) -> DesktopRequest {
    parse_request(tool, args(tool)).unwrap()
}

#[test]
fn the_arguments_are_checked_into_the_closed_input_calls() {
    assert!(matches!(
        request("key_press"),
        DesktopRequest::Call(DesktopCall::KeyPress(a)) if a.chord == "Ctrl+a"
    ));
    assert!(matches!(
        request("type_text"),
        DesktopRequest::Call(DesktopCall::TypeText(a)) if a.text == "hello"
    ));
    assert!(matches!(
        request("pointer_move"),
        DesktopRequest::Call(DesktopCall::PointerMove(a)) if a.x == 10 && a.y == 20
    ));
    for (tool, bad) in [
        (
            "key_press",
            json!({"request_id": "r1", "window_id": WINDOW, "chord": "Alt+a"}),
        ),
        (
            "key_press",
            json!({"request_id": "r1", "window_id": "short", "chord": "a"}),
        ),
        (
            "type_text",
            json!({"request_id": "r1", "window_id": WINDOW, "text": "a\nb"}),
        ),
        (
            "type_text",
            json!({"request_id": "r1", "window_id": WINDOW, "text": ""}),
        ),
        (
            "pointer_move",
            json!({"request_id": "r1", "window_id": WINDOW, "x": 32768, "y": 0}),
        ),
        (
            "pointer_move",
            json!({"request_id": "r1", "window_id": WINDOW, "x": 1, "y": 2, "button": 1}),
        ),
        (
            "pointer_move",
            json!({"request_id": "r1", "window_id": WINDOW, "x": 1}),
        ),
    ] {
        assert!(parse_request(tool, bad.clone()).is_none(), "{tool} {bad}");
    }
}

#[test]
fn the_mcp_deadline_is_45_seconds_and_the_tools_need_the_input_feature_only() {
    for tool in TOOLS {
        let r = request(tool);
        assert_eq!(r.mcp_ms(), 45_000, "{tool}");
        assert!(r.is_input(), "{tool}");
        assert!(!r.is_capture(), "{tool}");
        assert!(!r.is_recording(), "{tool}");
    }
    let notify = parse_request(
        "notify",
        json!({"request_id": "r1", "title": "t", "body": "b"}),
    )
    .unwrap();
    assert!(!notify.is_input());
}

#[tokio::test]
async fn without_the_input_feature_nothing_is_sent() {
    for features in [vec![DESKTOP], vec![INPUT], vec![]] {
        for tool in TOOLS {
            let fake = deck(&features);
            let r = forward_with(&fake)
                .desktop_within(caller(), request(tool), None)
                .await;
            assert!(r.is_error, "{tool} {features:?}");
            let body: Value = serde_json::from_str(r.content[0]["text"].as_str().unwrap()).unwrap();
            assert_eq!(
                body["error"],
                code::DESKTOP_UNSUPPORTED,
                "{tool} {features:?}"
            );
            assert!(fake.sent.lock().unwrap().is_empty(), "{tool} {features:?}");
        }
    }
}

#[tokio::test]
async fn with_the_features_the_call_is_forwarded_once_and_the_result_is_text_only() {
    for tool in TOOLS {
        let fake = deck(&[DESKTOP, INPUT]);
        let r = forward_with(&fake)
            .desktop_within(caller(), request(tool), None)
            .await;
        assert!(!r.is_error, "{tool}: {r:?}");
        // One text block with the deck's JSON answer: never an image block.
        assert_eq!(r.content.len(), 1, "{tool}");
        assert_eq!(r.content[0]["type"], "text", "{tool}");
        let body: Value = serde_json::from_str(r.content[0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(body, json!({"state": "committed", "operation_id": "op-1"}));
        let sent = fake.sent.lock().unwrap().clone();
        assert_eq!(sent.len(), 1, "{tool}");
        assert_eq!(sent[0].0, "coordinator.desktop_call");
        let envelope: CoordinatorDesktopCallEnvelope =
            serde_json::from_value(sent[0].1.clone()).unwrap();
        assert_eq!(serde_json::to_value(&envelope.call).unwrap()["tool"], *tool);
    }
}
