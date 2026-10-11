//! The session projection: the read-model shape of one session (PLAN §6.4,
//! §11). The station keeps it in its projections, sends it in snapshots
//! (G4), and the board renders it. `fleetdeck-core` folds events into it.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use ts_rs::TS;

use crate::capability::Capabilities;
use crate::event::{
    ApprovalOption, ProviderKind, RequestKind, ResponseChannel, SessionState, ToolKind, Usage,
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
    /// The last error, for example `usage_limit` with its retry time.
    pub last_error: Option<SessionError>,
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
            last_error: None,
            session_seq: 0,
            updated_at: 0,
        }
    }
}

/// The most running tools that a summary keeps; the oldest goes first.
pub const MAX_RUNNING_TOOLS: usize = 32;

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
