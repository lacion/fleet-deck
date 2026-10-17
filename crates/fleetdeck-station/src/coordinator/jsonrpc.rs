//! JSON-RPC 2.0 messages of the coordinator endpoint (ADR-006 §5.1).

use serde_json::{Value, json};

use super::bindings::{Binding, BindingState};
use super::ingress::{FleetTools, Forward};
use super::tools;

/// The MCP protocol versions that the station accepts. `2025-11-25` is
/// the version that Claude 2.1.296 sends (DL-220); the station uses only
/// `initialize`, `ping` and the tools methods, which it does not change.
pub const PROTOCOL_VERSIONS: [&str; 3] = ["2025-11-25", "2025-06-18", "2025-03-26"];

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

/// The tool error of a fleet-control call while the switch is off.
const FLEET_CONTROL_OFF: &str = "not_granted: fleet control is off on this station";

/// Answer one JSON-RPC message of an authenticated binding. `fleet_control`
/// is the live station setting `allow_fleet_control`, read for this
/// request (ADR-008 §3).
pub async fn answer(
    msg: Value,
    binding: &Binding,
    forward: &dyn Forward,
    fleet: Option<&dyn FleetTools>,
    fleet_control: bool,
) -> Answer {
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
            match tools::tool(name).filter(|t| t.granted(&binding.classes)) {
                None => result(
                    &id,
                    tool_text("forbidden: the grant has no such tool", true),
                ),
                Some(tool) => match (binding.state, binding.caller()) {
                    // A fleet-control grant (a new tool, or an ADR-006 tool
                    // under such a grant) is never forwarded while the
                    // switch is off. The session's own tools are not
                    // fleet control.
                    (BindingState::Active, Some(_))
                        if !fleet_control && binding.is_fleet_control() && !tool.is_own() =>
                    {
                        result(&id, tool_text(FLEET_CONTROL_OFF, true))
                    }
                    (BindingState::Active, Some(caller)) => {
                        let outcome = if tool.is_own() {
                            match fleet {
                                Some(fleet) => fleet.call(caller, tool.name, args).await,
                                None => {
                                    Err("unavailable: this station has no session tools".to_owned())
                                }
                            }
                        } else {
                            forward.call(caller, tool.name, args).await
                        };
                        match outcome {
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
