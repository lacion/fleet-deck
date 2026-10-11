//! The session projection: the read-model shape of one session (PLAN §6.4,
//! §11). The station keeps it in its projections, sends it in snapshots
//! (G4), and the board renders it. `fleetdeck-core` folds events into it.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use ts_rs::TS;

use crate::capability::Capabilities;
use crate::event::{
    ApprovalOption, ProviderKind, RequestKind, ResponseChannel, SessionState, Usage,
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
            usage: Usage::default(),
            last_error: None,
            session_seq: 0,
            updated_at: 0,
        }
    }
}

/// The last error of a session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "session/")]
pub struct SessionError {
    pub code: String,
    pub message: String,
    pub retry_at: Option<i64>,
}
