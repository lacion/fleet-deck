//! The coordinator grant of a start command (ADR-006 §4, ADR-008 §3). Moved
//! from `command.rs` for the module size rule (R13 P6 F0); `command.rs`
//! re-exports every item.

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
    /// The target stations that the owner picked (ADR-006 §"Creation"),
    /// besides the coordinator's own station. Board → deck only: the deck
    /// reads it in `owner_start` and clears it before the start command goes
    /// to a station, so a station never sees it (both coordinator.v1 and the
    /// current v2 decoders refuse unknown fields).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[ts(optional, as = "Option<Vec<StationId>>")]
    pub stations: Vec<StationId>,
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
