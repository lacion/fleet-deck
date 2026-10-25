//! The coordinator grant of a start command (ADR-006 §4, ADR-008 §3). Moved
//! from `command.rs` for the module size rule (R13 P6 F0); `command.rs`
//! re-exports every item.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::ids::StationId;

/// The coordinator grant that a session starts with (ADR-006 §4). It holds
/// no credential: the station mints the credential in the start effect.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "command/")]
#[serde(deny_unknown_fields)]
pub struct CoordinatorSpec {
    pub grant_id: String,
    /// Grant classes (`observe`, `message`, `spawn`, ...).
    pub classes: Vec<String>,
    #[ts(type = "number")]
    pub idle_ttl_ms: u64,
    #[ts(type = "number")]
    pub expires_at: i64,
    pub control_skill: ControlSkill,
    /// The reach of the grant (ADR-008 §3). Skipped when `lineage`, so a
    /// lineage grant serializes as under `coordinator.v1`.
    #[serde(default, skip_serializing_if = "GrantScope::is_lineage")]
    #[ts(optional, as = "Option<GrantScope>")]
    pub scope: GrantScope,
    /// The answer budget of a fleet-control grant (ADR-008 §3).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub answer_cap: Option<u32>,
    /// The desktop action budget of a grant with `desktop` or `capture`
    /// (R13 P6 §8.1). Board → deck only, like `stations`: the deck reads it
    /// in `owner_start` and clears it before the start command goes to a
    /// station.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub desktop_cap: Option<u32>,
    /// The screenshot budget of a grant with `capture` (R13 P6 §8.1).
    /// Board → deck only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub capture_cap: Option<u32>,
    /// The image-read byte budget of a grant with `capture` (R13 P6 §8.1).
    /// Board → deck only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub image_byte_cap: Option<u64>,
    /// R13 P5b-2 §13.2: the explicit opt-in to monitor recordings. Board →
    /// deck only: the deck reads it in `owner_start` and clears it before
    /// the start command goes to a station.
    #[serde(default, skip_serializing_if = "is_false")]
    #[ts(optional, as = "Option<bool>")]
    pub allow_recording: bool,
    /// The recording count budget of a grant with `allow_recording`. Board
    /// → deck only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub recording_cap: Option<u32>,
    /// The recording seconds budget of a grant with `allow_recording`.
    /// Board → deck only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub recording_second_cap: Option<u32>,
    /// R13 P7 §3.2: the explicit opt-in to keyboard input and pointer
    /// movement in the caller's own previews. Sent to a caller link that
    /// negotiated `desktop_input_mcp.v1` only; absent otherwise.
    #[serde(default, skip_serializing_if = "is_false")]
    #[ts(optional, as = "Option<bool>")]
    pub allow_input: bool,
    /// R13 P7 §3.2: the scope of the input allowance; with `allow_input` it
    /// is `own_windows`. Sent with `allow_input` only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub input_scope: Option<InputScope>,
    /// R13 P7b §3.1: the explicit opt-in to one left pointer click at a time.
    /// Needs `allow_input`. Sent to a caller link that negotiated
    /// `desktop_click_mcp.v1` only; absent otherwise.
    #[serde(default, skip_serializing_if = "is_false")]
    #[ts(optional, as = "Option<bool>")]
    pub allow_click: bool,
    /// The input action budget of a grant with `allow_input` (R13 P7 §3.2).
    /// Board → deck only: the deck reads it in `owner_start` and removes it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub input_cap: Option<u32>,
    /// The typed-character budget of a grant with `allow_input` (R13 P7
    /// §3.2). Board → deck only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub input_character_cap: Option<u32>,
    /// The target stations that the owner picked (ADR-006 §"Creation"),
    /// besides the coordinator's own station. Board → deck only: the deck
    /// reads it in `owner_start` and clears it before the start command goes
    /// to a station, so a station never sees it (both coordinator.v1 and the
    /// current v2 decoders refuse unknown fields).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[ts(optional, as = "Option<Vec<StationId>>")]
    pub stations: Vec<StationId>,
    /// The spawn budget of a grant with the class `spawn` (ADR-006 §6.4,
    /// MCP P0 §2.1). Board → deck only: the deck reads it in `owner_start`
    /// and clears it before the start command goes to a station.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub spawn_cap: Option<u32>,
    /// The most permissive native mode for each provider family (ADR-006 §4
    /// `max_modes`; keys `claude` and `codex`). Board → deck only.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    #[ts(optional, as = "Option<BTreeMap<String, String>>")]
    pub max_modes: BTreeMap<String, String>,
    /// Set by the deck only (MCP P4a §4): `worker` for a default worker
    /// grant; never sent by the board.
    #[serde(default, skip_serializing_if = "GrantKind::is_coordinator")]
    #[ts(optional, as = "Option<GrantKind>")]
    pub kind: GrantKind,
}

/// The serde skip rule of `allow_recording`.
#[allow(clippy::trivially_copy_pass_by_ref)]
const fn is_false(value: &bool) -> bool {
    !*value
}

/// The scope of the input allowance of a grant (R13 P7 §3.1, §3.2; R13 P7b
/// §3.1). `any` and every other name is refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "command/")]
pub enum InputScope {
    /// The preview windows that this caller opened.
    OwnWindows,
    /// R13 P7b §3.1: the preview windows of this caller and the windows that
    /// `window_list` issued a handle for (feature `desktop_input_any.v1`).
    AnyWindow,
}

/// The kind of a coordinator grant (MCP P4a §4). The deck owns it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "command/")]
pub enum GrantKind {
    /// A grant that the owner created, or any grant before MCP P4a.
    #[default]
    Coordinator,
    /// A default worker grant that the deck attached to a start (MCP P4a).
    Worker,
}

impl GrantKind {
    /// True for the default kind; the serde skip rule of `kind`.
    #[must_use]
    pub const fn is_coordinator(&self) -> bool {
        matches!(self, Self::Coordinator)
    }
}

/// The reach of a coordinator grant (ADR-008 §2, §3).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "command/")]
pub enum GrantScope {
    /// The ADR-006 drive rule: the target has this grant in its lineage.
    #[default]
    Lineage,
    /// Any session on a station of the grant, minus the ADR-008 §4.2
    /// exclusions.
    Fleet,
}

impl GrantScope {
    /// True for the default scope; the serde skip rule of `scope`.
    #[must_use]
    pub fn is_lineage(&self) -> bool {
        matches!(self, Self::Lineage)
    }
}

/// The versioned control skill that the coordinator MCP endpoint serves as
/// its `instructions` (PLAN §7.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "command/")]
#[serde(deny_unknown_fields)]
pub struct ControlSkill {
    pub version: String,
    pub text: String,
}
