//! The Taildrive report and the artifact share (design §15, FG-12, O35 to
//! O39): the station's `tailscale drive list` report with the exposure
//! check, the artifact-share authorization, and the artifact folder of a
//! session.
//!
//! Each enum has `Unknown` with `#[serde(other)]`: an unknown wire value
//! decodes as `unknown`, which no check treats as safe (fail closed).

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::ids::{SessionId, StationId};

/// Whether `tailscale drive list` could be read (design §15.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "fleet_git/")]
pub enum TaildriveState {
    Ok,
    Unavailable,
    #[serde(other)]
    Unknown,
}

/// What a share exposes (design §15.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "fleet_git/")]
pub enum Exposure {
    ProjectHome,
    Worktree,
    DataDir,
    ClaudeHome,
    CodexHome,
    Ssh,
    Gnupg,
    Aws,
    MemberDeny,
    /// An exposure of a newer station.
    #[serde(other)]
    Unknown,
}

/// The result of the exposure check of one share (design §15.4, B5). The
/// default is `unknown`: an old-shape share without the field is not safe.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "fleet_git/")]
pub enum ExposureState {
    Clear,
    Exposed,
    #[default]
    #[serde(other)]
    Unknown,
}

/// The state of the path of one share (design §15.3, M10). The default is
/// `unknown`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "fleet_git/")]
pub enum SharePathState {
    Ok,
    Missing,
    Ambiguous,
    #[default]
    #[serde(other)]
    Unknown,
}

/// One Taildrive share of a station (design §15.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "fleet_git/")]
pub struct TaildriveShare {
    pub name: String,
    /// The trimmed path field; for `ambiguous`, the shortest candidate.
    pub path: String,
    /// Every path that fits the printed field (one when unambiguous).
    #[serde(default)]
    pub candidates: Vec<String>,
    /// `None`: blank (Windows).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub as_user: Option<String>,
    /// Empty: not exposed.
    #[serde(default)]
    pub exposed: Vec<Exposure>,
    #[serde(default)]
    pub exposure_state: ExposureState,
    /// For `exposure_state = unknown`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub unknown_reason: Option<String>,
    #[serde(default)]
    pub path_state: SharePathState,
}

/// The Taildrive report in `station.info` (design §15.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "fleet_git/")]
pub struct TaildriveReport {
    pub state: TaildriveState,
    /// For `unknown` and `unavailable`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub reason: Option<String>,
    /// The tailnet machine name of this station.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub machine: Option<String>,
    #[serde(default)]
    pub shares: Vec<TaildriveShare>,
    /// Unix ms.
    pub checked_at: i64,
    /// `max(previous + 1, now_ms)` at each new report (B6).
    #[serde(default)]
    pub inventory_revision: u64,
}

/// The verdict of an artifact-share authorization (design §15.5, B6). Only
/// `ok` and `not_a_station` authorize; `unknown` (also an unknown wire
/// value) is refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "fleet_git/")]
pub enum ShareVerdict {
    Ok,
    Exposed,
    NotAStation,
    #[serde(other)]
    Unknown,
}

/// One verdict bound to its source (design §15.5, B6). It expires 10
/// minutes after `issued_at`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "fleet_git/")]
pub struct ArtifactAuthorization {
    /// `<machine>/<share>`.
    pub share: String,
    /// `None`: the machine is not a Fleet Deck station.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub exporting_station: Option<StationId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub inventory_revision: Option<u64>,
    pub verdict: ShareVerdict,
    #[serde(default)]
    pub exposed: Vec<Exposure>,
    /// The exporting station's report time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub checked_at: Option<i64>,
    pub issued_at: i64,
    pub expires_at: i64,
}

/// Params of `fleet_git.artifact_authorize` (station to deck).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "fleet_git/")]
pub struct ArtifactAuthorizeParams {
    pub project_id: String,
    pub session_id: SessionId,
    pub share: String,
}

/// Params of `fleet_git.share_verdict` (deck to the exporting station).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "fleet_git/")]
pub struct ShareVerdictParams {
    pub share_name: String,
}

/// The answer of `fleet_git.share_verdict`, from a report at most 60 s old.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "fleet_git/")]
pub struct ShareVerdictAnswer {
    pub inventory_revision: u64,
    pub checked_at: i64,
    #[serde(default)]
    pub path_state: SharePathState,
    #[serde(default)]
    pub exposure_state: ExposureState,
    #[serde(default)]
    pub exposed: Vec<Exposure>,
}

/// The artifact folder of a session, from `artifacts.ready` (design §15.5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "fleet_git/")]
pub struct ArtifactsDir {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub url: Option<String>,
}
