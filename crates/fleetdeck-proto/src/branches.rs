//! The branches of the repository of a session (DL-191): the params and
//! answers of the link methods `session.branches` and `files.branch_diff`
//! (feature `session_branches.v1`, deck → station, scope `read`, not
//! mutating).
//!
//! The station runs Git itself in the working directory of the session. The
//! diff answer is a [`crate::files::SessionDiff`] with the scope
//! [`crate::files::DiffScope::Committed`].

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::ids::SessionId;

/// The most branches in one answer (DL-191).
pub const MAX_BRANCHES: usize = 200;
/// The most bytes of one serialized answer (DL-191).
pub const MAX_ANSWER_BYTES: usize = 512 * 1024;

/// Wire error codes of the branch methods, besides the codes of
/// [`crate::history::code`] and [`crate::files::code`].
pub mod code {
    /// The Git of the station is older than 2.45.
    pub const GIT_TOO_OLD: &str = "git_too_old";
    /// The session folder is not a Git repository.
    pub const NOT_A_REPOSITORY: &str = "not_a_repository";
    /// The branch has no commit in common with `HEAD` (`files.branch_diff`).
    pub const NO_COMMON_HISTORY: &str = "no_common_history";
}

/// `session.branches` params.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "branches/")]
pub struct BranchesQuery {
    pub session_id: SessionId,
}

/// One branch other than the current one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "branches/")]
pub struct RepoBranch {
    /// The branch name without `refs/heads/`.
    pub name: String,
    /// The full hex object id of the tip.
    pub head: String,
    /// Commits of the branch that `HEAD` does not have.
    pub ahead: u32,
    /// Commits of `HEAD` that the branch does not have.
    pub behind: u32,
    /// Unix seconds of the tip commit.
    pub at: i64,
    /// The first line of the tip commit message, cut to 200 characters.
    pub subject: String,
}

/// The `session.branches` answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "branches/")]
pub struct RepoBranches {
    /// The full hex object id of `HEAD`; `None` for an unborn `HEAD`.
    pub head: Option<String>,
    /// The current branch; `None` for a detached `HEAD`.
    pub branch: Option<String>,
    /// Newest tip first. The current branch is not listed.
    pub branches: Vec<RepoBranch>,
    /// The list is cut: more than [`MAX_BRANCHES`] branches, or a record that
    /// the strict parser refused.
    pub truncated: bool,
}

/// `files.branch_diff` params.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "branches/")]
pub struct BranchDiffQuery {
    pub session_id: SessionId,
    /// The full hex object id of a listed branch tip.
    pub head: String,
}
