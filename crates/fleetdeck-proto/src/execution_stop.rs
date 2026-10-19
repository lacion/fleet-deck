//! `session.execution_stop_plan` and `session.stop_execution` (feature
//! `execution_stop.v1`; design `docs/v2/design/board-gaps.md` §5, BG-4): the
//! stop plan for the processes of a lost run and the outcome of the stop.
//!
//! Start times, UID evidence and pidfds stay inside the station. The plan
//! lists bounded, cleaned `comm` values and never arguments.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::execution::{ExecutionCheck, ProcessInfo};
use crate::ids::SessionId;

/// The codes of a refused plan or a stop outcome, with the texts of the
/// board (design §5.5).
pub mod code {
    /// The station cannot prove that the processes belong to the run.
    pub const IDENTITY_UNPROVEN: &str = "identity_unproven";
    /// The safety check did not finish.
    pub const INSPECTION_INCOMPLETE: &str = "inspection_incomplete";
    /// The process list changed since the plan.
    pub const PLAN_CHANGED: &str = "plan_changed";
    /// A process is not owned by the station user, or a signal was refused.
    pub const PERMISSION_DENIED: &str = "permission_denied";
    /// Some processes remain after the stop.
    pub const PROCESSES_REMAIN: &str = "processes_remain";
    /// The stop was attempted and its result is not known.
    pub const OUTCOME_UNKNOWN: &str = "outcome_unknown";

    /// The text of `identity_unproven`.
    pub const IDENTITY_UNPROVEN_TEXT: &str = "Fleet Deck cannot safely prove that these processes belong to the previous run. No processes were stopped.";
    /// The text of `inspection_incomplete`.
    pub const INSPECTION_INCOMPLETE_TEXT: &str =
        "Fleet Deck could not complete the safety check. No processes were stopped.";
    /// The text of `plan_changed`.
    pub const PLAN_CHANGED_TEXT: &str =
        "The process list changed. Review it again before stopping processes.";
    /// The text of `permission_denied`.
    pub const PERMISSION_DENIED_TEXT: &str =
        "Fleet Deck could not stop one or more processes as the station user.";
    /// The text of `processes_remain`.
    pub const PROCESSES_REMAIN_TEXT: &str = "Some processes remain. Review the updated list.";
    /// The text of `outcome_unknown`.
    pub const OUTCOME_UNKNOWN_TEXT: &str =
        "The stop result is unknown. Refresh the process check before taking another action.";

    /// Every code with its text.
    pub const ALL: [(&str, &str); 6] = [
        (IDENTITY_UNPROVEN, IDENTITY_UNPROVEN_TEXT),
        (INSPECTION_INCOMPLETE, INSPECTION_INCOMPLETE_TEXT),
        (PLAN_CHANGED, PLAN_CHANGED_TEXT),
        (PERMISSION_DENIED, PERMISSION_DENIED_TEXT),
        (PROCESSES_REMAIN, PROCESSES_REMAIN_TEXT),
        (OUTCOME_UNKNOWN, OUTCOME_UNKNOWN_TEXT),
    ];

    /// The board text of `code`; `None` for a code that is not in the list.
    #[must_use]
    pub fn text(code: &str) -> Option<&'static str> {
        ALL.iter().find(|(c, _)| *c == code).map(|(_, t)| *t)
    }
}

/// `session.execution_stop_plan` params.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "execution/")]
pub struct ExecutionStopPlanQuery {
    pub session_id: SessionId,
    pub shim_id: String,
}

/// The `session.execution_stop_plan` answer. The plan lives in the station
/// memory for 30 seconds. A refused or empty plan has no targets and an
/// empty `plan_id`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "execution/")]
pub struct ExecutionStopPlan {
    /// The opaque id of the stored plan; empty when no plan is stored.
    pub plan_id: String,
    /// Unix ms; 0 when no plan is stored.
    pub expires_at: i64,
    /// True when the plan has targets and no `code`.
    pub eligible: bool,
    /// Why the plan is refused; one of [`code`].
    pub code: Option<String>,
    /// The processes that the command signals, one by one.
    pub targets: Vec<ProcessInfo>,
    /// Processes outside the run's group and session, for information only.
    /// They are never targets.
    pub escaped: Vec<ProcessInfo>,
    /// A fresh execution check.
    pub check: ExecutionCheck,
}

/// How a stop ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "execution/")]
pub enum StopOutcome {
    Done,
    Failed,
    Unknown,
}

impl StopOutcome {
    /// The stored text.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Done => "done",
            Self::Failed => "failed",
            Self::Unknown => "unknown",
        }
    }
}

/// The outcome of the newest stop of a run (`ExecutionCheck.last_stop`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "execution/")]
pub struct ExecutionStopOutcome {
    pub command_id: String,
    pub outcome: StopOutcome,
    /// One of [`code`], or `None`.
    pub code: Option<String>,
    /// Targets that got SIGTERM.
    pub term_sent: u32,
    /// Targets that got SIGKILL.
    pub kill_sent: u32,
    /// Targets that were dead at the end.
    pub gone: u32,
    /// Targets that were not signalled: they had left the domain or ended.
    pub skipped: u32,
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn every_code_has_its_text() {
        for (c, t) in code::ALL {
            assert_eq!(code::text(c), Some(t));
        }
        assert_eq!(code::text("nope"), None);
        assert_eq!(
            code::text("plan_changed"),
            Some("The process list changed. Review it again before stopping processes.")
        );
    }

    #[test]
    fn the_outcome_texts_are_the_stored_texts() {
        let text = serde_json::to_string(&StopOutcome::Unknown).unwrap();
        assert_eq!(text, "\"unknown\"");
        assert_eq!(StopOutcome::Done.as_str(), "done");
        assert_eq!(StopOutcome::Failed.as_str(), "failed");
    }

    #[test]
    fn the_plan_query_refuses_an_unknown_key() {
        let ok = serde_json::json!({"session_id": "s", "shim_id": "x"});
        assert!(serde_json::from_value::<ExecutionStopPlanQuery>(ok).is_ok());
        let bad = serde_json::json!({"session_id": "s", "shim_id": "x", "pid": 1});
        assert!(serde_json::from_value::<ExecutionStopPlanQuery>(bad).is_err());
    }
}
