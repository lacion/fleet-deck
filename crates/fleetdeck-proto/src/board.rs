//! The board API shapes (M2 P3-9; PLAN §9 "REST for commands, WS for
//! snapshot + deltas"). The board's TypeScript types come from here.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use ts_rs::TS;

use crate::command::{DeckCommandState, Receipt, SessionCommand};
use crate::event::EventEnvelope;
use crate::ids::{CommandId, IncarnationId, SessionId, StationId};
use crate::link::StationInfo;
use crate::session::{CardStatus, SessionSummary};

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

/// The board WebSocket protocol version (`hello.protocol`).
pub const BOARD_PROTOCOL: u32 = 1;
/// Timeline subscriptions per WebSocket.
pub const MAX_SUBSCRIPTIONS: usize = 32;

/// One card of the board: a session of a station with the status
/// projection of PLAN §7.2.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export_to = "board/")]
pub struct DeckSession {
    pub station_id: StationId,
    pub summary: SessionSummary,
    pub status: CardStatus,
}

/// A paired station as the board and the CLI show it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "board/")]
pub struct StationView {
    pub station_id: StationId,
    pub name: String,
    /// The short fingerprint of the station key.
    pub key: String,
    /// `deck_dials` or `station_dials`.
    pub direction: String,
    pub dial_url: Option<String>,
    /// Transport health: a link to the station is up (§7.2).
    pub reachable: bool,
    /// Unix ms of the last link.
    pub last_seen: Option<i64>,
    /// The last `station_seq` folded into the read model (freshness).
    pub cursor: u64,
    /// The last `station.info` answer of this deck process: version,
    /// capacity and provider instances (M2 P4-2). `None` until a link of
    /// this deck process asked; it stays when the link drops (last known).
    pub info: Option<StationInfo>,
    /// Unix ms of [`Self::info`].
    pub info_at: Option<i64>,
}

/// `POST /api/stations/invite`: an invitation for a station that dials the
/// deck (`fleetdeck station join`). The text is a secret: it is shown once
/// and never stored or audited.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "board/")]
pub struct PairingInvitation {
    pub invitation: String,
    /// Unix ms.
    pub expires_at: i64,
    /// The command to run on the station (it reads the invitation from a
    /// prompt or stdin, G12).
    pub join_command: String,
}

/// `POST /api/stations`: pair with a listening station (`fleetdeck station
/// invite` printed `invitation`) and dial it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "board/")]
pub struct AddStationRequest {
    pub invitation: String,
    /// The station name; without it the deck uses the station's own name.
    pub name: Option<String>,
}

/// A session of a station.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, TS)]
#[ts(export_to = "board/")]
pub struct SessionKey {
    pub station_id: StationId,
    pub session_id: SessionId,
}

/// A message of the deck on the board WebSocket (`GET /api/ws`, DL-056).
///
/// 1. `hello`, then one `snapshot` of every station and card.
/// 2. A `delta` after each change of the read model or of a station: the
///    changed cards, the removed cards, and the station list when it
///    changed. `version` grows by at least one per message; a client that
///    reconnects gets a new `snapshot`.
/// 3. For each `subscribe`: `timeline_start`, then `timeline_events` with
///    the stored timeline after the cursor and every new event (also the
///    streaming deltas) as it is folded.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
#[ts(export_to = "board/")]
pub enum BoardServerMessage {
    Hello {
        protocol: u32,
        deck_incarnation: IncarnationId,
    },
    Snapshot {
        version: u64,
        stations: Vec<StationView>,
        sessions: Vec<DeckSession>,
    },
    Delta {
        version: u64,
        /// The complete station list, when it changed.
        stations: Option<Vec<StationView>>,
        sessions: Vec<DeckSession>,
        removed: Vec<SessionKey>,
    },
    /// The start of a timeline subscription. `reset`: the client's cursor
    /// does not apply (another station incarnation, or none given); drop
    /// the cached timeline of this session. Sent again with `reset` when
    /// the station's incarnation changes.
    TimelineStart {
        station_id: StationId,
        session_id: SessionId,
        incarnation: Option<IncarnationId>,
        reset: bool,
    },
    TimelineEvents {
        station_id: StationId,
        session_id: SessionId,
        events: Vec<EventEnvelope>,
    },
    /// A refused client message; the socket stays open.
    Error { code: String, message: String },
}

/// A message of the board on the WebSocket.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
#[ts(export_to = "board/")]
pub enum BoardClientMessage {
    /// Follow the timeline of a session. `incarnation` and `after` are the
    /// client's cursor (the last `station_seq` it holds); the deck resends
    /// from there when the incarnation still holds.
    Subscribe {
        station_id: StationId,
        session_id: SessionId,
        incarnation: Option<IncarnationId>,
        after: Option<u64>,
    },
    Unsubscribe {
        station_id: StationId,
        session_id: SessionId,
    },
}

/// `POST /api/ws/ticket`: a single-use ticket for `GET /api/ws?ticket=`
/// (bearer clients cannot set headers on a browser WebSocket; L15).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "board/")]
pub struct WsTicket {
    pub ticket: String,
    pub expires_at: i64,
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn command_ids_are_short_and_plain() {
        assert!(valid_command_id(&"01J9Z3-ab_c".into()));
        assert!(!valid_command_id(&"".into()));
        assert!(!valid_command_id(&"a b".into()));
        assert!(!valid_command_id(&"x".repeat(65).as_str().into()));
        assert!(valid_command_id(&"x".repeat(64).as_str().into()));
    }

    #[test]
    fn client_messages_are_strict() {
        let m: BoardClientMessage = serde_json::from_str(
            r#"{"type":"subscribe","station_id":"a","session_id":"s","incarnation":null,"after":3}"#,
        )
        .unwrap();
        assert_eq!(
            m,
            BoardClientMessage::Subscribe {
                station_id: "a".into(),
                session_id: "s".into(),
                incarnation: None,
                after: Some(3)
            }
        );
        assert!(
            serde_json::from_str::<BoardClientMessage>(
                r#"{"type":"unsubscribe","station_id":"a","session_id":"s","x":1}"#
            )
            .is_err()
        );
    }
}
