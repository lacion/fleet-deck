//! Wire types of `project.branches` and `project.fetch` (design §13.1,
//! ADR-009 §4). Phase 4 (FG-10). Everything here is additive (O5).

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// The most branches that one `project.branches` answer carries (design
/// §13.1).
pub const REMOTE_BRANCHES_MAX: usize = 200;
/// The most characters of a `RemoteBranch::subject` (design §13.1).
pub const REMOTE_BRANCH_SUBJECT_MAX: usize = 200;

/// Params of `project.branches` (design §13.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "fleet_git/")]
pub struct BranchesParams {
    pub project_id: String,
}

/// One `fd/*` branch on the remote of the radar home (design §13.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "fleet_git/")]
pub struct RemoteBranch {
    /// The branch name without the remote prefix.
    pub name: String,
    /// The station slug; none for a legacy `fd/<callsign>` branch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub slug: Option<String>,
    pub callsign: String,
    /// The tip commit.
    pub head: String,
    /// Commits of the branch that `<remote>/<default>` lacks.
    pub ahead: u32,
    /// Commits of `<remote>/<default>` that the branch lacks.
    pub behind: u32,
    /// The commit time of the tip, in Unix milliseconds.
    pub at: i64,
    /// The first line of the commit message, at most
    /// [`REMOTE_BRANCH_SUBJECT_MAX`] characters. Third-party text.
    pub subject: String,
}

/// The answer of `project.branches` (design §13.1). The station runs no fetch
/// in this call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "fleet_git/")]
pub struct RemoteBranches {
    pub remote: String,
    pub default_branch: String,
    /// The tip of `<remote>/<default>`; none before the first fetch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub default_oid: Option<String>,
    /// When the last fetch of the radar home ended; none before the first.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub fetched_at: Option<i64>,
    /// At most [`REMOTE_BRANCHES_MAX`], newest tip first.
    pub branches: Vec<RemoteBranch>,
    pub truncated: bool,
}

/// Params of `project.fetch` (design §13.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "fleet_git/")]
pub struct FetchParams {
    pub project_id: String,
}

/// The answer of `project.fetch` (design §13.1). `code` and `text` are set
/// when a fetch failed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "fleet_git/")]
pub struct FetchAnswer {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub fetched_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub text: Option<String>,
}
