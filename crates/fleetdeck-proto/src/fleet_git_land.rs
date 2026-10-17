//! Wire types of the conflict radar and the land flow (design §10, §11,
//! ADR-009 §4). Phase 3 (FG-7, FG-8). Everything here is additive (O5).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::{LandMode, RemoteIdentity};
use crate::integrations::ForgeBinding;

/// The key of the default branch in a radar pair (design §10.2).
pub const RADAR_DEFAULT_KEY: &str = "@default";
/// The most branches that one `project.radar` call carries (design §10.2).
pub const RADAR_MAX_BRANCHES: usize = 50;

/// One pushed branch that the radar compares (design §10.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "fleet_git/")]
pub struct RadarBranch {
    /// The key that a pair names: the session id.
    pub key: String,
    pub ref_name: String,
}

/// Params of `project.radar` (design §10.2). At most [`RADAR_MAX_BRANCHES`]
/// branches, newest session first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "fleet_git/")]
pub struct RadarParams {
    pub project_id: String,
    pub branches: Vec<RadarBranch>,
}

/// The result for one pair of branches. `b` is [`RADAR_DEFAULT_KEY`] for the
/// default branch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "fleet_git/")]
pub struct RadarPair {
    pub a: String,
    pub b: String,
    pub conflict: bool,
    pub files: Vec<String>,
    pub files_truncated: bool,
}

/// The answer of `project.radar` (design §10.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "fleet_git/")]
pub struct RadarResult {
    pub checked_at: i64,
    /// The default branch tip that was used; none when the fetch failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub base_oid: Option<String>,
    pub pairs: Vec<RadarPair>,
    /// Pairs that were not run (the limit or the 120 s stop).
    pub skipped: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub text: Option<String>,
}

/// The state of a land (design §11.5). `Queued` and `Cancelled` exist only on
/// the deck.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "fleet_git/")]
pub enum LandState {
    Queued,
    Running,
    Landed,
    Conflict,
    CheckFailed,
    CheckMutated,
    RemoteMoved,
    Failed,
    Unresolved,
    Cancelled,
    /// A state of a newer station. It frees nothing (fail closed, decision 82).
    #[serde(other)]
    Unknown,
}

impl LandState {
    /// True for a state that ends the land and frees the queue (design
    /// §11.2). `Unknown` is never terminal.
    #[must_use]
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Landed
                | Self::Conflict
                | Self::CheckFailed
                | Self::CheckMutated
                | Self::RemoteMoved
                | Self::Failed
                | Self::Unresolved
                | Self::Cancelled
        )
    }
}

/// The phase of a land (design §11.5). `Lost` exists only on the deck.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "fleet_git/")]
pub enum LandPhase {
    Admitted,
    Captured,
    WorktreeReady,
    Rebased,
    Checking,
    Checked,
    BranchPushIntent,
    BranchPushed,
    PrIntent,
    DefaultPushIntent,
    Lost,
    Done,
    #[serde(other)]
    Unknown,
}

/// Where a land pushes its branch (design §11.3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "fleet_git/")]
pub struct PushTarget {
    /// A remote name or a URL.
    pub target: String,
    pub dest: String,
}

/// What the deck pins at the admission of a land (design §11.3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "fleet_git/")]
pub struct LandPlan {
    pub project_id: String,
    pub settings_revision: u64,
    pub home_generation: u64,
    pub mode: LandMode,
    pub fetch_remote: String,
    pub remote: RemoteIdentity,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub default_branch: Option<String>,
    /// The local session branch that gives `source_oid`.
    pub branch_ref: String,
    /// The pushed ref and the pull request head (M9).
    pub publish_ref: String,
    pub branch_target: PushTarget,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub forge: Option<ForgeBinding>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub check_cmd: Option<String>,
    pub check_env: BTreeMap<String, String>,
    pub pr_title: String,
    pub pr_body: String,
}

/// The land state that the fold keeps in `SessionSummary.land` (design §3.6).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "fleet_git/")]
pub struct LandInfo {
    pub land_id: String,
    pub attempt: u32,
    pub state: LandState,
    pub phase: LandPhase,
    pub reconciling: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub text: Option<String>,
    #[serde(default)]
    pub files: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub source_oid: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub base_oid: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub rebased_oid: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub branch_published_oid: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub default_published_oid: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub pr_url: Option<String>,
}

/// Params of `fleet_git.land_status` (design §11.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "fleet_git/")]
pub struct LandStatusParams {
    pub land_id: String,
}

/// The status of a known land (ADR-009 §4): [`LandInfo`] without the id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "fleet_git/")]
pub struct LandStatus {
    pub state: LandState,
    pub phase: LandPhase,
    pub reconciling: bool,
    pub attempt: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub source_oid: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub base_oid: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub rebased_oid: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub branch_published_oid: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub default_published_oid: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub pr_url: Option<String>,
    #[serde(default)]
    pub files: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub text: Option<String>,
}

/// The answer of `fleet_git.land_status` (ADR-009 §4). `LandState::Unknown`
/// also reads `"unknown"`, so the two marker answers carry only `state` and a
/// status always carries a `phase`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(from = "LandStatusWire", into = "LandStatusWire")]
#[ts(export_to = "fleet_git/", as = "LandStatusWire")]
pub enum LandStatusAnswer {
    /// The station has a fence for this land.
    Fenced,
    /// The station does not know this land.
    Unknown,
    Known(Box<LandStatus>),
}

/// The wire form of [`LandStatusAnswer`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(untagged)]
#[ts(export_to = "fleet_git/")]
pub enum LandStatusWire {
    Marker(LandStatusMarker),
    Status(Box<LandStatus>),
}

/// `{ "state": "fenced" }` or `{ "state": "unknown" }`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "fleet_git/")]
pub struct LandStatusMarker {
    pub state: LandStatusKind,
}

/// The `state` of a marker answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "fleet_git/")]
pub enum LandStatusKind {
    Fenced,
    Unknown,
}

impl From<LandStatusWire> for LandStatusAnswer {
    fn from(wire: LandStatusWire) -> Self {
        match wire {
            LandStatusWire::Marker(LandStatusMarker {
                state: LandStatusKind::Fenced,
            }) => Self::Fenced,
            LandStatusWire::Marker(LandStatusMarker {
                state: LandStatusKind::Unknown,
            }) => Self::Unknown,
            LandStatusWire::Status(status) => Self::Known(status),
        }
    }
}

impl From<LandStatusAnswer> for LandStatusWire {
    fn from(answer: LandStatusAnswer) -> Self {
        match answer {
            LandStatusAnswer::Fenced => Self::Marker(LandStatusMarker {
                state: LandStatusKind::Fenced,
            }),
            LandStatusAnswer::Unknown => Self::Marker(LandStatusMarker {
                state: LandStatusKind::Unknown,
            }),
            LandStatusAnswer::Known(status) => Self::Status(status),
        }
    }
}

/// Params of `fleet_git.land_abandon` (design §11.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "fleet_git/")]
pub struct LandAbandonParams {
    pub land_id: String,
}

/// The outcome of the last remote write of a land that was fenced (M3).
/// `unknown` is a real value here, not a fallback.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "fleet_git/")]
pub enum LandWrite {
    None,
    Applied,
    Rejected,
    Unknown,
}

/// The answer of `fleet_git.land_abandon` (design §11.2, revision 4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "state", rename_all = "snake_case")]
#[ts(export_to = "fleet_git/")]
pub enum LandAbandonAnswer {
    /// The station recorded the fence; the write that it waited for ended.
    Fenced {
        write: LandWrite,
        run_state: LandState,
    },
    /// An active write did not end in time; nothing was recorded.
    Busy,
}
