//! R13 P5b-2 §13.1, §13.5: `record_monitor` is listed only for a binding with
//! the `capture` class and the station-internal `recording` class (the start
//! adds it only for a spec with `allow_recording`), with the exact schema.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;
use std::sync::Mutex;

use async_trait::async_trait;
use serde_json::{Value, json};

use super::super::bindings::{Binding, BindingState, Caller, ToolClass};
use super::super::desktop_forward::{DesktopRequest, McpToolResult};
use super::super::ingress::Forward;
use super::super::jsonrpc::{Answer, answer};
use super::super::tools;

const NAME: &str = "record_monitor";

fn classes(list: &[ToolClass]) -> BTreeSet<ToolClass> {
    list.iter().copied().collect()
}

fn names(list: &[ToolClass]) -> Vec<String> {
    tools::catalog(&classes(list))["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn it_is_listed_only_with_the_capture_and_the_recording_classes() {
    let with = names(&[ToolClass::Capture, ToolClass::Recording]);
    assert_eq!(
        with,
        [
            "desktop_info",
            "screenshot",
            NAME,
            "capture_status",
            "read_capture_image"
        ]
    );
    // A screenshot-only grant does not list it.
    assert!(!names(&[ToolClass::Capture]).iter().any(|n| n == NAME));
    assert!(
        !names(&[ToolClass::Desktop, ToolClass::Capture])
            .iter()
            .any(|n| n == NAME)
    );
    // The recording class alone, or with another class, does not either.
    for set in [
        vec![ToolClass::Recording],
        vec![ToolClass::Recording, ToolClass::Desktop],
        vec![ToolClass::Recording, ToolClass::Own, ToolClass::Inspect],
        vec![],
    ] {
        assert!(!names(&set).iter().any(|n| n == NAME), "{set:?}");
    }
    // It does not give `desktop`, `inspect`, `drive` or `input` tools.
    let all = names(&[ToolClass::Capture, ToolClass::Recording]);
    for other in ["wall_show", "notify", "preview_open", "window_close"] {
        assert!(!all.iter().any(|n| n == other), "{other}");
    }
    // No alias is a tool.
    for alias in ["record_demo", "desktop_record", "screen_record"] {
        assert!(tools::tool(alias).is_none(), "{alias}");
    }
}

#[test]
fn it_is_a_p6_desktop_tool_that_never_becomes_a_coordinator_op() {
    assert!(super::is_desktop(NAME));
    let tool = tools::tool(NAME).unwrap();
    assert_eq!(tool.classes, [ToolClass::Capture, ToolClass::Recording]);
    assert_eq!(
        tool.description,
        "Record one monitor of a grant station for 1 to 3600 seconds (the station's own limit can be lower). The recording is saved as a video artifact; this tool returns its reference, never the video."
    );
    assert_eq!(
        tools::to_op(NAME, &json!({})).unwrap_err(),
        "forbidden: the grant has no such tool"
    );
}

#[test]
fn the_schema_is_the_one_of_section_13_1_exactly() {
    let catalog = tools::catalog(&classes(&[ToolClass::Capture, ToolClass::Recording]));
    let listed = catalog["tools"].as_array().unwrap();
    let tool = listed.iter().find(|t| t["name"] == NAME).unwrap();
    let expected = json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["request_id", "monitor", "seconds"],
        "properties": {
            "request_id": {
                "type": "string",
                "minLength": 1,
                "maxLength": 64,
                "pattern": "^[A-Za-z0-9_-]+$"
            },
            "station": {
                "type": "string",
                "minLength": 1,
                "maxLength": 128,
                "pattern": "^[A-Za-z0-9_-]+$"
            },
            "monitor": {
                "type": "string",
                "minLength": 1,
                "maxLength": 64,
                "pattern": "^[A-Za-z0-9._-]+$"
            },
            "seconds": {
                "type": "integer",
                "minimum": 1,
                "maximum": 3600
            }
        }
    });
    assert_eq!(tool["inputSchema"], expected);
    // The other tools keep their `$defs` and the screenshot is unchanged.
    let shot = listed.iter().find(|t| t["name"] == "screenshot").unwrap();
    assert!(shot["inputSchema"]["$defs"].is_object());
}

struct Fake {
    forwarded: Mutex<Vec<DesktopRequest>>,
}

#[async_trait]
impl Forward for Fake {
    async fn call(&self, _caller: Caller, _tool: &str, _args: Value) -> Result<Value, String> {
        Ok(json!({}))
    }

    async fn call_desktop(&self, _caller: Caller, request: DesktopRequest) -> McpToolResult {
        self.forwarded.lock().unwrap().push(request);
        McpToolResult {
            content: vec![json!({"type": "text", "text": "{}"})],
            is_error: false,
        }
    }
}

fn binding(granted: &[ToolClass]) -> Binding {
    Binding {
        binding_id: "b-1".to_owned(),
        grant_id: "g-1".to_owned(),
        session_id: "coord-1".into(),
        provider_instance: "claude".to_owned(),
        native_id: Some("native-1".to_owned()),
        generation: 1,
        classes: classes(granted),
        state: BindingState::Active,
        control_skill: "SKILL".to_owned(),
        scope: fleetdeck_proto::command::GrantScope::Lineage,
    }
}

async fn rpc(b: &Binding, fake: &Fake, method: &str, params: Value) -> Value {
    let msg = json!({"jsonrpc": "2.0", "id": 7, "method": method, "params": params});
    match answer(msg, b, fake, None, true).await {
        Answer::Json(v) => v,
        Answer::Accepted => panic!("a notification"),
    }
}

#[tokio::test]
async fn the_intake_lists_and_forwards_it_only_for_a_recording_binding() {
    let fake = Fake {
        forwarded: Mutex::default(),
    };
    let args = json!({"request_id": "r1", "monitor": "DP-1", "seconds": 5});
    let call = json!({"name": NAME, "arguments": args});

    let granted = binding(&[ToolClass::Capture, ToolClass::Recording, ToolClass::Own]);
    let listed = rpc(&granted, &fake, "tools/list", json!({})).await;
    let tools = listed["result"]["tools"].as_array().unwrap();
    assert!(tools.iter().any(|t| t["name"] == NAME));
    let r = rpc(&granted, &fake, "tools/call", call.clone()).await;
    assert_eq!(r["result"]["isError"], false, "{r}");
    assert_eq!(fake.forwarded.lock().unwrap().len(), 1);

    // A screenshot-only binding neither lists nor accepts it.
    let shot = binding(&[ToolClass::Capture, ToolClass::Own]);
    let listed = rpc(&shot, &fake, "tools/list", json!({})).await;
    let tools = listed["result"]["tools"].as_array().unwrap();
    assert!(!tools.iter().any(|t| t["name"] == NAME));
    let r = rpc(&shot, &fake, "tools/call", call).await;
    assert_eq!(r["result"]["isError"], true, "{r}");
    assert_eq!(
        r["result"]["content"][0]["text"],
        "forbidden: the grant has no such tool"
    );
    assert_eq!(fake.forwarded.lock().unwrap().len(), 1);

    // Bad arguments are refused before any forward.
    let bad = json!({"name": NAME, "arguments": {"request_id": "r1", "monitor": "DP-1", "seconds": 3601}});
    let r = rpc(&granted, &fake, "tools/call", bad).await;
    assert_eq!(r["result"]["isError"], true, "{r}");
    assert_eq!(fake.forwarded.lock().unwrap().len(), 1);
}
