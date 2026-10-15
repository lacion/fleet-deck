//! The session history (DL-189): the params and answers of the link method
//! `session.history` (feature `history.v1`, deck → station, scope `read`,
//! not mutating).
//!
//! The station answers from its own snapshot of the provider transcript,
//! taken at import. The caller gives a session id, a cursor and a page size;
//! it never gives a path.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::ids::SessionId;

/// The most items in one page.
pub const MAX_LIMIT: u32 = 100;
/// The page size when the query gives none.
pub const DEFAULT_LIMIT: u32 = 50;

/// Wire error codes of `session.history`.
pub mod code {
    /// A param is out of range, or the cursor has a bad syntax.
    pub const INVALID_PARAMS: &str = "invalid_params";
    /// The session is unknown.
    pub const UNKNOWN_SESSION: &str = "unknown_session";
    /// The cursor belongs to another snapshot.
    pub const STALE_CURSOR: &str = "stale_cursor";
    /// The history work did not finish in time.
    pub const BUSY: &str = "busy";
}

/// `session.history` params.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "history/")]
pub struct SessionHistoryQuery {
    pub session_id: SessionId,
    /// The cursor of the next older page, from the previous answer.
    #[serde(default)]
    pub before: Option<String>,
    /// 1 to [`MAX_LIMIT`]; the default is [`DEFAULT_LIMIT`].
    #[serde(default)]
    pub limit: Option<u32>,
    /// The deck sets this for a member: the station then empties every tool
    /// summary.
    #[serde(default)]
    pub member: bool,
}

/// Why a history is not available.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "history/")]
pub enum HistoryUnavailable {
    NoNativeId,
    NotFound,
    Ambiguous,
    NotTrusted,
    TooLarge,
    UnsupportedFormat,
    NotCaptured,
    Busy,
}

/// The state of a history answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[ts(export_to = "history/")]
pub enum HistoryState {
    /// No record was skipped.
    Complete,
    /// The parser skipped records, or the index holds only the newest items.
    Partial {
        skipped: u32,
    },
    /// The session has no earlier part.
    Empty,
    Unavailable {
        reason: HistoryUnavailable,
    },
}

/// The kind of one history item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "history/")]
pub enum HistoryKind {
    User,
    Assistant,
    Tool,
}

/// The result of a tool call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "history/")]
pub enum ToolResult {
    Ok,
    Failed,
    Unknown,
}

/// A tool call of the history.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "history/")]
pub struct HistoryTool {
    pub name: String,
    /// The command, path or pattern; empty for a member.
    pub summary: String,
    pub status: ToolResult,
}

/// One item of the history.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "history/")]
pub struct HistoryItem {
    pub id: String,
    /// Unix ms; 0 when the transcript gives no time.
    pub at: i64,
    pub kind: HistoryKind,
    pub text: String,
    pub tool: Option<HistoryTool>,
}

/// The `session.history` answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "history/")]
pub struct SessionHistory {
    /// Oldest first.
    pub items: Vec<HistoryItem>,
    /// The cursor of the next older page; `None` at the start.
    pub before: Option<String>,
    pub state: HistoryState,
}
