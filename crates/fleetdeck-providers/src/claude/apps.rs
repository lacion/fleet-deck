//! MCP Apps in the Claude core (B15, design `docs/v2/design/mcp-apps.md` §4.2,
//! "Claude"): the readiness from `system/init.capabilities`, the server hint of
//! a `can_use_tool` request, the candidate of a completed `mcp__` tool call, and
//! the three app control requests (`mcp_status`, `mcp_read_resource`, `mcp_call`)
//! with their answers.

use std::collections::{HashMap, HashSet, VecDeque};

use fleetdeck_proto::apps::{MAX_APP_INPUT_BYTES, MAX_APP_RESULT_BYTES};
use serde_json::{Map, Value, json};

use crate::adapter::{ProviderError, Result};
use crate::apps::{AppCandidate, AppTool, claude_tool_name, json_len, ui_meta};
use crate::claude::control;
use crate::claude::core::{ClaudeCore, Output, ToolNode};
use crate::claude::frames::{ControlRequest, ControlResponse, MessageFrame};

/// The `system/init.capabilities` that the app path needs, all three.
const CAPABILITIES: [&str; 3] = [
    "mcp_read_resource_v1",
    "mcp_tool_ui_meta_v1",
    "mcp_call_result_not_for_model_v1",
];

/// The id prefix of an app request. The number is `ClaudeCore::app_seq`, which
/// the checkpoint keeps: the CLI aborts a request that reuses the id of a live
/// one, so an id is never used twice in one process.
const APP_ID_PREFIX: &str = "fd-app-";

/// The most `tool_use_id → server` hints that the core keeps; the oldest goes first.
const MAX_HINTS: usize = 256;

/// The raw MCP server name of the `can_use_tool` request of a tool use. It is
/// evidence that the station's name resolution must agree with.
#[derive(Debug, Default)]
pub(in crate::claude) struct ServerHints {
    map: HashMap<String, String>,
    order: VecDeque<String>,
}

impl ServerHints {
    pub(in crate::claude) fn insert(&mut self, tool_use_id: &str, server: &str) {
        if self
            .map
            .insert(tool_use_id.to_owned(), server.to_owned())
            .is_none()
        {
            self.order.push_back(tool_use_id.to_owned());
            while self.order.len() > MAX_HINTS {
                let Some(oldest) = self.order.pop_front() else {
                    break;
                };
                self.map.remove(&oldest);
            }
        }
    }

    pub(in crate::claude) fn take(&mut self, tool_use_id: &str) -> Option<String> {
        let server = self.map.remove(tool_use_id)?;
        self.order.retain(|id| id != tool_use_id);
        Some(server)
    }
}

/// The app requests in flight and their answers. Not in the checkpoint.
#[derive(Debug, Default)]
pub(in crate::claude) struct AppRequests {
    pending: HashSet<String>,
    answers: HashMap<String, std::result::Result<Value, String>>,
}

/// One app request to the CLI (the three control requests of §4.2).
#[derive(Debug, Clone, PartialEq)]
pub enum AppOp {
    /// `mcp_status`: the servers and their tools.
    Tools,
    /// `mcp_read_resource`.
    Read { server: String, uri: String },
    /// `mcp_call`.
    Call {
        server: String,
        tool: String,
        arguments: Value,
    },
}

impl AppOp {
    /// The `control_request` frame with the request id `id`. An `mcp_call`
    /// carries exactly `subtype`, `tool`, `arguments` and `result_not_for_model`:
    /// never `input_files`, `output_files`, `expires_at` or `timeout_ms`, which
    /// select Claude's staged path that turns errors into `isError` results.
    fn frame(&self, id: &str) -> Value {
        match self {
            Self::Tools => control::control_request(id, "mcp_status", json!({})),
            Self::Read { server, uri } => control::control_request(
                id,
                "mcp_read_resource",
                json!({"serverName": server, "uri": uri}),
            ),
            Self::Call {
                server,
                tool,
                arguments,
            } => control::control_request(
                id,
                "mcp_call",
                json!({
                    "tool": claude_tool_name(server, tool),
                    "arguments": arguments,
                    "result_not_for_model": true,
                }),
            ),
        }
    }
}

/// Whether a root `system/init` states all three capabilities, and the update
/// of the core. A child `init` (a `parent_tool_use_id`) changes nothing.
pub(super) fn note_init(core: &mut ClaudeCore, body: &Map<String, Value>) {
    if !matches!(body.get("parent_tool_use_id"), None | Some(Value::Null)) {
        return;
    }
    let listed = body.get("capabilities").and_then(Value::as_array);
    core.mcp_apps_ready = CAPABILITIES
        .iter()
        .all(|c| listed.is_some_and(|l| l.iter().any(|v| v.as_str() == Some(c))));
}

/// Keeps the server name of a `can_use_tool` request of an `mcp__` tool.
pub(super) fn note_hint(core: &mut ClaudeCore, request: &ControlRequest) {
    let ControlRequest::CanUseTool {
        tool_name,
        tool_use_id: Some(tool_use_id),
        body,
        ..
    } = request
    else {
        return;
    };
    if !tool_name.starts_with("mcp__") {
        return;
    }
    if let Some(server) = body
        .get("mcp_server")
        .and_then(|s| s.get("name"))
        .and_then(Value::as_str)
    {
        core.mcp_server_hints.insert(tool_use_id, server);
    }
}

/// The candidate of one `tool_result` block, after its `tool.completed`: the
/// main thread, status `Ok`, a tool whose name starts with `mcp__`, and a core
/// that is ready. The hint of the tool use is used up either way. `single` is
/// true when the frame has one `tool_result` block: only then does the
/// structured `tool_use_result` belong to this tool use.
pub(super) fn on_tool_result(
    core: &mut ClaudeCore,
    tool_use_id: &str,
    node: &ToolNode,
    ok: bool,
    single: bool,
    msg: &MessageFrame,
) -> Option<Output> {
    let hint = core.mcp_server_hints.take(tool_use_id);
    if !ok
        || !core.mcp_apps_ready
        || msg.parent_tool_use_id.is_some()
        || !node.name.starts_with("mcp__")
    {
        return None;
    }
    let arguments = if json_len(&node.input) <= MAX_APP_INPUT_BYTES {
        node.input.clone()
    } else {
        Value::Null
    };
    let result = single
        .then_some(msg.tool_use_result.as_ref())
        .flatten()
        .and_then(call_tool_result);
    Some(Output::AppCandidate(AppCandidate {
        node_id: node.node_id.clone(),
        server: None,
        server_hint: hint,
        tool: node.name.clone(),
        resource_uri: None,
        arguments,
        result,
    }))
}

/// The `CallToolResult` of a structured `tool_use_result`: a string `content`
/// becomes one text block; `structuredContent` is kept; `_meta` only when it is
/// an object. `None` when it is not an object or is over `MAX_APP_RESULT_BYTES`.
fn call_tool_result(raw: &Value) -> Option<Value> {
    let raw = raw.as_object()?;
    let mut out = Map::new();
    match raw.get("content") {
        Some(Value::String(text)) => {
            out.insert(
                "content".to_owned(),
                json!([{"type": "text", "text": text}]),
            );
        }
        Some(content @ Value::Array(_)) => {
            out.insert("content".to_owned(), content.clone());
        }
        _ => {}
    }
    if let Some(v) = raw.get("structuredContent").filter(|v| !v.is_null()) {
        out.insert("structuredContent".to_owned(), v.clone());
    }
    if let Some(meta) = raw.get("_meta").filter(|m| m.is_object()) {
        out.insert("_meta".to_owned(), meta.clone());
    }
    let out = Value::Object(out);
    (json_len(&out) <= MAX_APP_RESULT_BYTES).then_some(out)
}

/// The tools of an `mcp_status` answer, in the answer order.
#[must_use]
pub fn parse_tools(answer: &Value) -> Vec<AppTool> {
    let str_at = |v: &Value, key: &str| v.get(key).and_then(Value::as_str).map(str::to_owned);
    let mut tools = Vec::new();
    for server in answer
        .get("mcpServers")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let Some(name) = str_at(server, "name") else {
            continue;
        };
        for tool in server
            .get("tools")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let Some(tool_name) = str_at(tool, "name") else {
                continue;
            };
            let (visibility, resource_uri) = ui_meta(tool.get("_meta"));
            tools.push(AppTool {
                server: name.clone(),
                name: tool_name,
                title: str_at(tool, "title").or_else(|| {
                    tool.pointer("/annotations/title")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                }),
                read_only: tool.pointer("/annotations/readOnly") == Some(&Value::Bool(true)),
                visibility,
                resource_uri,
            });
        }
    }
    tools
}

impl ClaudeCore {
    /// Writes one app request and returns its host id. The answer comes with
    /// [`Self::take_app_answer`].
    ///
    /// # Errors
    /// [`ProviderError::SessionEnded`] after the process ended;
    /// [`ProviderError::NotReady`] when the CLI did not state the three MCP
    /// Apps capabilities (nothing is written).
    pub fn app_request(&mut self, op: &AppOp) -> Result<(String, Vec<Output>)> {
        if self.ended {
            return Err(ProviderError::SessionEnded);
        }
        if !self.mcp_apps_ready {
            return Err(ProviderError::NotReady("mcp apps"));
        }
        self.app_seq += 1;
        let id = format!("{APP_ID_PREFIX}{}", self.app_seq);
        self.app_requests.pending.insert(id.clone());
        let frame = op.frame(&id);
        Ok((id, vec![Output::Write(frame)]))
    }

    /// Takes the answer of an app request: the result, or the error text of the
    /// CLI. `None` while it waits.
    pub fn take_app_answer(&mut self, id: &str) -> Option<std::result::Result<Value, String>> {
        self.app_requests.answers.remove(id)
    }

    /// Forgets an app request that nobody waits for any more (a timeout or a
    /// dropped wait). Its late answer is then dropped.
    pub fn forget_app_request(&mut self, id: &str) {
        self.app_requests.pending.remove(id);
        self.app_requests.answers.remove(id);
    }

    /// Takes the `control_response` of an app request. An answer of an id that
    /// is not pending (a late answer after a timeout or a restart) is dropped.
    /// Returns `false` when the id is not an app request id.
    pub(super) fn app_answer(&mut self, resp: &ControlResponse) -> bool {
        let id = resp.request_id.as_str();
        if !id.starts_with(APP_ID_PREFIX) {
            return false;
        }
        if self.app_requests.pending.remove(id) {
            let answer = if resp.ok {
                Ok(resp.response.clone())
            } else {
                Err(resp.error.clone().unwrap_or_default())
            };
            self.app_requests.answers.insert(id.to_owned(), answer);
        }
        true
    }
}
