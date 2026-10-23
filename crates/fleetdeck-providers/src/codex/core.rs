//! The sans-IO Codex core: state and host commands (ADR-002 §2.4–§3).

use std::collections::{BTreeMap, HashMap, VecDeque};

use fleetdeck_proto::command::UserInput;
use fleetdeck_proto::event::{
    BackgroundTask, ExpiredReason, InputState, ProviderKind, RunOrigin, TurnStatus,
};
use fleetdeck_proto::{Capabilities, Event, InputId, NodeId, RunId, TurnId, input_text_of};
use serde_json::{Value, json};

pub(super) use super::core_types::{HostRequest, InputEntry, Ordinals, ToolNode};
use super::rpc::{self, Incoming};
use super::shell::PendingShell;
use super::{
    COLLABORATION_MODES, CoreConfig, MAX_MODEL_CHARS, Mode, SessionSpec, requests, summary_of,
};
use crate::adapter::{HostInput, ProviderError, Result};

/// One output of the core.
#[derive(Debug, Clone, PartialEq)]
pub enum Output {
    /// A canonical event for the event log.
    Emit(Event),
    /// A JSON-RPC frame for the app-server's stdin.
    Write(Value),
    /// The native id of a root thread statement. It is not an event: the
    /// durable driver passes it, in order with the events, to the output
    /// transaction (DL-194).
    NativeIdentity { native_id: String },
    /// A completed MCP tool call that may name an app (B15). The station
    /// decides; the direct driver hands it to its candidate channel.
    AppCandidate(crate::apps::AppCandidate),
}

/// The sans-IO Codex adapter core of one app-server process.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct CodexCore {
    /// Not part of the adapter checkpoint: the restoring station passes it
    /// again (ADR-005 §2.1; DL-108).
    #[serde(skip, default = "super::checkpoint::placeholder_config")]
    pub(super) config: CoreConfig,
    pub(super) ids: Ordinals,
    next_host_id: u64,
    pub(super) host_requests: HashMap<String, HostRequest>,
    pub(super) spec: SessionSpec,
    pub(super) mode: Option<Mode>,
    /// Mode that the next `turn/start` applies (trap 1: emitted on acceptance).
    pub(super) pending_mode: Option<Mode>,
    /// Model that the next `turn/start` applies (emitted on acceptance).
    pub(super) pending_model: Option<String>,
    pub(super) provider_version: Option<String>,
    /// The `initialize` reply arrived and `initialized` was sent.
    pub(super) initialized: bool,
    pub(super) ended: bool,
    pub(super) thread_id: Option<String>,
    /// Model that the thread reports, or the last accepted model change.
    pub(super) thread_model: Option<String>,
    /// Collaboration mode for the next turns (`plan` or `default`); `None`
    /// sends no `collaborationMode` (experimental API, DL-013).
    pub(super) collaboration: Option<String>,
    pub(super) run: Option<RunId>,
    pub(super) turn: Option<TurnId>,
    pub(super) native_turn: Option<String>,
    /// True after `turn/started` for `native_turn`: `turn/interrupt` before
    /// it fails with "no active turn to interrupt" (trap 3).
    pub(super) native_started: bool,
    /// A `turn/start` is on the wire and its reply is not in yet.
    pub(super) turn_requested: bool,
    /// The input whose `turn/start` opened, or will open, the next turn.
    pub(super) turn_input: Option<InputId>,
    /// A held interrupt and its control id (trap 3).
    #[serde(with = "super::checkpoint::held")]
    pub(super) interrupt_pending: Option<Option<String>>,
    pub(super) interrupt_requested: bool,
    /// The last `thread/rollback` and its answer (DL-087).
    pub(super) rollback: Option<super::rollback::RollbackState>,
    pub(super) follow_ups: VecDeque<(InputId, UserInput)>,
    pub(super) inputs: Vec<InputEntry>,
    pub(super) tools: BTreeMap<String, ToolNode>,
    pub(super) tool_order: u64,
    /// Sub-agent thread id → node id of the tool call that spawned it.
    pub(super) child_threads: HashMap<String, NodeId>,
    /// Sub-agent thread id → `(status, summary)` of its last `task.updated`
    /// event (B6).
    #[serde(default)]
    pub(super) agent_last: HashMap<String, (String, Option<String>)>,
    pub(super) files_seen: Vec<String>,
    pub(super) turn_diff: Option<String>,
    /// The last polled background terminals.
    pub(super) background: Vec<BackgroundTask>,
    pub(super) requests: requests::Requests,
    pub(super) unknown_frames: usize,
    /// The `!` shell command that waits for or runs its turn (B8); an older
    /// checkpoint has none.
    #[serde(default)]
    pub(super) pending_shell: Option<PendingShell>,
    /// Turns that ended in this process life; not in the checkpoint, so it
    /// is 0 after a restore. The shim-host poll lists the background work
    /// when it changes (DL-024, DL-199).
    #[serde(skip)]
    turns_ended: u64,
    /// The fleet skill requests (`skills/extraRoots/set`, `skills/list`)
    /// that wait for their answer: id → request name. Only the handshake
    /// uses them, so they are not in the checkpoint.
    #[serde(skip)]
    skill_requests: HashMap<String, &'static str>,
    /// Offer the MCP Apps extension in `initialize` (B15).
    #[serde(default)]
    offer_mcp_apps: bool,
    /// The app requests in flight and their answers (B15); not in the checkpoint.
    #[serde(skip)]
    app_requests: apps_impl::AppRequests,
}

impl CodexCore {
    #[must_use]
    pub fn new(config: CoreConfig) -> Self {
        Self {
            config,
            ids: Ordinals::default(),
            next_host_id: 0,
            host_requests: HashMap::new(),
            spec: SessionSpec::default(),
            mode: None,
            pending_mode: None,
            pending_model: None,
            provider_version: None,
            initialized: false,
            ended: false,
            thread_id: None,
            thread_model: None,
            collaboration: None,
            run: None,
            turn: None,
            native_turn: None,
            native_started: false,
            turn_requested: false,
            turn_input: None,
            interrupt_pending: None,
            interrupt_requested: false,
            rollback: None,
            follow_ups: VecDeque::new(),
            inputs: Vec::new(),
            tools: BTreeMap::new(),
            tool_order: 0,
            child_threads: HashMap::new(),
            agent_last: HashMap::new(),
            files_seen: Vec::new(),
            turn_diff: None,
            background: Vec::new(),
            requests: requests::Requests::default(),
            unknown_frames: 0,
            pending_shell: None,
            turns_ended: 0,
            skill_requests: HashMap::new(),
            offer_mcp_apps: false,
            app_requests: apps_impl::AppRequests::default(),
        }
    }

    #[must_use]
    pub fn kind(&self) -> ProviderKind {
        ProviderKind::Codex
    }

    #[must_use]
    pub fn capabilities(&self) -> &Capabilities {
        &self.config.capabilities
    }

    /// True after the `initialize` handshake.
    #[must_use]
    pub fn is_initialized(&self) -> bool {
        self.initialized
    }

    /// True while a `thread/start`, `thread/resume` or `thread/fork` waits
    /// for its reply.
    #[must_use]
    pub fn is_opening(&self) -> bool {
        self.host_requests.values().any(|k| {
            matches!(
                k,
                HostRequest::ThreadStart | HostRequest::ThreadResume | HostRequest::ThreadFork
            )
        })
    }

    #[must_use]
    pub fn thread_id(&self) -> Option<&str> {
        self.thread_id.as_deref()
    }

    #[must_use]
    pub fn provider_version(&self) -> Option<&str> {
        self.provider_version.as_deref()
    }

    #[must_use]
    pub fn current_turn(&self) -> Option<&TurnId> {
        self.turn.as_ref()
    }

    #[must_use]
    pub fn current_run(&self) -> Option<&RunId> {
        self.run.as_ref()
    }

    #[must_use]
    pub fn unknown_frames(&self) -> usize {
        self.unknown_frames
    }

    #[must_use]
    pub fn mode(&self) -> Option<String> {
        self.mode.as_ref().map(Mode::name)
    }

    #[must_use]
    pub fn has_ended(&self) -> bool {
        self.ended
    }

    /// Turns that ended since this core was created or restored.
    #[must_use]
    pub fn turns_ended(&self) -> u64 {
        self.turns_ended
    }

    /// The parent tool node of a sub-agent thread, once a
    /// `collabAgentToolCall` named the thread.
    #[must_use]
    pub fn child_thread_parent(&self, thread_id: &str) -> Option<&NodeId> {
        self.child_threads.get(thread_id)
    }

    /// Latest aggregated unified diff of the running or last turn.
    #[must_use]
    pub fn turn_diff(&self) -> Option<&str> {
        self.turn_diff.as_deref()
    }

    #[must_use]
    pub fn input_state(&self, id: &InputId) -> Option<InputState> {
        self.inputs
            .iter()
            .find(|i| &i.id == id)
            .and_then(|i| i.state)
    }

    /// Inputs that the app-server has not confirmed yet.
    #[must_use]
    pub fn pending_inputs(&self) -> usize {
        self.inputs
            .iter()
            .filter(|i| matches!(i.state, None | Some(InputState::Received)))
            .count()
    }

    #[must_use]
    pub fn is_idle(&self) -> bool {
        self.turn.is_none() && !self.turn_requested && self.follow_ups.is_empty()
    }

    /// The last polled background terminals.
    #[must_use]
    pub fn background(&self) -> &[BackgroundTask] {
        &self.background
    }

    /// Node ids of commands that outlived their turn and have not completed.
    #[must_use]
    pub fn background_tools(&self) -> Vec<NodeId> {
        let mut open: Vec<&ToolNode> = self
            .tools
            .values()
            .filter(|n| n.background && !n.done)
            .collect();
        open.sort_by_key(|n| n.order);
        open.into_iter().map(|n| n.node_id.clone()).collect()
    }

    pub(super) fn host_id(&mut self, kind: HostRequest) -> String {
        self.next_host_id += 1;
        let id = format!("fd-{}", self.next_host_id);
        self.host_requests.insert(id.clone(), kind);
        id
    }

    fn live(&self) -> Result<()> {
        if self.ended {
            return Err(ProviderError::SessionEnded);
        }
        Ok(())
    }

    pub(super) fn thread(&self) -> Result<String> {
        self.live()?;
        self.thread_id
            .clone()
            .ok_or(ProviderError::NotReady("thread"))
    }

    /// `initialize` with the experimental API (needed for
    /// `item/tool/requestUserInput`).
    ///
    /// # Errors
    /// [`ProviderError::SessionEnded`] after the process ended.
    pub fn initialize(&mut self) -> Result<Vec<Output>> {
        self.live()?;
        let id = self.host_id(HostRequest::Initialize);
        Ok(vec![Output::Write(rpc::request(
            &id,
            "initialize",
            json!({
                "clientInfo": {"name": self.config.client.name, "version": self.config.client.version},
                "capabilities": self.capabilities_json(),
            }),
        ))])
    }

    /// Opens the thread: `thread/resume` or `thread/fork` if the spec names a
    /// thread, else `thread/start`.
    ///
    /// # Errors
    /// [`ProviderError::SessionEnded`] after the process ended.
    pub fn open_thread(&mut self, spec: &SessionSpec) -> Result<Vec<Output>> {
        self.live()?;
        self.spec = spec.clone();
        let mode = Mode::or_safest(spec.mode.as_deref());
        let mut params = serde_json::Map::new();
        params.insert("approvalPolicy".into(), mode.approval_policy.clone().into());
        params.insert("sandbox".into(), mode.sandbox.clone().into());
        if let Some(m) = &spec.model {
            params.insert("model".into(), m.clone().into());
        }
        if let Some(c) = &spec.cwd {
            params.insert("cwd".into(), c.clone().into());
        }
        self.mode = Some(mode);
        let (method, kind) = match &spec.resume {
            Some(thread) => {
                params.insert("threadId".into(), thread.clone().into());
                if spec.fork {
                    ("thread/fork", HostRequest::ThreadFork)
                } else {
                    ("thread/resume", HostRequest::ThreadResume)
                }
            }
            None => {
                params.insert("ephemeral".into(), false.into());
                ("thread/start", HostRequest::ThreadStart)
            }
        };
        let id = self.host_id(kind);
        Ok(vec![Output::Write(rpc::request(
            &id,
            method,
            Value::Object(params),
        ))])
    }

    fn track(&mut self, input: &HostInput) -> Result<()> {
        if input.input.text.is_empty() {
            return Err(ProviderError::InvalidInput(input.id.as_str().to_owned()));
        }
        if self.inputs.iter().any(|i| i.id == input.id) {
            return Err(ProviderError::InvalidInput(input.id.as_str().to_owned()));
        }
        self.inputs.push(InputEntry {
            id: input.id.clone(),
            origin: input.origin,
            summary: summary_of(&input.input.text),
            input_text: input_text_of(&input.input.text),
            author: input.input.author.clone(),
            state: None,
            turn: None,
        });
        Ok(())
    }

    /// A prompt. Idle: `turn/start`. Busy: a follow-up that the core sends
    /// after the running turn completes (Codex would merge it into the
    /// running turn; DL-015).
    ///
    /// # Errors
    /// No thread yet, an ended session, or an empty or reused input id.
    pub fn prompt(&mut self, input: &HostInput) -> Result<Vec<Output>> {
        let thread = self.thread()?;
        self.no_rollback_outstanding()?;
        self.track(input)?;
        if self.turn.is_some() || self.turn_requested || !self.follow_ups.is_empty() {
            self.follow_ups
                .push_back((input.id.clone(), input.input.clone()));
            return Ok(Vec::new());
        }
        Ok(self.turn_start(&thread, &input.input, &input.id))
    }

    pub(super) fn turn_start(
        &mut self,
        thread: &str,
        input: &UserInput,
        input_id: &InputId,
    ) -> Vec<Output> {
        let mut params = serde_json::Map::new();
        params.insert("threadId".into(), thread.into());
        params.insert("clientUserMessageId".into(), input_id.as_str().into());
        params.insert("input".into(), rpc::input_items(input));
        if let Some(e) = &self.spec.effort {
            params.insert("effort".into(), e.clone().into());
        }
        if let Some(m) = &self.pending_model {
            params.insert("model".into(), m.clone().into());
        }
        let model = self
            .pending_model
            .clone()
            .or_else(|| self.thread_model.clone())
            .or_else(|| self.spec.model.clone());
        if let (Some(c), Some(model)) = (&self.collaboration, model) {
            params.insert(
                "collaborationMode".into(),
                json!({"mode": c, "settings": {
                    "model": model,
                    "reasoning_effort": self.spec.effort,
                    "developer_instructions": null,
                }}),
            );
        }
        if let Some(m) = &self.pending_mode {
            params.insert("approvalPolicy".into(), m.approval_policy.clone().into());
            params.insert("sandboxPolicy".into(), m.sandbox_policy());
        }
        self.turn_requested = true;
        self.turn_input = Some(input_id.clone());
        let id = self.host_id(HostRequest::TurnStart {
            input_id: input_id.clone(),
        });
        vec![Output::Write(rpc::request(
            &id,
            "turn/start",
            Value::Object(params),
        ))]
    }

    /// Steers the running turn (`turn/steer` with `expectedTurnId`).
    ///
    /// # Errors
    /// [`ProviderError::NoTurn`] before the native turn id is known.
    pub fn steer(&mut self, input: &HostInput) -> Result<Vec<Output>> {
        let thread = self.thread()?;
        self.no_rollback_outstanding()?;
        let turn = self.native_turn.clone().ok_or(ProviderError::NoTurn)?;
        self.track(input)?;
        let id = self.host_id(HostRequest::TurnSteer {
            input_id: input.id.clone(),
        });
        Ok(vec![Output::Write(rpc::request(
            &id,
            "turn/steer",
            json!({
                "threadId": thread,
                "expectedTurnId": turn,
                "clientUserMessageId": input.id.as_str(),
                "input": rpc::input_items(&input.input),
            }),
        ))])
    }

    /// `turn/interrupt`. Before the turn runs (trap 3) the core holds the
    /// interrupt and sends it on `turn/started`. A second interrupt while one
    /// is on its way is refused with `interrupt.result`.
    ///
    /// # Errors
    /// [`ProviderError::NoTurn`] when no turn runs or waits.
    pub fn interrupt(&mut self, control_id: Option<String>) -> Result<Vec<Output>> {
        let thread = self.thread()?;
        if self.interrupt_requested || self.interrupt_pending.is_some() {
            return Ok(vec![Output::Emit(Event::InterruptResult {
                control_id,
                accepted: false,
                message: Some("an interrupt is already on its way".to_owned()),
            })]);
        }
        match self.native_turn.clone() {
            Some(turn) if self.native_started => {
                Ok(self.send_interrupt(&thread, &turn, control_id))
            }
            Some(_) => {
                self.interrupt_pending = Some(control_id);
                Ok(Vec::new())
            }
            None if self.turn_requested => {
                self.interrupt_pending = Some(control_id);
                Ok(Vec::new())
            }
            None => Err(ProviderError::NoTurn),
        }
    }

    pub(super) fn send_interrupt(
        &mut self,
        thread: &str,
        turn: &str,
        control_id: Option<String>,
    ) -> Vec<Output> {
        self.interrupt_requested = true;
        self.interrupt_pending = None;
        let id = self.host_id(HostRequest::Interrupt(control_id));
        vec![Output::Write(rpc::request(
            &id,
            "turn/interrupt",
            json!({"threadId": thread, "turnId": turn}),
        ))]
    }

    /// Asks Codex for the thread's background terminals
    /// (`thread/backgroundTerminals/list`). Codex reports no background work
    /// on its own after a turn ends (trap 4), so the driver polls (DL-024);
    /// the reply becomes `background.updated`.
    ///
    /// # Errors
    /// No thread yet or an ended session.
    pub fn list_background(&mut self) -> Result<Vec<Output>> {
        let thread = self.thread()?;
        let id = self.host_id(HostRequest::BackgroundList);
        Ok(vec![Output::Write(rpc::request(
            &id,
            "thread/backgroundTerminals/list",
            json!({ "threadId": thread }),
        ))])
    }

    /// Sets the collaboration mode of the next turns (`plan` or `default`).
    ///
    /// # Errors
    /// [`ProviderError::InvalidMode`] for another name.
    pub fn set_collaboration(&mut self, mode: &str) -> Result<Vec<Output>> {
        self.live()?;
        if !COLLABORATION_MODES.contains(&mode) {
            return Err(ProviderError::InvalidMode(mode.to_owned()));
        }
        self.collaboration = Some(mode.to_owned());
        Ok(Vec::new())
    }

    /// Changes the mode for the next turn. Codex takes the approval policy and
    /// sandbox per `turn/start`; `mode.changed` follows only when the
    /// app-server accepted a turn with the new mode (trap 1).
    ///
    /// # Errors
    /// [`ProviderError::InvalidMode`] for a mode that the session does not offer.
    pub fn set_mode(&mut self, mode: &str) -> Result<Vec<Output>> {
        self.live()?;
        let offered = self.config.capabilities.modes.iter().any(|m| m == mode);
        let parsed = Mode::parse(mode).filter(|_| offered);
        self.pending_mode =
            Some(parsed.ok_or_else(|| ProviderError::InvalidMode(mode.to_owned()))?);
        Ok(Vec::new())
    }

    /// Changes the model for the next turn (`model` of `turn/start`);
    /// `model.changed` follows when the app-server accepted that turn.
    ///
    /// # Errors
    /// [`ProviderError::InvalidModel`] for an empty, long or spaced name.
    pub fn set_model(&mut self, model: &str) -> Result<Vec<Output>> {
        self.live()?;
        if model.is_empty()
            || model.chars().count() > MAX_MODEL_CHARS
            || model.chars().any(char::is_whitespace)
        {
            return Err(ProviderError::InvalidModel(model.to_owned()));
        }
        self.pending_model = Some(model.to_owned());
        Ok(Vec::new())
    }

    /// Handles one app-server frame.
    pub fn on_frame(&mut self, frame: Incoming) -> Vec<Output> {
        if self.ended {
            return Vec::new();
        }
        match frame {
            Incoming::Response { id, result } => {
                if self.skill_answer(&id, false) || self.app_answer(&id, Ok(&result)) {
                    return Vec::new();
                }
                self.on_response(&id, Ok(result))
            }
            Incoming::ErrorResponse { id, code, message } => {
                if self.skill_answer(&id, true) || self.app_answer(&id, Err(message.as_str())) {
                    return Vec::new();
                }
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
        if self.ended {
            return Vec::new();
        }
        self.ended = true;
        let mut out = self.expire_open(ExpiredReason::ProcessLost);
        // B8: a shell command that did not complete ends `unknown`.
        out.extend(self.shell_exit());
        out.extend(self.close_all_tools());
        // No frame follows an exit: inputs that wait or run end as discarded,
        // so none stays open on the board.
        self.follow_ups.clear();
        for input in &mut self.inputs {
            if input.is_open() {
                input.state = Some(InputState::Discarded);
                out.push(Output::Emit(Event::InputUpdated {
                    input_id: input.id.clone(),
                    turn_id: input.turn.clone(),
                    state: InputState::Discarded,
                }));
            }
        }
        out.extend(self.refuse_held_interrupt("the process ended"));
        out.extend(self.end_turn(TurnStatus::Failed, None));
        self.native_turn = None;
        self.native_started = false;
        self.turn_requested = false;
        self.background.clear();
        out.push(Output::Emit(Event::SessionEnded {
            reason: "process_exit".to_owned(),
            exit_code,
        }));
        out
    }

    /// A held interrupt that never went out: the station retires its effect.
    pub(super) fn refuse_held_interrupt(&mut self, why: &str) -> Vec<Output> {
        match self.interrupt_pending.take() {
            Some(control_id) => vec![Output::Emit(Event::InterruptResult {
                control_id,
                accepted: false,
                message: Some(format!("not sent: {why}")),
            })],
            None => Vec::new(),
        }
    }

    /// Starts the canonical run and turn of a native turn, once.
    pub(super) fn ensure_turn(&mut self, native: &str) -> Vec<Output> {
        if self.native_turn.as_deref() == Some(native) {
            return Vec::new();
        }
        if self.turn.is_some() {
            // Another native turn while one runs: never replace the running
            // turn (its completion would be lost). Count it as unexpected.
            self.unknown_frames += 1;
            return Vec::new();
        }
        let mut out = Vec::new();
        // The full text leaves the entry once `run.started` has it (B10).
        let (origin, summary, author, input_id, input_text) = match self
            .turn_input
            .take()
            .and_then(|id| self.inputs.iter_mut().find(|i| i.id == id))
        {
            Some(e) => (
                e.origin,
                e.summary.clone(),
                e.author.clone(),
                Some(e.id.clone()),
                e.input_text.take(),
            ),
            // B8: the turn of a `!` shell command binds to its pending command.
            None => match self.bind_shell_turn(native) {
                Some(summary) => (RunOrigin::Shell, summary, None, None, None),
                None => (
                    RunOrigin::Provider,
                    "provider turn".to_owned(),
                    None,
                    None,
                    None,
                ),
            },
        };
        let run_id = self.ids.run();
        self.run = Some(run_id.clone());
        out.push(Output::Emit(Event::RunStarted {
            run_id: run_id.clone(),
            input_summary: summary,
            origin,
            author,
            input_id,
            input_text,
        }));
        let turn_id = TurnId(self.ids.next("turn"));
        self.turn = Some(turn_id.clone());
        self.native_turn = Some(native.to_owned());
        self.files_seen.clear();
        self.turn_diff = None;
        out.push(Output::Emit(Event::TurnStarted {
            run_id,
            turn_id,
            provider_refs: super::map::refs(&[
                ("thread_id", self.thread_id.as_deref()),
                ("turn_id", Some(native)),
            ]),
        }));
        out
    }

    /// `turn.completed` and `run.completed` of the running turn.
    pub(super) fn end_turn(
        &mut self,
        status: TurnStatus,
        usage: Option<fleetdeck_proto::event::Usage>,
    ) -> Vec<Output> {
        let mut out = Vec::new();
        self.turns_ended += 1;
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
                background_pending: !self.background_tools().is_empty()
                    || !self.background.is_empty(),
            }));
        }
        out
    }

    pub(super) fn set_input_state(
        &mut self,
        input_id: &InputId,
        state: InputState,
        turn: Option<TurnId>,
    ) -> Vec<Output> {
        let Some(entry) = self.inputs.iter_mut().find(|i| &i.id == input_id) else {
            return Vec::new();
        };
        if entry.state == Some(state) {
            return Vec::new();
        }
        // A late `Received` never moves an input back.
        if state == InputState::Received && entry.state.is_some() {
            return Vec::new();
        }
        entry.state = Some(state);
        if turn.is_some() {
            entry.turn = turn;
        }
        vec![Output::Emit(Event::InputUpdated {
            input_id: input_id.clone(),
            turn_id: entry.turn.clone(),
            state,
        })]
    }

    pub(super) fn dispatch_follow_up(&mut self) -> Vec<Output> {
        if self.turn.is_some() || self.turn_requested {
            return Vec::new();
        }
        let (Some(thread), Some((input_id, input))) =
            (self.thread_id.clone(), self.follow_ups.pop_front())
        else {
            return Vec::new();
        };
        self.turn_start(&thread, &input, &input_id)
    }

    pub(super) fn tool_order(&mut self) -> u64 {
        self.tool_order += 1;
        self.tool_order
    }
}

#[path = "core_apps.rs"]
mod apps_impl;
#[path = "core_skills.rs"]
mod skills;
