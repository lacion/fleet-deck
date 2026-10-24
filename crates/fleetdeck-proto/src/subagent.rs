//! The log of one subagent (DL-191): the params and answer of the link
//! method `session.subagent` (feature `subagent.v1`, deck → station, scope
//! `read`, not mutating).
//!
//! The caller names a session and a tool use of the session. The station
//! finds the subagent transcript from its own binding row; the caller never
//! gives an agent id or a path. The error codes are those of
//! [`crate::history::code`].

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::history::{HistoryItem, HistoryState};
use crate::ids::SessionId;

/// The most items in one answer (DL-191).
pub const MAX_ITEMS: usize = 300;
/// The most bytes of the serialized answer (DL-191).
pub const MAX_ANSWER_BYTES: usize = 512 * 1024;

/// `session.subagent` params.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "subagent/")]
pub struct SubagentQuery {
    pub session_id: SessionId,
    /// The tool use of the `Agent` or `Task` call: 1 to 128 of `[A-Za-z0-9_-]`.
    pub tool_use_id: String,
}

/// The `session.subagent` answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "subagent/")]
pub struct SubagentLog {
    /// Oldest first.
    pub items: Vec<HistoryItem>,
    pub state: HistoryState,
}

/// `session.workflow_agent` params (B12, feature `workflows.v1`, deck →
/// station, scope `read`, not mutating). The station finds the transcript
/// from its own binding row of this agent; the caller never gives a path or a
/// run id. The answer is [`SubagentLog`] (same limits: [`MAX_ITEMS`],
/// [`MAX_ANSWER_BYTES`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "subagent/")]
pub struct WorkflowAgentQuery {
    pub session_id: SessionId,
    /// `[a-z0-9]{1,64}`.
    pub agent_id: String,
}
