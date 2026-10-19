//! R13 P7b §4, §5, §11.2: the caller station forwarder of `pointer_click`
//! and `window_list`: the routes, the deadlines and the feature gates on the
//! current deck link, checked at each call.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use fleetdeck_proto::coordinator_desktop::{DesktopCall, DesktopQuery};
use fleetdeck_proto::coordinator_desktop_text::{code, deadline_ms};
use fleetdeck_proto::coordinator_desktop_wire::{
    CoordinatorDesktopCallEnvelope, CoordinatorDesktopQueryEnvelope,
};
use fleetdeck_proto::link::Outcome;
use fleetdeck_proto::{CommandId, IncarnationId};
use serde_json::{Value, json};

use super::{DesktopRequest, parse_request};
use crate::coordinator::{Caller, LinkForward, Transport};

const DESKTOP: &str = "desktop_mcp.v1";
const INPUT: &str = "desktop_input_mcp.v1";
const CLICK: &str = "desktop_click_mcp.v1";
const ANY: &str = "desktop_input_any.v1";
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

fn caller(input_any: bool) -> Caller {
    Caller {
        binding_id: "b-1".to_owned(),
        grant_id: "g-1".to_owned(),
        session_id: "coord-1".into(),
        provider_instance: "claude".to_owned(),
        native_id: "native-1".to_owned(),
        generation: 2,
        input_any,
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
        "window_list" => json!({}),
        _ => json!({"request_id": "r1", "window_id": WINDOW, "x": 10, "y": 20}),
    }
}

fn request(tool: &str) -> DesktopRequest {
    parse_request(tool, args(tool)).unwrap()
}

/// Runs one call and returns the error code of the refusal (`None` when the
/// call was forwarded) with the methods that the deck received.
async fn run(
    features: &[&'static str],
    input_any: bool,
    tool: &str,
) -> (Option<String>, Vec<String>) {
    let fake = deck(features);
    let r = forward_with(&fake)
        .desktop_within(caller(input_any), request(tool), None)
        .await;
    let error = r.is_error.then(|| {
        let body: Value = serde_json::from_str(r.content[0]["text"].as_str().unwrap()).unwrap();
        body["error"].as_str().unwrap().to_owned()
    });
    let sent = fake
        .sent
        .lock()
        .unwrap()
        .iter()
        .map(|(m, _)| m.clone())
        .collect();
    (error, sent)
}

#[test]
fn the_tools_route_to_the_click_call_and_the_window_list_query() {
    assert!(matches!(
        request("pointer_click"),
        DesktopRequest::Call(DesktopCall::PointerClick(a))
            if a.x == 10 && a.y == 20 && a.window_id == WINDOW && a.request_id == "r1"
    ));
    assert!(matches!(
        request("window_list"),
        DesktopRequest::Query(DesktopQuery::WindowList(a)) if a.station.is_none()
    ));
    assert!(matches!(
        parse_request("window_list", json!({"station": "st-1"})),
        Some(DesktopRequest::Query(DesktopQuery::WindowList(a))) if a.station.as_deref() == Some("st-1")
    ));
    for (tool, bad) in [
        (
            "pointer_click",
            json!({"request_id": "r1", "window_id": WINDOW, "x": 32768, "y": 0}),
        ),
        (
            "pointer_click",
            json!({"request_id": "r1", "window_id": "short", "x": 1, "y": 2}),
        ),
        (
            "pointer_click",
            json!({"request_id": "r1", "window_id": WINDOW, "x": 1, "y": 2, "button": "left"}),
        ),
        (
            "pointer_click",
            json!({"request_id": "r1", "window_id": WINDOW, "x": 1}),
        ),
        ("window_list", json!({"title": "x"})),
        ("window_list", json!({"station": null})),
        ("window_list", json!({"station": "bad station"})),
    ] {
        assert!(parse_request(tool, bad.clone()).is_none(), "{tool} {bad}");
    }
}

#[test]
fn the_click_is_an_input_call_and_the_list_is_not() {
    let click = request("pointer_click");
    assert!(click.is_input());
    assert!(click.is_click());
    assert!(!click.is_window_list());
    assert!(!click.is_capture());
    assert!(!click.is_recording());
    assert_eq!(click.mcp_ms(), 45_000);

    let list = request("window_list");
    assert!(!list.is_input());
    assert!(!list.is_click());
    assert!(list.is_window_list());
    assert!(!list.is_capture());
    assert!(!list.is_recording());
    assert_eq!(list.mcp_ms(), deadline_ms::INFO_MCP);

    for tool in ["key_press", "type_text", "pointer_move"] {
        let r = request(tool);
        assert!(r.is_input(), "{tool}");
        assert!(!r.is_click(), "{tool}");
        assert!(!r.is_window_list(), "{tool}");
    }
}

#[tokio::test]
async fn the_click_needs_the_desktop_input_and_click_features() {
    for features in [
        vec![INPUT, CLICK],
        vec![DESKTOP, CLICK],
        vec![DESKTOP, INPUT],
        vec![DESKTOP],
        vec![],
    ] {
        let (error, sent) = run(&features, false, "pointer_click").await;
        assert_eq!(
            error.as_deref(),
            Some(code::DESKTOP_UNSUPPORTED),
            "{features:?}"
        );
        assert!(sent.is_empty(), "{features:?}");
    }
    let (error, sent) = run(&[DESKTOP, INPUT, CLICK], false, "pointer_click").await;
    assert_eq!(error, None);
    assert_eq!(sent, ["coordinator.desktop_call"]);
}

#[tokio::test]
async fn the_other_input_tools_do_not_need_the_click_feature() {
    for tool in ["key_press", "type_text", "pointer_move"] {
        let (error, sent) = run(&[DESKTOP, INPUT], false, tool).await;
        assert_eq!(error, None, "{tool}");
        assert_eq!(sent, ["coordinator.desktop_call"], "{tool}");
    }
}

#[tokio::test]
async fn an_any_window_binding_needs_the_any_feature_for_every_input_call() {
    for tool in ["key_press", "type_text", "pointer_move", "pointer_click"] {
        // A downgrade after the start: the link lost `desktop_input_any.v1`.
        let (error, sent) = run(&[DESKTOP, INPUT, CLICK], true, tool).await;
        assert_eq!(error.as_deref(), Some(code::DESKTOP_UNSUPPORTED), "{tool}");
        assert!(sent.is_empty(), "{tool}");
        // The feature alone is not enough: the input feature is needed too.
        let (error, sent) = run(&[DESKTOP, ANY, CLICK], true, tool).await;
        assert_eq!(error.as_deref(), Some(code::DESKTOP_UNSUPPORTED), "{tool}");
        assert!(sent.is_empty(), "{tool}");
        // With every feature the call is forwarded once.
        let (error, sent) = run(&[DESKTOP, INPUT, CLICK, ANY], true, tool).await;
        assert_eq!(error, None, "{tool}");
        assert_eq!(sent, ["coordinator.desktop_call"], "{tool}");
        // An own-scope binding never needs the any feature.
        let (error, _) = run(&[DESKTOP, INPUT, CLICK], false, tool).await;
        assert_eq!(error, None, "{tool}");
    }
    // The click of an any-window binding still needs the click feature.
    let (error, sent) = run(&[DESKTOP, INPUT, ANY], true, "pointer_click").await;
    assert_eq!(error.as_deref(), Some(code::DESKTOP_UNSUPPORTED));
    assert!(sent.is_empty());
}

#[tokio::test]
async fn the_window_list_needs_the_desktop_and_any_features_only() {
    for (features, input_any) in [
        (vec![DESKTOP], false),
        (vec![DESKTOP, INPUT, CLICK], false),
        (vec![DESKTOP, INPUT, CLICK], true),
        (vec![ANY], true),
        (vec![], true),
    ] {
        let (error, sent) = run(&features, input_any, "window_list").await;
        assert_eq!(
            error.as_deref(),
            Some(code::DESKTOP_UNSUPPORTED),
            "{features:?} {input_any}"
        );
        assert!(sent.is_empty(), "{features:?} {input_any}");
    }
    // The input and click features are not needed for a read.
    for input_any in [false, true] {
        let (error, sent) = run(&[DESKTOP, ANY], input_any, "window_list").await;
        assert_eq!(error, None, "{input_any}");
        assert_eq!(sent, ["coordinator.desktop_query"], "{input_any}");
    }
}

#[tokio::test]
async fn the_envelopes_carry_the_new_tools() {
    let fake = deck(&[DESKTOP, INPUT, CLICK, ANY]);
    let forward = forward_with(&fake);
    let r = forward
        .desktop_within(caller(true), request("pointer_click"), None)
        .await;
    assert!(!r.is_error, "{r:?}");
    let r = forward
        .desktop_within(caller(true), request("window_list"), None)
        .await;
    assert!(!r.is_error, "{r:?}");
    let sent = fake.sent.lock().unwrap().clone();
    assert_eq!(sent.len(), 2);
    assert_eq!(sent[0].0, "coordinator.desktop_call");
    let call: CoordinatorDesktopCallEnvelope = serde_json::from_value(sent[0].1.clone()).unwrap();
    assert_eq!(
        serde_json::to_value(&call.call).unwrap()["tool"],
        "pointer_click"
    );
    assert_eq!(sent[1].0, "coordinator.desktop_query");
    let query: CoordinatorDesktopQueryEnvelope = serde_json::from_value(sent[1].1.clone()).unwrap();
    assert_eq!(
        serde_json::to_value(&query.query).unwrap()["tool"],
        "window_list"
    );
    // The deck deadline of the query is the one that F mirrored.
    assert_eq!(
        query.deadline,
        1_000_000 + i64::try_from(deadline_ms::INFO_DECK).unwrap()
    );
}
