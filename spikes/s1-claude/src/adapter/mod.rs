//! Sans-IO core of the Claude adapter spike.
//!
//! The core holds no process and does no I/O. Host commands and CLI frames go
//! in; canonical events and frames to write go out ([`Output`]). The live
//! process driver and the replay harness both wrap this core, so a replay runs
//! the real mapping code (PLAN §13, L20).

mod map;

use std::collections::BTreeMap;

use serde_json::Value;
use spike_canon::{ApprovalOption, CanonEvent, OrdinalIds};

use crate::control::{self, PermissionDecision};
use crate::frames::CliFrame;

/// Native Claude permission modes that `claude` 2.1.289 accepts
/// (`claude --help`, `--permission-mode`). The station offers only these (L8).
pub const NATIVE_MODES: &[&str] = &[
    "manual",
    "acceptEdits",
    "plan",
    "auto",
    "dontAsk",
    "bypassPermissions",
];

/// The most restrictive native mode: the fallback for an unknown stored mode (L8).
pub const SAFEST_MODE: &str = "manual";

/// One output of the core.
#[derive(Debug, Clone, PartialEq)]
pub enum Output {
    /// A canonical event for the event log.
    Emit(CanonEvent),
    /// A frame to write to the CLI's stdin.
    Write(Value),
}

/// Error of a host command.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum AdapterError {
    #[error("unknown request {0}")]
    UnknownRequest(String),
    #[error("option {option} is not offered by request {request}")]
    UnknownOption { request: String, option: String },
    #[error("mode {0} is not a native Claude mode")]
    UnsupportedMode(String),
    #[error("no turn is running")]
    NoTurn,
}

/// The answer of the owner to a runtime request. `option_id` is a native option id.
#[derive(Debug, Clone, PartialEq)]
pub struct Answer {
    pub option_id: String,
    /// Replaces the tool input before allow ("edit input before allow").
    pub updated_input: Option<Value>,
    /// Reason text for a deny.
    pub message: Option<String>,
}

/// Receipt of an answer. A second answer to the same request returns the first receipt (L3).
#[derive(Debug, Clone, PartialEq)]
pub struct AnswerReceipt {
    pub request_id: String,
    pub option_id: String,
    pub duplicate: bool,
}

/// Session options that become `claude` arguments.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SessionSpec {
    pub model: Option<String>,
    /// Native permission mode. An unknown value runs as [`SAFEST_MODE`].
    pub mode: Option<String>,
    pub resume: Option<String>,
    pub fork: bool,
}

/// The `claude` argument list for a driven session.
#[must_use]
pub fn cli_args(spec: &SessionSpec) -> Vec<String> {
    let mut args: Vec<String> = [
        "-p",
        "--input-format",
        "stream-json",
        "--output-format",
        "stream-json",
        "--verbose",
        "--include-partial-messages",
        "--replay-user-messages",
        "--permission-prompt-tool",
        "stdio",
    ]
    .iter()
    .map(|s| (*s).to_owned())
    .collect();
    // Always pass a mode: the user's settings can default to bypassPermissions.
    let mode = spec
        .mode
        .as_deref()
        .filter(|m| NATIVE_MODES.contains(m))
        .unwrap_or(SAFEST_MODE);
    args.extend(["--permission-mode".to_owned(), mode.to_owned()]);
    if let Some(model) = &spec.model {
        args.extend(["--model".to_owned(), model.clone()]);
    }
    if let Some(id) = &spec.resume {
        args.extend(["--resume".to_owned(), id.clone()]);
        if spec.fork {
            args.push("--fork-session".to_owned());
        }
    }
    args
}

/// An open or resolved runtime request.
#[derive(Debug, Clone)]
struct RuntimeRequest {
    native_request_id: String,
    tool_use_id: Option<String>,
    input: Value,
    suggestions: Option<Value>,
    options: Vec<ApprovalOption>,
    receipt: Option<AnswerReceipt>,
}

/// What a host control request was for, so its response can be mapped.
#[derive(Debug, Clone, PartialEq)]
enum HostRequest {
    Initialize,
    Interrupt,
    SetMode(String),
    SetModel,
}

/// A tool node of the current session.
#[derive(Debug, Clone)]
struct ToolNode {
    node_id: String,
    name: String,
    input: Value,
    denied: bool,
}

/// The sans-IO Claude adapter core.
#[derive(Debug, Default)]
pub struct ClaudeCore {
    ids: OrdinalIds,
    host_ids: u64,
    native_session_id: Option<String>,
    started: bool,
    mode: Option<String>,
    /// Inputs written but not yet confirmed by the CLI (FIFO).
    pending_inputs: Vec<String>,
    /// App id of the running turn.
    turn: Option<String>,
    interrupt_requested: bool,
    host_requests: BTreeMap<String, HostRequest>,
    requests: BTreeMap<String, RuntimeRequest>,
    native_to_app: BTreeMap<String, String>,
    tools: BTreeMap<String, ToolNode>,
    unknown_frames: usize,
}

impl ClaudeCore {
    /// A new core.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Native session id, after `system/init`.
    #[must_use]
    pub fn native_session_id(&self) -> Option<&str> {
        self.native_session_id.as_deref()
    }

    /// Number of frames that the core did not know and ignored.
    #[must_use]
    pub fn unknown_frames(&self) -> usize {
        self.unknown_frames
    }

    /// App id of the running turn.
    #[must_use]
    pub fn current_turn(&self) -> Option<&str> {
        self.turn.as_deref()
    }

    /// Ids of the requests that are open (not answered, not expired).
    #[must_use]
    pub fn open_requests(&self) -> Vec<String> {
        self.requests
            .iter()
            .filter(|(_, r)| r.receipt.is_none())
            .map(|(id, _)| id.clone())
            .collect()
    }

    /// The native control request id of an app request id.
    #[must_use]
    pub fn native_request_id(&self, request_id: &str) -> Option<&str> {
        self.requests
            .get(request_id)
            .map(|r| r.native_request_id.as_str())
    }

    /// The app request id of a native control request id.
    #[must_use]
    pub fn app_request_id(&self, native: &str) -> Option<&str> {
        self.native_to_app.get(native).map(String::as_str)
    }

    /// The tool input of a request, as the CLI sent it.
    #[must_use]
    pub fn request_input(&self, request_id: &str) -> Option<&Value> {
        self.requests.get(request_id).map(|r| &r.input)
    }

    fn next_host_id(&mut self, kind: HostRequest) -> String {
        self.host_ids += 1;
        let id = format!("fd-{}", self.host_ids);
        self.host_requests.insert(id.clone(), kind);
        id
    }

    /// Sends `initialize`. Returns the host request id and the outputs.
    pub fn initialize(&mut self) -> (String, Vec<Output>) {
        let id = self.next_host_id(HostRequest::Initialize);
        let out = vec![Output::Write(control::initialize(&id))];
        (id, out)
    }

    /// Writes a user message. During a running turn this is a mid-turn message
    /// (steer or queue, see ADR-001). The turn starts when the CLI confirms it.
    pub fn prompt(&mut self, text: &str) -> Vec<Output> {
        self.pending_inputs.push(text.to_owned());
        let session = self.native_session_id.clone().unwrap_or_default();
        vec![Output::Write(control::user_message(text, &session))]
    }

    /// Requests an interrupt. The terminal state arrives later with `result` (trap 10).
    ///
    /// # Errors
    /// [`AdapterError::NoTurn`] when no turn runs and no input is pending (trap 3).
    pub fn interrupt(&mut self) -> Result<(String, Vec<Output>), AdapterError> {
        if self.turn.is_none() && self.pending_inputs.is_empty() {
            return Err(AdapterError::NoTurn);
        }
        self.interrupt_requested = true;
        let id = self.next_host_id(HostRequest::Interrupt);
        let out = vec![Output::Write(control::interrupt(&id))];
        Ok((id, out))
    }

    /// Changes the permission mode. Only native modes are accepted (L8).
    ///
    /// # Errors
    /// [`AdapterError::UnsupportedMode`] for a mode that Claude does not offer.
    pub fn set_mode(&mut self, mode: &str) -> Result<(String, Vec<Output>), AdapterError> {
        if !NATIVE_MODES.contains(&mode) {
            return Err(AdapterError::UnsupportedMode(mode.to_owned()));
        }
        let id = self.next_host_id(HostRequest::SetMode(mode.to_owned()));
        let out = vec![Output::Write(control::set_permission_mode(&id, mode))];
        Ok((id, out))
    }

    /// Changes the model. `None` returns to the default model (trap 2).
    pub fn set_model(&mut self, model: Option<&str>) -> (String, Vec<Output>) {
        let id = self.next_host_id(HostRequest::SetModel);
        let out = vec![Output::Write(control::set_model(&id, model))];
        (id, out)
    }

    /// Answers a runtime request with a native option id. Idempotent (L3).
    ///
    /// # Errors
    /// Unknown request id or an option id that the request does not offer.
    pub fn answer(
        &mut self,
        request_id: &str,
        answer: &Answer,
    ) -> Result<(AnswerReceipt, Vec<Output>), AdapterError> {
        let req = self
            .requests
            .get_mut(request_id)
            .ok_or_else(|| AdapterError::UnknownRequest(request_id.to_owned()))?;
        if let Some(receipt) = &req.receipt {
            let mut dup = receipt.clone();
            dup.duplicate = true;
            return Ok((dup, Vec::new()));
        }
        if !req.options.iter().any(|o| o.option_id == answer.option_id) {
            return Err(AdapterError::UnknownOption {
                request: request_id.to_owned(),
                option: answer.option_id.clone(),
            });
        }
        let decision = match answer.option_id.as_str() {
            "allow" | "allow_always" => PermissionDecision::Allow {
                updated_input: answer
                    .updated_input
                    .clone()
                    .unwrap_or_else(|| req.input.clone()),
            },
            _ => PermissionDecision::Deny {
                message: answer
                    .message
                    .clone()
                    .unwrap_or_else(|| "Denied by the owner".to_owned()),
                interrupt: false,
            },
        };
        let mut frame = control::permission_response(&req.native_request_id, &decision);
        if answer.option_id == "allow_always"
            && let (Some(body), Some(s)) = (
                frame["response"]["response"].as_object_mut(),
                req.suggestions.clone(),
            )
        {
            body.insert("updatedPermissions".to_owned(), s);
        }
        if matches!(decision, PermissionDecision::Deny { .. })
            && let Some(node) = req.tool_use_id.as_ref().and_then(|t| self.tools.get_mut(t))
        {
            node.denied = true;
        }
        let receipt = AnswerReceipt {
            request_id: request_id.to_owned(),
            option_id: answer.option_id.clone(),
            duplicate: false,
        };
        req.receipt = Some(receipt.clone());
        let out = vec![
            Output::Write(frame),
            Output::Emit(CanonEvent::ApprovalResolved {
                request_id: request_id.to_owned(),
                option_id: answer.option_id.clone(),
            }),
        ];
        Ok((receipt, out))
    }

    /// Feeds one CLI frame.
    pub fn on_frame(&mut self, frame: CliFrame) -> Vec<Output> {
        map::on_frame(self, frame)
    }

    /// The CLI process exited. Closes everything that is open (trap 6).
    pub fn on_exit(&mut self, exit_code: Option<i32>) -> Vec<Output> {
        let mut out = self.expire_open("process_lost");
        if let Some(turn_id) = self.turn.take() {
            out.push(Output::Emit(CanonEvent::TurnCompleted {
                turn_id,
                status: spike_canon::TurnStatus::Failed,
                usage: None,
            }));
        }
        out.push(Output::Emit(CanonEvent::SessionEnded {
            reason: "process_exit".to_owned(),
            exit_code,
        }));
        out
    }

    fn expire_open(&mut self, reason: &str) -> Vec<Output> {
        let mut out = Vec::new();
        for (id, req) in &mut self.requests {
            if req.receipt.is_none() {
                req.receipt = Some(AnswerReceipt {
                    request_id: id.clone(),
                    option_id: String::new(),
                    duplicate: false,
                });
                out.push(Output::Emit(CanonEvent::ApprovalExpired {
                    request_id: id.clone(),
                    reason: reason.to_owned(),
                }));
            }
        }
        out
    }
}

#[cfg(test)]
mod tests;
