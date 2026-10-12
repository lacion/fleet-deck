//! JSON-RPC 2.0 messages of the coordinator endpoint (ADR-006 §5.1).

use serde_json::{Value, json};

use super::bindings::{Binding, BindingState};
use super::ingress::Forward;
use super::tools;

/// The MCP protocol versions that the station accepts.
pub const PROTOCOL_VERSIONS: [&str; 2] = ["2025-06-18", "2025-03-26"];

/// The answer to one parsed message.
pub enum Answer {
    /// A notification: HTTP 202, no body.
    Accepted,
    /// A JSON-RPC response.
    Json(Value),
}

fn error(id: &Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

fn result(id: &Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

fn tool_text(text: &str, is_error: bool) -> Value {
    json!({"content": [{"type": "text", "text": text}], "isError": is_error})
}

/// Answer one JSON-RPC message of an authenticated binding.
pub async fn answer(msg: Value, binding: &Binding, forward: &dyn Forward) -> Answer {
    let Some(id) = msg.get("id").cloned() else {
        return Answer::Accepted;
    };
    let method = msg.get("method").and_then(Value::as_str).unwrap_or("");
    let params = msg.get("params").cloned().unwrap_or(Value::Null);
    let out = match method {
        "initialize" => {
            let version = params.get("protocolVersion").and_then(Value::as_str);
            match version.filter(|v| PROTOCOL_VERSIONS.contains(v)) {
                Some(v) => result(
                    &id,
                    json!({
                        "protocolVersion": v,
                        "capabilities": {"tools": {}},
                        "serverInfo": {"name": "fleetdeck", "version": env!("CARGO_PKG_VERSION")},
                        "instructions": binding.control_skill,
                    }),
                ),
                None => error(&id, -32602, "unsupported protocol version"),
            }
        }
        "ping" => result(&id, json!({})),
        "tools/list" => result(&id, tools::catalog(&binding.classes)),
        "tools/call" => {
            let name = params.get("name").and_then(Value::as_str).unwrap_or("");
            let args = params.get("arguments").cloned().unwrap_or(json!({}));
            match tools::tool(name).filter(|t| binding.classes.contains(&t.class)) {
                None => result(
                    &id,
                    tool_text("forbidden: the grant has no such tool", true),
                ),
                Some(tool) => match (binding.state, binding.caller()) {
                    (BindingState::Active, Some(caller)) => {
                        match forward.call(caller, tool.name, args).await {
                            Ok(v) => result(&id, tool_text(&v.to_string(), false)),
                            Err(e) => result(&id, tool_text(&e, true)),
                        }
                    }
                    _ => result(&id, tool_text("not_bound", true)),
                },
            }
        }
        _ => error(&id, -32601, "method not found"),
    };
    Answer::Json(out)
}
