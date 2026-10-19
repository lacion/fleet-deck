//! R13 P5b-2 §13.4, §13.5, §14: the caller station forwarder of
//! `record_monitor`: the checked arguments, the feature check, the
//! `seconds + 45` s deadline and the text-only result.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use fleetdeck_proto::coordinator_desktop::DesktopCall;
use fleetdeck_proto::coordinator_desktop_text::{code, deadline_ms};
use fleetdeck_proto::coordinator_desktop_wire::CoordinatorDesktopCallEnvelope;
use fleetdeck_proto::link::Outcome;
use fleetdeck_proto::{CommandId, IncarnationId};
use serde_json::{Value, json};

use super::{DesktopRequest, parse_request};
use crate::coordinator::{Caller, LinkForward, Transport};

const DESKTOP: &str = "desktop_mcp.v1";
const CAPTURE: &str = "capture_mcp.v1";
const RECORDING: &str = "capture_recording_mcp.v1";

struct FakeDeck {
    features: Vec<&'static str>,
    sent: Mutex<Vec<(String, Value)>>,
    reply: Value,
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
        Ok(Outcome::Ok(self.reply.clone()))
    }

    fn has_feature(&self, feature: &str) -> bool {
        self.features.contains(&feature)
    }
}

fn deck(features: &[&'static str]) -> Arc<FakeDeck> {
    Arc::new(FakeDeck {
        features: features.to_vec(),
        sent: Mutex::default(),
        reply: json!({"status": "ok", "result": {
            "state": "committed",
            "capture_id": "r1",
            "stop_reason": "duration"
        }}),
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
        input_any: false,
    }
}

fn forward_with(deck: &Arc<FakeDeck>) -> LinkForward {
    let forward = LinkForward::with_clock(Arc::new(|| 1_000_000));
    forward.connect_transport(deck.clone(), IncarnationId::from("deck-inc".to_owned()));
    forward
}

fn record(seconds: u32) -> DesktopRequest {
    parse_request(
        "record_monitor",
        json!({"request_id": "r1", "monitor": "DP-1", "seconds": seconds}),
    )
    .unwrap()
}

#[test]
fn the_arguments_are_checked_into_the_closed_record_call() {
    let request = parse_request(
        "record_monitor",
        json!({"request_id": "r1", "station": "st-1", "monitor": "DP-1", "seconds": 120}),
    )
    .unwrap();
    assert!(matches!(
        request,
        DesktopRequest::Call(DesktopCall::RecordMonitor(a))
            if a.seconds == 120 && a.station.as_deref() == Some("st-1")
    ));
    for bad in [
        json!({"request_id": "r1", "monitor": "DP-1", "seconds": 0}),
        json!({"request_id": "r1", "monitor": "DP-1", "seconds": 3601}),
        json!({"request_id": "r1", "monitor": "DP-1"}),
        json!({"monitor": "DP-1", "seconds": 5}),
        json!({"request_id": "r1", "monitor": "DP 1", "seconds": 5}),
        json!({"request_id": "r1", "monitor": "DP-1", "seconds": 5, "path": "/x"}),
        json!({"request_id": "r1", "monitor": "DP-1", "seconds": "5"}),
    ] {
        assert!(
            parse_request("record_monitor", bad.clone()).is_none(),
            "{bad}"
        );
    }
}

#[test]
fn the_mcp_deadline_is_the_seconds_plus_forty_five_seconds() {
    for seconds in [1_u32, 60, 120, 3600] {
        assert_eq!(
            record(seconds).mcp_ms(),
            u64::from(seconds) * 1000 + 45_000,
            "{seconds}"
        );
    }
    assert_eq!(deadline_ms::RECORD_MCP_EXTRA, 45_000);
}

#[test]
fn the_tool_needs_the_capture_and_the_recording_features() {
    let request = record(5);
    assert!(request.is_capture());
    assert!(request.is_recording());
    let screenshot =
        parse_request("screenshot", json!({"request_id": "r1", "monitor": "DP-1"})).unwrap();
    assert!(!screenshot.is_recording());
}

#[tokio::test]
async fn without_the_recording_feature_nothing_is_sent() {
    for features in [
        vec![DESKTOP],
        vec![DESKTOP, CAPTURE],
        vec![DESKTOP, RECORDING],
        vec![CAPTURE, RECORDING],
    ] {
        let fake = deck(&features);
        let r = forward_with(&fake)
            .desktop_within(caller(), record(5), None)
            .await;
        assert!(r.is_error, "{features:?}");
        let body: Value = serde_json::from_str(r.content[0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(body["error"], code::DESKTOP_UNSUPPORTED, "{features:?}");
        assert!(fake.sent.lock().unwrap().is_empty(), "{features:?}");
    }
}

#[tokio::test]
async fn with_the_features_the_call_is_forwarded_once_and_the_result_is_text_only() {
    let fake = deck(&[DESKTOP, CAPTURE, RECORDING]);
    let r = forward_with(&fake)
        .desktop_within(caller(), record(7), None)
        .await;
    assert!(!r.is_error, "{r:?}");
    // One text block with the status JSON: no image, no media block.
    assert_eq!(r.content.len(), 1);
    assert_eq!(r.content[0]["type"], "text");
    let body: Value = serde_json::from_str(r.content[0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(
        body,
        json!({"state": "committed", "capture_id": "r1", "stop_reason": "duration"})
    );
    let sent = fake.sent.lock().unwrap().clone();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].0, "coordinator.desktop_call");
    let envelope: CoordinatorDesktopCallEnvelope =
        serde_json::from_value(sent[0].1.clone()).unwrap();
    assert!(matches!(
        envelope.call,
        DesktopCall::RecordMonitor(a) if a.seconds == 7 && a.monitor == "DP-1"
    ));
}
