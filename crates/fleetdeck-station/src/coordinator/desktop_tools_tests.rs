//! R13 P6 T4: the catalog and the intake of the desktop and capture tools
//! (`docs/v2/design/omarchy-p6.md` §3, §6.1, §10).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;
use std::sync::Mutex;

use async_trait::async_trait;
use fleetdeck_proto::coordinator_desktop_text::{code, text};
use serde_json::{Value, json};

use super::bindings::{Binding, BindingState, Caller, ToolClass};
use super::desktop_forward::{DesktopRequest, McpToolResult};
use super::ingress::Forward;
use super::jsonrpc::{Answer, PROTOCOL_VERSIONS, answer};
use super::tools;

const NINE: [&str; 9] = [
    "desktop_info",
    "wall_show",
    "notify",
    "preview_open",
    "window_close",
    "desktop_action_status",
    "screenshot",
    "capture_status",
    "read_capture_image",
];

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
fn the_catalog_lists_the_tools_of_the_granted_classes_in_the_fixed_order() {
    let desktop = [
        "desktop_info",
        "wall_show",
        "notify",
        "preview_open",
        "window_close",
        "desktop_action_status",
    ];
    let capture = [
        "desktop_info",
        "screenshot",
        "capture_status",
        "read_capture_image",
    ];
    assert_eq!(names(&[ToolClass::Desktop]), desktop);
    assert_eq!(names(&[ToolClass::Capture]), capture);
    assert_eq!(names(&[ToolClass::Desktop, ToolClass::Capture]), NINE);
    // Neither class: none of the nine, also with every other class.
    for set in [
        vec![],
        vec![
            ToolClass::Read,
            ToolClass::Mail,
            ToolClass::Drive,
            ToolClass::Spawn,
        ],
        vec![ToolClass::Inspect, ToolClass::Manage, ToolClass::Answer],
        vec![ToolClass::Own],
    ] {
        assert!(
            names(&set).iter().all(|n| !NINE.contains(&n.as_str())),
            "{set:?}"
        );
    }
    // A fleet-control class that is not a P6 class does not list them either.
    assert!(!names(&[ToolClass::Inspect]).contains(&"desktop_info".to_owned()));
    // No alias is a tool.
    for alias in [
        "desktop_screenshot",
        "desktop_record",
        "record_demo",
        "terminal_open",
    ] {
        assert!(tools::tool(alias).is_none(), "{alias}");
        let all = names(&[ToolClass::Desktop, ToolClass::Capture]);
        assert!(!all.iter().any(|n| n == alias), "{alias}");
    }
    for name in NINE {
        assert!(super::desktop_tools::is_desktop(name));
        assert!(tools::tool(name).is_some(), "{name}");
    }
    assert!(!super::desktop_tools::is_desktop("list_sessions"));
}

/// Every object node of `schema` refuses additional properties, and every
/// `$ref` resolves in the root `$defs`.
fn check_schema(node: &Value, root: &Value, path: &str) {
    match node {
        Value::Object(map) => {
            if map.get("type") == Some(&json!("object")) {
                assert_eq!(
                    map.get("additionalProperties"),
                    Some(&json!(false)),
                    "{path}"
                );
            }
            if let Some(Value::String(r)) = map.get("$ref") {
                let key = r.strip_prefix("#/$defs/").expect(r);
                assert!(root["$defs"].get(key).is_some(), "{path}: {r}");
            }
            for (k, v) in map {
                check_schema(v, root, &format!("{path}/{k}"));
            }
        }
        Value::Array(items) => {
            for (i, v) in items.iter().enumerate() {
                check_schema(v, root, &format!("{path}/{i}"));
            }
        }
        _ => {}
    }
}

#[test]
fn every_schema_closes_every_object_level_and_carries_the_defs_in_its_root() {
    let catalog = tools::catalog(&classes(&[ToolClass::Desktop, ToolClass::Capture]));
    let listed = catalog["tools"].as_array().unwrap();
    assert_eq!(listed.len(), 9);
    for tool in listed {
        let name = tool["name"].as_str().unwrap();
        let schema = &tool["inputSchema"];
        assert_eq!(schema["type"], "object", "{name}");
        assert_eq!(schema["additionalProperties"], false, "{name}");
        // Each schema carries only the `$defs` that it references,
        // transitively (P1 §4.3).
        let keys: &[&str] = match name {
            "desktop_info" => &["id"],
            "wall_show" => &[
                "artifact_ref",
                "display_id",
                "id",
                "request_id",
                "session_ref",
                "wall_content",
            ],
            "notify" => &["id", "request_id"],
            "preview_open" | "screenshot" => &["id", "monitor", "request_id"],
            "window_close" => &["id", "request_id", "window_id"],
            "desktop_action_status" | "capture_status" => &["request_id"],
            "read_capture_image" => &["artifact_ref", "id"],
            other => panic!("unexpected tool {other}"),
        };
        let mut have: Vec<&str> = schema["$defs"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        have.sort_unstable();
        assert_eq!(have, keys, "{name}");
        assert!(schema.get("schemas").is_none(), "{name}");
        check_schema(schema, schema, name);
        assert!(tool["description"].as_str().is_some_and(|d| !d.is_empty()));
    }
    // The wall content is a closed union and the request ID is bounded.
    let wall = listed.iter().find(|t| t["name"] == "wall_show").unwrap();
    assert_eq!(
        wall["inputSchema"]["required"],
        json!(["request_id", "display_id", "content"])
    );
    assert_eq!(wall["inputSchema"]["$defs"]["request_id"]["maxLength"], 64);
    assert_eq!(
        wall["inputSchema"]["$defs"]["wall_content"]["oneOf"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn the_nine_names_never_become_a_coordinator_op() {
    for name in NINE {
        let err = tools::to_op(name, &json!({})).unwrap_err();
        assert_eq!(err, "forbidden: the grant has no such tool", "{name}");
    }
}

struct Fake {
    desktop: Mutex<Vec<(Caller, DesktopRequest)>>,
    plain: Mutex<u32>,
    reply: McpToolResult,
}

impl Fake {
    fn new() -> Self {
        Self::replying(McpToolResult {
            content: vec![json!({"type": "text", "text": "{\"ok\":true}"})],
            is_error: false,
        })
    }

    fn replying(reply: McpToolResult) -> Self {
        Self {
            desktop: Mutex::default(),
            plain: Mutex::default(),
            reply,
        }
    }

    fn forwarded(&self) -> usize {
        self.desktop.lock().unwrap().len()
    }
}

#[async_trait]
impl Forward for Fake {
    async fn call(&self, _caller: Caller, _tool: &str, _args: Value) -> Result<Value, String> {
        *self.plain.lock().unwrap() += 1;
        Ok(json!({}))
    }

    async fn call_desktop(&self, caller: Caller, request: DesktopRequest) -> McpToolResult {
        self.desktop.lock().unwrap().push((caller, request));
        self.reply.clone()
    }
}

fn binding(state: BindingState, granted: &[ToolClass]) -> Binding {
    Binding {
        binding_id: "b-1".to_owned(),
        grant_id: "g-1".to_owned(),
        session_id: "coord-1".into(),
        provider_instance: "claude".to_owned(),
        native_id: (state != BindingState::Pending).then(|| "native-1".to_owned()),
        generation: u32::from(state != BindingState::Pending),
        classes: classes(granted),
        state,
        control_skill: "SKILL".to_owned(),
        scope: fleetdeck_proto::command::GrantScope::Lineage,
    }
}

fn rpc(method: &str, params: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": 7, "method": method, "params": params})
}

async fn run(b: &Binding, fake: &Fake, fleet_control: bool, msg: Value) -> Value {
    match answer(msg, b, fake, None, fleet_control).await {
        Answer::Json(v) => v,
        Answer::Accepted => panic!("a notification"),
    }
}

async fn tool_call(
    b: &Binding,
    fake: &Fake,
    fleet_control: bool,
    name: &str,
    args: Value,
) -> (bool, String) {
    let v = run(
        b,
        fake,
        fleet_control,
        rpc("tools/call", json!({"name": name, "arguments": args})),
    )
    .await;
    let r = &v["result"];
    assert_eq!(r["content"].as_array().unwrap().len(), 1, "{v}");
    (
        r["isError"].as_bool().unwrap(),
        r["content"][0]["text"].as_str().unwrap().to_owned(),
    )
}

fn error_text(code: &str) -> String {
    json!({"error": code, "message": text(code).unwrap()}).to_string()
}

fn wall() -> Value {
    json!({"request_id": "r1", "display_id": "d1",
           "content": {"kind": "session", "session": {"station_id": "st", "session_id": "s1"}}})
}

#[tokio::test]
async fn a_p6_tool_of_a_binding_that_is_not_bound_is_not_bound_and_nothing_is_forwarded() {
    let fake = Fake::new();
    let pending = binding(
        BindingState::Pending,
        &[ToolClass::Desktop, ToolClass::Capture],
    );
    for state in [BindingState::Revoked, BindingState::Expired] {
        let b = binding(state, &[ToolClass::Desktop]);
        let (is_error, t) = tool_call(&b, &fake, true, "notify", json!({"junk": 1})).await;
        assert!(is_error);
        assert_eq!(t, error_text(code::NOT_BOUND), "{state:?}");
    }
    for name in NINE {
        let (is_error, t) = tool_call(&pending, &fake, true, name, wall()).await;
        assert!(is_error, "{name}");
        assert_eq!(t, error_text(code::NOT_BOUND), "{name}");
    }
    assert_eq!(fake.forwarded(), 0);
    assert_eq!(*fake.plain.lock().unwrap(), 0);
}

#[tokio::test]
async fn bad_arguments_are_invalid_params_without_any_supplied_value() {
    let fake = Fake::new();
    let b = binding(
        BindingState::Active,
        &[ToolClass::Desktop, ToolClass::Capture],
    );
    let mut unknown = wall();
    unknown["zz_secret_field"] = json!("zz_secret_value");
    let mut nested = wall();
    nested["content"]["session"]["nn_secret_nested"] = json!("nn_secret_value");
    let mut nested_content = wall();
    nested_content["content"]["cc_secret_extra"] = json!(1);
    let mut kind = wall();
    kind["content"]["kind"] = json!("kk_secret_kind");
    let cases = [
        ("wall_show", unknown),
        ("wall_show", nested),
        ("wall_show", nested_content),
        ("wall_show", kind),
        (
            "notify",
            json!({"request_id": "r1", "title": "T".repeat(81), "body": "b"}),
        ),
        (
            "notify",
            json!({"request_id": "bad id", "title": "tt_secret_title", "body": "b"}),
        ),
        (
            "notify",
            json!({"request_id": "r1", "title": "t", "body": "b".repeat(301)}),
        ),
        (
            "notify",
            json!({"request_id": "r1", "title": "t", "body": "b", "urgency": "ug_secret"}),
        ),
        ("notify", json!({"title": "t", "body": "b"})),
        ("notify", json!("not an object")),
        ("preview_open", json!({"request_id": "r1", "url": ""})),
        (
            "preview_open",
            json!({"request_id": "r1", "url": "http://localhost", "monitor": "bad mon"}),
        ),
        (
            "window_close",
            json!({"request_id": "r1", "window_id": "w/1"}),
        ),
        ("desktop_info", json!({"station": "st st"})),
        (
            "desktop_action_status",
            json!({"request_id": "x".repeat(65)}),
        ),
        ("capture_status", json!({"request_id": "r1", "extra": 1})),
        (
            "screenshot",
            json!({"request_id": "r1", "monitor": "mm secret"}),
        ),
        (
            "read_capture_image",
            json!({"artifact": {"station_id": "st", "artifact_id": "a1", "version": 0}}),
        ),
        (
            "read_capture_image",
            json!({"artifact": {"station_id": "st", "artifact_id": "a1", "version": 1, "x": 1}}),
        ),
        (
            "read_capture_image",
            json!({"artifact": {"station_id": "st", "artifact_id": "a1", "version": "1"}}),
        ),
    ];
    for (name, args) in cases {
        for fleet_control in [true, false] {
            let (is_error, t) = tool_call(&b, &fake, fleet_control, name, args.clone()).await;
            assert!(is_error, "{name} {args}");
            assert_eq!(t, error_text(code::INVALID_PARAMS), "{name} {args}");
            for secret in [
                "secret",
                "TTTT",
                "bad id",
                "w/1",
                "st st",
                "bad mon",
                "not an object",
            ] {
                assert!(!t.contains(secret), "{name}: {t}");
            }
        }
    }
    assert_eq!(fake.forwarded(), 0);
}

#[tokio::test]
async fn the_live_fleet_control_switch_refuses_a_valid_call_as_not_granted() {
    let fake = Fake::new();
    let b = binding(
        BindingState::Active,
        &[ToolClass::Desktop, ToolClass::Capture],
    );
    let (is_error, t) = tool_call(&b, &fake, false, "wall_show", wall()).await;
    assert!(is_error);
    assert_eq!(t, error_text(code::NOT_GRANTED));
    assert_eq!(fake.forwarded(), 0);
    // The setting is read per call: the same call passes when it is on.
    let (is_error, t) = tool_call(&b, &fake, true, "wall_show", wall()).await;
    assert!(!is_error, "{t}");
    assert_eq!(fake.forwarded(), 1);
    let (caller, request) = fake.desktop.lock().unwrap()[0].clone();
    assert_eq!(caller.session_id.as_str(), "coord-1");
    assert_eq!(caller.native_id, "native-1");
    assert!(matches!(request, DesktopRequest::Call(_)));
    assert_eq!(*fake.plain.lock().unwrap(), 0, "never `forward.call`");
}

#[tokio::test]
async fn a_p6_tool_outside_the_granted_classes_keeps_the_forbidden_answer() {
    let fake = Fake::new();
    let desktop = binding(BindingState::Active, &[ToolClass::Desktop]);
    let capture = binding(BindingState::Active, &[ToolClass::Capture]);
    let none = binding(BindingState::Active, &[ToolClass::Read]);
    let forbidden = "forbidden: the grant has no such tool";
    for name in ["screenshot", "capture_status", "read_capture_image"] {
        assert_eq!(
            tool_call(&desktop, &fake, true, name, json!({})).await,
            (true, forbidden.to_owned()),
            "{name}"
        );
    }
    for name in [
        "wall_show",
        "notify",
        "preview_open",
        "window_close",
        "desktop_action_status",
    ] {
        assert_eq!(
            tool_call(&capture, &fake, true, name, wall()).await,
            (true, forbidden.to_owned()),
            "{name}"
        );
    }
    for name in NINE {
        assert_eq!(
            tool_call(&none, &fake, true, name, json!({})).await,
            (true, forbidden.to_owned()),
            "{name}"
        );
    }
    // `desktop_info` is given by either class.
    for b in [&desktop, &capture] {
        let (is_error, _) = tool_call(b, &fake, true, "desktop_info", json!({})).await;
        assert!(!is_error);
    }
    assert_eq!(fake.forwarded(), 2);
}

#[tokio::test]
async fn a_forwarder_without_the_p6_path_answers_desktop_unsupported() {
    struct Old;
    #[async_trait]
    impl Forward for Old {
        async fn call(&self, _: Caller, _: &str, _: Value) -> Result<Value, String> {
            panic!("a P6 tool never reaches `call`");
        }
    }
    let b = binding(BindingState::Active, &[ToolClass::Desktop]);
    let v = match answer(
        rpc(
            "tools/call",
            json!({"name": "desktop_info", "arguments": {}}),
        ),
        &b,
        &Old,
        None,
        true,
    )
    .await
    {
        Answer::Json(v) => v,
        Answer::Accepted => panic!(),
    };
    assert_eq!(v["result"]["isError"], true);
    assert_eq!(
        v["result"]["content"][0]["text"].as_str().unwrap(),
        error_text(code::DESKTOP_UNSUPPORTED)
    );
}

#[tokio::test]
async fn every_mcp_version_gives_the_same_content_blocks_for_an_image() {
    let reply = McpToolResult {
        content: vec![
            json!({"type": "text", "text": "{\"capture_id\":\"cap-1\"}"}),
            json!({"type": "image", "mimeType": "image/png", "data": "iVBORw0KGgo="}),
        ],
        is_error: false,
    };
    let b = binding(BindingState::Active, &[ToolClass::Capture]);
    let mut seen = Vec::new();
    for version in PROTOCOL_VERSIONS {
        let fake = Fake::replying(reply.clone());
        let init = run(
            &b,
            &fake,
            true,
            rpc(
                "initialize",
                json!({"protocolVersion": version, "capabilities": {},
                       "clientInfo": {"name": "t", "version": "1"}}),
            ),
        )
        .await;
        assert_eq!(init["result"]["protocolVersion"], version);
        let v = run(
            &b,
            &fake,
            true,
            rpc(
                "tools/call",
                json!({"name": "read_capture_image", "arguments":
                    {"artifact": {"station_id": "st", "artifact_id": "a1", "version": 1}}}),
            ),
        )
        .await;
        assert_eq!(v["id"], 7);
        assert_eq!(v["result"]["isError"], false);
        seen.push(v["result"].clone());
    }
    assert_eq!(seen.len(), 3);
    assert!(seen.iter().all(|r| r == &seen[0]));
    assert_eq!(seen[0]["content"].as_array().unwrap().len(), 2);
    assert_eq!(seen[0]["content"][1]["type"], "image");
    assert_eq!(seen[0]["content"][1]["mimeType"], "image/png");
}
