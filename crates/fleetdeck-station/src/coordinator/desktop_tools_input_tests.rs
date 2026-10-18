//! R13 P7 §3.2, §4: the three input tools are listed only for a binding with
//! the `input` class and the station-internal `input_enabled` class, with the
//! exact descriptions and schemas.
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
use super::input::BUNDLE;

const NAMES: [&str; 3] = ["key_press", "type_text", "pointer_move"];
const WINDOW: &str = "0123456789abcdef0123456789abcdef";

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

fn has_input_tools(list: &[ToolClass]) -> [bool; 3] {
    let listed = names(list);
    NAMES.map(|n| listed.iter().any(|l| l == n))
}

const FULL: [ToolClass; 5] = [
    ToolClass::Desktop,
    ToolClass::Capture,
    ToolClass::Input,
    ToolClass::InputEnabled,
    ToolClass::Own,
];

#[test]
fn the_tools_are_listed_only_with_both_input_classes() {
    assert_eq!(has_input_tools(&FULL), [true; 3]);
    // Without either input class none of the three is listed.
    for missing in [ToolClass::Input, ToolClass::InputEnabled] {
        let set: Vec<ToolClass> = FULL.iter().copied().filter(|c| *c != missing).collect();
        assert_eq!(has_input_tools(&set), [false; 3], "{missing:?}");
    }
    // A P6 grant, a P5b-2 grant and one input class alone have none.
    for set in [
        vec![ToolClass::Desktop, ToolClass::Capture, ToolClass::Own],
        vec![
            ToolClass::Desktop,
            ToolClass::Capture,
            ToolClass::Recording,
            ToolClass::Own,
        ],
        vec![ToolClass::Input],
        vec![ToolClass::InputEnabled],
        vec![],
    ] {
        assert_eq!(has_input_tools(&set), [false; 3], "{set:?}");
    }
    // No click and no scroll tool exists.
    for name in ["pointer_click", "scroll", "click"] {
        assert!(tools::tool(name).is_none(), "{name}");
    }
}

#[test]
fn they_are_p7_desktop_tools_that_never_become_a_coordinator_op() {
    for name in NAMES {
        assert!(super::is_desktop(name), "{name}");
        let tool = tools::tool(name).unwrap();
        assert_eq!(tool.classes, [ToolClass::Input, ToolClass::InputEnabled]);
        assert!(tool.is_fleet_control(), "{name}");
        assert_eq!(
            tools::to_op(name, &json!({})).unwrap_err(),
            "forbidden: the grant has no such tool"
        );
    }
}

#[test]
fn the_descriptions_are_exact() {
    for (name, text) in [
        (
            "key_press",
            "Press one key or one chord (for example Return or Ctrl+Shift+a) in a preview window that this session opened, named by its window_id. A key can activate buttons and submit forms.",
        ),
        (
            "type_text",
            "Type 1 to 2000 characters into a preview window that this session opened, named by its window_id. The text has no line breaks or control characters; use key_press for Return or Tab.",
        ),
        (
            "pointer_move",
            "Move the pointer to a point in a preview window that this session opened, named by its window_id. x and y count from the top-left corner of the preview. There is no click.",
        ),
    ] {
        assert_eq!(tools::tool(name).unwrap().description, text, "{name}");
    }
}

#[test]
fn the_schemas_are_the_bundle_of_section_4_with_the_defs_in_each_root() {
    let bundle: Value = serde_json::from_str(BUNDLE).unwrap();
    assert_eq!(bundle["schemas"].as_object().unwrap().len(), 3);
    let catalog = tools::catalog(&classes(&FULL));
    let listed = catalog["tools"].as_array().unwrap();
    for name in NAMES {
        let tool = listed.iter().find(|t| t["name"] == name).unwrap();
        let mut expected = bundle["schemas"][name].clone();
        expected["$defs"] = bundle["$defs"].clone();
        assert_eq!(tool["inputSchema"], expected, "{name}");
    }
    // Spot checks against the contract text.
    let key = &listed.iter().find(|t| t["name"] == "key_press").unwrap()["inputSchema"];
    assert_eq!(key["required"], json!(["request_id", "window_id", "chord"]));
    assert_eq!(key["additionalProperties"], false);
    assert_eq!(key["$defs"]["window_id"]["pattern"], "^[0-9a-f]{32}$");
    assert_eq!(
        key["$defs"]["chord"]["pattern"],
        "^(Ctrl\\+)?(Shift\\+)?([a-z0-9]|Return|Tab|Escape|BackSpace|Delete|Insert|Home|End|PageUp|PageDown|Left|Right|Up|Down|Space)$"
    );
    let text = &listed.iter().find(|t| t["name"] == "type_text").unwrap()["inputSchema"];
    assert_eq!(text["properties"]["text"]["maxLength"], 2000);
    let pointer = &listed.iter().find(|t| t["name"] == "pointer_move").unwrap()["inputSchema"];
    assert_eq!(pointer["properties"]["x"]["maximum"], 32767);
    // The P6 schemas are unchanged: their `window_id` def is the P6 one.
    let close = listed.iter().find(|t| t["name"] == "window_close").unwrap();
    assert_eq!(close["inputSchema"]["$defs"]["window_id"]["maxLength"], 64);
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
async fn the_intake_lists_and_forwards_them_only_for_an_input_binding() {
    let fake = Fake {
        forwarded: Mutex::default(),
    };
    let call = json!({"name": "key_press", "arguments": {
        "request_id": "r1", "window_id": WINDOW, "chord": "Return"}});

    let granted = binding(&FULL);
    let listed = rpc(&granted, &fake, "tools/list", json!({})).await;
    let tools = listed["result"]["tools"].as_array().unwrap();
    for name in NAMES {
        assert!(tools.iter().any(|t| t["name"] == name), "{name}");
    }
    let r = rpc(&granted, &fake, "tools/call", call.clone()).await;
    assert_eq!(r["result"]["isError"], false, "{r}");
    assert_eq!(fake.forwarded.lock().unwrap().len(), 1);

    // A grant with the public class but without `input_enabled` neither
    // lists nor accepts them.
    let plain = binding(&[
        ToolClass::Desktop,
        ToolClass::Capture,
        ToolClass::Input,
        ToolClass::Own,
    ]);
    let listed = rpc(&plain, &fake, "tools/list", json!({})).await;
    let tools = listed["result"]["tools"].as_array().unwrap();
    assert!(
        !tools
            .iter()
            .any(|t| NAMES.contains(&t["name"].as_str().unwrap()))
    );
    let r = rpc(&plain, &fake, "tools/call", call).await;
    assert_eq!(r["result"]["isError"], true, "{r}");
    assert_eq!(
        r["result"]["content"][0]["text"],
        "forbidden: the grant has no such tool"
    );
    assert_eq!(fake.forwarded.lock().unwrap().len(), 1);

    // Bad arguments are refused before any forward.
    let bad = json!({"name": "key_press", "arguments": {
        "request_id": "r1", "window_id": WINDOW, "chord": "Alt+x"}});
    let r = rpc(&granted, &fake, "tools/call", bad).await;
    assert_eq!(r["result"]["isError"], true, "{r}");
    assert_eq!(fake.forwarded.lock().unwrap().len(), 1);
}
