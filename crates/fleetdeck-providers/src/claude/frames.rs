//! Parser for the frames that the Claude Code CLI writes on stdout in
//! `--output-format stream-json` mode (CLI → host).
//!
//! Rules (PLAN §13):
//! - The parser never panics. A line that is not JSON is a [`ParseError`].
//! - A frame with an unknown `type` is [`CliFrame::Unknown`]: the adapter logs it and ignores it.
//! - A `control_request` with an unknown `subtype` is [`ControlRequest::Unknown`]: the adapter
//!   answers it with an error, so an unknown permission request fails closed.

use serde_json::{Map, Value};

/// Error for a stdout line that is not a JSON object.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ParseError {
    #[error("line is not valid JSON: {0}")]
    NotJson(String),
    #[error("frame is not a JSON object")]
    NotObject,
}

/// One frame from the CLI.
#[derive(Debug, Clone, PartialEq)]
pub enum CliFrame {
    /// `{"type":"system", "subtype": ...}`: `init`, `status`, `hook_*`, `thinking_tokens`, ...
    System {
        subtype: String,
        body: Map<String, Value>,
    },
    /// A complete (or growing) assistant message.
    Assistant(MessageFrame),
    /// A user message (tool results, or a replayed user message).
    User(MessageFrame),
    /// A raw Anthropic streaming event (`--include-partial-messages`).
    StreamEvent {
        event: Value,
        parent_tool_use_id: Option<String>,
    },
    /// End of one turn.
    Result(ResultFrame),
    /// The CLI asks the host something (permission, hook, MCP).
    ControlRequest {
        request_id: String,
        request: ControlRequest,
    },
    /// The CLI answers a host control request.
    ControlResponse(ControlResponse),
    /// The CLI cancels one of its own control requests.
    ControlCancelRequest { request_id: String },
    /// Fate of a host input that carried a client `uuid` (`msg_lifecycle_v1`):
    /// `queued`, `started`, `completed`, `cancelled`, `discarded`, `refused`.
    CommandLifecycle { command_uuid: String, state: String },
    /// `rate_limit_event`: the state of a plan usage limit (trap 7). Shape from
    /// the Agent SDK types; no recording has one yet (matrix: unverified).
    RateLimit(RateLimitInfo),
    /// A frame type that this adapter does not know. Logged and ignored.
    Unknown(Value),
}

/// `rate_limit_event.rate_limit_info`.
#[derive(Debug, Clone, PartialEq)]
pub struct RateLimitInfo {
    /// `allowed`, `allowed_warning` or `rejected`.
    pub status: String,
    /// For example `five_hour` or `seven_day`.
    pub limit_type: Option<String>,
    /// Unix seconds.
    pub resets_at: Option<i64>,
    /// Used share of the limit, 0.0 to 1.0.
    pub utilization: Option<f64>,
}

/// Fields shared by `assistant` and `user` frames.
#[derive(Debug, Clone, PartialEq)]
pub struct MessageFrame {
    pub session_id: Option<String>,
    pub parent_tool_use_id: Option<String>,
    pub uuid: Option<String>,
    /// True for a user message that the CLI echoes back (`--replay-user-messages`).
    pub is_replay: bool,
    /// The Anthropic message object (`role`, `content`, `id`, `model`, `usage`).
    pub message: Value,
    /// `tool_use_result.agentId` of a user frame, kept only when it matches
    /// `[a-z0-9]{1,64}`; the rest of the structured result is dropped (DL-191).
    pub agent_id: Option<String>,
    /// `tool_use_result.runId` of a user frame (a workflow run id), kept only
    /// when it matches `wf_[a-z0-9-]{1,64}` (B12).
    pub workflow_run_id: Option<String>,
    /// The structured `tool_use_result` (or `toolUseResult`) of a user frame.
    /// Only the MCP Apps candidate of an `mcp__` result reads it (B15); every
    /// other use of the structured result stays closed (DL-191).
    pub tool_use_result: Option<Value>,
}

impl MessageFrame {
    /// Content blocks of the message. A string content is one `text` block.
    #[must_use]
    pub fn blocks(&self) -> Vec<Value> {
        match self.message.get("content") {
            Some(Value::Array(items)) => items.clone(),
            Some(Value::String(text)) => {
                vec![serde_json::json!({"type": "text", "text": text})]
            }
            _ => Vec::new(),
        }
    }
}

/// The `result` frame at the end of a turn.
#[derive(Debug, Clone, PartialEq)]
pub struct ResultFrame {
    pub subtype: String,
    pub is_error: bool,
    pub session_id: Option<String>,
    pub stop_reason: Option<String>,
    pub total_cost_usd: Option<f64>,
    pub usage: Option<Value>,
    pub result: Option<String>,
    pub body: Map<String, Value>,
}

/// A control request from the CLI to the host.
#[derive(Debug, Clone, PartialEq)]
pub enum ControlRequest {
    /// Permission prompt (`--permission-prompt-tool stdio`).
    CanUseTool {
        tool_name: String,
        input: Value,
        tool_use_id: Option<String>,
        permission_suggestions: Option<Value>,
        blocked_path: Option<String>,
        body: Map<String, Value>,
    },
    /// A hook callback registered by the host at `initialize`.
    HookCallback { callback_id: String, input: Value },
    /// A message for an in-process (SDK) MCP server.
    McpMessage { server_name: String, message: Value },
    /// A subtype that this adapter does not know. Fails closed.
    Unknown { subtype: String, body: Value },
}

/// A control response from the CLI to the host.
#[derive(Debug, Clone, PartialEq)]
pub struct ControlResponse {
    pub request_id: String,
    pub ok: bool,
    pub response: Value,
    pub error: Option<String>,
    /// `can_use_tool` requests that this CLI process issued and did not resolve
    /// (Claude Code 2.1.268 or later). `None` on an older CLI.
    pub pending_permission_requests: Option<Vec<Value>>,
}

fn opt_str(map: &Map<String, Value>, key: &str) -> Option<String> {
    map.get(key).and_then(Value::as_str).map(str::to_owned)
}

/// The agent id of the top-level structured tool result (`tool_use_result`,
/// or `toolUseResult` in a transcript). Only a string of `[a-z0-9]{1,64}`
/// counts; any other value is dropped (DL-191).
fn structured_agent_id(map: &Map<String, Value>) -> Option<String> {
    let result = map
        .get("tool_use_result")
        .or_else(|| map.get("toolUseResult"))?;
    let id = result.get("agentId")?.as_str()?;
    valid_agent_id(id).then(|| id.to_owned())
}

/// True for an agent id of `[a-z0-9]{1,64}`.
pub(super) fn valid_agent_id(id: &str) -> bool {
    (1..=64).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
}

/// The workflow run id of the top-level structured tool result. Only a string
/// of `wf_[a-z0-9-]{1,64}` counts; nothing is read from text (B12).
fn structured_workflow_run_id(map: &Map<String, Value>) -> Option<String> {
    let result = map
        .get("tool_use_result")
        .or_else(|| map.get("toolUseResult"))?;
    let id = result.get("runId")?.as_str()?;
    let rest = id.strip_prefix("wf_")?;
    let valid = (1..=64).contains(&rest.len())
        && rest
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    valid.then(|| id.to_owned())
}

fn str_or_empty(map: &Map<String, Value>, key: &str) -> String {
    opt_str(map, key).unwrap_or_default()
}

/// Parses one stdout line.
///
/// # Errors
/// Returns [`ParseError`] when the line is not a JSON object.
pub fn parse_line(line: &str) -> Result<CliFrame, ParseError> {
    let value: Value =
        serde_json::from_str(line.trim()).map_err(|e| ParseError::NotJson(e.to_string()))?;
    parse_value(value)
}

/// Parses one frame that is already JSON.
///
/// # Errors
/// Returns [`ParseError::NotObject`] when the value is not a JSON object.
pub fn parse_value(value: Value) -> Result<CliFrame, ParseError> {
    let Value::Object(mut map) = value else {
        return Err(ParseError::NotObject);
    };
    let kind = str_or_empty(&map, "type");
    let frame = match kind.as_str() {
        "system" => CliFrame::System {
            subtype: str_or_empty(&map, "subtype"),
            body: map,
        },
        "assistant" | "user" => {
            let agent_id = structured_agent_id(&map);
            let workflow_run_id = structured_workflow_run_id(&map);
            // Moved out, not copied: the frame is not used after this.
            let tool_use_result = map
                .remove("tool_use_result")
                .or_else(|| map.remove("toolUseResult"));
            let msg = MessageFrame {
                session_id: opt_str(&map, "session_id"),
                parent_tool_use_id: opt_str(&map, "parent_tool_use_id"),
                uuid: opt_str(&map, "uuid"),
                is_replay: map.get("isReplay").and_then(Value::as_bool) == Some(true),
                message: map.get("message").cloned().unwrap_or(Value::Null),
                agent_id,
                workflow_run_id,
                tool_use_result,
            };
            if kind == "assistant" {
                CliFrame::Assistant(msg)
            } else {
                CliFrame::User(msg)
            }
        }
        "stream_event" => CliFrame::StreamEvent {
            event: map.get("event").cloned().unwrap_or(Value::Null),
            parent_tool_use_id: opt_str(&map, "parent_tool_use_id"),
        },
        "result" => CliFrame::Result(ResultFrame {
            subtype: str_or_empty(&map, "subtype"),
            is_error: map
                .get("is_error")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            session_id: opt_str(&map, "session_id"),
            stop_reason: opt_str(&map, "stop_reason"),
            total_cost_usd: map.get("total_cost_usd").and_then(Value::as_f64),
            usage: map.get("usage").cloned(),
            result: opt_str(&map, "result"),
            body: map,
        }),
        "control_request" => parse_control_request(map),
        "control_response" => parse_control_response(&map),
        "control_cancel_request" => CliFrame::ControlCancelRequest {
            request_id: str_or_empty(&map, "request_id"),
        },
        "command_lifecycle" => match (opt_str(&map, "command_uuid"), opt_str(&map, "state")) {
            (Some(command_uuid), Some(state)) => CliFrame::CommandLifecycle {
                command_uuid,
                state,
            },
            _ => CliFrame::Unknown(Value::Object(map)),
        },
        "rate_limit_event" => match map.get("rate_limit_info").and_then(Value::as_object) {
            Some(info) => CliFrame::RateLimit(RateLimitInfo {
                status: str_or_empty(info, "status"),
                limit_type: opt_str(info, "rateLimitType"),
                resets_at: info.get("resetsAt").and_then(Value::as_i64),
                utilization: info.get("utilization").and_then(Value::as_f64),
            }),
            None => CliFrame::Unknown(Value::Object(map)),
        },
        _ => CliFrame::Unknown(Value::Object(map)),
    };
    Ok(frame)
}

fn parse_control_request(map: Map<String, Value>) -> CliFrame {
    let request_id = str_or_empty(&map, "request_id");
    let Some(Value::Object(req)) = map.get("request").cloned() else {
        return CliFrame::ControlRequest {
            request_id,
            request: ControlRequest::Unknown {
                subtype: String::new(),
                body: map.get("request").cloned().unwrap_or(Value::Null),
            },
        };
    };
    let subtype = str_or_empty(&req, "subtype");
    let request = match subtype.as_str() {
        "can_use_tool" => match req.get("tool_name").and_then(Value::as_str) {
            Some(tool_name) => ControlRequest::CanUseTool {
                tool_name: tool_name.to_owned(),
                input: req.get("input").cloned().unwrap_or(Value::Null),
                tool_use_id: opt_str(&req, "tool_use_id"),
                permission_suggestions: req.get("permission_suggestions").cloned(),
                blocked_path: opt_str(&req, "blocked_path"),
                body: req,
            },
            // A permission request without a tool name is malformed: fail closed.
            None => ControlRequest::Unknown {
                subtype,
                body: Value::Object(req),
            },
        },
        "hook_callback" => ControlRequest::HookCallback {
            callback_id: str_or_empty(&req, "callback_id"),
            input: req.get("input").cloned().unwrap_or(Value::Null),
        },
        "mcp_message" => ControlRequest::McpMessage {
            server_name: str_or_empty(&req, "server_name"),
            message: req.get("message").cloned().unwrap_or(Value::Null),
        },
        _ => ControlRequest::Unknown {
            subtype,
            body: Value::Object(req),
        },
    };
    CliFrame::ControlRequest {
        request_id,
        request,
    }
}

fn parse_control_response(map: &Map<String, Value>) -> CliFrame {
    let resp = map
        .get("response")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    CliFrame::ControlResponse(ControlResponse {
        request_id: str_or_empty(&resp, "request_id"),
        ok: resp.get("subtype").and_then(Value::as_str) == Some("success"),
        response: resp.get("response").cloned().unwrap_or(Value::Null),
        error: opt_str(&resp, "error"),
        pending_permission_requests: resp
            .get("pending_permission_requests")
            .and_then(Value::as_array)
            .cloned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use serde_json::json;

    #[test]
    fn parses_init_and_result() {
        let init =
            parse_line(r#"{"type":"system","subtype":"init","session_id":"s1","model":"m"}"#)
                .unwrap();
        assert!(matches!(init, CliFrame::System { ref subtype, .. } if subtype == "init"));
        let res = parse_line(
            r#"{"type":"result","subtype":"success","is_error":false,"session_id":"s1","total_cost_usd":0.5,"stop_reason":"end_turn"}"#,
        )
        .unwrap();
        let CliFrame::Result(r) = res else {
            panic!("not a result")
        };
        assert_eq!(r.total_cost_usd, Some(0.5));
        assert_eq!(r.stop_reason.as_deref(), Some("end_turn"));
    }

    #[test]
    fn parses_can_use_tool() {
        let f = parse_value(json!({
            "type":"control_request","request_id":"abc",
            "request":{"subtype":"can_use_tool","tool_name":"Bash","input":{"command":"ls"},"tool_use_id":"toolu_1"}
        }))
        .unwrap();
        let CliFrame::ControlRequest {
            request_id,
            request:
                ControlRequest::CanUseTool {
                    tool_name,
                    tool_use_id,
                    ..
                },
        } = f
        else {
            panic!("not can_use_tool")
        };
        assert_eq!(request_id, "abc");
        assert_eq!(tool_name, "Bash");
        assert_eq!(tool_use_id.as_deref(), Some("toolu_1"));
    }

    #[test]
    fn unknown_permission_request_is_unknown() {
        let f = parse_value(json!({
            "type":"control_request","request_id":"x",
            "request":{"subtype":"new_kind_of_prompt","foo":1}
        }))
        .unwrap();
        assert!(matches!(
            f,
            CliFrame::ControlRequest {
                request: ControlRequest::Unknown { .. },
                ..
            }
        ));
        let malformed = parse_value(json!({
            "type":"control_request","request_id":"y","request":{"subtype":"can_use_tool"}
        }))
        .unwrap();
        assert!(matches!(
            malformed,
            CliFrame::ControlRequest {
                request: ControlRequest::Unknown { .. },
                ..
            }
        ));
    }

    #[test]
    fn unknown_frame_type_is_kept() {
        let f = parse_line(r#"{"type":"brand_new","x":1}"#).unwrap();
        assert!(matches!(f, CliFrame::Unknown(_)));
        assert_eq!(parse_line("[1,2]"), Err(ParseError::NotObject));
        assert!(matches!(
            parse_line("not json"),
            Err(ParseError::NotJson(_))
        ));
    }

    #[test]
    fn parses_a_rate_limit_event() {
        let f = parse_value(json!({
            "type":"rate_limit_event","session_id":"s",
            "rate_limit_info":{"status":"rejected","resetsAt":1_767_230_000,"rateLimitType":"five_hour","utilization":1.0}
        }))
        .unwrap();
        let CliFrame::RateLimit(info) = f else {
            panic!("not a rate limit")
        };
        assert_eq!(info.status, "rejected");
        assert_eq!(info.limit_type.as_deref(), Some("five_hour"));
        assert_eq!(info.resets_at, Some(1_767_230_000));
        assert!(matches!(
            parse_value(json!({"type":"rate_limit_event"})).unwrap(),
            CliFrame::Unknown(_)
        ));
    }

    #[test]
    fn string_content_is_one_text_block() {
        let f =
            parse_value(json!({"type":"user","message":{"role":"user","content":"hi"}})).unwrap();
        let CliFrame::User(m) = f else { panic!() };
        assert_eq!(m.blocks(), vec![json!({"type":"text","text":"hi"})]);
    }

    fn arb_json() -> impl Strategy<Value = Value> {
        let leaf = prop_oneof![
            Just(Value::Null),
            any::<bool>().prop_map(Value::Bool),
            any::<i64>().prop_map(Value::from),
            "[a-z_]{0,12}".prop_map(Value::String),
            prop_oneof![
                Just("system"),
                Just("assistant"),
                Just("user"),
                Just("result"),
                Just("stream_event"),
                Just("control_request"),
                Just("control_response"),
                Just("can_use_tool"),
                Just("subtype"),
                Just("request"),
                Just("type")
            ]
            .prop_map(|s| Value::String(s.to_owned())),
        ];
        leaf.prop_recursive(4, 32, 6, |inner| {
            prop_oneof![
                prop::collection::vec(inner.clone(), 0..4).prop_map(Value::Array),
                prop::collection::btree_map(
                    prop_oneof![
                        Just("type".to_owned()),
                        Just("subtype".to_owned()),
                        Just("request".to_owned()),
                        Just("response".to_owned()),
                        Just("message".to_owned()),
                        Just("tool_name".to_owned()),
                        "[a-z]{1,6}"
                    ],
                    inner,
                    0..5
                )
                .prop_map(|m| Value::Object(m.into_iter().collect())),
            ]
        })
    }

    proptest! {
        #[test]
        fn parser_never_panics_on_any_text(s in ".{0,200}") {
            let _ = parse_line(&s);
        }

        #[test]
        fn parser_never_panics_on_any_json(v in arb_json()) {
            let line = v.to_string();
            let _ = parse_line(&line);
        }
    }
}
