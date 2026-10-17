//! Fleet Git wire types (design `docs/v2/design/fleet-git.md`, ADR-009): the
//! project settings, the start snapshot, the station snapshot, the push and
//! cleanup state, the error codes, and the pure checks that the deck and the
//! station share. Phase 1 (FG-1 to FG-4). Everything here is additive (O5).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::ids::{SessionId, StationId};
use crate::integrations::ForgeBinding;

#[path = "fleet_git_identity.rs"]
mod identity;
pub use identity::*;
#[path = "fleet_git_homes.rs"]
mod homes;
pub use homes::*;
#[path = "station_traits.rs"]
mod station_traits;
pub use station_traits::*;
#[path = "drift.rs"]
mod drift;
pub use drift::*;
#[path = "taildrive.rs"]
mod taildrive;
pub use taildrive::*;
#[path = "fleet_git_land.rs"]
mod land;
pub use land::*;

/// Wire strings of the Fleet Git error codes (ADR-009 §4). A code is never
/// reused for another meaning.
pub mod code {
    pub const NOT_SYNCED: &str = "not_synced";
    pub const HELD: &str = "held";
    pub const BRANCH_NAMESPACE_BLOCKED: &str = "branch_namespace_blocked";
    pub const GIT_TOO_OLD: &str = "git_too_old";
    pub const SLUG_MISMATCH: &str = "slug_mismatch";
    pub const STALE_SETTINGS: &str = "stale_settings";
    pub const STALE_GENERATION: &str = "stale_generation";
    pub const NOT_A_HOME: &str = "not_a_home";
    pub const UNSUPPORTED_MODE: &str = "unsupported_mode";
    pub const FETCH_FAILED: &str = "fetch_failed";
    pub const FETCH_TIMEOUT: &str = "fetch_timeout";
    pub const PUSH_FAILED: &str = "push_failed";
    pub const PUSH_REJECTED: &str = "push_rejected";
    pub const PUSH_AUTH: &str = "push_auth";
    pub const PUSH_TIMEOUT: &str = "push_timeout";
    pub const REMOTE_MISMATCH: &str = "remote_mismatch";
    pub const REMOTE_REWRITTEN: &str = "remote_rewritten";
    pub const REMOTE_AMBIGUOUS: &str = "remote_ambiguous";
    pub const HOME_CONFLICT: &str = "home_conflict";
    pub const CLONE_FAILED: &str = "clone_failed";
    pub const CLONE_ROOT_MISSING: &str = "clone_root_missing";
    pub const CLONE_INTERRUPTED: &str = "clone_interrupted";
    pub const INCOMPLETE_FOLDER: &str = "incomplete_folder";
    pub const DESTINATION_RESERVED: &str = "destination_reserved";
    pub const ATTEMPT_REUSED: &str = "attempt_reused";
    pub const RETRY_REFUSED: &str = "retry_refused";
    pub const LAND_NOT_READY: &str = "land_not_ready";
    pub const LAND_OPEN: &str = "land_open";
    pub const LAND_PR_NEEDS_FORGE_BRANCH: &str = "land_pr_needs_forge_branch";
    pub const NOTHING_TO_LAND: &str = "nothing_to_land";
    pub const CHECK_DISABLED: &str = "check_disabled";
    pub const CHECK_MUTATED: &str = "check_mutated";
    pub const BRANCH_MOVED: &str = "branch_moved";
    pub const PR_FAILED: &str = "pr_failed";
    pub const PR_AMBIGUOUS: &str = "pr_ambiguous";
    pub const LAND_NO_IDENTITY: &str = "land_no_identity";
    pub const LAND_TEMP_UNSAFE: &str = "land_temp_unsafe";
    pub const STATION_RESTARTED: &str = "station_restarted";
    pub const SHARE_EXPOSED: &str = "share_exposed";
    pub const SHARE_UNVERIFIED: &str = "share_unverified";
    pub const MOVE_NOT_READY: &str = "move_not_ready";
    pub const MOVE_TOO_LARGE: &str = "move_too_large";
    pub const MOVE_CHECKSUM: &str = "move_checksum";
    pub const MOVE_BRANCH_MOVED: &str = "move_branch_moved";
    pub const MOVE_RESUME_FAILED: &str = "move_resume_failed";
    pub const NO_REMOTE: &str = "no_remote";
    pub const REMOTE_HAS_CREDENTIALS: &str = "remote_has_credentials";
    pub const REMOTE_LOCAL_REFUSED: &str = "remote_local_refused";
    pub const REMOTE_PATH_INVALID: &str = "remote_path_invalid";
    pub const REMOTE_HOST_INVALID: &str = "remote_host_invalid";
    pub const HOME_NOT_READY: &str = "home_not_ready";
    pub const DESTINATION_CLEARED: &str = "destination_cleared";
    pub const LAND_FENCED: &str = "land_fenced";
    pub const AUTHORITY_EXPIRED: &str = "authority_expired";
    pub const MOVE_PUBLICATION_MISMATCH: &str = "move_publication_mismatch";
    pub const MOVE_BRANCH_EXISTS: &str = "move_branch_exists";
    pub const MOVE_REF_EXHAUSTED: &str = "move_ref_exhausted";
    pub const MOVE_RESUME_TIMEOUT: &str = "move_resume_timeout";
    pub const IDENTITY_CHANGED: &str = "identity_changed";
    pub const WORKTREE_CHANGED: &str = "worktree_changed";
    /// `project.home_status` names an attempt that the station does not know (FG-5).
    pub const UNKNOWN_ATTEMPT: &str = "unknown_attempt";
    pub const ARTIFACTS_UNAVAILABLE: &str = "artifacts_unavailable";
    pub const REMOTE_UNSUPPORTED: &str = "remote_unsupported";
    pub const MOVE_DISCARDED: &str = "move_discarded";
    pub const MOVE_CANCELLED: &str = "move_cancelled";
    pub const MOVE_NOT_UNDOABLE: &str = "move_not_undoable";
    pub const AUTHORITY_LOST: &str = "authority_lost";
    pub const AUTHORITY_UNKNOWN: &str = "authority_unknown";
    pub const INVALID_PARAMS: &str = "invalid_params";
    pub const UNKNOWN_SESSION: &str = "unknown_session";
    pub const INVALID_STATE: &str = "invalid_state";
    pub const FORBIDDEN: &str = "forbidden";
    pub const BUSY: &str = "busy";
    pub const INTERNAL: &str = "internal";
    pub const NO_DEFAULT_BRANCH: &str = "no_default_branch";
    pub const INVALID_GIT_SETTINGS: &str = "invalid_git_settings";
    pub const FLEET_GIT_UNSUPPORTED: &str = "fleet_git_unsupported";
    /// A `project.radar` run is active for the project (FG-7).
    pub const RADAR_BUSY: &str = "radar_busy";
    /// An `unresolved` land that the owner stopped (FG-8, M3).
    pub const ABANDONED: &str = "abandoned";
    /// An `unresolved` land whose station was removed (FG-8, M3).
    pub const STATION_REMOVED: &str = "station_removed";
}

fn origin() -> String {
    "origin".to_owned()
}

fn h24() -> u32 {
    24
}

/// When the station pushes the session branch (FG-2, O9).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "fleet_git/")]
pub enum PushMode {
    #[default]
    Off,
    OnTurn,
    OnExit,
    /// A mode of a newer peer: the station pushes nothing (fail closed).
    #[serde(other)]
    Unknown,
}

/// Automatic cleanup of merged worktrees (FG-3, O12).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "fleet_git/")]
pub enum CleanupMode {
    #[default]
    Off,
    Merged,
    /// A mode of a newer peer: the station cleans nothing (fail closed).
    #[serde(other)]
    Unknown,
}

/// How a land publishes (FG-8, O25).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "fleet_git/")]
pub enum LandMode {
    Pr,
    Merge,
    /// A mode of a newer peer: the land is refused (`unsupported_mode`).
    #[serde(other)]
    Unknown,
}

/// The Fleet Git settings of a project (design §3.1). `sync = false` (the
/// default) leaves every behavior as it is today (O1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "fleet_git/")]
pub struct GitSettings {
    #[serde(default)]
    pub sync: bool,
    #[serde(default)]
    pub push: PushMode,
    #[serde(default = "origin")]
    pub push_remote: String,
    /// A URL that receives the pushes instead of `push_remote`.
    #[serde(default)]
    pub wip_remote: Option<String>,
    #[serde(default)]
    pub default_branch: Option<String>,
    #[serde(default)]
    pub cleanup: CleanupMode,
    #[serde(default = "h24")]
    pub cleanup_after_hours: u32,
    #[serde(default)]
    pub worktree_include: Vec<String>,
    #[serde(default)]
    pub worktree_env: BTreeMap<String, String>,
    /// `None`: `pr` with a forge binding, else `merge`.
    #[serde(default)]
    pub land: Option<LandMode>,
    #[serde(default)]
    pub check_cmd: Option<String>,
    /// `<machine>/<share>` (FG-12).
    #[serde(default)]
    pub artifact_share: Option<String>,
    #[serde(default)]
    pub radar_home: Option<StationId>,
}

impl Default for GitSettings {
    fn default() -> Self {
        Self {
            sync: false,
            push: PushMode::Off,
            push_remote: origin(),
            wip_remote: None,
            default_branch: None,
            cleanup: CleanupMode::Off,
            cleanup_after_hours: h24(),
            worktree_include: Vec::new(),
            worktree_env: BTreeMap::new(),
            land: None,
            check_cmd: None,
            artifact_share: None,
            radar_home: None,
        }
    }
}

/// A refused setting: the field and the text for the board (design §3.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingsError {
    pub field: String,
    pub message: String,
}

fn refuse(field: &str, message: &str) -> Result<(), SettingsError> {
    Err(SettingsError {
        field: field.to_owned(),
        message: message.to_owned(),
    })
}

const UNKNOWN_VALUE: &str = "This value is not known. Update Fleet Deck.";

/// Variable names and prefixes that `worktree_env` refuses (design §3.1):
/// `PATH`, `HOME`, `LD_*`, `DYLD_*`, `GIT_*`, and the provider credential
/// scrub of `fleetdeck_core::childenv` (`SCRUBBED_NAMES`, `SCRUBBED_PREFIXES`).
const ENV_REFUSED_NAMES: &[&str] = &["PATH", "HOME", "CLAUDECODE", "AI_AGENT"];
const ENV_REFUSED_PREFIXES: &[&str] = &[
    "LD_",
    "DYLD_",
    "GIT_",
    "ANTHROPIC_",
    "CLAUDE_",
    "OPENAI_",
    "CODEX_",
    "FLEETDECK_",
];

/// The syntax rules of `provision::check_branch_choice` for one branch name.
fn branch_name_ok(name: &str) -> bool {
    let n = name.chars().count();
    (1..=200).contains(&n)
        && !name.starts_with('-')
        && !name
            .chars()
            .any(|c| c.is_whitespace() || c.is_ascii_control())
        && !name.contains("..")
        && !name.contains(['~', '^', ':', '?', '*', '[', '\\'])
        && !name.ends_with('/')
        && !name.ends_with(".lock")
}

fn include_ok(pattern: &str) -> bool {
    let n = pattern.chars().count();
    (1..=200).contains(&n)
        && !pattern
            .chars()
            .any(|c| c.is_control() || c == '\\' || c == '[')
        && !pattern.contains("**")
        && pattern
            .split('/')
            .all(|segment| !segment.is_empty() && segment != "..")
}

fn env_name_ok(name: &str) -> bool {
    let mut chars = name.chars();
    name.len() <= 64
        && chars
            .next()
            .is_some_and(|c| c == '_' || c.is_ascii_uppercase())
        && chars.all(|c| c == '_' || c.is_ascii_uppercase() || c.is_ascii_digit())
}

fn share_ok(share: &str) -> bool {
    let Some((machine, name)) = share.split_once('/') else {
        return false;
    };
    (1..=63).contains(&machine.len())
        && machine
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && (1..=64).contains(&name.len())
        && !name.starts_with(' ')
        && !name.ends_with(' ')
        && name.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b'(' | b')' | b' ')
        })
}

/// Checks the settings by their own syntax and ranges (design §3.1). The
/// rules that need the deck's data (the project's `remote_url`, the homes,
/// `radar_home`, `allow_local_remotes`) are the deck's. `wip_remote` is
/// checked with [`LocalRemotes::Allow`]: the deck decides about local remotes.
///
/// # Errors
/// [`SettingsError`] for the first refused field, in the order of the fields.
pub fn check_settings(settings: &GitSettings) -> Result<(), SettingsError> {
    if settings.push == PushMode::Unknown {
        return refuse("push", UNKNOWN_VALUE);
    }
    if settings.cleanup == CleanupMode::Unknown {
        return refuse("cleanup", UNKNOWN_VALUE);
    }
    if settings.land == Some(LandMode::Unknown) {
        return refuse("land", UNKNOWN_VALUE);
    }
    let remote = &settings.push_remote;
    if !(1..=64).contains(&remote.len())
        || remote.starts_with(['-', '.'])
        || !remote
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
    {
        return refuse(
            "push_remote",
            "The push remote must be a remote name, for example origin.",
        );
    }
    if let Some(url) = &settings.wip_remote
        && let Err(err) = remote_identity(url, LocalRemotes::Allow)
    {
        return Err(SettingsError {
            field: "wip_remote".to_owned(),
            message: match err {
                IdentityError::RemoteUnsupported { escaped_path: true } => err.to_string(),
                _ => {
                    "The WIP remote must be an https, ssh or user@host:path URL without a password."
                        .to_owned()
                }
            },
        });
    }
    if settings
        .default_branch
        .as_deref()
        .is_some_and(|b| !branch_name_ok(b))
    {
        return refuse("default_branch", "The default branch name is not valid.");
    }
    if !(1..=720).contains(&settings.cleanup_after_hours) {
        return refuse("cleanup_after_hours", "Clean up after 1 to 720 hours.");
    }
    if settings.worktree_include.len() > 32
        || !settings.worktree_include.iter().all(|p| include_ok(p))
    {
        return refuse(
            "worktree_include",
            "Use relative paths without .. (at most 32).",
        );
    }
    check_env(&settings.worktree_env)?;
    if settings
        .check_cmd
        .as_deref()
        .is_some_and(|c| c.is_empty() || c.chars().count() > 4096 || c.contains('\0'))
    {
        return refuse(
            "check_cmd",
            "The check command must be 1 to 4096 characters without NUL.",
        );
    }
    if settings
        .artifact_share
        .as_deref()
        .is_some_and(|s| !share_ok(s))
    {
        return refuse(
            "artifact_share",
            "Use <machine>/<share>, for example nas/artifacts.",
        );
    }
    if settings.push != PushMode::Off && !settings.sync {
        return refuse("push", "Turn on sync first.");
    }
    if settings.cleanup != CleanupMode::Off && !settings.sync {
        return refuse("cleanup", "Turn on sync first.");
    }
    Ok(())
}

fn check_env(env: &BTreeMap<String, String>) -> Result<(), SettingsError> {
    let too_many = env.len() > 32;
    for (name, value) in env {
        let refused = ENV_REFUSED_NAMES.contains(&name.as_str())
            || ENV_REFUSED_PREFIXES.iter().any(|p| name.starts_with(p));
        if too_many
            || !env_name_ok(name)
            || refused
            || value.chars().count() > 4096
            || value.contains('\0')
        {
            return Err(SettingsError {
                field: "worktree_env".to_owned(),
                message: format!("The variable {name} is not allowed."),
            });
        }
    }
    Ok(())
}

/// The slug of a station name (O6, design §3.2): lower case, every character
/// outside `[a-z0-9-]` becomes `-`. No truncation; consecutive `-` stay.
#[must_use]
pub fn station_slug(name: &str) -> String {
    name.to_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect()
}

/// What kind of start the deck asks for (design §2.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "fleet_git/")]
pub enum StartKind {
    Worktree,
    InPlace,
    /// A kind of a newer peer: the start is refused (`unsupported_mode`).
    #[serde(other)]
    Unknown,
}

/// The Fleet Git part of `SessionCommand::Start` (design §3.3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "fleet_git/")]
pub struct FleetGitStart {
    pub kind: StartKind,
    pub project_id: String,
    pub project_name: String,
    pub settings: GitSettings,
    /// `projects.git_revision` at the start.
    pub settings_revision: u64,
    pub home_generation: u64,
    /// The verified canonical home path.
    pub home_path: String,
    pub remote: RemoteIdentity,
    pub slug: String,
    pub forge: Option<ForgeBinding>,
    /// `remote_url` with the SSH login name put back.
    pub clone_url: String,
}

/// One project of a station in the snapshot (design §3.3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "fleet_git/")]
pub struct GitProjectView {
    pub project_id: String,
    pub project_name: String,
    pub settings_revision: u64,
    pub settings: GitSettings,
    pub remote: RemoteIdentity,
    pub forge: Option<ForgeBinding>,
    pub clone_url: String,
    pub home_generation: u64,
    pub home_path: String,
    pub radar_home: bool,
}

/// One home on a station, sync on or off (design §3.3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "fleet_git/")]
pub struct StationHome {
    pub project_id: String,
    pub generation: u64,
    pub path: String,
    pub sync: bool,
}

/// The full Fleet Git state of one station (`fleet_git.projects_set`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "fleet_git/")]
pub struct GitSnapshot {
    /// Per station: `max(previous + 1, now in unix ms)`.
    pub revision: u64,
    pub projects: Vec<GitProjectView>,
    pub homes: Vec<StationHome>,
}

/// The result of the station's home check (design §3.8 item 3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "fleet_git/")]
pub enum HomeCheckState {
    Verified,
    Mismatch,
    Missing,
    NotARepo,
    RemoteAmbiguous,
    NoRemote,
    Refused,
    #[serde(other)]
    Unknown,
}

/// The station's check of one home.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "fleet_git/")]
pub struct HomeCheck {
    pub project_id: String,
    pub generation: u64,
    pub state: HomeCheckState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub canonical_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub seen: Option<RemoteIdentity>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub code: Option<String>,
}

/// The answer to `fleet_git.projects_set`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "fleet_git/")]
pub struct SnapshotAck {
    pub stored: bool,
    /// The station's newest revision.
    pub revision: u64,
    pub checks: Vec<HomeCheck>,
}

/// The state of the push of one session (design §5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "fleet_git/")]
pub enum PushState {
    Idle,
    Due,
    Pushing,
    Pushed,
    Failing,
    Stopped,
    #[serde(other)]
    Unknown,
}

/// The state of the cleanup of one session (design §6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "fleet_git/")]
pub enum CleanupState {
    Waiting,
    Kept,
    Trashed,
    #[serde(other)]
    Unknown,
}

/// The push state that the fold keeps in `SessionSummary.push`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "fleet_git/")]
pub struct PushInfo {
    pub state: PushState,
    pub ref_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub oid: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub desired_oid: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub stop_code: Option<String>,
    pub attempts: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub next_attempt_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub text: Option<String>,
}

/// The cleanup state that the fold keeps in `SessionSummary.cleanup`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "fleet_git/")]
pub struct CleanupInfo {
    pub state: CleanupState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub reason: Option<String>,
}

/// A file that the include step did not copy (design §7.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "fleet_git/")]
pub struct SkippedFile {
    pub path: String,
    pub reason: String,
}

/// Params of `fleet_git.push_now` (design §5.8).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "fleet_git/")]
pub struct PushNowParams {
    pub session_id: SessionId,
}

/// Answer of `fleet_git.push_now`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "fleet_git/")]
pub struct PushNowAnswer {
    pub state: PushState,
}

/// The state of a project home on the board (design §8.1). Phase 1 derives
/// it from the station's home check: `ready` or `unverified`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "fleet_git/")]
pub enum GitHomeState {
    Cloning,
    Ready,
    Unverified,
    Failed,
    #[serde(other)]
    Unknown,
}

/// One home of a project (design §8.1). Phase 1 has exactly one, derived
/// from `projects.station_id` and `projects.path` (position 0, generation 1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "fleet_git/")]
pub struct ProjectHome {
    pub station_id: StationId,
    pub generation: u64,
    pub position: u32,
    pub state: GitHomeState,
    pub path: Option<String>,
    pub reserved_path: Option<String>,
    pub attempt_id: Option<String>,
    pub sync_owner: bool,
    pub code: Option<String>,
    pub message: Option<String>,
}

/// A reserved Git name: the station that owns `slug` for the repository of
/// the project (design §3.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "fleet_git/")]
pub struct SlugReservation {
    pub slug: String,
    pub station_id: StationId,
}

/// The body of `PUT /api/projects/{id}/git`. `revision` is the
/// `git_revision` that the client loaded; `remote_url` absent means
/// unchanged.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "fleet_git/")]
pub struct GitSettingsPut {
    pub settings: GitSettings,
    pub revision: u64,
    #[serde(default)]
    #[ts(optional)]
    pub remote_url: Option<String>,
}

/// The answer of `PUT /api/projects/{id}/git`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "fleet_git/")]
pub struct GitSettingsAnswer {
    pub git: GitSettings,
    pub git_revision: u64,
}

/// The Fleet Git part of `station.info` (design §3.7).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "fleet_git/")]
pub struct FleetGitInfo {
    /// The folder below which the station clones homes.
    pub clone_root: String,
    /// `allow_check_cmd` of `station.toml`.
    pub allow_check_cmd: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub git_version: Option<String>,
    /// The station accepts a local remote (M13). False from an older station.
    #[serde(default)]
    pub allow_local_remotes: bool,
}

#[cfg(test)]
#[path = "fleet_git_homes_tests.rs"]
mod homes_tests;
#[cfg(test)]
#[path = "fleet_git_identity_tests.rs"]
mod identity_tests;
#[cfg(test)]
#[path = "fleet_git_land_tests.rs"]
mod land_tests;
#[cfg(test)]
#[path = "fleet_git_tests.rs"]
mod tests;
