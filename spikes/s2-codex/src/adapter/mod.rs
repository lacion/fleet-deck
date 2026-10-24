//! Sans-IO core of the native Codex adapter (spike S2).
//!
//! The core has no I/O. The host gives it commands ([`HostCmd`]) and the frames
//! that `codex app-server` writes; the core returns [`Output`]: canonical events
//! (PLAN §6.3) and JSON-RPC frames to write to the app-server's stdin. A live
//! driver, the recorder and the replay harness all use the same core.
//!
//! Wire facts (codex-cli 0.160.0, see `docs/v2/adr/ADR-002-*.md`):
//! - `turn/start` while a turn runs joins the running turn (its reply carries the
//!   running turn id). The stable API has no follow-up queue, so the core keeps
//!   follow-ups itself and sends each one after `turn/completed`.
//! - A prompt is confirmed when `item/started` reports a `userMessage` whose
//!   `clientId` is the input id the host passed as `clientUserMessageId`.
//! - Server request ids are per-process integers that restart at 0.

mod host;
mod map;
mod requests;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_review;

use std::collections::{BTreeMap, HashMap, VecDeque};

use serde_json::{Value, json};
use spike_canon::{CanonEvent, InputState, OrdinalIds};

pub use host::HostCmd;
pub use map::{HANDLED_NOTIFICATIONS, IGNORED_NOTIFICATIONS};
pub use requests::{Answer, AnswerReceipt, DECLINED_REQUESTS, HANDLED_REQUESTS};

/// Codex collaboration modes (`ModeKind` of the 0.160.0 schema).
pub const COLLABORATION_MODES: &[&str] = &["plan", "default"];

/// Client requests that the core sends.
pub const CLIENT_REQUESTS: &[&str] = &[
    "initialize",
    "thread/start",
    "thread/resume",
    "thread/fork",
    "turn/start",
    "turn/steer",
    "turn/interrupt",
    "thread/backgroundTerminals/list",
];

use crate::rpc::{self, Incoming};

/// Codex approval policies (`AskForApproval`) that the app-server enforces.
pub const APPROVAL_POLICIES: &[&str] = &["untrusted", "on-request", "never"];
/// Codex sandbox modes (`SandboxMode`).
pub const SANDBOX_MODES: &[&str] = &["read-only", "workspace-write", "danger-full-access"];
/// Mode of a new session when the owner stored none.
pub const DEFAULT_MODE: &str = "untrusted/workspace-write";
/// Most restrictive mode: used for an unknown stored mode (L8).
pub const SAFEST_MODE: &str = "untrusted/read-only";

/// One result of the core.
#[derive(Debug, Clone, PartialEq)]
pub enum Output {
    /// A canonical event for the station.
    Emit(CanonEvent),
    /// A JSON-RPC frame for the app-server's stdin.
    Write(Value),
}

/// A host command that the core refuses.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AdapterError {
    #[error("unknown request {0}")]
    UnknownRequest(String),
    #[error("request {0} expired before it was answered")]
    Expired(String),
    #[error("option {option} is not offered by request {request}")]
    UnknownOption { request: String, option: String },
    #[error("mode {0} is not a native Codex mode")]
    UnsupportedMode(String),
    #[error("no turn is running")]
    NoTurn,
    #[error("no thread is open")]
    NoThread,
    #[error("{0} is not supported by codex app-server")]
    Unsupported(&'static str),
    #[error("invalid answer: {0}")]
    InvalidAnswer(String),
}

/// A native mode: approval policy × sandbox, written `"<policy>/<sandbox>"`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mode {
    pub approval_policy: String,
    pub sandbox: String,
}

impl Mode {
    /// Parses `"<policy>/<sandbox>"`; `None` if either part is not native.
    pub fn parse(s: &str) -> Option<Self> {
        let (a, b) = s.split_once('/')?;
        (APPROVAL_POLICIES.contains(&a) && SANDBOX_MODES.contains(&b)).then(|| Self {
            approval_policy: a.to_owned(),
            sandbox: b.to_owned(),
        })
    }

    /// Parses a stored mode; an unknown or missing mode becomes the safest one.
    pub fn or_safest(s: Option<&str>) -> Self {
        let wanted = s.unwrap_or(DEFAULT_MODE);
        Self::parse(wanted).unwrap_or_else(|| {
            Self::parse(SAFEST_MODE).unwrap_or_else(|| Self {
                approval_policy: "untrusted".into(),
                sandbox: "read-only".into(),
            })
        })
    }

    pub fn name(&self) -> String {
        format!("{}/{}", self.approval_policy, self.sandbox)
    }

    /// `SandboxPolicy` value for `turn/start` (the per-turn shape differs from
    /// the `thread/start` `sandbox` enum).
    pub fn sandbox_policy(&self) -> Value {
        match self.sandbox.as_str() {
            "danger-full-access" => json!({"type": "dangerFullAccess"}),
            "workspace-write" => json!({"type": "workspaceWrite"}),
            _ => json!({"type": "readOnly"}),
        }
    }
}

/// Settings of a driven session.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SessionSpec {
    pub model: Option<String>,
    pub effort: Option<String>,
    /// `"<policy>/<sandbox>"`; missing → [`DEFAULT_MODE`], unknown → [`SAFEST_MODE`].
    pub mode: Option<String>,
    /// Working directory of the thread.
    pub cwd: Option<String>,
    /// Native thread id to resume instead of starting a new thread.
    pub resume: Option<String>,
    /// With `resume`: fork that thread into a new thread (`thread/fork`)
    /// instead of resuming it.
    pub fork: bool,
}

/// Arguments for the `codex` binary.
pub fn app_server_args() -> Vec<String> {
    vec!["app-server".into()]
}

#[derive(Debug, Clone)]
pub(crate) struct ToolNode {
    pub(crate) node_id: String,
    /// Native item type (`commandExecution`, `fileChange`, ...).
    pub(crate) item_type: String,
    pub(crate) done: bool,
    pub(crate) denied: bool,
    /// The turn completed while this command ran: it is a background terminal
    /// and stays open until its own `item/completed`.
    pub(crate) background: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct InputEntry {
    pub(crate) id: String,
    pub(crate) state: Option<InputState>,
    pub(crate) turn: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum HostRequest {
    Initialize,
    ThreadStart,
    ThreadResume,
    ThreadFork,
    BackgroundList,
    TurnStart { input_id: String },
    TurnSteer { input_id: String },
    Interrupt,
}

/// Sans-IO adapter state of one app-server process.
#[derive(Debug, Default)]
pub struct CodexCore {
    pub(crate) ids: OrdinalIds,
    pub(crate) next_host_id: u64,
    pub(crate) host_requests: HashMap<String, HostRequest>,
    pub(crate) spec: SessionSpec,
    pub(crate) mode: Option<Mode>,
    /// Mode that the next `turn/start` applies (trap 1: emitted on acceptance).
    pub(crate) pending_mode: Option<Mode>,
    pub(crate) provider_version: Option<String>,
    /// The `initialize` reply arrived and `initialized` was sent.
    pub(crate) initialized: bool,
    pub(crate) thread_id: Option<String>,
    /// Model that the thread reports (`thread/start` / `thread/resume` reply).
    pub(crate) thread_model: Option<String>,
    /// Collaboration mode for the next turns (`plan` or `default`); `None`
    /// sends no `collaborationMode` (experimental API, DL-013).
    pub(crate) collaboration: Option<String>,
    /// Running turn: canonical id and native id once known.
    pub(crate) turn: Option<String>,
    pub(crate) native_turn: Option<String>,
    /// True after `turn/started` for `native_turn`. The `turn/start` reply
    /// names the turn before it runs; `turn/interrupt` before `turn/started`
    /// fails with "no active turn to interrupt" (fixture
    /// `interrupt_right_after_send`, first recording).
    pub(crate) native_started: bool,
    /// A `turn/start` is on the wire and its reply is not in yet.
    pub(crate) turn_requested: bool,
    pub(crate) interrupt_pending: bool,
    pub(crate) interrupt_requested: bool,
    pub(crate) follow_ups: VecDeque<(String, String)>,
    pub(crate) inputs: Vec<InputEntry>,
    pub(crate) tools: BTreeMap<String, ToolNode>,
    /// Sub-agent thread id → node id of the tool call that spawned it.
    pub(crate) child_threads: HashMap<String, String>,
    pub(crate) files_seen: Vec<String>,
    pub(crate) turn_diff: Option<String>,
    pub(crate) requests: requests::Requests,
    pub(crate) unknown_frames: usize,
}

impl CodexCore {
    pub fn new() -> Self {
        Self::default()
    }

    /// True after the `initialize` handshake.
    pub fn is_initialized(&self) -> bool {
        self.initialized
    }

    pub fn thread_id(&self) -> Option<&str> {
        self.thread_id.as_deref()
    }

    pub fn current_turn(&self) -> Option<&str> {
        self.turn.as_deref()
    }

    pub fn unknown_frames(&self) -> usize {
        self.unknown_frames
    }

    pub fn mode(&self) -> Option<String> {
        self.mode.as_ref().map(Mode::name)
    }

    /// The parent tool node of a sub-agent thread, once a
    /// `collabAgentToolCall` named the thread.
    pub fn child_thread_parent(&self, thread_id: &str) -> Option<&str> {
        self.child_threads.get(thread_id).map(String::as_str)
    }

    /// Latest aggregated unified diff of the running or last turn.
    pub fn turn_diff(&self) -> Option<&str> {
        self.turn_diff.as_deref()
    }

    pub fn input_state(&self, input_id: &str) -> Option<InputState> {
        self.inputs
            .iter()
            .find(|i| i.id == input_id)
            .and_then(|i| i.state)
    }

    /// Inputs that the app-server has not confirmed yet.
    pub fn pending_inputs(&self) -> usize {
        self.inputs
            .iter()
            .filter(|i| i.state.is_none() || i.state == Some(InputState::Received))
            .count()
    }

    pub fn is_idle(&self) -> bool {
        self.turn.is_none() && !self.turn_requested && self.follow_ups.is_empty()
    }

    pub(crate) fn host_id(&mut self, kind: HostRequest) -> String {
        self.next_host_id += 1;
        let id = format!("fd-{}", self.next_host_id);
        self.host_requests.insert(id.clone(), kind);
        id
    }

    /// `initialize` with the experimental API (needed for `item/tool/requestUserInput`).
    pub fn initialize(&mut self) -> Vec<Output> {
        let id = self.host_id(HostRequest::Initialize);
        vec![Output::Write(rpc::request(
            &id,
            "initialize",
            json!({
                "clientInfo": {"name": "fleet-deck-spike", "version": env!("CARGO_PKG_VERSION")},
                "capabilities": {"experimentalApi": true},
            }),
        ))]
    }

    /// Opens the thread: `thread/resume` if the spec names one, else `thread/start`.
    pub fn open_thread(&mut self, spec: &SessionSpec) -> Vec<Output> {
        self.spec = spec.clone();
        let mode = Mode::or_safest(spec.mode.as_deref());
        let mut params = json!({
            "approvalPolicy": mode.approval_policy,
            "sandbox": mode.sandbox,
        });
        if let Some(obj) = params.as_object_mut() {
            if let Some(m) = &spec.model {
                obj.insert("model".into(), m.clone().into());
            }
            if let Some(c) = &spec.cwd {
                obj.insert("cwd".into(), c.clone().into());
            }
        }
        self.mode = Some(mode);
        let (method, kind) = match &spec.resume {
            Some(thread) => {
                if let Some(obj) = params.as_object_mut() {
                    obj.insert("threadId".into(), thread.clone().into());
                }
                if spec.fork {
                    ("thread/fork", HostRequest::ThreadFork)
                } else {
                    ("thread/resume", HostRequest::ThreadResume)
                }
            }
            None => {
                if let Some(obj) = params.as_object_mut() {
                    obj.insert("ephemeral".into(), false.into());
                }
                ("thread/start", HostRequest::ThreadStart)
            }
        };
        let id = self.host_id(kind);
        vec![Output::Write(rpc::request(&id, method, params))]
    }

    /// A prompt. Idle: `turn/start`. Busy: a follow-up that the core sends after
    /// the running turn completes (Codex would merge it into the running turn).
    pub fn prompt(&mut self, text: &str, input_id: &str) -> Result<Vec<Output>, AdapterError> {
        let thread = self.thread_id.clone().ok_or(AdapterError::NoThread)?;
        self.inputs.push(InputEntry {
            id: input_id.to_owned(),
            state: None,
            turn: None,
        });
        if self.turn.is_some() || self.turn_requested || !self.follow_ups.is_empty() {
            self.follow_ups
                .push_back((input_id.to_owned(), text.to_owned()));
            return Ok(Vec::new());
        }
        Ok(self.turn_start(&thread, text, input_id))
    }

    pub(crate) fn turn_start(&mut self, thread: &str, text: &str, input_id: &str) -> Vec<Output> {
        let mut params = json!({
            "threadId": thread,
            "clientUserMessageId": input_id,
            "input": [{"type": "text", "text": text}],
        });
        if let Some(obj) = params.as_object_mut() {
            if let Some(e) = &self.spec.effort {
                obj.insert("effort".into(), e.clone().into());
            }
            if let Some(c) = &self.collaboration
                && let Some(model) = self
                    .thread_model
                    .clone()
                    .or_else(|| self.spec.model.clone())
            {
                obj.insert(
                    "collaborationMode".into(),
                    json!({"mode": c, "settings": {
                        "model": model,
                        "reasoning_effort": self.spec.effort,
                        "developer_instructions": null,
                    }}),
                );
            }
            if let Some(m) = &self.pending_mode {
                obj.insert("approvalPolicy".into(), m.approval_policy.clone().into());
                obj.insert("sandboxPolicy".into(), m.sandbox_policy());
            }
        }
        self.turn_requested = true;
        let id = self.host_id(HostRequest::TurnStart {
            input_id: input_id.to_owned(),
        });
        vec![Output::Write(rpc::request(&id, "turn/start", params))]
    }

    /// Steers the running turn (`turn/steer` with `expectedTurnId`).
    pub fn steer(&mut self, text: &str, input_id: &str) -> Result<Vec<Output>, AdapterError> {
        let thread = self.thread_id.clone().ok_or(AdapterError::NoThread)?;
        let turn = self.native_turn.clone().ok_or(AdapterError::NoTurn)?;
        self.inputs.push(InputEntry {
            id: input_id.to_owned(),
            state: None,
            turn: None,
        });
        let id = self.host_id(HostRequest::TurnSteer {
            input_id: input_id.to_owned(),
        });
        Ok(vec![Output::Write(rpc::request(
            &id,
            "turn/steer",
            json!({
                "threadId": thread,
                "expectedTurnId": turn,
                "clientUserMessageId": input_id,
                "input": [{"type": "text", "text": text}],
            }),
        ))])
    }

    /// `turn/interrupt`. Before the native turn id is known (trap 3) the core
    /// holds the interrupt and sends it when the `turn/start` reply arrives.
    pub fn interrupt(&mut self) -> Result<Vec<Output>, AdapterError> {
        let thread = self.thread_id.clone().ok_or(AdapterError::NoThread)?;
        if self.interrupt_requested || self.interrupt_pending {
            return Ok(Vec::new());
        }
        match self.native_turn.clone() {
            Some(turn) if self.native_started => Ok(self.send_interrupt(&thread, &turn)),
            Some(_) => {
                self.interrupt_pending = true;
                Ok(Vec::new())
            }
            None if self.turn_requested => {
                self.interrupt_pending = true;
                Ok(Vec::new())
            }
            None => Err(AdapterError::NoTurn),
        }
    }

    pub(crate) fn send_interrupt(&mut self, thread: &str, turn: &str) -> Vec<Output> {
        self.interrupt_requested = true;
        self.interrupt_pending = false;
        let id = self.host_id(HostRequest::Interrupt);
        vec![Output::Write(rpc::request(
            &id,
            "turn/interrupt",
            json!({"threadId": thread, "turnId": turn}),
        ))]
    }

    /// Node ids of commands that outlived their turn and have not completed.
    pub fn background_tools(&self) -> Vec<String> {
        let mut ids: Vec<String> = self
            .tools
            .values()
            .filter(|n| n.background && !n.done)
            .map(|n| n.node_id.clone())
            .collect();
        ids.sort();
        ids
    }

    /// Asks Codex for the thread's background terminals
    /// (`thread/backgroundTerminals/list`). Codex reports no background work
    /// on its own after a turn ends (trap 4), so the adapter polls; the reply
    /// becomes `background.updated`.
    pub fn list_background(&mut self) -> Result<Vec<Output>, AdapterError> {
        let thread = self.thread_id.clone().ok_or(AdapterError::NoThread)?;
        let id = self.host_id(HostRequest::BackgroundList);
        Ok(vec![Output::Write(rpc::request(
            &id,
            "thread/backgroundTerminals/list",
            json!({ "threadId": thread }),
        ))])
    }

    /// Sets the collaboration mode of the next turns (`plan` or `default`).
    /// Codex plan mode is where the model asks questions with
    /// `item/tool/requestUserInput` and writes `plan` items.
    pub fn set_collaboration(&mut self, mode: &str) -> Result<Vec<Output>, AdapterError> {
        if !COLLABORATION_MODES.contains(&mode) {
            return Err(AdapterError::UnsupportedMode(mode.to_owned()));
        }
        self.collaboration = Some(mode.to_owned());
        Ok(Vec::new())
    }

    /// Changes the mode for the next turn. Codex takes the approval policy and
    /// sandbox per `turn/start`; the core emits `mode.changed` only when the
    /// app-server accepted a turn with the new mode (trap 1).
    pub fn set_mode(&mut self, mode: &str) -> Result<Vec<Output>, AdapterError> {
        let m = Mode::parse(mode).ok_or_else(|| AdapterError::UnsupportedMode(mode.to_owned()))?;
        self.pending_mode = Some(m);
        Ok(Vec::new())
    }

    /// Handles one app-server frame.
    pub fn on_frame(&mut self, frame: Incoming) -> Vec<Output> {
        match frame {
            Incoming::Response { id, result } => self.on_response(&id, Ok(result)),
            Incoming::ErrorResponse { id, code, message } => {
                self.on_response(&id, Err((code, message)))
            }
            Incoming::Request { id, method, params } => {
                self.on_server_request(id, &method, &params)
            }
            Incoming::Notification { method, params } => self.on_notification(&method, &params),
            Incoming::Unknown(_) => {
                self.unknown_frames += 1;
                Vec::new()
            }
        }
    }

    /// The app-server process ended.
    pub fn on_exit(&mut self, exit_code: Option<i32>) -> Vec<Output> {
        let mut out = self.expire_open("process_lost");
        out.extend(self.close_all_tools());
        if let Some(turn_id) = self.turn.take() {
            out.push(Output::Emit(CanonEvent::TurnCompleted {
                turn_id,
                status: spike_canon::TurnStatus::Failed,
                usage: None,
            }));
        }
        self.native_turn = None;
        self.native_started = false;
        self.turn_requested = false;
        for (input_id, _) in std::mem::take(&mut self.follow_ups) {
            out.extend(self.set_input_state(&input_id, InputState::Discarded, None));
        }
        out.push(Output::Emit(CanonEvent::SessionEnded {
            reason: "process_exited".into(),
            exit_code,
        }));
        out
    }
}
