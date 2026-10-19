//! R13 P7b §4, §5: `pointer_click` and `window_list` are listed only for the
//! internal classes `click_enabled` and `input_any`, with the exact
//! descriptions and schemas. The P7 input tools keep their classes.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::{Value, json};

use super::super::bindings::{Binding, BindingState, Caller, ToolClass};
use super::super::desktop_forward::{DesktopRequest, McpToolResult};
use super::super::ingress::Forward;
use super::super::mcp_p1_rig_tests::serve_json;
use super::super::tools;

const P7: [&str; 3] = ["key_press", "type_text", "pointer_move"];
const WINDOW: &str = "0123456789abcdef0123456789abcdef";

const BASE: [ToolClass; 5] = [
    ToolClass::Desktop,
    ToolClass::Capture,
    ToolClass::Input,
    ToolClass::InputEnabled,
    ToolClass::Own,
];

fn classes(list: &[ToolClass]) -> BTreeSet<ToolClass> {
    list.iter().copied().collect()
}

fn with(extra: &[ToolClass]) -> Vec<ToolClass> {
    BASE.iter().chain(extra).copied().collect()
}

fn listed(list: &[ToolClass]) -> Vec<String> {
    tools::catalog(&classes(list))["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_owned())
        .collect()
}

/// `[pointer_click, window_list]` listed for the class set.
fn p7b(list: &[ToolClass]) -> [bool; 2] {
    let names = listed(list);
    ["pointer_click", "window_list"].map(|n| names.iter().any(|l| l == n))
}

fn p7_listed(list: &[ToolClass]) -> [bool; 3] {
    let names = listed(list);
    P7.map(|n| names.iter().any(|l| l == n))
}

#[test]
fn the_tools_are_listed_by_their_internal_classes() {
    assert_eq!(p7b(&BASE), [false, false]);
    assert_eq!(p7b(&with(&[ToolClass::ClickEnabled])), [true, false]);
    assert_eq!(p7b(&with(&[ToolClass::InputAny])), [false, true]);
    assert_eq!(
        p7b(&with(&[ToolClass::ClickEnabled, ToolClass::InputAny])),
        [true, true]
    );
    // The P7 tools are listed in each of those sets.
    for extra in [
        vec![],
        vec![ToolClass::ClickEnabled],
        vec![ToolClass::InputAny],
        vec![ToolClass::ClickEnabled, ToolClass::InputAny],
    ] {
        assert_eq!(p7_listed(&with(&extra)), [true; 3], "{extra:?}");
    }
}

#[test]
fn each_tool_needs_the_public_and_the_enabled_classes_too() {
    // An internal class alone, or without `input` or `input_enabled`, lists
    // nothing of the new tools.
    for set in [
        vec![ToolClass::ClickEnabled],
        vec![ToolClass::InputAny],
        vec![ToolClass::ClickEnabled, ToolClass::InputAny],
        vec![
            ToolClass::Input,
            ToolClass::ClickEnabled,
            ToolClass::InputAny,
        ],
        vec![
            ToolClass::InputEnabled,
            ToolClass::ClickEnabled,
            ToolClass::InputAny,
        ],
        vec![
            ToolClass::Desktop,
            ToolClass::Capture,
            ToolClass::Own,
            ToolClass::ClickEnabled,
            ToolClass::InputAny,
        ],
        vec![],
    ] {
        assert_eq!(p7b(&set), [false, false], "{set:?}");
    }
}

#[test]
fn they_are_desktop_tools_that_never_become_a_coordinator_op() {
    for (name, last) in [
        ("pointer_click", ToolClass::ClickEnabled),
        ("window_list", ToolClass::InputAny),
    ] {
        assert!(super::is_desktop(name), "{name}");
        let tool = tools::tool(name).unwrap();
        assert_eq!(
            tool.classes,
            [ToolClass::Input, ToolClass::InputEnabled, last]
        );
        assert!(tool.is_fleet_control(), "{name}");
        assert!(!tool.is_own(), "{name}");
        assert_eq!(
            tools::to_op(name, &json!({})).unwrap_err(),
            "forbidden: the grant has no such tool"
        );
    }
    // The P7 tools keep their classes.
    for name in P7 {
        assert_eq!(
            tools::tool(name).unwrap().classes,
            [ToolClass::Input, ToolClass::InputEnabled],
            "{name}"
        );
    }
}

#[test]
fn the_descriptions_are_exact() {
    assert_eq!(
        tools::tool("pointer_click").unwrap().description,
        "Send one left-button click at a point in a window named by its window_id. x and y count from the top-left corner of the window. One press and release; two calls can act as a double click. A click can submit forms or approve actions."
    );
    assert_eq!(
        tools::tool("window_list").unwrap().description,
        "List the application windows on a grant station that this agent may give input to: an opaque window_id, the position and size, the monitors and whether it has the focus. No titles. A window_id lasts 60 seconds; list again after that."
    );
    // The P7 tools change the target phrase only.
    let target = "a window named by its window_id (a preview window that this session opened, or a window from window_list when that tool is listed)";
    for (name, text) in [
        (
            "key_press",
            format!(
                "Press one key or one chord (for example Return or Ctrl+Shift+a) in {target}. A key can activate buttons and submit forms."
            ),
        ),
        (
            "type_text",
            format!(
                "Type 1 to 2000 characters into {target}. The text has no line breaks or control characters; use key_press for Return or Tab."
            ),
        ),
        (
            "pointer_move",
            format!(
                "Move the pointer to a point in {target}. x and y count from the top-left corner of the preview. There is no click."
            ),
        ),
    ] {
        assert_eq!(tools::tool(name).unwrap().description, text, "{name}");
    }
}

fn expected_defs(keys: &[&str]) -> Value {
    let all = json!({
        "station": {
            "type": "string",
            "minLength": 1,
            "maxLength": 128,
            "pattern": "^[A-Za-z0-9_-]+$"
        },
        "request_id": {
            "type": "string",
            "minLength": 1,
            "maxLength": 64,
            "pattern": "^[A-Za-z0-9_-]+$"
        },
        "window_id": {
            "type": "string",
            "minLength": 32,
            "maxLength": 32,
            "pattern": "^[0-9a-f]{32}$"
        }
    });
    let mut out = serde_json::Map::new();
    for key in keys {
        out.insert((*key).to_owned(), all[*key].clone());
    }
    Value::Object(out)
}

#[test]
fn the_schemas_are_sections_5_1_and_5_2_with_the_referenced_defs_in_each_root() {
    let catalog = tools::catalog(&classes(&with(&[
        ToolClass::ClickEnabled,
        ToolClass::InputAny,
    ])));
    let listed = catalog["tools"].as_array().unwrap();
    let schema =
        |name: &str| listed.iter().find(|t| t["name"] == name).unwrap()["inputSchema"].clone();
    assert_eq!(
        schema("pointer_click"),
        json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["request_id", "window_id", "x", "y"],
            "properties": {
                "request_id": {"$ref": "#/$defs/request_id"},
                "station": {"$ref": "#/$defs/station"},
                "window_id": {"$ref": "#/$defs/window_id"},
                "x": {"type": "integer", "minimum": 0, "maximum": 32767},
                "y": {"type": "integer", "minimum": 0, "maximum": 32767}
            },
            "$defs": expected_defs(&["station", "request_id", "window_id"])
        })
    );
    assert_eq!(
        schema("window_list"),
        json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "station": {"$ref": "#/$defs/station"}
            },
            "$defs": expected_defs(&["station"])
        })
    );
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

async fn rpc(b: &Binding, fake: &Arc<Fake>, method: &str, params: Value) -> Value {
    let msg = json!({"jsonrpc": "2.0", "id": 7, "method": method, "params": params});
    serve_json(b, fake.clone(), true, None, &msg).await
}

#[test]
fn the_caller_of_a_binding_knows_the_any_window_class() {
    let plain = binding(&BASE).caller().unwrap();
    assert!(!plain.input_any);
    // `click_enabled` alone does not mean `any_window`.
    let click = binding(&with(&[ToolClass::ClickEnabled])).caller().unwrap();
    assert!(!click.input_any);
    let any = binding(&with(&[ToolClass::InputAny])).caller().unwrap();
    assert!(any.input_any);
}

#[tokio::test]
async fn the_intake_forwards_each_new_tool_only_for_its_binding() {
    let fake = Arc::new(Fake {
        forwarded: Mutex::default(),
    });
    let click = json!({"name": "pointer_click", "arguments": {
        "request_id": "r1", "window_id": WINDOW, "x": 4, "y": 5}});
    let list = json!({"name": "window_list", "arguments": {}});

    let both = binding(&with(&[ToolClass::ClickEnabled, ToolClass::InputAny]));
    let r = rpc(&both, &fake, "tools/call", click.clone()).await;
    assert_eq!(r["result"]["isError"], false, "{r}");
    let r = rpc(&both, &fake, "tools/call", list.clone()).await;
    assert_eq!(r["result"]["isError"], false, "{r}");
    {
        let sent = fake.forwarded.lock().unwrap();
        assert_eq!(sent.len(), 2);
        assert!(matches!(
            &sent[0],
            DesktopRequest::Call(fleetdeck_proto::coordinator_desktop::DesktopCall::PointerClick(a))
                if a.x == 4 && a.y == 5 && a.window_id == WINDOW
        ));
        assert!(matches!(
            &sent[1],
            DesktopRequest::Query(
                fleetdeck_proto::coordinator_desktop::DesktopQuery::WindowList(_)
            )
        ));
    }

    // A binding without the classes: the P7 refusal, nothing forwarded.
    let plain = binding(&BASE);
    for (name, call) in [
        ("pointer_click", click.clone()),
        ("window_list", list.clone()),
    ] {
        let r = rpc(&plain, &fake, "tools/call", call).await;
        assert_eq!(r["result"]["isError"], true, "{r}");
        assert_eq!(
            r["result"]["content"][0]["text"],
            format!(
                "forbidden: your grant has no tool `{name}`. Call tools/list to see the tools you have."
            )
        );
    }
    // The click binding does not list or accept `window_list`, and the
    // list binding does not list or accept `pointer_click`.
    let only_click = binding(&with(&[ToolClass::ClickEnabled]));
    let r = rpc(&only_click, &fake, "tools/call", list).await;
    assert_eq!(r["result"]["isError"], true, "{r}");
    let only_any = binding(&with(&[ToolClass::InputAny]));
    let r = rpc(&only_any, &fake, "tools/call", click).await;
    assert_eq!(r["result"]["isError"], true, "{r}");
    assert_eq!(fake.forwarded.lock().unwrap().len(), 2);

    // Bad arguments are refused before any forward.
    let bad = json!({"name": "pointer_click", "arguments": {
        "request_id": "r1", "window_id": WINDOW, "x": 4, "y": 5, "button": "left"}});
    let r = rpc(&both, &fake, "tools/call", bad).await;
    assert_eq!(r["result"]["isError"], true, "{r}");
    let bad = json!({"name": "window_list", "arguments": {"title": "x"}});
    let r = rpc(&both, &fake, "tools/call", bad).await;
    assert_eq!(r["result"]["isError"], true, "{r}");
    assert_eq!(fake.forwarded.lock().unwrap().len(), 2);
}
