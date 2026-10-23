//! MCP Apps in the Claude adapter (B15, design `docs/v2/design/mcp-apps.md`
//! §4.2): the launch variable, the readiness from `system/init`, the candidate
//! of a completed `mcp__` tool call, the three app control requests and the
//! direct-driver [`AppBridge`].

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use fleetdeck_core::ChildEnv;
use fleetdeck_proto::Event;
use fleetdeck_proto::apps::{MAX_APP_INPUT_BYTES, MAX_APP_RESULT_BYTES};
use fleetdeck_proto::command::SessionSpec;
use serde_json::{Value, json};

use super::super::{ClaudeAdapter, ClaudeAdapterConfig, Origin};
use crate::adapter::{EventSink, ProviderAdapter as _};
use crate::apps::{AppCandidate, AppTool};
use crate::claude::frames::parse_value;
use crate::claude::map::apps::{AppOp, parse_tools};
use crate::claude::tests::{config, emits, feed, running_core, tool_use, writes};
use crate::claude::{ClaudeCore, HostCmd, Output};
use crate::host::{HostError, HostKind, HostedProcess, ProcessSpec, SessionHost};

/// The three capabilities that the app path needs.
pub(super) const ALL: [&str; 3] = [
    "mcp_read_resource_v1",
    "mcp_tool_ui_meta_v1",
    "mcp_call_result_not_for_model_v1",
];

/// A root `system/init` frame that states `capabilities`.
fn init_frame(capabilities: &[&str]) -> Value {
    json!({"type": "system", "subtype": "init", "session_id": "s-1", "model": "claude-haiku-5-5",
           "permissionMode": "default", "claude_code_version": "2.1.308",
           "capabilities": capabilities})
}

/// A started core with a running turn whose `init` states `capabilities`.
pub(super) fn core_with(capabilities: &[&str]) -> ClaudeCore {
    let mut core = running_core();
    feed(&mut core, init_frame(capabilities));
    core
}

pub(super) fn ready_core() -> ClaudeCore {
    core_with(&ALL)
}

/// The outputs of a recorded fixture (`spikes/s1-claude/fixtures/rust`)
/// through the production core, in order.
fn replay(name: &str) -> Vec<Output> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../spikes/s1-claude/fixtures/rust")
        .join(name);
    let text = std::fs::read_to_string(path).unwrap();
    let mut core: Option<ClaudeCore> = None;
    let mut all = Vec::new();
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let rec: Value = serde_json::from_str(line).unwrap();
        let out = match rec.get("dir").and_then(Value::as_str) {
            Some("spawn") => {
                core = Some(ClaudeCore::new(config(false)));
                continue;
            }
            Some("host") => {
                let cmd: HostCmd = serde_json::from_value(rec["cmd"].clone()).unwrap();
                core.as_mut().unwrap().apply(&cmd).unwrap()
            }
            Some("out") => core
                .as_mut()
                .unwrap()
                .on_frame(parse_value(rec["frame"].clone()).unwrap()),
            Some("exit") => core.as_mut().unwrap().on_exit(None),
            _ => continue,
        };
        all.extend(out);
    }
    all
}

fn candidates(out: &[Output]) -> Vec<AppCandidate> {
    out.iter()
        .filter_map(|o| match o {
            Output::AppCandidate(c) => Some(c.clone()),
            _ => None,
        })
        .collect()
}

/// A user frame with one `tool_result` block and its structured result.
fn result_frame(tool_use_id: &str, is_error: bool, parent: Option<&str>) -> Value {
    json!({
        "type": "user", "parent_tool_use_id": parent,
        "message": {"role": "user", "content": [
            {"tool_use_id": tool_use_id, "type": "tool_result", "is_error": is_error, "content": "{\"count\":2}"}]},
        "tool_use_result": {"content": "{\"count\":2}", "structuredContent": {"count": 2}},
    })
}

/// A `can_use_tool` request of `tool` for the tool use `id`, naming `server`.
fn mcp_permission(native: &str, tool: &str, id: &str, server: &str) -> Value {
    json!({"type": "control_request", "request_id": native,
           "request": {"subtype": "can_use_tool", "tool_name": tool,
                       "mcp_server": {"name": server, "source": "dynamic"},
                       "input": {}, "tool_use_id": id}})
}

// ---- launch ----------------------------------------------------------------

/// A host that records the spawn spec and refuses to start.
#[derive(Debug)]
struct Recorder(Arc<std::sync::Mutex<Vec<ProcessSpec>>>);

#[async_trait]
impl SessionHost for Recorder {
    fn kind(&self) -> HostKind {
        HostKind::DirectChild
    }

    fn survives_station_restart(&self) -> bool {
        false
    }

    async fn spawn(&self, spec: ProcessSpec) -> Result<HostedProcess, HostError> {
        self.0.lock().unwrap().push(spec);
        Err(HostError::NoPipe)
    }
}

fn spec(dir: &Path) -> SessionSpec {
    SessionSpec {
        provider_instance: "claude".into(),
        cwd: dir.to_string_lossy().into_owned(),
        mode: None,
        model: None,
        title: None,
        coordinator: None,
        launch: None,
        extra_env: None,
    }
}

#[tokio::test]
async fn launch_env_sets_mcp_apps_host() {
    let dir = tempfile::tempdir().unwrap();
    let var = "CLAUDE_CODE_MCP_APPS_HOST";
    // The direct launch.
    let spawned = Arc::new(std::sync::Mutex::new(Vec::new()));
    let config =
        ClaudeAdapterConfig::new("claude".into(), "claude".into(), ChildEnv::from_process());
    let adapter = ClaudeAdapter::new(config, Arc::new(Recorder(Arc::clone(&spawned))));
    let (sink, _events) = EventSink::channel(8);
    let started = adapter
        .start_session(spec(dir.path()), sink, Origin::New)
        .await;
    assert!(started.is_err());
    let direct = spawned.lock().unwrap().remove(0);
    assert_eq!(direct.env.get(var).unwrap(), "1");
    // The durable launch, new and resumed.
    let durable = adapter.durable_launch(&spec(dir.path())).unwrap();
    assert_eq!(durable.process.env.get(var).unwrap(), "1");
    let id = crate::adapter::NativeSessionId("native-1".to_owned());
    let resumed = adapter
        .durable_resume_launch(&spec(dir.path()), &id)
        .unwrap();
    assert_eq!(resumed.process.env.get(var).unwrap(), "1");
    // The instance environment does not need to set it, and a value of the
    // instance environment does not switch it off.
    let mut base = ChildEnv::from_process();
    base.set(var, "0").unwrap();
    let config = ClaudeAdapterConfig::new("claude".into(), "claude".into(), base);
    let adapter = ClaudeAdapter::new(config, Arc::new(Recorder(Arc::default())));
    let launch = adapter.durable_launch(&spec(dir.path())).unwrap();
    assert_eq!(launch.process.env.get(var).unwrap(), "1");
}

// ---- readiness, hints and candidates ---------------------------------------

#[test]
fn init_capabilities_enable_apps_only_with_all_three() {
    for (missing, name) in ALL.iter().enumerate() {
        let some: Vec<&str> = ALL
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != missing)
            .map(|(_, c)| *c)
            .collect();
        let mut core = core_with(&some);
        assert!(!core.mcp_apps_ready, "without {name}");
        let op = AppOp::Tools;
        assert!(core.app_request(&op).is_err(), "nothing is written");
        // An mcp tool result gives no candidate without the capabilities.
        tool_use(&mut core, "tu-1", "mcp__demo__show_counter", json!({}));
        let out = feed(&mut core, result_frame("tu-1", false, None));
        assert!(candidates(&out).is_empty());
    }
    // No capabilities list at all.
    let mut core = running_core();
    assert!(!core.mcp_apps_ready);
    feed(
        &mut core,
        json!({"type": "system", "subtype": "init", "session_id": "s-1"}),
    );
    assert!(!core.mcp_apps_ready);
    // All three, and a child `init` does not change it.
    let mut core = ready_core();
    assert!(core.mcp_apps_ready);
    let mut child = init_frame(&[]);
    child["parent_tool_use_id"] = json!("tu-agent");
    feed(&mut core, child);
    assert!(
        core.mcp_apps_ready,
        "a child init states no root capability"
    );
    // The next root `init` states it again.
    feed(&mut core, init_frame(&ALL[..2]));
    assert!(!core.mcp_apps_ready);
}

#[test]
fn mcp_tool_result_gives_candidate_with_call_tool_result() {
    let out = replay("mcp_app.jsonl");
    let found = candidates(&out);
    assert_eq!(found.len(), 1, "{found:?}: only the mcp__ result");
    let at = out
        .iter()
        .position(|o| matches!(o, Output::AppCandidate(_)))
        .unwrap();
    let Output::Emit(Event::ToolCompleted { node_id, .. }) = &out[at - 1] else {
        panic!("the candidate follows tool.completed: {:?}", out[at - 1]);
    };
    let c = &found[0];
    assert_eq!(&c.node_id, node_id);
    assert_eq!(c.tool, "mcp__demo__show_counter");
    assert_eq!(c.server, None);
    assert_eq!(c.server_hint.as_deref(), Some("demo"), "from can_use_tool");
    assert_eq!(c.resource_uri, None);
    assert_eq!(c.arguments, json!({}));
    assert_eq!(
        c.result,
        Some(json!({
            "content": [{"type": "text", "text": "{\"count\":2}"}],
            "structuredContent": {"count": 2},
        }))
    );
}

#[test]
fn candidate_without_can_use_tool_has_no_server_hint() {
    let mut core = ready_core();
    tool_use(
        &mut core,
        "tu-1",
        "mcp__demo__show_counter",
        json!({"n": 1}),
    );
    let out = feed(&mut core, result_frame("tu-1", false, None));
    let found = candidates(&out);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].server_hint, None);
    assert_eq!(found[0].arguments, json!({"n": 1}));
}

#[test]
fn server_hint_is_taken_once_and_comes_only_from_mcp_tools() {
    let mut core = ready_core();
    tool_use(&mut core, "tu-1", "mcp__demo__show_counter", json!({}));
    tool_use(&mut core, "tu-2", "Bash", json!({"command": "ls"}));
    feed(
        &mut core,
        mcp_permission("n-1", "mcp__demo__show_counter", "tu-1", "demo"),
    );
    // A non-mcp tool never leaves a hint, also when it names a server.
    feed(&mut core, mcp_permission("n-2", "Bash", "tu-2", "evil"));
    assert!(core.mcp_server_hints.take("tu-2").is_none());
    let out = feed(&mut core, result_frame("tu-1", false, None));
    assert_eq!(candidates(&out)[0].server_hint.as_deref(), Some("demo"));
    assert!(core.mcp_server_hints.take("tu-1").is_none(), "used up");
}

#[test]
fn server_hint_map_keeps_the_newest_256() {
    let mut core = ready_core();
    for n in 0..300 {
        let id = format!("tu-{n}");
        feed(
            &mut core,
            mcp_permission(&format!("n-{n}"), "mcp__demo__t", &id, "demo"),
        );
    }
    assert!(core.mcp_server_hints.take("tu-0").is_none());
    assert!(core.mcp_server_hints.take("tu-43").is_none());
    assert_eq!(core.mcp_server_hints.take("tu-44").as_deref(), Some("demo"));
    assert_eq!(
        core.mcp_server_hints.take("tu-299").as_deref(),
        Some("demo")
    );
}

#[test]
fn subagent_mcp_result_gives_no_candidate() {
    let mut core = ready_core();
    tool_use(&mut core, "tu-agent", "Agent", json!({"prompt": "go"}));
    feed(
        &mut core,
        json!({"type": "assistant", "parent_tool_use_id": "tu-agent",
               "message": {"role": "assistant", "content": [
                   {"type": "tool_use", "id": "tu-child", "name": "mcp__demo__show_counter", "input": {}}]}}),
    );
    let out = feed(&mut core, result_frame("tu-child", false, Some("tu-agent")));
    assert!(candidates(&out).is_empty(), "a child thread result");
    assert!(
        emits(&out)
            .iter()
            .any(|e| matches!(e, Event::ToolCompleted { .. })),
        "the tool still completes"
    );
}

#[test]
fn denied_or_error_result_gives_no_candidate() {
    // An error result.
    let mut core = ready_core();
    tool_use(&mut core, "tu-1", "mcp__demo__show_counter", json!({}));
    let out = feed(&mut core, result_frame("tu-1", true, None));
    assert!(candidates(&out).is_empty());
    // A denied tool use: its hint is used up and no candidate follows.
    let mut core = ready_core();
    tool_use(&mut core, "tu-2", "mcp__demo__show_counter", json!({}));
    feed(
        &mut core,
        mcp_permission("n-2", "mcp__demo__show_counter", "tu-2", "demo"),
    );
    core.answer(&crate::claude::tests::answer("req-3", "deny"))
        .unwrap();
    let out = feed(&mut core, result_frame("tu-2", true, None));
    assert!(candidates(&out).is_empty());
    assert!(core.mcp_server_hints.take("tu-2").is_none());
    // A tool that is not an MCP tool.
    let mut core = ready_core();
    tool_use(&mut core, "tu-3", "Bash", json!({"command": "ls"}));
    let out = feed(&mut core, result_frame("tu-3", false, None));
    assert!(candidates(&out).is_empty());
}

#[test]
fn oversized_input_and_result_are_dropped_from_the_candidate() {
    let mut core = ready_core();
    let big = "x".repeat(MAX_APP_INPUT_BYTES + 1);
    tool_use(&mut core, "tu-1", "mcp__demo__t", json!({"big": big}));
    let mut frame = result_frame("tu-1", false, None);
    frame["tool_use_result"]["content"] = json!("y".repeat(MAX_APP_RESULT_BYTES + 1));
    let found = candidates(&feed(&mut core, frame));
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].arguments, Value::Null);
    assert_eq!(found[0].result, None);
}

#[test]
fn candidate_result_is_normalized_and_needs_one_result_block() {
    let mut core = ready_core();
    tool_use(&mut core, "tu-1", "mcp__demo__t", json!({}));
    let mut frame = result_frame("tu-1", false, None);
    frame["tool_use_result"] = json!({
        "content": [{"type": "text", "text": "a"}],
        "structuredContent": null, "_meta": {"k": 1},
    });
    let found = candidates(&feed(&mut core, frame));
    assert_eq!(
        found[0].result,
        Some(json!({"content": [{"type": "text", "text": "a"}], "_meta": {"k": 1}}))
    );
    // A `_meta` that is not an object is dropped; a result that is not an
    // object gives no result.
    tool_use(&mut core, "tu-2", "mcp__demo__t", json!({}));
    let mut frame = result_frame("tu-2", false, None);
    frame["tool_use_result"]["_meta"] = json!("text");
    let found = candidates(&feed(&mut core, frame));
    assert!(found[0].result.as_ref().unwrap().get("_meta").is_none());
    tool_use(&mut core, "tu-3", "mcp__demo__t", json!({}));
    let mut frame = result_frame("tu-3", false, None);
    frame["tool_use_result"] = json!("plain text");
    assert_eq!(candidates(&feed(&mut core, frame))[0].result, None);
    // Two result blocks in one frame: the structured result belongs to none.
    tool_use(&mut core, "tu-4", "mcp__demo__t", json!({}));
    tool_use(&mut core, "tu-5", "mcp__demo__t", json!({}));
    let found = candidates(&feed(
        &mut core,
        json!({"type": "user", "message": {"role": "user", "content": [
            {"tool_use_id": "tu-4", "type": "tool_result", "content": "a"},
            {"tool_use_id": "tu-5", "type": "tool_result", "content": "b"}]},
               "tool_use_result": {"content": "a"}}),
    ));
    assert_eq!(found.len(), 2);
    assert!(found.iter().all(|c| c.result.is_none()));
}

// ---- requests and answers --------------------------------------------------

#[test]
fn app_requests_write_control_requests() {
    let mut core = ready_core();
    let ops = [
        AppOp::Tools,
        AppOp::Read {
            server: "demo".to_owned(),
            uri: "ui://demo/counter.html".to_owned(),
        },
        AppOp::Call {
            server: "my.server".to_owned(),
            tool: "bump".to_owned(),
            arguments: json!({"step": 1}),
        },
    ];
    let mut frames = Vec::new();
    for op in &ops {
        let (id, out) = core.app_request(op).unwrap();
        let mut sent = writes(&out);
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0]["request_id"], json!(id));
        frames.push(sent.remove(0));
    }
    assert_eq!(
        frames,
        vec![
            json!({"type": "control_request", "request_id": "fd-app-1",
                   "request": {"subtype": "mcp_status"}}),
            json!({"type": "control_request", "request_id": "fd-app-2",
                   "request": {"subtype": "mcp_read_resource", "serverName": "demo",
                               "uri": "ui://demo/counter.html"}}),
            json!({"type": "control_request", "request_id": "fd-app-3",
                   "request": {"subtype": "mcp_call", "tool": "mcp__my_server__bump",
                               "arguments": {"step": 1}, "result_not_for_model": true}}),
        ]
    );
    // After the exit nothing is written.
    core.on_exit(None);
    assert!(matches!(
        core.app_request(&AppOp::Tools),
        Err(crate::adapter::ProviderError::SessionEnded)
    ));
}

#[test]
fn control_error_answer_is_an_error() {
    let mut core = ready_core();
    let answer = |core: &mut ClaudeCore, id: &str, frame: Value| {
        assert!(feed(core, frame).is_empty(), "no event and no write");
        core.take_app_answer(id)
    };
    let (ok, _) = core.app_request(&AppOp::Tools).unwrap();
    let body = json!({"mcpServers": []});
    let success = json!({"type": "control_response", "response":
        {"subtype": "success", "request_id": ok, "response": body}});
    assert_eq!(answer(&mut core, &ok, success), Some(Ok(body)));
    let (bad, _) = core.app_request(&AppOp::Tools).unwrap();
    let error = json!({"type": "control_response", "response":
        {"subtype": "error", "request_id": bad, "error": "mcp_read_resource: uri must use the ui:// scheme"}});
    assert_eq!(
        answer(&mut core, &bad, error),
        Some(Err(
            "mcp_read_resource: uri must use the ui:// scheme".to_owned()
        ))
    );
    // An answer is taken once; a late answer of a forgotten request and an
    // answer of an unknown app id are dropped without a trace.
    assert_eq!(core.take_app_answer(&bad), None);
    let (late, _) = core.app_request(&AppOp::Tools).unwrap();
    core.forget_app_request(&late);
    let frame = json!({"type": "control_response", "response":
        {"subtype": "success", "request_id": late, "response": {}}});
    assert_eq!(answer(&mut core, &late, frame), None);
    let frame = json!({"type": "control_response", "response":
        {"subtype": "success", "request_id": "fd-app-99", "response": {}}});
    assert_eq!(answer(&mut core, "fd-app-99", frame), None);
    assert_eq!(core.unknown_frames(), 0);
}

#[test]
fn tools_parse_read_only_and_visibility() {
    // The recorded `mcp_status` answer (Claude 2.1.308, `rec-claude-turn.jsonl`).
    let answer = json!({"mcpServers": [{
        "name": "demo", "status": "connected",
        "serverInfo": {"name": "demo-apps", "version": "0.1.0"},
        "config": {"type": "stdio", "command": "python3", "args": ["-I", "demo_app_server.py"]},
        "scope": "dynamic", "source": "dynamic",
        "tools": [
            {"name": "show_counter", "annotations": {"readOnly": true},
             "_meta": {"ui": {"resourceUri": "ui://demo/counter.html", "visibility": ["model", "app"]}}},
            {"name": "bump", "annotations": {},
             "_meta": {"ui": {"resourceUri": "ui://demo/counter.html", "visibility": ["app"]}}},
        ]},
        {"name": "plain", "status": "connected", "tools": [{"name": "t", "annotations": {"title": "T"}}]},
        {"name": "down", "status": "failed"},
    ]});
    let uri = Some("ui://demo/counter.html".to_owned());
    let tool = |server: &str,
                name: &str,
                title: Option<&str>,
                read_only,
                visibility: Option<&[&str]>,
                uri| AppTool {
        server: server.to_owned(),
        name: name.to_owned(),
        title: title.map(str::to_owned),
        read_only,
        visibility: visibility.map(|v| v.iter().map(|s| (*s).to_owned()).collect()),
        resource_uri: uri,
    };
    assert_eq!(
        parse_tools(&answer),
        vec![
            tool(
                "demo",
                "show_counter",
                None,
                true,
                Some(&["model", "app"]),
                uri.clone()
            ),
            tool("demo", "bump", None, false, Some(&["app"]), uri),
            tool("plain", "t", Some("T"), false, None, None),
        ]
    );
    assert!(parse_tools(&json!({})).is_empty());
}
