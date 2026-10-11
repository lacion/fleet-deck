//! Mapping of CLI frames to canonical events (ADR-001 §2.4).

use fleetdeck_proto::event::{
    BackgroundTask, ExpiredReason, FileChange, InputState, RateLimit, ToolKind, ToolStatus,
    TurnStatus, Usage,
};
use fleetdeck_proto::{Event, NodeId};
use serde_json::{Map, Value};

use super::core::{ClaudeCore, HostRequest, Output, ToolNode, truncate};
use super::frames::{CliFrame, ControlResponse, MessageFrame, RateLimitInfo, ResultFrame};
use super::requests;

/// A `provider_refs` map; `None` values are skipped.
pub(super) fn refs(pairs: &[(&str, Option<&str>)]) -> Map<String, Value> {
    pairs
        .iter()
        .filter_map(|(k, v)| v.map(|v| ((*k).to_owned(), Value::from(v))))
        .collect()
}

/// Maps a Claude tool name to a canonical tool kind.
#[must_use]
pub fn tool_kind(name: &str) -> ToolKind {
    match name {
        "Bash" | "BashOutput" | "KillShell" | "Monitor" => ToolKind::Shell,
        "Edit" | "MultiEdit" | "Write" | "NotebookEdit" => ToolKind::Edit,
        "Read" => ToolKind::Read,
        "Grep" | "Glob" | "LS" => ToolKind::Search,
        "WebFetch" | "WebSearch" => ToolKind::Web,
        "Task" | "Agent" => ToolKind::Subagent,
        n if n.starts_with("mcp__") => ToolKind::Mcp,
        _ => ToolKind::Other,
    }
}

/// A one-line summary of a tool input for the board: the command, path,
/// pattern, URL or description, else compact JSON. At most 200 characters.
#[must_use]
pub fn input_summary(_tool: &str, input: &Value) -> String {
    const KEYS: &[&str] = &[
        "command",
        "file_path",
        "notebook_path",
        "path",
        "pattern",
        "url",
        "query",
        "description",
        "prompt",
    ];
    let text = KEYS
        .iter()
        .find_map(|k| input.get(*k).and_then(Value::as_str))
        .map_or_else(|| input.to_string(), str::to_owned);
    truncate(text.lines().next().unwrap_or_default(), 200)
}

fn edited_file(node: &ToolNode) -> Option<String> {
    (tool_kind(&node.name) == ToolKind::Edit)
        .then(|| {
            ["file_path", "notebook_path"]
                .iter()
                .find_map(|k| node.input.get(*k).and_then(Value::as_str))
                .map(str::to_owned)
        })
        .flatten()
}

pub(super) fn on_frame(core: &mut ClaudeCore, frame: CliFrame) -> Vec<Output> {
    match frame {
        CliFrame::System { subtype, body } => on_system(core, &subtype, &body),
        CliFrame::User(msg) => on_user(core, &msg),
        CliFrame::Assistant(msg) => on_assistant(core, &msg),
        CliFrame::StreamEvent {
            event,
            parent_tool_use_id,
        } => on_stream_event(core, &event, parent_tool_use_id.is_some()),
        CliFrame::Result(res) => on_result(core, &res),
        CliFrame::ControlRequest {
            request_id,
            request,
        } => requests::on_control_request(core, &request_id, request),
        CliFrame::ControlResponse(resp) => on_control_response(core, &resp),
        CliFrame::ControlCancelRequest { request_id } => requests::on_cancel(core, &request_id),
        CliFrame::CommandLifecycle {
            command_uuid,
            state,
        } => on_lifecycle(core, &command_uuid, &state),
        CliFrame::RateLimit(info) => on_rate_limit(&info),
        CliFrame::Unknown(_) => {
            core.unknown_frames += 1;
            Vec::new()
        }
    }
}

fn on_system(core: &mut ClaudeCore, subtype: &str, body: &Map<String, Value>) -> Vec<Output> {
    let get = |k: &str| body.get(k).and_then(Value::as_str).map(str::to_owned);
    match subtype {
        "init" => {
            core.native_session_id = get("session_id");
            core.mode = get("permissionMode");
            core.model = get("model");
            if core.started {
                // Claude sends `init` again for each turn: not a new session.
                return Vec::new();
            }
            core.started = true;
            let config = &core.config;
            vec![Output::Emit(Event::SessionStarted {
                provider: core.kind(),
                provider_instance: config.provider_instance.clone(),
                provider_version: get("claude_code_version"),
                model: get("model"),
                cwd: config.cwd.clone(),
                mode: get("permissionMode"),
                capabilities: Box::new(config.capabilities.clone()),
                resumed: config.resumed,
                provider_refs: refs(&[("session_id", get("session_id").as_deref())]),
                title: None,
                branch: None,
            })]
        }
        "background_tasks_changed" => {
            let tasks: Vec<BackgroundTask> = body
                .get("tasks")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(|t| {
                            Some(BackgroundTask {
                                task_id: t.get("task_id")?.as_str()?.to_owned(),
                                task_kind: t
                                    .get("task_type")
                                    .and_then(Value::as_str)
                                    .unwrap_or("unknown")
                                    .to_owned(),
                            })
                        })
                        .collect()
                })
                .unwrap_or_default();
            core.background.clone_from(&tasks);
            vec![Output::Emit(Event::BackgroundUpdated { tasks })]
        }
        // `status`, `ui_invalidate`, hook frames, ... never confirm an input:
        // only a frame that names the input's client uuid does (ADR-001).
        _ => Vec::new(),
    }
}

fn input_state(native: &str) -> Option<InputState> {
    Some(match native {
        "queued" => InputState::Received,
        "started" => InputState::Started,
        "completed" => InputState::Completed,
        "cancelled" => InputState::Cancelled,
        "discarded" => InputState::Discarded,
        "refused" => InputState::Refused,
        _ => return None,
    })
}

/// `command_lifecycle`: the CLI reports the fate of an input with a client uuid.
fn on_lifecycle(core: &mut ClaudeCore, command_uuid: &str, native: &str) -> Vec<Output> {
    core.lifecycle_seen = true;
    let Some(state) = input_state(native) else {
        core.unknown_frames += 1;
        return Vec::new();
    };
    set_input_state(core, command_uuid, state)
}

/// Records a reported input state. `Started` starts a turn when none runs;
/// during a running turn the input joins that turn (a steer, ADR-001).
fn set_input_state(core: &mut ClaudeCore, uuid: &str, state: InputState) -> Vec<Output> {
    let Some(idx) = core.inputs.iter().position(|i| i.uuid == uuid) else {
        // Not an input of this process (for example a Remote Control client).
        return Vec::new();
    };
    let mut out = Vec::new();
    if state == InputState::Started && core.inputs[idx].turn.is_none() {
        let turn_id = match core.turn.clone() {
            Some(t) => t,
            None => {
                let (t, started) = core.start_turn(Some(idx));
                out.extend(started);
                t
            }
        };
        core.inputs[idx].turn = Some(turn_id);
    }
    let entry = &mut core.inputs[idx];
    entry.state = Some(state);
    out.push(Output::Emit(Event::InputUpdated {
        input_id: entry.id.clone(),
        turn_id: entry.turn.clone(),
        state,
    }));
    // An interrupt that cancelled a waiting input ends here: no turn to mark.
    if state == InputState::Cancelled && core.is_quiet() {
        core.interrupt_requested = false;
    }
    out
}

fn on_user(core: &mut ClaudeCore, msg: &MessageFrame) -> Vec<Output> {
    let blocks = msg.blocks();
    let results: Vec<&Value> = blocks
        .iter()
        .filter(|b| b.get("type").and_then(Value::as_str) == Some("tool_result"))
        .collect();
    if results.is_empty() {
        // Fallback for a CLI without `command_lifecycle`: the replay echo of an
        // input with our client uuid means that the input went into a turn.
        let fallback = !core.lifecycle_seen && msg.is_replay;
        let pending = msg.uuid.as_deref().filter(|u| {
            core.inputs
                .iter()
                .any(|i| i.uuid == *u && i.state.is_none_or(|s| s == InputState::Received))
        });
        return match (fallback, pending) {
            (true, Some(uuid)) => set_input_state(core, uuid, InputState::Started),
            _ => Vec::new(),
        };
    }
    let mut out = Vec::new();
    for block in results {
        let Some(tool_use_id) = block.get("tool_use_id").and_then(Value::as_str) else {
            continue;
        };
        let Some(node) = core.tools.get_mut(tool_use_id) else {
            continue;
        };
        if node.done {
            continue;
        }
        node.done = true;
        let node = node.clone();
        let is_error = block
            .get("is_error")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let status = if node.denied {
            ToolStatus::Denied
        } else if is_error {
            ToolStatus::Failed
        } else {
            ToolStatus::Ok
        };
        let mut files = Vec::new();
        if status == ToolStatus::Ok
            && let Some(path) = edited_file(&node)
        {
            out.push(Output::Emit(Event::FileChanged {
                path: path.clone(),
                change: FileChange::Modified,
                node_id: Some(node.node_id.clone()),
            }));
            files.push(path);
        }
        out.push(Output::Emit(Event::ToolCompleted {
            node_id: node.node_id,
            status,
            files,
            exit_code: None,
        }));
    }
    out
}

/// Starts a turn that no host input started: the CLI works on its own, for
/// example after a background task finished (trap 4). Such a turn never
/// confirms a waiting host input. Child (subagent) frames never start a turn.
fn ensure_provider_turn(core: &mut ClaudeCore, is_child: bool) -> Vec<Output> {
    if core.turn.is_some() || is_child {
        return Vec::new();
    }
    core.start_turn(None).1
}

fn on_assistant(core: &mut ClaudeCore, msg: &MessageFrame) -> Vec<Output> {
    let mut out = ensure_provider_turn(core, msg.parent_tool_use_id.is_some());
    let parent_node: Option<NodeId> = msg
        .parent_tool_use_id
        .as_ref()
        .and_then(|p| core.tools.get(p))
        .map(|n| n.node_id.clone());
    for block in msg.blocks() {
        match block.get("type").and_then(Value::as_str) {
            Some("text") => {
                let text = block
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                // Child text after the parent turn ended has no turn (trap 5):
                // only its tool calls are kept.
                if let (false, Some(turn_id)) = (text.is_empty(), core.turn.clone()) {
                    out.push(Output::Emit(Event::MessageCompleted {
                        turn_id,
                        parent_node_id: parent_node.clone(),
                        text: text.to_owned(),
                    }));
                }
            }
            Some("tool_use") => out.extend(on_tool_use(core, &block, parent_node.as_ref())),
            _ => {}
        }
    }
    out
}

fn on_tool_use(core: &mut ClaudeCore, block: &Value, parent: Option<&NodeId>) -> Vec<Output> {
    let Some(id) = block.get("id").and_then(Value::as_str) else {
        return Vec::new();
    };
    if core.tools.contains_key(id) {
        return Vec::new();
    }
    let name = block
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let input = block.get("input").cloned().unwrap_or(Value::Null);
    let node_id = NodeId(core.ids.next("node"));
    let node = ToolNode {
        node_id: node_id.clone(),
        name: name.to_owned(),
        input: input.clone(),
        denied: false,
        done: false,
    };
    let files = edited_file(&node).into_iter().collect();
    core.tools.insert(id.to_owned(), node);
    let mut out = vec![Output::Emit(Event::ToolStarted {
        node_id,
        parent_node_id: parent.cloned(),
        tool_kind: tool_kind(name),
        title: name.to_owned(),
        input_summary: input_summary(name, &input),
        files,
        provider_refs: refs(&[("tool_use_id", Some(id))]),
    })];
    if name == "ExitPlanMode"
        && let Some(plan) = input.get("plan").and_then(Value::as_str)
    {
        let plan_id = core.ids.plan();
        out.push(Output::Emit(Event::PlanProposed {
            plan_id,
            turn_id: core.turn.clone(),
            markdown: plan.to_owned(),
        }));
    }
    out
}

fn on_stream_event(core: &mut ClaudeCore, event: &Value, is_child: bool) -> Vec<Output> {
    let kind = event.get("type").and_then(Value::as_str);
    let mut out = if kind == Some("message_start") {
        ensure_provider_turn(core, is_child)
    } else {
        Vec::new()
    };
    let Some(turn_id) = core.turn.clone() else {
        return out;
    };
    if is_child || kind != Some("content_block_delta") {
        return out;
    }
    let delta = event.get("delta").cloned().unwrap_or(Value::Null);
    let text = |key: &str| {
        delta
            .get(key)
            .and_then(Value::as_str)
            .filter(|t| !t.is_empty())
            .map(str::to_owned)
    };
    match delta.get("type").and_then(Value::as_str) {
        Some("text_delta") => {
            if let Some(text) = text("text") {
                out.push(Output::Emit(Event::MessageDelta {
                    turn_id,
                    parent_node_id: None,
                    text,
                }));
            }
        }
        Some("thinking_delta") => {
            if let Some(text) = text("thinking") {
                out.push(Output::Emit(Event::ReasoningDelta { turn_id, text }));
            }
        }
        _ => {}
    }
    out
}

fn usage_of(res: &ResultFrame) -> Usage {
    let u = res.usage.as_ref();
    let tokens = |k: &str| u.and_then(|u| u.get(k)).and_then(Value::as_u64);
    Usage {
        input_tokens: tokens("input_tokens"),
        output_tokens: tokens("output_tokens"),
        cost_usd: res.total_cost_usd,
    }
}

/// `result` subtypes that mean a usage limit of the run (trap 7).
const LIMIT_SUBTYPES: &[&str] = &["error_max_turns", "error_max_budget_usd"];

fn on_result(core: &mut ClaudeCore, res: &ResultFrame) -> Vec<Output> {
    let mut out = Vec::new();
    let usage = usage_of(res);
    let terminal_reason = res.body.get("terminal_reason").and_then(Value::as_str);
    let status = if core.interrupt_requested
        || terminal_reason.is_some_and(|r| r.contains("interrupt") || r.contains("abort"))
    {
        TurnStatus::Interrupted
    } else if res.is_error || res.subtype != "success" {
        TurnStatus::Failed
    } else {
        TurnStatus::Done
    };
    core.interrupt_requested = false;
    if LIMIT_SUBTYPES.contains(&res.subtype.as_str()) {
        out.push(Output::Emit(Event::Error {
            code: "usage_limit".to_owned(),
            message: format!("Claude stopped the turn: {}", res.subtype),
            retry_at: None,
        }));
    }
    // A finished turn leaves no request open (trap 6).
    out.extend(core.expire_open(ExpiredReason::TurnEnded));
    out.extend(close_open_tools(core));
    out.push(Output::Emit(Event::UsageUpdated {
        usage: usage.clone(),
        rate_limits: Vec::new(),
    }));
    out.extend(core.end_turn(status, Some(usage)));
    out
}

/// `rate_limit_event` (trap 7): the limit state goes to the usage panel; a
/// rejected request is an error with the reset time.
fn on_rate_limit(info: &RateLimitInfo) -> Vec<Output> {
    let resets_at = info.resets_at.map(|s| s.saturating_mul(1000));
    let scope = info
        .limit_type
        .clone()
        .unwrap_or_else(|| "unknown".to_owned());
    let mut out = vec![Output::Emit(Event::UsageUpdated {
        usage: Usage::default(),
        rate_limits: vec![RateLimit {
            scope: scope.clone(),
            used_percent: info.utilization.map(|u| u * 100.0),
            resets_at,
        }],
    })];
    if info.status == "rejected" {
        out.push(Output::Emit(Event::Error {
            code: "usage_limit".to_owned(),
            message: format!("Claude usage limit reached ({scope})"),
            retry_at: resets_at,
        }));
    }
    out
}

/// Closes every tool node without a result on a terminal path (turn end or
/// process exit, trap 6): `denied` if the owner denied it, else `failed`.
pub(super) fn close_open_tools(core: &mut ClaudeCore) -> Vec<Output> {
    let mut open: Vec<_> = core.tools.values_mut().filter(|n| !n.done).collect();
    open.sort_by(|a, b| a.node_id.cmp(&b.node_id));
    open.into_iter()
        .map(|n| {
            n.done = true;
            let status = if n.denied {
                ToolStatus::Denied
            } else {
                ToolStatus::Failed
            };
            Output::Emit(Event::ToolCompleted {
                node_id: n.node_id.clone(),
                status,
                files: Vec::new(),
                exit_code: None,
            })
        })
        .collect()
}

/// The model values of an `initialize` response (`value` and `resolvedModel`).
fn advertised_models(response: &Value) -> Vec<String> {
    let mut models: Vec<String> = response
        .get("models")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .flat_map(|m| {
            ["value", "resolvedModel"]
                .into_iter()
                .filter_map(|k| m.get(k).and_then(Value::as_str).map(str::to_owned))
        })
        .collect();
    models.sort();
    models.dedup();
    models
}

fn on_control_response(core: &mut ClaudeCore, resp: &ControlResponse) -> Vec<Output> {
    // The CLI also echoes responses to its own requests: ignore unknown ids.
    let Some(kind) = core.host_requests.remove(&resp.request_id) else {
        return Vec::new();
    };
    // Only the matching response confirms or refuses an interrupt.
    if let HostRequest::Interrupt(control_id) = kind {
        if !resp.ok {
            // A refused interrupt does not interrupt the turn.
            core.interrupt_requested = false;
        }
        return vec![Output::Emit(Event::InterruptResult {
            control_id,
            accepted: resp.ok,
            message: if resp.ok { None } else { resp.error.clone() },
        })];
    }
    if !resp.ok {
        return vec![Output::Emit(Event::Error {
            code: "control_request_failed".to_owned(),
            message: resp.error.clone().unwrap_or_default(),
            retry_at: None,
        })];
    }
    match kind {
        HostRequest::Initialize => {
            core.models = Some(advertised_models(&resp.response));
            if let Some(mode) = resp
                .response
                .get("current_permission_mode")
                .and_then(Value::as_str)
            {
                core.mode = Some(mode.to_owned());
            }
            Vec::new()
        }
        HostRequest::SetMode(mode) => {
            core.mode = Some(mode.clone());
            vec![Output::Emit(Event::ModeChanged { mode })]
        }
        HostRequest::SetModel(model) => {
            let model = model.unwrap_or_else(|| "default".to_owned());
            core.model = Some(model.clone());
            vec![Output::Emit(Event::ModelChanged { model })]
        }
        // Answered above.
        HostRequest::Interrupt(_) => Vec::new(),
    }
}
