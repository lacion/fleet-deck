//! The plan library of the board (M3 6-6, PLAN Phase 6, §16 "Plan library:
//! keep; capture from the protocol", DL-301).
//!
//! The deck keeps every `plan.proposed` event that it folds (Claude
//! `ExitPlanMode`, Codex `plan` items) as one library row. The row stays
//! when the timeline is capped or a station gets a new incarnation: the
//! deck is the source of truth for fleet intent (A1).

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::ids::{PlanId, SessionId, StationId, TurnId};

/// The longest plan text that the deck keeps; a longer plan is cut at a
/// character boundary and marked `truncated`.
pub const MAX_PLAN_BYTES: usize = 256 * 1024;

/// The owner's state of a library plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "plans/")]
pub enum PlanStatus {
    /// Captured from the protocol; nobody acted on it yet.
    Proposed,
    /// The owner marked it as carried out.
    Executed,
    /// Hidden from the default list.
    Archived,
}

impl PlanStatus {
    /// The database and wire name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Proposed => "proposed",
            Self::Executed => "executed",
            Self::Archived => "archived",
        }
    }

    /// The status of a database name.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "proposed" => Some(Self::Proposed),
            "executed" => Some(Self::Executed),
            "archived" => Some(Self::Archived),
            _ => None,
        }
    }
}

/// One plan of the library (`GET /api/plans`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "plans/")]
pub struct DeckPlan {
    /// The deck's library id (`station/session/plan`, stable).
    pub id: String,
    pub station_id: StationId,
    pub session_id: SessionId,
    pub plan_id: PlanId,
    pub turn_id: Option<TurnId>,
    /// The session title when the plan was captured (`None`: untitled).
    pub session_title: Option<String>,
    pub markdown: String,
    /// The text was longer than [`MAX_PLAN_BYTES`].
    pub truncated: bool,
    pub status: PlanStatus,
    /// The event time (Unix ms).
    pub proposed_at: i64,
    pub updated_at: i64,
}

/// `POST /api/plans/mark`: sets the owner's state of one plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "plans/")]
pub struct PlanMarkRequest {
    pub id: String,
    pub status: PlanStatus,
}

/// The library id of a plan.
#[must_use]
pub fn plan_key(station: &StationId, session: &SessionId, plan: &PlanId) -> String {
    format!(
        "{}/{}/{}",
        station.as_str(),
        session.as_str(),
        plan.as_str()
    )
}

/// Cuts `text` to at most [`MAX_PLAN_BYTES`] at a character boundary.
#[must_use]
pub fn bounded_plan(text: &str) -> (String, bool) {
    if text.len() <= MAX_PLAN_BYTES {
        return (text.to_owned(), false);
    }
    let mut end = MAX_PLAN_BYTES;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    (text[..end].to_owned(), true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_long_plan_is_cut_at_a_character_boundary() {
        let text = "é".repeat(MAX_PLAN_BYTES);
        let (cut, truncated) = bounded_plan(&text);
        assert!(truncated);
        assert!(cut.len() <= MAX_PLAN_BYTES);
        assert!(cut.chars().all(|c| c == 'é'));
        assert_eq!(bounded_plan("# plan"), ("# plan".to_owned(), false));
    }

    #[test]
    fn statuses_round_trip() {
        for s in [
            PlanStatus::Proposed,
            PlanStatus::Executed,
            PlanStatus::Archived,
        ] {
            assert_eq!(PlanStatus::parse(s.as_str()), Some(s));
            assert_eq!(
                serde_json::to_value(s).unwrap_or_default(),
                serde_json::Value::String(s.as_str().to_owned())
            );
        }
        assert_eq!(PlanStatus::parse("done"), None);
    }
}
