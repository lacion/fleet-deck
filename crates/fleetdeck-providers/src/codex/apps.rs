//! MCP Apps in the Codex adapter (B15, design `docs/v2/design/mcp-apps.md`
//! §4.2): the candidate of a completed `mcpToolCall` item, the three app
//! requests and the reading of their answers.

use fleetdeck_proto::NodeId;
use fleetdeck_proto::apps::{MAX_APP_INPUT_BYTES, MAX_APP_RESULT_BYTES};
use serde_json::{Map, Value, json};

use crate::apps::{AppCandidate, AppTool, ToolAnswer, json_len, ui_meta};

/// The most pages of `mcpServerStatus/list` that one `tools` call follows.
pub const TOOLS_PAGES: usize = 5;

/// One app request to the app-server (the three methods of §4.2).
#[derive(Debug, Clone, PartialEq)]
pub enum AppOp {
    /// `mcpServerStatus/list`; `cursor` is the `nextCursor` of the page before.
    Tools { cursor: Option<String> },
    /// `mcpServer/resource/read`.
    Read { server: String, uri: String },
    /// `mcpServer/tool/call`.
    Call {
        server: String,
        tool: String,
        arguments: Value,
    },
}

impl AppOp {
    /// The method and the params of this request for the thread `thread`.
    #[must_use]
    pub fn wire(&self, thread: &str) -> (&'static str, Value) {
        match self {
            Self::Tools { cursor } => {
                let mut params = json!({"threadId": thread, "detail": "toolsAndAuthOnly"});
                if let Some(c) = cursor {
                    params["cursor"] = Value::from(c.as_str());
                }
                ("mcpServerStatus/list", params)
            }
            Self::Read { server, uri } => (
                "mcpServer/resource/read",
                json!({"threadId": thread, "server": server, "uri": uri}),
            ),
            Self::Call {
                server,
                tool,
                arguments,
            } => (
                "mcpServer/tool/call",
                json!({
                    "threadId": thread,
                    "server": server,
                    "tool": tool,
                    "arguments": arguments,
                }),
            ),
        }
    }
}

fn str_at<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str)
}

/// The candidate of a completed `mcpToolCall` item: status `completed`, no
/// `error`, a `result` object and a `ui://` URI (`mcpAppUi.resourceUri`, else
/// `mcpAppResourceUri`). `None` for every other item.
pub(super) fn candidate(item: &Value, node_id: &NodeId) -> Option<AppCandidate> {
    if str_at(item, "status") != Some("completed") {
        return None;
    }
    if item.get("error").is_some_and(|e| !e.is_null()) {
        return None;
    }
    let result = item.get("result").filter(|r| r.is_object())?;
    let uri = item
        .pointer("/mcpAppUi/resourceUri")
        .and_then(Value::as_str)
        .or_else(|| str_at(item, "mcpAppResourceUri"))
        .filter(|u| u.starts_with("ui://"))?;
    let server = str_at(item, "server").filter(|s| !s.is_empty())?;
    let tool = str_at(item, "tool").filter(|t| !t.is_empty())?;
    let arguments = match item.get("arguments") {
        Some(a) if json_len(a) <= MAX_APP_INPUT_BYTES => a.clone(),
        _ => Value::Null,
    };
    Some(AppCandidate {
        node_id: node_id.clone(),
        server: Some(server.to_owned()),
        server_hint: None,
        tool: tool.to_owned(),
        resource_uri: Some(uri.to_owned()),
        arguments,
        result: call_tool_result(result),
    })
}

/// The `CallToolResult` of an item result: `content`, `structuredContent` and
/// `_meta` (only when it is an object). `None` over `MAX_APP_RESULT_BYTES`.
fn call_tool_result(result: &Value) -> Option<Value> {
    let mut out = Map::new();
    for key in ["content", "structuredContent"] {
        if let Some(v) = result.get(key).filter(|v| !v.is_null()) {
            out.insert(key.to_owned(), v.clone());
        }
    }
    if let Some(meta) = result.get("_meta").filter(|m| m.is_object()) {
        out.insert("_meta".to_owned(), meta.clone());
    }
    let out = Value::Object(out);
    (json_len(&out) <= MAX_APP_RESULT_BYTES).then_some(out)
}

/// The tools of one `mcpServerStatus/list` page and its `nextCursor`.
#[must_use]
pub fn parse_tools_page(page: &Value) -> (Vec<AppTool>, Option<String>) {
    let mut tools = Vec::new();
    for status in page
        .get("data")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let Some(server) = str_at(status, "name") else {
            continue;
        };
        let Some(listed) = status.get("tools").and_then(Value::as_object) else {
            continue;
        };
        for (key, tool) in listed {
            let (visibility, resource_uri) = ui_meta(tool.get("_meta"));
            tools.push(AppTool {
                server: server.to_owned(),
                name: str_at(tool, "name").unwrap_or(key).to_owned(),
                title: str_at(tool, "title")
                    .or_else(|| tool.pointer("/annotations/title").and_then(Value::as_str))
                    .map(str::to_owned),
                read_only: tool.pointer("/annotations/readOnlyHint") == Some(&Value::Bool(true)),
                visibility,
                resource_uri,
            });
        }
    }
    let next = str_at(page, "nextCursor")
        .filter(|c| !c.is_empty())
        .map(str::to_owned);
    (tools, next)
}

/// The answer of `mcpServer/tool/call`: it proves that the tool handler
/// returned unless the result says `isError: true` (Codex may carry a
/// client-side `tools/call` timeout as an error result).
#[must_use]
pub fn tool_answer(result: Value) -> ToolAnswer {
    let proves_end = result.get("isError") != Some(&Value::Bool(true));
    ToolAnswer { result, proves_end }
}
