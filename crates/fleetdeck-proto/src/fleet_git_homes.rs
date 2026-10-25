//! Wire types of the homes of a project (design §8.4, ADR-009 §4): the
//! `project.ensure_home` params, the `project.home_status` params and the
//! `HomeStatus` answer. Phase 2 (FG-5).

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::RemoteIdentity;

/// The state of one home attempt on the station (design §3.5 V29, §8.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "fleet_git/")]
pub enum HomeAttemptState {
    Cloning,
    Ready,
    Adopted,
    Failed,
    Interrupted,
    Cleared,
    /// A state of a newer station. It frees nothing (fail closed).
    #[serde(other)]
    Unknown,
}

/// Params of `project.ensure_home` (design §8.4). The deck resends the
/// stored request unchanged until the first answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "fleet_git/")]
pub struct EnsureHomeParams {
    pub project_id: String,
    pub project_name: String,
    /// The folder name below the station's `clone_root`.
    pub folder: String,
    /// The clone URL, with the SSH login name.
    pub url: String,
    pub remote: RemoteIdentity,
    /// The remote that the clone creates (`push_remote` at the add).
    pub remote_name: String,
    pub generation: u64,
    pub attempt_id: String,
    pub request_digest: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub retry_of: Option<String>,
    /// An absolute folder on the station to adopt instead of a new clone in
    /// `clone_root` (needs `fleet_git.homes_adopt.v1`). The station never
    /// clones into it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub adopt_path: Option<String>,
}

/// Params of `project.home_status`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "fleet_git/")]
pub struct HomeStatusParams {
    pub project_id: String,
    pub attempt_id: String,
}

/// The answer of `project.ensure_home` and `project.home_status`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "fleet_git/")]
pub struct HomeStatus {
    pub attempt_id: String,
    pub generation: u64,
    pub state: HomeAttemptState,
    /// The verified canonical path; only for `ready` and `adopted`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub path: Option<String>,
    /// The clone destination while cloning or failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub reserved_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub message: Option<String>,
}
