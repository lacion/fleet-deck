//! The board API shapes (M2 P3-9; PLAN §9 "REST for commands, WS for
//! snapshot + deltas"). The board's TypeScript types come from here.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use ts_rs::TS;

use crate::command::{DeckCommandState, Receipt, SessionCommand};
use crate::ids::{CommandId, StationId};

/// The longest client-chosen command id.
pub const MAX_COMMAND_ID_CHARS: usize = 64;

/// `POST /api/commands/<method>`: one session command for a station. The
/// board chooses `command_id` (a ULID); a resend after a lost response
/// with the same id and command returns the stored command and sends
/// nothing new (PLAN Phase 3 exit "duplicate no commands").
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "board/")]
pub struct CommandRequest {
    /// The station name or id.
    pub station: String,
    pub command_id: CommandId,
    pub command: SessionCommand,
}

/// `true` for an id of 1 to 64 characters from `[0-9A-Za-z_-]`.
#[must_use]
pub fn valid_command_id(id: &CommandId) -> bool {
    let s = id.as_str();
    !s.is_empty()
        && s.len() <= MAX_COMMAND_ID_CHARS
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// One command that the deck routes to a station (PLAN §8.4).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export_to = "board/")]
pub struct DeckCommand {
    pub command_id: CommandId,
    pub station_id: StationId,
    pub method: String,
    /// `board`, `mail`, `coordinator`, `local` or `owner`.
    pub origin: String,
    /// The stored envelope (the request params).
    pub envelope: Value,
    pub retry_until: i64,
    pub state: DeckCommandState,
    pub attempts: u32,
    pub sent_at: Option<i64>,
    pub receipt: Option<Receipt>,
    /// Why a command is `failed` or `ambiguous`.
    pub reason: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_ids_are_short_and_plain() {
        assert!(valid_command_id(&"01J9Z3-ab_c".into()));
        assert!(!valid_command_id(&"".into()));
        assert!(!valid_command_id(&"a b".into()));
        assert!(!valid_command_id(&"x".repeat(65).as_str().into()));
        assert!(valid_command_id(&"x".repeat(64).as_str().into()));
    }
}
