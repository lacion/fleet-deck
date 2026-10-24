//! The sans-IO Claude core (ADR-001 §2.2), hardened from `spikes/s1-claude`.
//!
//! The core holds no process and does no I/O. Host commands and CLI frames go
//! in; canonical `fleetdeck-proto` events and frames to write go out
//! ([`Output`]). The live driver and the replay harness both wrap this core,
//! so a replay runs the real mapping code (PLAN §13, L20).

use std::collections::{BTreeMap, VecDeque};

use fleetdeck_proto::event::{
    ApprovalOption, BackgroundTask, ExpiredReason, InputState, ModelChoice, ProviderKind,
    RunOrigin, TurnStatus,
};
use fleetdeck_proto::{
    Capabilities, Event, InputId, NodeId, PlanId, ProviderInstanceId, RequestId, RunId, TurnId,
    input_text_of,
};
use serde_json::Value;

use super::args::input_uuid;
use super::commands::CommandState;
use super::frames::CliFrame;
use super::process_usage::{ProcessUsage, UsageBase};
use super::workflows::WorkflowEntry;
use super::{control, map};
use crate::adapter::{HostInput, ProviderError, Result};

/// One output of the core.
#[derive(Debug, Clone, PartialEq)]
pub enum Output {
    /// A canonical event for the event log.
    Emit(Event),
    /// A frame to write to the CLI's stdin.
    Write(Value),
    /// The native session id that a root `system/init` states. It is not an
    /// event: the durable driver passes it, in order with the events, to
    /// the output transaction (DL-194).
    NativeIdentity { native_id: String },
}

/// What the core needs to know about its session.
#[derive(Debug, Clone, PartialEq)]
pub struct CoreConfig {
    pub provider_instance: ProviderInstanceId,
    /// The canonical workspace path (trap 8).
    pub cwd: String,
    pub capabilities: Capabilities,
    /// `Some(true)` for a resumed session, `Some(false)` for the "not resumed"
    /// degradation, `None` for a new session.
    pub resumed: Option<bool>,
    /// What the CLI counts its usage from (B21, DL-443).
    pub usage_base: UsageBase,
}

/// Ordinal ids such as `turn-3`. Turns, nodes and requests share one counter,
/// the same as the spike, so recorded host commands name the same ids.
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
pub(super) struct Ordinals {
    shared: u64,
    runs: u64,
    plans: u64,
}

impl Ordinals {
    pub(super) fn next(&mut self, prefix: &str) -> String {
        self.shared += 1;
        format!("{prefix}-{}", self.shared)
    }

    fn run(&mut self) -> RunId {
        self.runs += 1;
        RunId(format!("run-{}", self.runs))
    }

    pub(super) fn plan(&mut self) -> PlanId {
        self.plans += 1;
        PlanId(format!("plan-{}", self.plans))
    }
}

/// A host input and its fate as the CLI reported it.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(super) struct InputEntry {
    pub(super) id: InputId,
    /// The client uuid sent with the user message.
    pub(super) uuid: String,
    pub(super) origin: RunOrigin,
    pub(super) summary: String,
    /// The bounded full text, until `run.started` carries it (B10).
    #[serde(default)]
    pub(super) input_text: Option<String>,
    /// Who typed the input (R11.5, DL-166); a checkpoint of an older build has none.
    #[serde(default)]
    pub(super) author: Option<String>,
    /// `None` until the CLI reports the input (written, not confirmed).
    pub(super) state: Option<InputState>,
    pub(super) turn: Option<TurnId>,
}

impl InputEntry {
    pub(super) fn is_pending(&self) -> bool {
        matches!(self.state, None | Some(InputState::Received))
    }
}

/// The state of a runtime request.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub(super) enum ReqState {
    Open,
    Answered(String),
    Expired(ExpiredReason),
}

/// An open or closed runtime request.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(super) struct RuntimeRequest {
    pub(super) native_request_id: String,
    pub(super) tool_use_id: Option<String>,
    pub(super) input: Value,
    /// Session-scoped allow rules for `allow_always` (DL-009).
    pub(super) suggestions: Option<Value>,
    pub(super) options: Vec<ApprovalOption>,
    pub(super) state: ReqState,
}

/// What a host control request was for, so its response can be mapped.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub(super) enum HostRequest {
    Initialize,
    /// With the host's control id, echoed in `interrupt.result`.
    Interrupt(Option<String>),
    SetMode(String),
    /// `None` is the default-model sentinel (trap 2).
    SetModel(Option<String>),
    /// `get_usage`: the plan limit windows (R4.5).
    Usage,
}

/// A tool node of the session.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(super) struct ToolNode {
    pub(super) node_id: NodeId,
    pub(super) name: String,
    pub(super) input: Value,
    pub(super) denied: bool,
    /// The tool has a terminal `tool.completed` event.
    pub(super) done: bool,
    /// The `tool_use_id` of the subagent call that this tool runs under (B6).
    #[serde(default)]
    pub(super) parent_tool: Option<String>,
    /// For an `Agent` or `Task` call: the turn that started it (B6).
    #[serde(default)]
    pub(super) launch_turn: Option<TurnId>,
}

/// What the core knows of one task of `system/task_*` frames (B6).
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub(super) struct TaskEntry {
    /// The `tool_use_id` of the card that started the task.
    pub(super) tool_use_id: Option<String>,
    pub(super) task_kind: String,
    /// From `task_started` until a final status.
    pub(super) running: bool,
    /// The `(status, summary)` of the last `task.updated` event of the task.
    pub(super) last: Option<(String, Option<String>)>,
}

/// The sans-IO Claude adapter core.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct ClaudeCore {
    /// Not part of the adapter checkpoint (ADR-005 §2.1; DL-108).
    #[serde(skip, default = "super::checkpoint::placeholder_config")]
    pub(super) config: CoreConfig,
    pub(super) ids: Ordinals,
    host_ids: u64,
    pub(super) native_session_id: Option<String>,
    pub(super) started: bool,
    pub(super) ended: bool,
    pub(super) mode: Option<String>,
    pub(super) model: Option<String>,
    /// Model values from the `initialize` response; `None` until advertised (trap 1).
    pub(super) models: Option<Vec<String>>,
    /// The models of an `initialize` answer that came before the first `init`;
    /// `session.models` follows `session.started` (DL-151).
    #[serde(default)]
    pub(super) pending_models: Vec<ModelChoice>,
    /// Every input written in this process, in order.
    pub(super) inputs: Vec<InputEntry>,
    /// True after the first `command_lifecycle` frame: the CLI reports input
    /// fates, so the replay echo is not used as a fallback confirmation.
    pub(super) lifecycle_seen: bool,
    pub(super) run: Option<RunId>,
    pub(super) turn: Option<TurnId>,
    pub(super) interrupt_requested: bool,
    pub(super) host_requests: BTreeMap<String, HostRequest>,
    pub(super) requests: BTreeMap<RequestId, RuntimeRequest>,
    pub(super) native_to_app: BTreeMap<String, RequestId>,
    /// The ids of the answered and expired requests, oldest first. The
    /// oldest ones leave `requests` when there are too many (NV-18).
    #[serde(default)]
    pub(super) closed_order: VecDeque<RequestId>,
    pub(super) tools: BTreeMap<String, ToolNode>,
    pub(super) unknown_frames: usize,
    /// Events of frames before the first `system/init`; they follow
    /// `session.started`.
    pub(super) early: Vec<Event>,
    /// Background work that the CLI reports as running (traps 4 and 5).
    pub(super) background: Vec<BackgroundTask>,
    /// The tasks of `system/task_*` frames, by task id (B6).
    #[serde(default)]
    pub(super) tasks: BTreeMap<String, TaskEntry>,
    /// The workflow runs of `local_workflow` tasks, by task id (B12).
    #[serde(default)]
    pub(super) workflows: BTreeMap<String, WorkflowEntry>,
    /// Running workflows that the core does not follow (§4.1 limits), oldest first.
    #[serde(default)]
    pub(super) untracked_workflows: VecDeque<String>,
    /// The start counter of `WorkflowEntry::started`.
    #[serde(default)]
    pub(super) next_workflow: u64,
    /// When the last `get_usage` request was written (R4.5). Not part of the
    /// checkpoint: a restored core may ask once more.
    #[serde(skip)]
    pub(super) usage_asked: Option<std::time::Instant>,
    /// The CLI refused `get_usage` or answered in another shape: no more asks.
    #[serde(default)]
    pub(super) usage_stopped: bool,
    /// Counter of the `fd-usage-N` request ids. `get_usage` keeps its own
    /// counter so the ids of the other host requests do not move.
    #[serde(default)]
    pub(super) usage_seq: u64,
    /// The `total_cost_usd` of the last `result` of a CLI process, written by
    /// a build before the per-model totals. It is read from an older
    /// checkpoint and no longer written.
    #[serde(default, skip_serializing)]
    pub(super) last_total_cost: Option<f64>,
    /// The command list of this process (B11, DL-410).
    #[serde(default)]
    pub(super) commands: CommandState,
    /// The per-model running totals of the CLI process (B14, DL-442).
    /// `None` only in a checkpoint of an older build.
    #[serde(default)]
    pub(super) usage: Option<ProcessUsage>,
}

impl ClaudeCore {
    #[must_use]
    pub fn new(config: CoreConfig) -> Self {
        let usage = Some(ProcessUsage::new(&config.usage_base));
        Self {
            config,
            ids: Ordinals::default(),
            host_ids: 0,
            native_session_id: None,
            started: false,
            ended: false,
            mode: None,
            model: None,
            models: None,
            pending_models: Vec::new(),
            inputs: Vec::new(),
            lifecycle_seen: false,
            run: None,
            turn: None,
            interrupt_requested: false,
            host_requests: BTreeMap::new(),
            requests: BTreeMap::new(),
            native_to_app: BTreeMap::new(),
            closed_order: VecDeque::new(),
            tools: BTreeMap::new(),
            unknown_frames: 0,
            early: Vec::new(),
            background: Vec::new(),
            tasks: BTreeMap::new(),
            workflows: BTreeMap::new(),
            untracked_workflows: VecDeque::new(),
            next_workflow: 0,
            usage_asked: None,
            usage_stopped: false,
            usage_seq: 0,
            last_total_cost: None,
            commands: CommandState::default(),
            usage,
        }
    }

    /// The usage state; always set after `new` or `restore`.
    pub(super) fn usage_mut(&mut self) -> &mut ProcessUsage {
        let last = self.last_total_cost;
        self.usage.get_or_insert_with(|| ProcessUsage::legacy(last))
    }

    #[must_use]
    pub fn capabilities(&self) -> &Capabilities {
        &self.config.capabilities
    }

    /// Native session id, after `system/init`.
    #[must_use]
    pub fn native_session_id(&self) -> Option<&str> {
        self.native_session_id.as_deref()
    }

    /// The current native mode, as the CLI reported it.
    #[must_use]
    pub fn mode(&self) -> Option<&str> {
        self.mode.as_deref()
    }

    /// The model values that the CLI advertised (`None` before `initialize`).
    #[must_use]
    pub fn models(&self) -> Option<&[String]> {
        self.models.as_deref()
    }

    /// Number of frames that the core did not know and ignored.
    #[must_use]
    pub fn unknown_frames(&self) -> usize {
        self.unknown_frames
    }

    /// Number of written inputs that the CLI did not start yet.
    #[must_use]
    pub fn pending_inputs(&self) -> usize {
        self.inputs.iter().filter(|i| i.is_pending()).count()
    }

    /// The reported fate of an input (`None` while it is only written).
    #[must_use]
    pub fn input_state(&self, id: &InputId) -> Option<InputState> {
        self.inputs
            .iter()
            .find(|i| i.id == *id)
            .and_then(|i| i.state)
    }

    /// True when no turn runs, no input waits for confirmation, and no
    /// background work is pending (PLAN §6.3, L7): a session whose background
    /// task still runs shows "background work", not "idle".
    #[must_use]
    pub fn is_idle(&self) -> bool {
        self.is_quiet() && self.background.is_empty()
    }

    /// True when no turn runs and no input waits: there is nothing that an
    /// interrupt can stop. Background work may still run.
    #[must_use]
    pub fn is_quiet(&self) -> bool {
        self.turn.is_none() && self.pending_inputs() == 0
    }

    #[must_use]
    pub fn has_ended(&self) -> bool {
        self.ended
    }

    #[must_use]
    pub fn current_turn(&self) -> Option<&TurnId> {
        self.turn.as_ref()
    }

    #[must_use]
    pub fn current_run(&self) -> Option<&RunId> {
        self.run.as_ref()
    }

    /// Background work that the CLI reports as running. A session with
    /// background work is not idle (L7), even when no turn runs.
    #[must_use]
    pub fn background(&self) -> &[BackgroundTask] {
        &self.background
    }

    /// Ids of the open requests (not answered, not expired).
    #[must_use]
    pub fn open_requests(&self) -> Vec<RequestId> {
        self.requests
            .iter()
            .filter(|(_, r)| r.state == ReqState::Open)
            .map(|(id, _)| id.clone())
            .collect()
    }

    /// The native control request id of an app request id.
    #[must_use]
    pub fn native_request_id(&self, request: &RequestId) -> Option<&str> {
        self.requests
            .get(request)
            .map(|r| r.native_request_id.as_str())
    }

    /// The request that a CLI `request_id` opened.
    #[must_use]
    pub fn request_for_native(&self, native: &str) -> Option<&RequestId> {
        self.requests
            .iter()
            .find(|(_, r)| r.native_request_id == native)
            .map(|(id, _)| id)
    }

    /// The tool input of a request, as the CLI sent it.
    #[must_use]
    pub fn request_input(&self, request: &RequestId) -> Option<&Value> {
        self.requests.get(request).map(|r| &r.input)
    }

    pub(super) fn next_host_id(&mut self, kind: HostRequest) -> String {
        self.host_ids += 1;
        let id = format!("fd-{}", self.host_ids);
        self.host_requests.insert(id.clone(), kind);
        id
    }

    fn live(&self) -> Result<()> {
        if self.ended {
            Err(ProviderError::SessionEnded)
        } else {
            Ok(())
        }
    }

    /// Sends `initialize`. Its response advertises the models (trap 1).
    pub fn initialize(&mut self) -> Result<Vec<Output>> {
        self.live()?;
        let id = self.next_host_id(HostRequest::Initialize);
        Ok(vec![Output::Write(control::initialize(&id))])
    }

    /// Writes a user message. During a running turn it joins the turn (a
    /// steer); while a turn waits it queues (ADR-001). A turn starts only when
    /// the CLI reports the input as started.
    ///
    /// # Errors
    /// An input id that is not a ULID or uuid, or one that was used before.
    pub fn prompt(&mut self, input: &HostInput) -> Result<Vec<Output>> {
        self.live()?;
        let uuid = input_uuid(&input.id)?;
        if self
            .inputs
            .iter()
            .any(|i| i.uuid == uuid || i.id == input.id)
        {
            return Err(ProviderError::InvalidInput(input.id.to_string()));
        }
        self.inputs.push(InputEntry {
            id: input.id.clone(),
            uuid: uuid.clone(),
            origin: input.origin,
            summary: summary_of(&input.input.text),
            input_text: input_text_of(&input.input.text),
            author: input.input.author.clone(),
            state: None,
            turn: None,
        });
        let session = self.native_session_id.clone().unwrap_or_default();
        Ok(vec![Output::Write(control::user_message(
            &input.input,
            &input.context,
            &session,
            &uuid,
        ))])
    }

    /// Requests an interrupt. The terminal state arrives later with `result`
    /// (trap 10). An interrupt before the turn started cancels the input (trap 3).
    ///
    /// # Errors
    /// [`ProviderError::NoTurn`] when no turn runs and no input waits.
    pub fn interrupt(&mut self, control_id: Option<String>) -> Result<Vec<Output>> {
        self.live()?;
        if self.is_quiet() {
            return Err(ProviderError::NoTurn);
        }
        self.interrupt_requested = true;
        let id = self.next_host_id(HostRequest::Interrupt(control_id));
        Ok(vec![Output::Write(control::interrupt(&id))])
    }

    /// Changes the permission mode. Only the session's native modes (L8).
    ///
    /// # Errors
    /// [`ProviderError::InvalidMode`] for a mode that the session does not offer.
    pub fn set_mode(&mut self, mode: &str) -> Result<Vec<Output>> {
        self.live()?;
        if !self.config.capabilities.modes.iter().any(|m| m == mode) {
            return Err(ProviderError::InvalidMode(mode.to_owned()));
        }
        let id = self.next_host_id(HostRequest::SetMode(mode.to_owned()));
        Ok(vec![Output::Write(control::set_permission_mode(&id, mode))])
    }

    /// Changes the model. `default` returns to the default model (trap 2).
    ///
    /// # Errors
    /// [`ProviderError::NotReady`] before the CLI advertised its models (trap 1);
    /// [`ProviderError::InvalidModel`] for a model that it did not advertise.
    pub fn set_model(&mut self, model: &str) -> Result<Vec<Output>> {
        self.live()?;
        let models = self
            .models
            .as_ref()
            .ok_or(ProviderError::NotReady("model"))?;
        let native = if model == "default" {
            None
        } else if models.iter().any(|m| m == model) {
            Some(model.to_owned())
        } else {
            return Err(ProviderError::InvalidModel(model.to_owned()));
        };
        let id = self.next_host_id(HostRequest::SetModel(native.clone()));
        Ok(vec![Output::Write(control::set_model(
            &id,
            native.as_deref(),
        ))])
    }

    /// Feeds one CLI frame.
    ///
    /// Frames before the first `system/init` are held, except the answers to
    /// the host's own control requests (mode, model, interrupt): they are released at
    /// once (DL-141). The command list is also released at once (DL-410). The
    /// other events of the frames before `system/init` follow
    /// `session.started`. Writes are never held.
    pub fn on_frame(&mut self, frame: CliFrame) -> Vec<Output> {
        if self.ended {
            return Vec::new();
        }
        let answer = matches!(frame, CliFrame::ControlResponse(_));
        let out = map::on_frame(self, frame);
        if !self.started && answer {
            return out;
        }
        if !self.started {
            // Writes and identities are never held; nor is the command list
            // (B11, DL-410): a session without a prompt gets no `init`.
            let mut released = Vec::new();
            for o in out {
                match o {
                    e @ Output::Emit(Event::SlashCommands { .. }) => released.push(e),
                    Output::Emit(e) => self.early.push(e),
                    w @ (Output::Write(_) | Output::NativeIdentity { .. }) => released.push(w),
                }
            }
            return released;
        }
        if self.early.is_empty() {
            return out;
        }
        let mut res = Vec::with_capacity(out.len() + self.early.len());
        let mut early = std::mem::take(&mut self.early);
        for o in out {
            let opens = matches!(o, Output::Emit(Event::SessionStarted { .. }));
            res.push(o);
            if opens {
                res.extend(early.drain(..).map(Output::Emit));
            }
        }
        res.extend(early.into_iter().map(Output::Emit));
        res
    }

    /// The CLI process exited. Closes everything that is open (trap 6).
    pub fn on_exit(&mut self, exit_code: Option<i32>) -> Vec<Output> {
        if self.ended {
            return Vec::new();
        }
        self.ended = true;
        let mut out: Vec<Output> = std::mem::take(&mut self.early)
            .into_iter()
            .map(Output::Emit)
            .collect();
        out.extend(self.expire_open(ExpiredReason::ProcessLost));
        out.extend(map::close_open_tools(self));
        // No lifecycle frame follows an exit: inputs that were waiting or
        // running end as discarded, so none stays open on the board.
        for input in &mut self.inputs {
            if input.is_pending() || input.state == Some(InputState::Started) {
                input.state = Some(InputState::Discarded);
                out.push(Output::Emit(Event::InputUpdated {
                    input_id: input.id.clone(),
                    turn_id: input.turn.clone(),
                    state: InputState::Discarded,
                }));
            }
        }
        out.extend(self.end_turn(TurnStatus::Failed, None));
        self.background.clear();
        out.push(Output::Emit(Event::SessionEnded {
            reason: "process_exit".to_owned(),
            exit_code,
        }));
        out
    }

    /// Starts a turn, and a run when none is open. `input` is the index of
    /// the host input that starts it; `None` is a provider turn (trap 4).
    pub(super) fn start_turn(&mut self, input: Option<usize>) -> (TurnId, Vec<Output>) {
        let mut out = Vec::new();
        let (origin, summary, uuid, author, input_id) = match input.and_then(|i| self.inputs.get(i))
        {
            Some(e) => (
                e.origin,
                e.summary.clone(),
                Some(e.uuid.clone()),
                e.author.clone(),
                Some(e.id.clone()),
            ),
            None => (
                RunOrigin::Provider,
                "provider turn".to_owned(),
                None,
                None,
                None,
            ),
        };
        let run_id = match &self.run {
            Some(r) => r.clone(),
            None => {
                let r = self.ids.run();
                self.run = Some(r.clone());
                // The full text leaves the entry once `run.started` has it (B10).
                let input_text = input
                    .and_then(|i| self.inputs.get_mut(i))
                    .and_then(|e| e.input_text.take());
                out.push(Output::Emit(Event::RunStarted {
                    run_id: r.clone(),
                    input_summary: summary,
                    origin,
                    author,
                    input_id,
                    input_text,
                }));
                r
            }
        };
        let turn_id = TurnId(self.ids.next("turn"));
        self.turn = Some(turn_id.clone());
        let mut refs = serde_json::Map::new();
        if let Some(s) = &self.native_session_id {
            refs.insert("session_id".to_owned(), Value::from(s.as_str()));
        }
        match uuid {
            Some(u) => refs.insert("command_uuid".to_owned(), Value::from(u)),
            None => refs.insert("origin".to_owned(), Value::from("provider")),
        };
        out.push(Output::Emit(Event::TurnStarted {
            run_id,
            turn_id: turn_id.clone(),
            provider_refs: refs,
        }));
        (turn_id, out)
    }

    /// Ends the running turn and its run. A run ends with its turn; the
    /// report says if background work is still running (L7).
    pub(super) fn end_turn(
        &mut self,
        status: TurnStatus,
        usage: Option<fleetdeck_proto::event::Usage>,
    ) -> Vec<Output> {
        let mut out = Vec::new();
        if let Some(turn_id) = self.turn.take() {
            out.push(Output::Emit(Event::TurnCompleted {
                turn_id,
                status,
                usage,
            }));
        }
        if let Some(run_id) = self.run.take() {
            out.push(Output::Emit(Event::RunCompleted {
                run_id,
                status,
                background_pending: !self.background.is_empty(),
            }));
        }
        out
    }

    pub(super) fn expire_open(&mut self, reason: ExpiredReason) -> Vec<Output> {
        let mut out = Vec::new();
        let mut closed = Vec::new();
        for (id, req) in &mut self.requests {
            if req.state == ReqState::Open {
                req.state = ReqState::Expired(reason);
                closed.push(id.clone());
                out.push(Output::Emit(Event::ApprovalExpired {
                    request_id: id.clone(),
                    reason,
                }));
            }
        }
        for id in closed {
            self.note_closed(id);
        }
        out
    }

    /// The kind of provider. Always Claude.
    #[must_use]
    pub fn kind(&self) -> ProviderKind {
        ProviderKind::Claude
    }
}

/// The first line of an input, at most 120 characters.
pub(super) fn summary_of(text: &str) -> String {
    let line = text.lines().next().unwrap_or_default().trim();
    truncate(line, 120)
}

/// `text` cut at `max` characters, with an ellipsis when cut.
pub(super) fn truncate(text: &str, max: usize) -> String {
    match text.char_indices().nth(max) {
        Some((at, _)) => format!("{}…", &text[..at]),
        None => text.to_owned(),
    }
}

#[cfg(test)]
#[path = "core_shell_tests.rs"]
mod shell_tests;
