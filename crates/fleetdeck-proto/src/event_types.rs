//! The small types and enums of the canonical events (moved from `event.rs`
//! for the module size rule, T1.0).

use serde::{Deserialize, Serialize};
use serde_json::Value;
use ts_rs::TS;

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

/// The outcome of a worker grant that did not reach a provider (MCP P4a
/// §6.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "event/")]
pub enum WorkerOutcome {
    /// The station skipped the grant: the start went on without it.
    Skipped,
}

/// One answer option with the provider's own option id (L3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "event/")]
pub struct ApprovalOption {
    pub option_id: String,
    pub label: String,
    /// What the option means, set by the provider adapter (ADR-008 §5.1).
    /// `None`: a request stored before the field, a question option, or an
    /// option that the adapter cannot classify.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub kind: Option<OptionKind>,
}

/// The semantic kind of an approval option (ADR-008 §5.1). The provider
/// adapter owns the classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "event/")]
pub enum OptionKind {
    AllowOnce,
    AllowSession,
    AllowAlways,
    Deny,
    Amend,
    Other,
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
    /// `cost_usd` comes from a local price table (R4.6, DL-161), not from
    /// the provider.
    #[serde(default)]
    pub cost_inferred: bool,
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
