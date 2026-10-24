//! The shared fleet (R11, DL-154, DL-166): shares, roles, invites, spaces,
//! the side chat and the mob-mode driver. The board's TypeScript types come
//! from here.
//!
//! A share gives other people a part of the deck:
//!
//! - `session`: one session (`target` is `<station_id>/<session_id>`);
//! - `space`: a named set of sessions (`target` is the space id);
//! - `board`: every session (`target` is empty).
//!
//! The role of a share is `viewer`, `collaborator` or `approver`. A member
//! of a share never gets more than its role.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::board::SessionKey;
use crate::ids::SessionId;

/// The default lifetime of a share in days.
pub const DEFAULT_SHARE_DAYS: u32 = 7;
/// The longest lifetime of a share in days.
pub const MAX_SHARE_DAYS: u32 = 90;
/// The default number of uses of an invite.
pub const DEFAULT_INVITE_USES: u32 = 1;
/// The most uses of an invite.
pub const MAX_INVITE_USES: u32 = 100;
/// The longest display name of a member, in characters.
pub const MAX_DISPLAY_NAME_CHARS: usize = 40;
/// The longest name of a space, in characters.
pub const MAX_SPACE_NAME_CHARS: usize = 80;
/// The longest side chat message, in characters.
pub const MAX_CHAT_CHARS: usize = 2000;
/// The side chat messages that the deck keeps for each session.
pub const CHAT_KEEP: u32 = 500;
/// The side chat messages that one list answer holds at most.
pub const CHAT_PAGE: u32 = 200;
/// A viewer sends a presence heartbeat at this interval (seconds).
pub const PRESENCE_HEARTBEAT_S: u64 = 20;
/// A viewer without a heartbeat for this long is gone (milliseconds).
pub const PRESENCE_GONE_MS: i64 = 60_000;

/// What a share gives.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "share/")]
pub enum ShareKind {
    Session,
    Space,
    Board,
}

impl ShareKind {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Session => "session",
            Self::Space => "space",
            Self::Board => "board",
        }
    }

    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "session" => Self::Session,
            "space" => Self::Space,
            "board" => Self::Board,
            _ => return None,
        })
    }
}

/// What a member of a share may do. Each role holds the rights of the
/// roles before it.
///
/// - `viewer`: read the sessions, follow them live, the side chat.
/// - `collaborator`: also prompt, steer, follow up, interrupt, answer the
///   agent's questions and send mail to the shared sessions.
/// - `approver`: also answer tool approvals.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "share/")]
pub enum ShareRole {
    Viewer,
    Collaborator,
    Approver,
}

impl ShareRole {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Viewer => "viewer",
            Self::Collaborator => "collaborator",
            Self::Approver => "approver",
        }
    }

    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "viewer" => Self::Viewer,
            "collaborator" => Self::Collaborator,
            "approver" => Self::Approver,
            _ => return None,
        })
    }
}

/// How the members of a share steer a session.
///
/// - `open`: every collaborator steers; the messages queue in order.
/// - `mob`: one driver at a time. The others ask to drive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "share/")]
pub enum Steering {
    Open,
    Mob,
}

impl Steering {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Mob => "mob",
        }
    }

    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "open" => Self::Open,
            "mob" => Self::Mob,
            _ => return None,
        })
    }
}

/// `POST /api/shares` (owner only).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "share/")]
pub struct NewShare {
    pub kind: ShareKind,
    /// `<station_id>/<session_id>` for a session, the space id for a space,
    /// empty for the board.
    #[serde(default)]
    pub target: String,
    pub role: ShareRole,
    /// `open` when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub steering: Option<Steering>,
    /// 1 to [`MAX_SHARE_DAYS`]; [`DEFAULT_SHARE_DAYS`] when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub expires_in_days: Option<u32>,
    /// 1 to [`MAX_INVITE_USES`]; [`DEFAULT_INVITE_USES`] when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub uses: Option<u32>,
    /// Members can read the earlier history of imported sessions (DL-189). `false` when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub history: Option<bool>,
    /// The owner's elevation; needed when `history` is true.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub elevation: Option<String>,
}

/// The answer of `POST /api/shares`. The invite URL holds a secret token: the
/// deck shows it once and never stores or audits it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "share/")]
pub struct ShareCreated {
    pub share_id: String,
    /// `https://<board origin>/#join=<token>`.
    pub url: String,
    pub expires_at: i64,
}

/// One share as `GET /api/shares` lists it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "share/")]
pub struct ShareInfo {
    pub share_id: String,
    pub kind: ShareKind,
    pub target: String,
    pub role: ShareRole,
    pub steering: Steering,
    /// Members can read the earlier history of imported sessions (DL-189).
    pub history: bool,
    pub expires_at: i64,
    pub revoked_at: Option<i64>,
    pub created_at: i64,
    /// The uses of the invite that are left (0 after revoke or expiry).
    pub uses_left: u32,
    /// The display names of the members that joined.
    pub members: Vec<String>,
}

/// `POST /api/shares/{share_id}/history` (owner only, DL-189).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "share/")]
pub struct ShareHistory {
    pub enabled: bool,
    /// The owner's elevation; needed to turn the history on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub elevation: Option<String>,
}

/// `POST /api/spaces` (owner only).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "share/")]
pub struct NewSpace {
    /// 1 to [`MAX_SPACE_NAME_CHARS`] printable characters.
    pub name: String,
}

/// A space: a named set of sessions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "share/")]
pub struct SpaceView {
    pub space_id: String,
    pub name: String,
    pub sessions: Vec<SessionKey>,
}

/// `POST /api/spaces/{space_id}/sessions` (owner only).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "share/")]
pub struct SpaceSession {
    /// The station name or id.
    pub station: String,
    pub session_id: SessionId,
}

/// `POST /api/auth/join`: the invite token of `#join=<token>` and the
/// display name that the person picks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "share/")]
pub struct JoinRequest {
    pub token: String,
    /// 1 to [`MAX_DISPLAY_NAME_CHARS`] printable characters.
    pub display_name: String,
}

/// What a member sees: the scope of its share.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "share/")]
pub struct SharedScope {
    pub kind: ShareKind,
    pub target: String,
    /// The name of the space, for a space share.
    pub space_name: Option<String>,
    pub steering: Steering,
    /// The sessions that the share covers now (empty for the board: every
    /// session).
    pub sessions: Vec<SessionKey>,
}

/// One message of the side chat of a session. The agent never reads it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "share/")]
pub struct ChatMessage {
    /// Grows by one for each message of the session.
    pub seq: u64,
    /// `owner`, `member:<name>` or `token:<name>`.
    pub author: String,
    pub text: String,
    pub at: i64,
}

/// `POST /api/sessions/{station}/{session}/chat`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "share/")]
pub struct ChatPost {
    /// 1 to [`MAX_CHAT_CHARS`] characters.
    pub text: String,
}

/// `POST /api/sessions/{station}/{session}/drive/grant`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "share/")]
pub struct DriveGrant {
    /// The actor that becomes the driver: `owner` or `member:<name>`.
    pub to: String,
}

/// Who drives a session in mob mode, and who asks to drive. The answer of
/// the drive routes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "share/")]
pub struct DriverState {
    pub driver: Option<String>,
    pub since: Option<i64>,
    /// The actors that ask to drive, oldest first.
    pub requests: Vec<String>,
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn roles_are_ordered_and_names_round_trip() {
        assert!(ShareRole::Viewer < ShareRole::Collaborator);
        assert!(ShareRole::Collaborator < ShareRole::Approver);
        for role in [
            ShareRole::Viewer,
            ShareRole::Collaborator,
            ShareRole::Approver,
        ] {
            assert_eq!(ShareRole::parse(role.as_str()), Some(role));
        }
        for kind in [ShareKind::Session, ShareKind::Space, ShareKind::Board] {
            assert_eq!(ShareKind::parse(kind.as_str()), Some(kind));
        }
        for s in [Steering::Open, Steering::Mob] {
            assert_eq!(Steering::parse(s.as_str()), Some(s));
        }
        assert_eq!(ShareRole::parse("owner"), None);
    }

    #[test]
    fn a_new_share_takes_defaults_and_refuses_unknown_fields() {
        let s: NewShare =
            serde_json::from_str(r#"{"kind":"session","target":"st/s1","role":"collaborator"}"#)
                .unwrap();
        assert_eq!(s.steering, None);
        assert_eq!(s.uses, None);
        assert!(
            serde_json::from_str::<NewShare>(r#"{"kind":"board","role":"viewer","x":1}"#).is_err()
        );
        let b: NewShare = serde_json::from_str(r#"{"kind":"board","role":"viewer"}"#).unwrap();
        assert_eq!(b.target, "");
    }
}
