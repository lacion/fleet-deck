//! Mapping of CLI frames to canonical events.

use serde_json::{Map, Value};
use spike_canon::{
    ApprovalOption, BackgroundTask, CanonEvent, FileChange, InputState, ProviderRefs, RequestKind,
    ResponseChannel, ToolKind, ToolStatus, TurnStatus, Usage,
};

use super::{ClaudeCore, HostRequest, Output, RuntimeRequest, ToolNode};
use crate::control;
use crate::frames::{CliFrame, ControlRequest, ControlResponse, MessageFrame, ResultFrame};

fn refs(pairs: &[(&str, Option<&str>)]) -> ProviderRefs {
    let mut map = Map::new();
    for (k, v) in pairs {
        if let Some(v) = v {
            map.insert((*k).to_owned(), Value::String((*v).to_owned()));
        }
    }
    map
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

/// Maps a Claude tool name to the kind of its permission request.
#[must_use]
pub fn request_kind(name: &str) -> RequestKind {
    match tool_kind(name) {
        ToolKind::Shell => RequestKind::Command,
        ToolKind::Edit => RequestKind::FileChange,
        _ if name == "AskUserQuestion" => RequestKind::Question,
        _ => RequestKind::Tool,
    }
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
        } => on_control_request(core, &request_id, request),
        CliFrame::ControlResponse(resp) => on_control_response(core, &resp),
        CliFrame::ControlCancelRequest { request_id } => {
            let Some(app) = core.native_to_app.get(&request_id).cloned() else {
                return Vec::new();
            };
            match core.requests.get_mut(&app) {
                Some(req) if req.receipt.is_none() => {
                    req.receipt = Some(super::AnswerReceipt {
                        request_id: app.clone(),
                        option_id: String::new(),
                        duplicate: false,
                    });
                    vec![Output::Emit(CanonEvent::ApprovalExpired {
                        request_id: app,
                        reason: "cancelled_by_provider".to_owned(),
                    })]
                }
                _ => Vec::new(),
            }
        }
        CliFrame::CommandLifecycle {
            command_uuid,
            state,
        } => on_lifecycle(core, &command_uuid, &state),
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
            if core.started {
                // A second init (for example after a resume inside one process) is not a new session.
                return Vec::new();
            }
            core.started = true;
            vec![Output::Emit(CanonEvent::SessionStarted {
                provider: "claude".to_owned(),
                provider_version: get("claude_code_version"),
                model: get("model"),
                mode: get("permissionMode"),
                provider_refs: refs(&[("session_id", get("session_id").as_deref())]),
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
            vec![Output::Emit(CanonEvent::BackgroundUpdated { tasks })]
        }
        // `status`, `ui_invalidate`, hook frames, ... never confirm an input: only a
        // frame that names the input's client uuid does (ADR-001 "prompt confirmed").
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

/// Records a reported input state. `Started` starts a turn when none runs; during a
/// running turn the input is folded into that turn (a steer, ADR-001).
fn set_input_state(core: &mut ClaudeCore, input_id: &str, state: InputState) -> Vec<Output> {
    let Some(idx) = core.inputs.iter().position(|i| i.id == input_id) else {
        // Not an input of this process (for example a Remote Control client): ignore.
        return Vec::new();
    };
    let mut out = Vec::new();
    if state == InputState::Started && core.inputs[idx].turn.is_none() {
        let turn_id = match core.turn.clone() {
            Some(t) => t,
            None => {
                let t = core.ids.next("turn");
                core.turn = Some(t.clone());
                let session = core.native_session_id.clone();
                out.push(Output::Emit(CanonEvent::TurnStarted {
                    turn_id: t.clone(),
                    provider_refs: refs(&[
                        ("session_id", session.as_deref()),
                        ("command_uuid", Some(input_id)),
                    ]),
                }));
                t
            }
        };
        core.inputs[idx].turn = Some(turn_id);
    }
    let entry = &mut core.inputs[idx];
    entry.state = Some(state);
    out.push(Output::Emit(CanonEvent::InputUpdated {
        input_id: input_id.to_owned(),
        turn_id: entry.turn.clone(),
        state,
    }));
    out
}

fn on_user(core: &mut ClaudeCore, msg: &MessageFrame) -> Vec<Output> {
    let blocks = msg.blocks();
    let results: Vec<&Value> = blocks
        .iter()
        .filter(|b| b.get("type").and_then(Value::as_str) == Some("tool_result"))
        .collect();
    if results.is_empty() {
        // Fallback for a CLI without `command_lifecycle`: the replay echo of an input
        // with our client uuid means that the input went into a turn.
        let fallback = !core.lifecycle_seen && msg.is_replay;
        let pending = msg.uuid.as_deref().filter(|u| {
            core.inputs
                .iter()
                .any(|i| i.id == *u && i.state.is_none_or(|s| s == InputState::Received))
        });
        return match (fallback, pending) {
            (true, Some(id)) => set_input_state(core, id, InputState::Started),
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
        if status == ToolStatus::Ok
            && tool_kind(&node.name) == ToolKind::Edit
            && let Some(path) = node.input.get("file_path").and_then(Value::as_str)
        {
            out.push(Output::Emit(CanonEvent::FileChanged {
                path: path.to_owned(),
                change: FileChange::Modified,
            }));
        }
        out.push(Output::Emit(CanonEvent::ToolCompleted {
            node_id: node.node_id,
            status,
        }));
    }
    out
}

/// Starts a turn that no host input started: the CLI works on its own, for example
/// after a background task finished (`system/task_notification`, trap 4). Such a
/// turn never confirms a pending host input.
fn ensure_provider_turn(core: &mut ClaudeCore, is_child: bool) -> Vec<Output> {
    if core.turn.is_some() || is_child {
        return Vec::new();
    }
    let t = core.ids.next("turn");
    core.turn = Some(t.clone());
    let session = core.native_session_id.clone();
    vec![Output::Emit(CanonEvent::TurnStarted {
        turn_id: t,
        provider_refs: refs(&[
            ("session_id", session.as_deref()),
            ("origin", Some("provider")),
        ]),
    })]
}

fn on_assistant(core: &mut ClaudeCore, msg: &MessageFrame) -> Vec<Output> {
    let mut out = ensure_provider_turn(core, msg.parent_tool_use_id.is_some());
    let Some(turn_id) = core.turn.clone() else {
        return out;
    };
    let parent_node = msg
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
                if !text.is_empty() && parent_node.is_none() {
                    out.push(Output::Emit(CanonEvent::MessageCompleted {
                        turn_id: turn_id.clone(),
                        text: text.to_owned(),
                    }));
                }
            }
            Some("tool_use") => {
                let Some(id) = block.get("id").and_then(Value::as_str) else {
                    continue;
                };
                if core.tools.contains_key(id) {
                    continue;
                }
                let name = block
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let input = block.get("input").cloned().unwrap_or(Value::Null);
                let node_id = core.ids.next("node");
                core.tools.insert(
                    id.to_owned(),
                    ToolNode {
                        node_id: node_id.clone(),
                        name: name.to_owned(),
                        input: input.clone(),
                        denied: false,
                        done: false,
                    },
                );
                out.push(Output::Emit(CanonEvent::ToolStarted {
                    node_id,
                    parent_node_id: parent_node.clone(),
                    tool_kind: tool_kind(name),
                    title: name.to_owned(),
                    provider_refs: refs(&[("tool_use_id", Some(id))]),
                }));
                if name == "ExitPlanMode"
                    && let Some(plan) = input.get("plan").and_then(Value::as_str)
                {
                    out.push(Output::Emit(CanonEvent::PlanProposed {
                        markdown: plan.to_owned(),
                    }));
                }
            }
            _ => {}
        }
    }
    out
}

fn on_stream_event(core: &mut ClaudeCore, event: &Value, is_child: bool) -> Vec<Output> {
    let mut out = if event.get("type").and_then(Value::as_str) == Some("message_start") {
        ensure_provider_turn(core, is_child)
    } else {
        Vec::new()
    };
    let Some(turn_id) = core.turn.clone() else {
        return out;
    };
    if is_child || event.get("type").and_then(Value::as_str) != Some("content_block_delta") {
        return out;
    }
    let delta = event.get("delta").cloned().unwrap_or(Value::Null);
    match delta.get("type").and_then(Value::as_str) {
        Some("text_delta") => {
            if let Some(text) = delta.get("text").and_then(Value::as_str) {
                out.push(Output::Emit(CanonEvent::MessageDelta {
                    turn_id,
                    text: text.to_owned(),
                }));
            }
        }
        Some("thinking_delta") => {
            if let Some(text) = delta.get("thinking").and_then(Value::as_str)
                && !text.is_empty()
            {
                out.push(Output::Emit(CanonEvent::ReasoningDelta {
                    turn_id,
                    text: text.to_owned(),
                }));
            }
        }
        _ => {}
    }
    out
}

fn usage_of(res: &ResultFrame) -> Usage {
    let u = res.usage.as_ref();
    Usage {
        input_tokens: u
            .and_then(|u| u.get("input_tokens"))
            .and_then(Value::as_u64),
        output_tokens: u
            .and_then(|u| u.get("output_tokens"))
            .and_then(Value::as_u64),
        cost_usd: res.total_cost_usd,
    }
}

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
    // A finished turn leaves no request open (trap 6).
    out.extend(core.expire_open("turn_ended"));
    out.extend(close_open_tools(core));
    out.push(Output::Emit(CanonEvent::UsageUpdated {
        usage: usage.clone(),
    }));
    if let Some(turn_id) = core.turn.take() {
        out.push(Output::Emit(CanonEvent::TurnCompleted {
            turn_id,
            status,
            usage: Some(usage),
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
            Output::Emit(CanonEvent::ToolCompleted {
                node_id: n.node_id.clone(),
                status,
            })
        })
        .collect()
}

/// The part of the CLI's `permission_suggestions` that `allow_always` may send
/// (DL-009): allow rules only, re-scoped to `destination: "session"`. Suggestions
/// that write settings files (`localSettings`, `projectSettings`, `userSettings`
/// destinations are rewritten), change the mode (`setMode`, L8: modes change only
/// through `set_permission_mode`), or widen the workspace (`addDirectories`) are
/// dropped. `None` when nothing is left.
pub(super) fn session_rules(suggestions: Option<&Value>) -> Option<Value> {
    let kept: Vec<Value> = suggestions?
        .as_array()?
        .iter()
        .filter(|s| {
            matches!(
                s.get("type").and_then(Value::as_str),
                Some("addRules" | "replaceRules")
            ) && s.get("behavior").and_then(Value::as_str) == Some("allow")
        })
        .map(|s| {
            let mut s = s.clone();
            if let Some(m) = s.as_object_mut() {
                m.insert("destination".to_owned(), Value::from("session"));
            }
            s
        })
        .collect();
    (!kept.is_empty()).then_some(Value::Array(kept))
}

fn options_for(session_rules: Option<&Value>) -> Vec<ApprovalOption> {
    let mut options = vec![ApprovalOption {
        option_id: "allow".to_owned(),
        label: "Allow once".to_owned(),
    }];
    if session_rules.is_some() {
        options.push(ApprovalOption {
            option_id: "allow_always".to_owned(),
            label: "Allow always (this session)".to_owned(),
        });
    }
    options.push(ApprovalOption {
        option_id: "deny".to_owned(),
        label: "Deny".to_owned(),
    });
    options
}

fn on_control_request(
    core: &mut ClaudeCore,
    native_id: &str,
    request: ControlRequest,
) -> Vec<Output> {
    match request {
        ControlRequest::CanUseTool {
            tool_name,
            input,
            tool_use_id,
            permission_suggestions,
            ..
        } => {
            if let Some(app) = core.native_to_app.get(native_id) {
                // A repeated request with the same native id is the same request.
                let _ = app;
                return Vec::new();
            }
            let request_id = core.ids.next("req");
            let node_id = tool_use_id
                .as_ref()
                .and_then(|t| core.tools.get(t))
                .map(|n| n.node_id.clone());
            let rules = session_rules(permission_suggestions.as_ref());
            let options = options_for(rules.as_ref());
            core.native_to_app
                .insert(native_id.to_owned(), request_id.clone());
            core.requests.insert(
                request_id.clone(),
                RuntimeRequest {
                    native_request_id: native_id.to_owned(),
                    tool_use_id: tool_use_id.clone(),
                    input,
                    suggestions: rules,
                    options: options.clone(),
                    receipt: None,
                },
            );
            vec![Output::Emit(CanonEvent::ApprovalRequested {
                request_id,
                node_id,
                request_kind: request_kind(&tool_name),
                title: tool_name,
                options,
                response: ResponseChannel::RpcReply,
                provider_refs: refs(&[
                    ("control_request_id", Some(native_id)),
                    ("tool_use_id", tool_use_id.as_deref()),
                ]),
            })]
        }
        // The host registers no hooks and no SDK MCP servers: fail closed.
        ControlRequest::HookCallback { .. } | ControlRequest::McpMessage { .. } => {
            vec![Output::Write(control::control_error(
                native_id,
                "fleetdeck: not registered",
            ))]
        }
        ControlRequest::Unknown { subtype, .. } => vec![
            Output::Write(control::control_error(
                native_id,
                &format!("fleetdeck: unsupported control request {subtype}"),
            )),
            Output::Emit(CanonEvent::Error {
                code: "unknown_control_request".to_owned(),
                message: format!("denied unknown control request subtype '{subtype}'"),
            }),
        ],
    }
}

fn on_control_response(core: &mut ClaudeCore, resp: &ControlResponse) -> Vec<Output> {
    let Some(kind) = core.host_requests.remove(&resp.request_id) else {
        return Vec::new();
    };
    if !resp.ok {
        return vec![Output::Emit(CanonEvent::Error {
            code: "control_request_failed".to_owned(),
            message: resp.error.clone().unwrap_or_default(),
        })];
    }
    match kind {
        HostRequest::SetMode(mode) => {
            core.mode = Some(mode.clone());
            vec![Output::Emit(CanonEvent::ModeChanged { mode })]
        }
        HostRequest::Initialize | HostRequest::Interrupt | HostRequest::SetModel => Vec::new(),
    }
}
