//! The board API shapes (M2 P3-9; PLAN §9 "REST for commands, WS for
//! snapshot + deltas"). The board's TypeScript types come from here.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use ts_rs::TS;

use crate::command::{DeckCommandState, Receipt, SessionCommand};
use crate::event::{EventEnvelope, RequestKind};
use crate::ids::{CommandId, IncarnationId, SessionId, StationId};
use crate::link::StationInfo;
use crate::session::{CardStatus, SessionSummary};
use crate::share::ChatMessage;

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
    /// The owner's elevation code (DL-172). A coordinator start with a
    /// fleet-control grant needs a fresh one (ADR-008 §3); every other
    /// command ignores it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub elevation: Option<String>,
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
    /// Who sent the command: `owner`, `member:<name>` or `token:<name>`
    /// (R11.5, DL-166). For an approval it is who answered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub author: Option<String>,
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
    /// The events after the caller's read cursor that are agent output or
    /// need the caller (R6.1, DL-169). The deck fills it per caller; it is
    /// absent when zero.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub unread: Option<u32>,
    /// The project of the session (DL-176): the owner's choice, else the
    /// match by folder, repository or forge. Absent when there is none. The
    /// deck fills it for the owner and for tokens, never for a member.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub project_id: Option<String>,
    /// The review counter of the session (DL-179): it grows with every
    /// comment change, send and delivery change, so every open board fetches
    /// the comments again. Absent when zero.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub review_rev: Option<u32>,
}

/// `POST /api/sessions/{station}/{session}/read` (R6.1, DL-169): the board
/// shows the timeline of a session up to `seq` (a `session_seq`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "board/")]
pub struct ReadMark {
    pub seq: u64,
}

/// The kind of an [`ActivityItem`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "board/")]
pub enum ActivityKind {
    /// A completed agent message.
    Message,
    /// A completed tool call.
    Tool,
    /// An approval or a question: requested, resolved or expired.
    Approval,
    /// Mail to the session.
    Mail,
    /// A session started, ended, was hibernated or was named.
    Lifecycle,
}

/// One row of the activity stream (R6.5, DL-169).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "board/")]
pub struct ActivityItem {
    pub station_id: StationId,
    pub session_id: SessionId,
    /// The title of the session, when it has one.
    pub session_title: Option<String>,
    /// The repository of the session: its origin, else its root.
    pub repo: Option<String>,
    pub kind: ActivityKind,
    /// The kind of the request of a "Needs you" row (`approval.requested`); absent on every
    /// other row and from an older deck.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub request_kind: Option<RequestKind>,
    /// A one-line text, at most [`ACTIVITY_TEXT_CHARS`] characters.
    pub text: String,
    /// Unix ms.
    pub at: i64,
    /// The `station_seq` of a timeline row; absent for mail.
    pub station_seq: Option<u64>,
}

/// The longest [`ActivityItem::text`] in characters.
pub const ACTIVITY_TEXT_CHARS: usize = 200;
/// The default and the largest page of `GET /api/activity`.
pub const ACTIVITY_DEFAULT_LIMIT: u32 = 100;
pub const ACTIVITY_MAX_LIMIT: u32 = 500;

/// `GET /api/activity?since=&repo=&session=&limit=` (R6.5): the newest rows
/// first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "board/")]
pub struct ActivityList {
    pub items: Vec<ActivityItem>,
}

/// `GET /api/fleet/extras` (DL-141): what the cards show besides the summary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "board/")]
pub struct FleetExtras {
    pub sessions: Vec<SessionExtras>,
}

/// The extras of one session card.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "board/")]
pub struct SessionExtras {
    pub station_id: StationId,
    pub session_id: SessionId,
    /// Mails to the session that are not delivered and not dead yet.
    pub mail_pending: u32,
    /// The newest distinct files that the session touched (at most 4, newest first),
    /// from the conflict radar's touches inside its window.
    pub files: Vec<String>,
}

/// The most files per session in [`SessionExtras::files`].
pub const MAX_EXTRAS_FILES: usize = 4;

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
    /// The reduced resources that a wall display gets in place of `info`
    /// (R13 §5D item 10). Always `None` for an owner or a member.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub display_resources: Option<crate::display_event::DisplayResources>,
    /// MCP P4b §9: the station's current link negotiated `coordinator_live.v1`,
    /// so a grant can be given and changed live. Absent in an old answer:
    /// false.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    #[ts(optional, as = "Option<bool>")]
    pub coordinator_live: bool,
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
    /// The timeline events of a session for a wall display (R13 §5D item 7).
    /// `high_water` is the highest `station_seq` that the deck examined,
    /// also when it dropped every event: the client advances its cursor to
    /// it, so a suppressed event is never replayed.
    DisplayTimelineEvents {
        station_id: StationId,
        session_id: SessionId,
        events: Vec<crate::display_event::DisplayEvent>,
        high_water: u64,
    },
    /// Who has a session open now (R11.5, DL-166): the actors (`owner`,
    /// `member:<name>`) with a heartbeat in the last 60 s. Sent when the
    /// set changes, for the sessions that the socket may see.
    Presence {
        station_id: StationId,
        session_id: SessionId,
        who: Vec<String>,
    },
    /// A new message of the side chat of a session (people only; the agent
    /// never reads it).
    Chat {
        station_id: StationId,
        session_id: SessionId,
        message: ChatMessage,
    },
    /// The driver of a session in mob mode changed (`None`: nobody drives).
    Driver {
        station_id: StationId,
        session_id: SessionId,
        driver: Option<String>,
    },
    /// The people who ask to drive a session in mob mode, oldest first. The
    /// full list: an empty list clears every request.
    DriveRequest {
        station_id: StationId,
        session_id: SessionId,
        requests: Vec<String>,
    },
    /// A wall frame for a display socket that declared `wall_control.v1`
    /// (B17 §3.1). No other socket gets it.
    WallControl {
        frame: crate::wall_control::WallControlFrame,
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
    /// The heartbeat of a viewer that has `session_id` open: send it every
    /// 20 s. Without one for 60 s the viewer is gone (R11.5).
    Presence {
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

/// One login link of a [`DeviceLink`]: the same single-use token on one
/// board address.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "board/")]
pub struct DeviceLinkEntry {
    /// For example `Tailnet deck.tail1234.ts.net`, `Network 192.168.1.5` or
    /// `This computer`.
    pub label: String,
    /// `<board origin>/#login=<token>`.
    pub url: String,
    /// The SHA-256 of the certificate that this address presents now.
    pub certificate_sha256: String,
    /// How far a browser trusts that certificate.
    pub trust: CertTrust,
}

/// How far a browser trusts the certificate of one board address.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "board/")]
pub enum CertTrust {
    /// The certificate chains to a public root for this address.
    Trusted,
    /// The deck's own self-signed certificate: the browser warns.
    SelfSigned,
    /// An owner certificate that the deck cannot check.
    Unknown,
}

/// `POST /api/auth/device-link` (DL-146): a one-time login link for a new
/// device. Every entry of `links` carries the same single-use token.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "board/")]
pub struct DeviceLink {
    pub links: Vec<DeviceLinkEntry>,
    pub expires_at: i64,
    /// The SHA-256 of the fallback board certificate (the one for addresses
    /// that get no other), for the browser check.
    pub certificate_sha256: String,
    /// The fallback certificate is the deck's self-signed one.
    pub self_signed: bool,
}

/// One archived (cleared) session of the board (DL-149).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "board/")]
pub struct ArchiveRef {
    pub station_id: StationId,
    pub session_id: SessionId,
}

/// `GET /api/archive` and the answer of `POST /api/archive` (DL-149): the
/// sessions that the owner archived, ordered by station and session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "board/")]
pub struct ArchiveList {
    pub archived: Vec<ArchiveRef>,
}

/// One change of an [`ArchiveChange`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "board/")]
pub struct ArchiveItem {
    pub station_id: StationId,
    pub session_id: SessionId,
    /// `true` archives the session, `false` brings it back.
    pub archived: bool,
}

/// `POST /api/archive` (DL-149): changes of the archived set. A resend
/// with the same `mutation_id` changes nothing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "board/")]
pub struct ArchiveChange {
    /// 8 to 128 characters from `[A-Za-z0-9_.:-]`.
    pub mutation_id: String,
    /// 1 to 500 items.
    pub changes: Vec<ArchiveItem>,
}

/// The shortest and longest `mutation_id` of an [`ArchiveChange`].
pub const ARCHIVE_MUTATION_ID_CHARS: std::ops::RangeInclusive<usize> = 8..=128;
/// The number of items of an [`ArchiveChange`].
pub const ARCHIVE_CHANGES: std::ops::RangeInclusive<usize> = 1..=500;
/// The length of a station id or a session id of an [`ArchiveItem`].
pub const ARCHIVE_ID_CHARS: std::ops::RangeInclusive<usize> = 1..=256;

impl ArchiveChange {
    /// Checks the lengths and the characters of the change.
    ///
    /// # Errors
    /// The text of the first rule that the change breaks.
    pub fn validate(&self) -> Result<(), &'static str> {
        let id = &self.mutation_id;
        if !ARCHIVE_MUTATION_ID_CHARS.contains(&id.len())
            || !id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b':' | b'-'))
        {
            return Err("mutation_id must be 8 to 128 characters from [A-Za-z0-9_.:-]");
        }
        if !ARCHIVE_CHANGES.contains(&self.changes.len()) {
            return Err("changes must hold 1 to 500 items");
        }
        let ok = |s: &str| ARCHIVE_ID_CHARS.contains(&s.chars().count());
        if !self
            .changes
            .iter()
            .all(|c| ok(c.station_id.as_str()) && ok(c.session_id.as_str()))
        {
            return Err("station_id and session_id must be 1 to 256 characters");
        }
        Ok(())
    }
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

    fn item(station: &str, session: &str) -> ArchiveItem {
        ArchiveItem {
            station_id: station.into(),
            session_id: session.into(),
            archived: true,
        }
    }

    #[test]
    fn archive_changes_are_checked() {
        let ok = ArchiveChange {
            mutation_id: "migrate-a.b:c_d".to_owned(),
            changes: vec![item("st", "s1")],
        };
        assert_eq!(ok.validate(), Ok(()));
        let mut c = ok.clone();
        c.mutation_id = "short".to_owned();
        assert!(c.validate().is_err());
        c.mutation_id = "x".repeat(129);
        assert!(c.validate().is_err());
        c.mutation_id = "has space 123".to_owned();
        assert!(c.validate().is_err());
        let mut c = ok.clone();
        c.changes = vec![];
        assert!(c.validate().is_err());
        c.changes = vec![item("st", "s1"); 501];
        assert!(c.validate().is_err());
        c.changes = vec![item("st", "s1"); 500];
        assert_eq!(c.validate(), Ok(()));
        let mut c = ok.clone();
        c.changes = vec![item("", "s1")];
        assert!(c.validate().is_err());
        c.changes = vec![item("st", &"s".repeat(257))];
        assert!(c.validate().is_err());
        c.changes = vec![item("st", &"s".repeat(256))];
        assert_eq!(c.validate(), Ok(()));
    }

    #[test]
    fn an_activity_item_without_request_kind_round_trips() {
        let item = ActivityItem {
            station_id: "st".into(),
            session_id: "s1".into(),
            session_title: None,
            repo: None,
            kind: ActivityKind::Message,
            request_kind: None,
            text: "Fixed it".to_owned(),
            at: 1,
            station_seq: Some(1),
        };
        let json = serde_json::to_value(&item).unwrap();
        assert!(json.as_object().unwrap().get("request_kind").is_none());
        let back: ActivityItem = serde_json::from_value(json).unwrap();
        assert_eq!(back.request_kind, None);
        assert_eq!(back, item);
        let asked = ActivityItem {
            kind: ActivityKind::Approval,
            request_kind: Some(RequestKind::Question),
            ..item
        };
        let json = serde_json::to_value(&asked).unwrap();
        assert_eq!(json["request_kind"], "question");
        assert_eq!(serde_json::from_value::<ActivityItem>(json).unwrap(), asked);
    }

    #[test]
    fn archive_bodies_are_strict() {
        assert!(
            serde_json::from_str::<ArchiveChange>(
                r#"{"mutation_id":"abcdefgh","changes":[],"x":1}"#
            )
            .is_err()
        );
        assert!(
            serde_json::from_str::<ArchiveItem>(
                r#"{"station_id":"a","session_id":"s","archived":true,"x":1}"#
            )
            .is_err()
        );
    }
}
