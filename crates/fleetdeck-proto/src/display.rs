//! The display principal (R13, DL-201): a read-only browser login for a wall
//! display. The board's TypeScript types come from here.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::ids::StationId;

/// The desktop that a display is bound to (R13 §5.4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "display/")]
pub struct DisplayBinding {
    pub station_id: StationId,
    pub monitor: String,
}

/// One display, as the owner sees it (`GET /api/displays`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "display/")]
pub struct DisplayView {
    pub display_id: String,
    pub name: String,
    /// The stations that the display may show (station ids).
    pub ceiling: Vec<StationId>,
    pub show_diffs: bool,
    pub wake_on_needs_you: bool,
    /// B17 wall apps: the wall may run an HTML artifact as an app.
    pub allow_apps: bool,
    /// `HH:MM-HH:MM`, or absent.
    pub quiet_hours: Option<String>,
    pub binding: Option<DisplayBinding>,
    /// Unix ms.
    pub created_at: i64,
    /// Unix ms of the last use.
    pub last_seen: Option<i64>,
}

/// A pairing that waits for the owner (`GET /api/displays`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "display/")]
pub struct PendingPairing {
    pub pairing_id: String,
    pub code: String,
    /// The source key of the request (an IP address).
    pub source: String,
    pub user_agent: String,
    pub created_at: i64,
    pub expires_at: i64,
    /// The display that a managed `wall.open` launch expects (R13 §5E item
    /// 4). The approval dialog preselects it.
    #[serde(default)]
    #[ts(optional = nullable)]
    pub expected_display_id: Option<String>,
}

/// `GET /api/displays`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "display/")]
pub struct DisplayList {
    pub displays: Vec<DisplayView>,
    pub pending: Vec<PendingPairing>,
}

/// `POST /api/displays/pair`: the code that the kiosk shows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "display/")]
pub struct PairingStarted {
    pub pairing_id: String,
    pub code: String,
    pub expires_at: i64,
}

/// `GET /api/displays/pair/{id}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "display/")]
pub enum PairingState {
    Pending,
    ApprovedClaimed,
}

/// The answer of `GET /api/displays/pair/{id}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "display/")]
pub struct PairingPoll {
    pub state: PairingState,
}

/// `GET /api/displays/self`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "display/")]
pub struct DisplaySelf {
    pub display_id: String,
    pub name: String,
    pub ceiling: Vec<StationId>,
    pub binding: Option<DisplayBinding>,
}

/// The answer of `POST /api/displays/link`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "display/")]
pub struct DisplayLinkMade {
    pub url: String,
    pub expires_at: i64,
}
