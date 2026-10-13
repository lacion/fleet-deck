//! Canonical, provider-blind events (PLAN §6.3, L6, L7, L16).
//!
//! Wire rules:
//! - The tag is `kind` with dotted names (`turn.completed`).
//! - A known kind is strict: a missing required field is an error.
//! - An unknown kind from a newer peer is kept as [`EventBody::Unknown`] and
//!   is passed through, so an old deck does not drop a newer station (L16).
//! - Extra fields are ignored; optional fields default to `null`.
//! - Times are Unix milliseconds (`i64`).

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;
use ts_rs::TS;

use crate::capability::Capabilities;
use crate::ids::{
    InputId, NodeId, PlanId, ProviderInstanceId, RequestId, RunId, SessionId, TurnId,
};
use crate::session::RepoInfo;

/// Native ids of a provider, scoped by provider + session (L6). Never a global key.
pub type ProviderRefs = serde_json::Map<String, Value>;

/// The provider family of a session (D14: Claude and Codex in v2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "event/")]
pub enum ProviderKind {
    Claude,
    Codex,
    /// A provider of a newer peer.
    #[serde(other)]
    Unknown,
}

/// Process lifecycle of a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "event/")]
pub enum SessionState {
    Starting,
    Running,
    Stopping,
    Exited,
    Lost,
}

/// Where a run came from. `local` is the station-local CLI; `provider` is a
/// turn that the provider started without host input (background work, L7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "event/")]
pub enum RunOrigin {
    Board,
    Mail,
    Coordinator,
    Resume,
    Local,
    Provider,
    /// The station's fixed continuation input after a restart (L19,
    /// ADR-007 §2.3). Older peers decode it as unknown.
    Continuation,
}

/// Kind of a tool call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "event/")]
pub enum ToolKind {
    Shell,
    Edit,
    Read,
    Search,
    Web,
    Mcp,
    Subagent,
    #[serde(other)]
    Other,
}

/// Terminal status of a tool call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "event/")]
pub enum ToolStatus {
    Ok,
    Failed,
    Denied,
}

/// Change kind of a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "event/")]
pub enum FileChange {
    Added,
    Modified,
    Deleted,
}

/// Kind of a runtime request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "event/")]
pub enum RequestKind {
    Command,
    FileChange,
    Tool,
    Question,
}

/// How the provider expects the answer to a runtime request (L4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "event/")]
pub enum ResponseChannel {
    /// The answer is a reply to the provider's own request id.
    RpcReply,
    /// The answer is a new user message.
    Message,
}

/// One answer option with the provider's own option id (L3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "event/")]
pub struct ApprovalOption {
    pub option_id: String,
    pub label: String,
}

/// Who answered a runtime request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "event/")]
pub enum ResolvedBy {
    Board,
    Policy,
    Timeout,
    Coordinator,
    Local,
}

/// Why a runtime request can no longer be answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "event/")]
pub enum ExpiredReason {
    /// The provider restarted and does not ask again.
    NotResumable,
    /// The provider process is gone.
    ProcessLost,
    /// The turn ended before an answer (for example an interrupt).
    TurnEnded,
    /// The provider withdrew the request.
    CancelledByProvider,
}

/// Why provider output bytes were not parsed (ADR-005 §4.2; DL-113).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "event/")]
pub enum OutputGapReason {
    /// One line reached the shim buffer size without a newline; the shim
    /// dropped it and stopped the provider.
    OutputFrameTooLarge,
    /// The provider ended inside a line; the bytes after the last newline
    /// are not an event.
    UnterminatedLine,
    /// The bytes were freed below the shim floor before this station read
    /// them.
    BelowFloor,
}

/// Terminal status of a turn or a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "event/")]
pub enum TurnStatus {
    Done,
    Interrupted,
    Failed,
}

/// The fate of a host input as the provider reports it (ADR-001, ADR-002).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "event/")]
pub enum InputState {
    Received,
    Started,
    Completed,
    Cancelled,
    Discarded,
    Refused,
}

/// Token usage. `None` means unknown, which is not zero.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, TS)]
#[ts(export_to = "event/")]
pub struct Usage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cost_usd: Option<f64>,
}

/// One provider rate limit window.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export_to = "event/")]
pub struct RateLimit {
    /// Provider name of the window, for example `five_hour`.
    pub scope: String,
    pub used_percent: Option<f64>,
    /// Unix ms when the window resets.
    pub resets_at: Option<i64>,
}

/// One unit of provider background work (L7).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "event/")]
pub struct BackgroundTask {
    pub task_id: String,
    /// The provider's kind name, for example `local_bash` or `local_agent`.
    pub task_kind: String,
}

/// One model that a session can switch to (DL-151).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "event/")]
pub struct ModelChoice {
    /// What the provider takes to select the model (`set_model`).
    pub value: String,
    pub label: String,
    #[serde(default)]
    pub description: Option<String>,
}

/// One side (start or completion) of a turn checkpoint (PLAN §6.6, L13).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "state", rename_all = "snake_case")]
#[ts(export_to = "event/")]
pub enum CheckpointSide {
    /// A hidden ref points at this checkpoint commit.
    Available { commit: String },
    /// No checkpoint; `reason` is a stable code (for example
    /// `queued_input`, `background_writers`, `submodule`).
    Unavailable { reason: String },
}

/// The checkpoints of one finalized turn (PLAN §6.6, L13). A revert to the
/// turn start needs `start` available; a diff needs both sides.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "event/")]
pub struct TurnCheckpoint {
    /// The canonical worktree identity; `None` when the session directory
    /// is not a Git worktree.
    pub worktree_id: Option<String>,
    /// The snapshot scope (`tracked`).
    pub scope: String,
    pub start: CheckpointSide,
    pub end: CheckpointSide,
}

/// The size of a turn diff, computed from the two checkpoints.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "event/")]
pub struct DiffSummary {
    pub files_changed: u32,
    pub insertions: u64,
    pub deletions: u64,
}

/// The state of one revert (PLAN §6.6, DL-087).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "event/")]
pub enum RevertState {
    /// Admitted under the session fence; nothing external happened yet.
    Admitted,
    /// The provider confirmed the conversation rollback; the workspace
    /// restore comes next.
    ConversationRolledBack,
    /// The conversation and the workspace both match the turn start.
    Succeeded,
    /// Nothing changed: neither the conversation nor the workspace.
    Failed,
    /// The conversation and the workspace can differ; session input stays
    /// blocked until the owner reconciles the session.
    ReconciliationRequired,
    /// The owner resolved a revert in `reconciliation_required` and accepted
    /// the state of the conversation and the files (DL-097). The session
    /// takes input again; no earlier turn can be reverted.
    Reconciled,
}

/// Where a raw provider frame of an event is in the diagnostic log (L12).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "event/")]
pub struct RawRef {
    pub file: String,
    pub offset: u64,
}

/// A canonical event of a known kind.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "kind")]
#[ts(export_to = "event/")]
pub enum Event {
    #[serde(rename = "session.started")]
    SessionStarted {
        provider: ProviderKind,
        provider_instance: ProviderInstanceId,
        provider_version: Option<String>,
        model: Option<String>,
        cwd: String,
        mode: Option<String>,
        /// Boxed: it is the largest field of the largest event (DL-061).
        capabilities: Box<Capabilities>,
        /// `Some(false)`: a resume was asked for but a new session started.
        resumed: Option<bool>,
        provider_refs: ProviderRefs,
        /// The title and the git branch that the station recorded when it
        /// created the session (M2 P4-4, DL-061). A provider core leaves
        /// them out; the station's store fills them in, so the deck's read
        /// model has them without a snapshot.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        title: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        branch: Option<String>,
        /// The repository of `cwd` that the station recorded (M3 6-7,
        /// DL-306); the conflict radar groups file touches by it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        /// Boxed to keep the event small (clippy `large_enum_variant`).
        repo: Option<Box<RepoInfo>>,
    },
    #[serde(rename = "session.state")]
    SessionStateChanged {
        state: SessionState,
        detail: Option<String>,
    },
    #[serde(rename = "run.started")]
    RunStarted {
        run_id: RunId,
        input_summary: String,
        origin: RunOrigin,
    },
    #[serde(rename = "turn.started")]
    TurnStarted {
        run_id: RunId,
        turn_id: TurnId,
        provider_refs: ProviderRefs,
    },
    #[serde(rename = "message.delta")]
    MessageDelta {
        turn_id: TurnId,
        /// The subagent tool call that produced the text, if any (L7).
        parent_node_id: Option<NodeId>,
        text: String,
    },
    #[serde(rename = "message.completed")]
    MessageCompleted {
        turn_id: TurnId,
        parent_node_id: Option<NodeId>,
        text: String,
    },
    #[serde(rename = "reasoning.delta")]
    ReasoningDelta { turn_id: TurnId, text: String },
    #[serde(rename = "tool.started")]
    ToolStarted {
        node_id: NodeId,
        parent_node_id: Option<NodeId>,
        /// PLAN §6.3 calls this field `kind`; `kind` is the event tag (DL-027).
        tool_kind: ToolKind,
        title: String,
        input_summary: String,
        #[serde(default)]
        files: Vec<String>,
        provider_refs: ProviderRefs,
    },
    #[serde(rename = "tool.output")]
    ToolOutput {
        node_id: NodeId,
        chunk: String,
        /// True if the station cut the output to its budget.
        #[serde(default)]
        truncated: bool,
    },
    #[serde(rename = "tool.completed")]
    ToolCompleted {
        node_id: NodeId,
        status: ToolStatus,
        #[serde(default)]
        files: Vec<String>,
        exit_code: Option<i32>,
    },
    #[serde(rename = "file.changed")]
    FileChanged {
        path: String,
        change: FileChange,
        node_id: Option<NodeId>,
    },
    #[serde(rename = "approval.requested")]
    ApprovalRequested {
        request_id: RequestId,
        node_id: Option<NodeId>,
        /// PLAN §6.3 calls this field `kind` (DL-027).
        request_kind: RequestKind,
        title: String,
        details: String,
        /// The tool input that an allow can edit (only with `edit_input`).
        input: Option<Value>,
        options: Vec<ApprovalOption>,
        response: ResponseChannel,
        provider_refs: ProviderRefs,
    },
    #[serde(rename = "approval.resolved")]
    ApprovalResolved {
        request_id: RequestId,
        option_id: String,
        by: ResolvedBy,
    },
    #[serde(rename = "approval.expired")]
    ApprovalExpired {
        request_id: RequestId,
        reason: ExpiredReason,
    },
    #[serde(rename = "plan.proposed")]
    PlanProposed {
        plan_id: PlanId,
        turn_id: Option<TurnId>,
        markdown: String,
    },
    #[serde(rename = "usage.updated")]
    UsageUpdated {
        usage: Usage,
        #[serde(default)]
        rate_limits: Vec<RateLimit>,
    },
    #[serde(rename = "turn.completed")]
    TurnCompleted {
        turn_id: TurnId,
        status: TurnStatus,
        usage: Option<Usage>,
    },
    /// The checkpoint record of a completed turn (PLAN §6.3, §6.6, L13).
    /// It comes after `turn.completed`, at most once per app turn, and never
    /// changes the turn's duration or the session activity.
    #[serde(rename = "turn.finalized")]
    TurnFinalized {
        turn_id: TurnId,
        /// Always set by a station with the workspace service.
        checkpoint: Option<TurnCheckpoint>,
        /// Set when both checkpoint sides are available.
        diff_summary: Option<DiffSummary>,
    },
    /// A revert changed state (PLAN §6.6, DL-087). `turn_id` is the turn
    /// whose start is the target; `turns` is the count of whole turns that
    /// the provider rolls back. `reason` is a stable code for `failed` and
    /// `reconciliation_required`.
    #[serde(rename = "revert.updated")]
    RevertUpdated {
        revert_id: String,
        turn_id: TurnId,
        turns: u32,
        state: RevertState,
        reason: Option<String>,
    },
    /// The owner set or cleared the board title (DL-095). The station is
    /// the source of the title; the title is already normalized.
    #[serde(rename = "session.renamed")]
    SessionRenamed { title: Option<String> },
    /// The station named a new session (M3 6-1, DL-200): its callsign,
    /// unique on the station, and the worktree that its start creates.
    /// Older peers decode this kind as unknown.
    #[serde(rename = "session.named")]
    SessionNamed {
        callsign: String,
        worktree: Option<String>,
    },
    /// The owner turned auto-continue on or off (ADR-007 §2.1).
    #[serde(rename = "session.auto_continue_changed")]
    AutoContinueChanged { enabled: bool },
    /// A lost session was not continued at a station start; `reason` is
    /// the stable code of the first failed rule (ADR-007 §2.2).
    #[serde(rename = "session.auto_continue_skipped")]
    AutoContinueSkipped { reason: String },
    /// The station bound a coordinator credential to the session's native
    /// identity, or renewed it (ADR-006 §5.5, §6.1). No secret.
    #[serde(rename = "coordinator.bound")]
    CoordinatorBound {
        binding_id: String,
        grant_id: String,
        generation: u32,
        provider_instance: String,
        native_id: String,
    },
    /// A coordinator binding ended: `idle` (expired, renewable) or
    /// another reason (revoked) (ADR-006 §5.5).
    #[serde(rename = "coordinator.ended")]
    CoordinatorEnded {
        binding_id: String,
        generation: u32,
        reason: String,
    },
    /// Provider stdout bytes `[from, to)` were not parsed as events
    /// (ADR-005 §4.2; DL-113). Older peers decode this kind as unknown.
    #[serde(rename = "session.output_gap")]
    OutputGap {
        from: u64,
        to: u64,
        reason: OutputGapReason,
    },
    /// A provider TUI holds the session (`shell_id` set), or the hold ended
    /// (`shell_id` absent) (DL-140). `pid` is the TUI process once it runs.
    #[serde(rename = "session.terminal")]
    SessionTerminal {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        shell_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        pid: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        reason: Option<String>,
    },
    /// The slash commands that the provider offers in this session (DL-140).
    #[serde(rename = "session.slash_commands")]
    SlashCommands { commands: Vec<String> },
    /// A notice for the session chat. `code` names the cause; `text` is for
    /// the user. The fold ignores it.
    #[serde(rename = "session.notice")]
    SessionNotice { code: String, text: String },
    /// The models that the provider offers in this session (DL-151). The
    /// fold ignores it.
    #[serde(rename = "session.models")]
    SessionModels { models: Vec<ModelChoice> },
    #[serde(rename = "run.completed")]
    RunCompleted {
        run_id: RunId,
        status: TurnStatus,
        /// Background work still runs; the session is not idle (L7).
        background_pending: bool,
    },
    #[serde(rename = "mode.changed")]
    ModeChanged { mode: String },
    #[serde(rename = "model.changed")]
    ModelChanged { model: String },
    /// The provider answered an interrupt request (ADR-001 §3.1): this is
    /// the evidence that confirms or refuses the station's interrupt. The
    /// turn ends later with its own `turn.completed` (trap 10).
    #[serde(rename = "interrupt.result")]
    InterruptResult {
        /// The host's id for the request (the station's effect id); `None`
        /// when the host gave no id.
        control_id: Option<String>,
        accepted: bool,
        message: Option<String>,
    },
    #[serde(rename = "input.updated")]
    InputUpdated {
        input_id: InputId,
        turn_id: Option<TurnId>,
        state: InputState,
    },
    #[serde(rename = "background.updated")]
    BackgroundUpdated { tasks: Vec<BackgroundTask> },
    #[serde(rename = "error")]
    Error {
        /// Stable code, for example `usage_limit` or `auth_required`.
        code: String,
        message: String,
        /// Unix ms when a retry can succeed (for example a usage limit reset).
        retry_at: Option<i64>,
    },
    #[serde(rename = "session.ended")]
    SessionEnded {
        reason: String,
        exit_code: Option<i32>,
    },
}

impl Event {
    /// Every known kind name, in declaration order.
    pub const KINDS: [&'static str; 38] = [
        "session.started",
        "session.state",
        "run.started",
        "turn.started",
        "message.delta",
        "message.completed",
        "reasoning.delta",
        "tool.started",
        "tool.output",
        "tool.completed",
        "file.changed",
        "approval.requested",
        "approval.resolved",
        "approval.expired",
        "plan.proposed",
        "usage.updated",
        "turn.completed",
        "turn.finalized",
        "run.completed",
        "mode.changed",
        "model.changed",
        "interrupt.result",
        "input.updated",
        "background.updated",
        "error",
        "session.ended",
        "revert.updated",
        "session.renamed",
        "session.named",
        "session.output_gap",
        "session.auto_continue_changed",
        "session.auto_continue_skipped",
        "coordinator.bound",
        "coordinator.ended",
        "session.terminal",
        "session.slash_commands",
        "session.notice",
        "session.models",
    ];

    /// The dotted kind name.
    #[must_use]
    pub fn kind_name(&self) -> &'static str {
        match self {
            Self::SessionStarted { .. } => "session.started",
            Self::SessionStateChanged { .. } => "session.state",
            Self::RunStarted { .. } => "run.started",
            Self::TurnStarted { .. } => "turn.started",
            Self::MessageDelta { .. } => "message.delta",
            Self::MessageCompleted { .. } => "message.completed",
            Self::ReasoningDelta { .. } => "reasoning.delta",
            Self::ToolStarted { .. } => "tool.started",
            Self::ToolOutput { .. } => "tool.output",
            Self::ToolCompleted { .. } => "tool.completed",
            Self::FileChanged { .. } => "file.changed",
            Self::ApprovalRequested { .. } => "approval.requested",
            Self::ApprovalResolved { .. } => "approval.resolved",
            Self::ApprovalExpired { .. } => "approval.expired",
            Self::PlanProposed { .. } => "plan.proposed",
            Self::UsageUpdated { .. } => "usage.updated",
            Self::TurnCompleted { .. } => "turn.completed",
            Self::TurnFinalized { .. } => "turn.finalized",
            Self::RunCompleted { .. } => "run.completed",
            Self::ModeChanged { .. } => "mode.changed",
            Self::ModelChanged { .. } => "model.changed",
            Self::InterruptResult { .. } => "interrupt.result",
            Self::InputUpdated { .. } => "input.updated",
            Self::BackgroundUpdated { .. } => "background.updated",
            Self::Error { .. } => "error",
            Self::SessionEnded { .. } => "session.ended",
            Self::RevertUpdated { .. } => "revert.updated",
            Self::SessionRenamed { .. } => "session.renamed",
            Self::SessionNamed { .. } => "session.named",
            Self::AutoContinueChanged { .. } => "session.auto_continue_changed",
            Self::AutoContinueSkipped { .. } => "session.auto_continue_skipped",
            Self::CoordinatorBound { .. } => "coordinator.bound",
            Self::CoordinatorEnded { .. } => "coordinator.ended",
            Self::OutputGap { .. } => "session.output_gap",
            Self::SessionTerminal { .. } => "session.terminal",
            Self::SlashCommands { .. } => "session.slash_commands",
            Self::SessionNotice { .. } => "session.notice",
            Self::SessionModels { .. } => "session.models",
        }
    }

    /// True if the event ends the session's process life.
    #[must_use]
    pub fn is_terminal_for_session(&self) -> bool {
        matches!(self, Self::SessionEnded { .. })
    }
}

/// An event body: a known [`Event`], or an unknown kind kept verbatim (L16).
#[derive(Debug, Clone, PartialEq)]
pub enum EventBody {
    Known(Event),
    Unknown {
        kind: String,
        payload: serde_json::Map<String, Value>,
    },
}

impl EventBody {
    /// The dotted kind name.
    #[must_use]
    pub fn kind_name(&self) -> &str {
        match self {
            Self::Known(event) => event.kind_name(),
            Self::Unknown { kind, .. } => kind,
        }
    }

    /// The known event, if any.
    #[must_use]
    pub fn known(&self) -> Option<&Event> {
        match self {
            Self::Known(event) => Some(event),
            Self::Unknown { .. } => None,
        }
    }
}

impl From<Event> for EventBody {
    fn from(event: Event) -> Self {
        Self::Known(event)
    }
}

impl Serialize for EventBody {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Known(event) => event.serialize(serializer),
            Self::Unknown { payload, .. } => payload.serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for EventBody {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let payload = serde_json::Map::<String, Value>::deserialize(deserializer)?;
        let kind = match payload.get("kind") {
            Some(Value::String(kind)) => kind.clone(),
            Some(_) => return Err(D::Error::custom("event `kind` is not a string")),
            None => return Err(D::Error::missing_field("kind")),
        };
        if !Event::KINDS.contains(&kind.as_str()) {
            return Ok(Self::Unknown { kind, payload });
        }
        serde_json::from_value(Value::Object(payload))
            .map(Self::Known)
            .map_err(|err| D::Error::custom(format!("invalid `{kind}` event: {err}")))
    }
}

/// One stored event with its sequence numbers (PLAN §6.3, §6.4).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export_to = "event/")]
pub struct EventEnvelope {
    /// Per-station monotonic counter; the deck's resync cursor.
    pub station_seq: u64,
    pub session_id: SessionId,
    /// Per-session counter for UI ordering.
    pub session_seq: u64,
    /// Unix ms.
    pub at: i64,
    pub proto_version: u32,
    pub raw_ref: Option<RawRef>,
    /// An unknown kind is typed as `Event` in TypeScript; the board's reducer
    /// ignores kinds that it does not know.
    #[ts(as = "Event")]
    pub event: EventBody,
}

#[cfg(test)]
#[path = "event_tests.rs"]
mod tests;
