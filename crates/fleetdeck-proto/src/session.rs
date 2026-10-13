//! The session projection: the read-model shape of one session (PLAN §6.4,
//! §11). The station keeps it in its projections, sends it in snapshots
//! (G4), and the board renders it. `fleetdeck-core` folds events into it.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use ts_rs::TS;

use crate::capability::Capabilities;
use crate::event::{
    ApprovalOption, ModelChoice, ProviderKind, RateLimit, RequestKind, ResponseChannel,
    RevertState, SessionState, ToolKind, Usage,
};
use crate::ids::{NodeId, ProviderInstanceId, RequestId, RunId, SessionId, TurnId};

/// What a session does now, for the board's lanes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "session/")]
pub enum Activity {
    /// No run, no background work.
    Idle,
    /// A run is active.
    Working,
    /// A runtime request waits for an answer.
    NeedsYou,
    /// The run ended but background work still runs (L7).
    Background,
    /// The process ended or is lost.
    Ended,
}

/// An open runtime request (approval or question) with native option ids (L3).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export_to = "session/")]
pub struct OpenRequest {
    pub request_id: RequestId,
    pub node_id: Option<NodeId>,
    pub request_kind: RequestKind,
    pub title: String,
    pub details: String,
    pub input: Option<Value>,
    pub options: Vec<ApprovalOption>,
    pub response: ResponseChannel,
    /// Unix ms.
    pub requested_at: i64,
}

/// The projection of one session.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export_to = "session/")]
pub struct SessionSummary {
    pub session_id: SessionId,
    pub provider: Option<ProviderKind>,
    pub provider_instance: Option<ProviderInstanceId>,
    pub provider_version: Option<String>,
    pub cwd: Option<String>,
    /// The git branch of the workspace when the session started (M2 P4-2);
    /// `None` outside a repository or with a detached HEAD. Absent in a
    /// summary stored before P4-2.
    #[serde(default)]
    pub branch: Option<String>,
    /// The station's callsign (M3 6-1, DL-200): unique on its station; the
    /// mail and task address of the session. Absent before M3.
    #[serde(default)]
    pub callsign: Option<String>,
    /// The worktree that the station created for the session (M3 6-1).
    #[serde(default)]
    pub worktree: Option<String>,
    /// The repository of `cwd` (M3 6-7, DL-306). Absent before M3.
    #[serde(default)]
    pub repo: Option<RepoInfo>,
    pub title: Option<String>,
    pub state: SessionState,
    pub activity: Activity,
    pub mode: Option<String>,
    pub model: Option<String>,
    pub capabilities: Option<Capabilities>,
    pub current_run: Option<RunId>,
    pub current_turn: Option<TurnId>,
    pub open_requests: Vec<OpenRequest>,
    /// Ids of running background tasks (L7).
    pub background_tasks: Vec<String>,
    /// How many of `background_tasks` are subagents
    /// ([`SUBAGENT_TASK_KINDS`]): a subagent that runs on after its tool
    /// call completed (D15). Absent in a summary stored before M2 P4-3.
    #[serde(default)]
    pub background_agents: u32,
    /// Tools that started and did not complete yet, oldest first, at most
    /// [`MAX_RUNNING_TOOLS`]. The deck derives `verifying` from them (§7.2).
    /// Absent in a summary stored before M2 P3-6.
    #[serde(default)]
    pub running_tools: Vec<RunningTool>,
    /// Sum of the reported usage; `None` parts are unknown.
    pub usage: Usage,
    /// The latest provider rate-limit windows, one per scope, sorted by
    /// scope (M3 6-9, DL-303). Empty: no window reported (unknown, not
    /// zero). Absent in a summary stored before M3.
    #[serde(default)]
    pub rate_limits: Vec<RateLimit>,
    /// The last error, for example `usage_limit` with its retry time.
    pub last_error: Option<SessionError>,
    /// The latest revert of the session (PLAN §6.6, DL-087). While it is
    /// `admitted`, `conversation_rolled_back` or `reconciliation_required`,
    /// the station refuses new input. Absent in a summary stored before M3.
    #[serde(default)]
    pub revert: Option<SessionRevert>,
    /// Auto-continue after a station restart (L19, ADR-007 §2.1). Off by
    /// default, also for a summary stored before it existed.
    #[serde(default)]
    pub auto_continue: bool,
    /// The TUI that holds the session (DL-140). Absent in older summaries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub terminal: Option<TerminalHold>,
    /// The models that the provider harness offers, from the last
    /// `session.models` (DL-151). Absent before the first one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub models: Option<Vec<ModelChoice>>,
    /// The commit that the session started from, from `session.base`
    /// (R1.1). Absent before the first one and outside a repository.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub base: Option<SessionBase>,
    /// The `Nx` batch that started the session, from `session.batch`
    /// (R1.4, DL-170). Absent for a session that is not in a batch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub batch: Option<BatchRef>,
    /// The highest `session_seq` folded into this summary.
    pub session_seq: u64,
    /// Unix ms of the last folded event.
    pub updated_at: i64,
}

impl SessionSummary {
    /// An empty summary for a session that has no events yet.
    #[must_use]
    pub fn new(session_id: SessionId) -> Self {
        Self {
            session_id,
            provider: None,
            provider_instance: None,
            provider_version: None,
            cwd: None,
            branch: None,
            callsign: None,
            worktree: None,
            repo: None,
            title: None,
            state: SessionState::Starting,
            activity: Activity::Idle,
            mode: None,
            model: None,
            capabilities: None,
            current_run: None,
            current_turn: None,
            open_requests: Vec::new(),
            background_tasks: Vec::new(),
            background_agents: 0,
            running_tools: Vec::new(),
            usage: Usage::default(),
            rate_limits: Vec::new(),
            last_error: None,
            revert: None,
            auto_continue: false,
            terminal: None,
            models: None,
            base: None,
            batch: None,
            session_seq: 0,
            updated_at: 0,
        }
    }
}

/// A provider TUI that holds the session (DL-140). While it exists the
/// station refuses resume, auto-continue, import and a second handover.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "session/")]
pub struct TerminalHold {
    pub shell_id: String,
    /// The TUI process, once it runs.
    pub pid: Option<u32>,
    /// Unix ms of the first `session.terminal` of this hold.
    #[ts(type = "number")]
    pub since: i64,
    /// The reason of the newest `session.terminal` of this hold.
    pub reason: Option<String>,
}

/// The commit that a session started from (R1.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "session/")]
pub struct SessionBase {
    /// The branch or ref name that the base came from, when there is one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub git_ref: Option<String>,
    /// The commit id.
    pub oid: String,
    /// The station guessed the base (an import or a resume).
    #[serde(default)]
    pub inferred: bool,
}

/// The batch that a session started in (R1.4, DL-170): one `batch_id` for each
/// press of "Start N sessions", the position of the session in it, and the
/// size of the batch. The board makes the id (a ULID); the station keeps it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "session/")]
pub struct BatchRef {
    pub batch_id: String,
    /// The place in the batch, from 1.
    pub index: u32,
    /// How many sessions the batch asked for.
    pub size: u32,
}

/// The most sessions that one batch holds (a refusal above it).
pub const MAX_BATCH_SIZE: u32 = 100;

impl BatchRef {
    /// Why the reference is not valid, or `None`: a `batch_id` of 1 to 64
    /// characters (`[0-9A-Za-z_-]`), `index` from 1 to `size`, and `size`
    /// from 1 to [`MAX_BATCH_SIZE`].
    #[must_use]
    pub fn problem(&self) -> Option<&'static str> {
        let id_ok = !self.batch_id.is_empty()
            && self.batch_id.len() <= 64
            && self
                .batch_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
        if !id_ok {
            return Some("the batch id must have 1 to 64 characters of [0-9A-Za-z_-]");
        }
        if self.size == 0 || self.size > MAX_BATCH_SIZE {
            return Some("the batch size must be from 1 to 100");
        }
        if self.index == 0 || self.index > self.size {
            return Some("the batch index must be from 1 to the batch size");
        }
        None
    }
}

/// The most running tools that a summary keeps; the oldest goes first.
pub const MAX_RUNNING_TOOLS: usize = 32;

/// The repository that holds a session's `cwd` on its station (M3 6-7,
/// DL-306).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "session/")]
pub struct RepoInfo {
    /// The absolute work tree root on the station.
    pub root: String,
    /// The normalized origin (`host/path`) when the repository has exactly
    /// one remote. A heuristic, not an identity (PLAN §6.6).
    pub origin: Option<String>,
}

/// Background task kinds that are subagents (D15): Claude reports a
/// backgrounded `Agent`/`Task` call as `local_agent`.
pub const SUBAGENT_TASK_KINDS: &[&str] = &["local_agent", "remote_agent"];

/// A tool call that runs now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "session/")]
pub struct RunningTool {
    pub node_id: NodeId,
    pub tool_kind: ToolKind,
    pub title: String,
    pub input_summary: String,
}

/// The status column of a card (PLAN §7.2). The deck derives it from the
/// session projection and the state of the station link.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "session/")]
pub enum CardStatus {
    /// The session is starting; no run yet.
    Queued,
    Working,
    /// A running shell tool matches the test-runner pattern.
    Verifying,
    /// An open approval or question.
    NeedsYou,
    Idle,
    Ended,
    /// The station link is down; the card shows the last known state.
    Unreachable,
    /// The station restarted and the process is gone; the card offers resume.
    Lost,
}

/// The last error of a session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "session/")]
pub struct SessionError {
    pub code: String,
    pub message: String,
    pub retry_at: Option<i64>,
}

/// The latest revert of a session, as the board shows it (DL-087).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "session/")]
pub struct SessionRevert {
    pub revert_id: String,
    pub turn_id: TurnId,
    pub state: RevertState,
    pub reason: Option<String>,
}

impl SessionRevert {
    /// True while the revert blocks new session input.
    #[must_use]
    pub fn blocks_input(&self) -> bool {
        matches!(
            self.state,
            RevertState::Admitted
                | RevertState::ConversationRolledBack
                | RevertState::ReconciliationRequired
        )
    }
}
