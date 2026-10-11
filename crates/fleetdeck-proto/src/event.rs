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
        capabilities: Capabilities,
        /// `Some(false)`: a resume was asked for but a new session started.
        resumed: Option<bool>,
        provider_refs: ProviderRefs,
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
    pub const KINDS: [&'static str; 24] = [
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
        "run.completed",
        "mode.changed",
        "model.changed",
        "input.updated",
        "background.updated",
        "error",
        "session.ended",
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
            Self::RunCompleted { .. } => "run.completed",
            Self::ModeChanged { .. } => "mode.changed",
            Self::ModelChanged { .. } => "model.changed",
            Self::InputUpdated { .. } => "input.updated",
            Self::BackgroundUpdated { .. } => "background.updated",
            Self::Error { .. } => "error",
            Self::SessionEnded { .. } => "session.ended",
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
