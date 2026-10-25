//! Provider CLI updates by the station (B28, design `cli-update.md`).
//! No type here holds a path, a URL or command output.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::event::ProviderKind;
use crate::ids::{IncarnationId, SessionId};

/// How a provider CLI was installed (§3.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "cli_update/")]
pub enum InstallMethod {
    /// Claude's native installer (`~/.local/share/claude/versions`).
    Native,
    Npm,
    Bun,
    Homebrew,
    SystemPackage,
    #[serde(other)]
    Unknown,
}

/// What started an attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "cli_update/")]
pub enum UpdateTrigger {
    Schedule,
    Owner,
    /// The station start found an attempt that a stop interrupted.
    Recovery,
    /// The station saw a version change that it did not make.
    External,
    #[serde(other)]
    Unknown,
}

/// The result of an attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "cli_update/")]
pub enum UpdateOutcome {
    Running,
    Updated,
    UpToDate,
    /// Sessions of the provider ran during the whole window.
    SkippedBusy,
    /// A rule or a check refused the target before any change.
    Refused,
    /// The install failed and the previous version is in place.
    Failed,
    /// The new version failed a check; the previous version is back.
    RolledBack,
    /// The new version failed and the way back failed too.
    RollbackFailed,
    /// The method is not one that the station updates.
    ReportOnly,
    /// The station stopped during the attempt (§3.7 recovery).
    Interrupted,
    /// A quarantined CLI passed the checks of §3.8.1 again; its starts are
    /// allowed again (revision 2).
    Verified,
    #[serde(other)]
    Unknown,
}

/// One attempt, as `station.db` keeps it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "cli_update/")]
pub struct CliUpdateAttempt {
    pub seq: u64,
    pub attempt_id: String,
    pub provider: ProviderKind,
    pub trigger: UpdateTrigger,
    pub method: InstallMethod,
    /// `stable`, `latest` or `pin`.
    pub channel: String,
    pub from_version: Option<String>,
    pub target_version: Option<String>,
    /// The version after the attempt (the previous one after a rollback).
    pub to_version: Option<String>,
    pub outcome: UpdateOutcome,
    /// A code of [`reason`]; `None` for a plain `updated` or `up_to_date`.
    pub reason: Option<String>,
    /// Unix ms, station clock.
    pub started_at: i64,
    pub ended_at: Option<i64>,
}

/// The update state of one provider of a station.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "cli_update/")]
pub struct ProviderUpdateState {
    pub provider: ProviderKind,
    pub method: InstallMethod,
    /// The station can update this install now: a supported method and
    /// its verify tool are present.
    pub updatable: bool,
    /// Why not, when `updatable` is false: a code of [`reason`].
    pub blocked: Option<String>,
    /// `stable`, `latest` or `pin`.
    pub channel: String,
    pub installed: Option<String>,
    /// The target of the last check when it is newer than `installed`.
    pub available: Option<String>,
    /// Unix ms of the last check (station clock).
    pub checked_at: Option<i64>,
    pub last: Option<CliUpdateAttempt>,
    /// The starts of this provider are refused after a failed update or an
    /// unfinished recovery (§3.8.1): a code of [`reason`]
    /// (`rollback_failed`, `recovery_pending`, `process_left`).
    pub quarantined: Option<String>,
}

/// `cli_update.status`, and `StationInfo::cli_update`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "cli_update/")]
pub struct CliUpdateReport {
    /// `provider_updates.enabled` (the nightly window).
    pub enabled: bool,
    /// The window as text, for example `03:00-05:00 local`.
    pub window: String,
    pub providers: Vec<ProviderUpdateState>,
    /// The attempt that runs now.
    pub running: Option<String>,
    /// The highest `seq` of a finished attempt (the deck's audit import).
    pub last_seq: u64,
}

/// `cli_update.run` params.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "cli_update/")]
pub struct CliUpdateRun {
    pub provider: ProviderKind,
    /// Only resolve and verify the target; change nothing; no attempt row.
    #[serde(default)]
    pub check_only: bool,
    /// Update while sessions of the provider run (native Claude only).
    #[serde(default)]
    pub force: bool,
}

/// The answer of `cli_update.run`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "cli_update/")]
pub struct CliUpdateRunAnswer {
    pub state: ProviderUpdateState,
    /// The attempt that runs now (an update that started).
    pub attempt_id: Option<String>,
    /// The update did not start: a code of [`reason`].
    pub refused: Option<String>,
    /// The live sessions of the provider, with `refused = sessions_running`.
    #[serde(default)]
    pub busy: Vec<SessionId>,
}

/// `cli_update.attempts` params: finished attempts after `after_seq`,
/// oldest first, at most `limit` (1 to 100).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "cli_update/")]
pub struct CliUpdateAttemptsQuery {
    pub after_seq: u64,
    pub limit: u32,
}

/// The answer of `cli_update.attempts` (revision 2): the rows carry the
/// incarnation of the database that they come from, so the deck never files
/// rows of a restored database under the old cursor (§3.11).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "cli_update/")]
pub struct CliUpdateAttemptsPage {
    pub incarnation: IncarnationId,
    pub rows: Vec<CliUpdateAttempt>,
}

/// The reason codes of attempts and refusals. The board has a text for
/// each (§3.12); an unknown code shows as "not updated".
pub mod reason {
    pub const REPORT_ONLY_METHOD: &str = "report_only_method";
    pub const SECOND_INSTALL: &str = "second_install";
    pub const NOT_FOUND: &str = "not_found";
    pub const GPGV_MISSING: &str = "gpgv_missing";
    pub const TOOL_MISSING: &str = "tool_missing";
    pub const DISK_FULL: &str = "disk_full";
    pub const CHANNEL_UNREACHABLE: &str = "channel_unreachable";
    pub const NOT_A_VERSION: &str = "not_a_version";
    pub const SIGNATURE_INVALID: &str = "signature_invalid";
    pub const SIGNATURE_UNKNOWN_KEY: &str = "signature_unknown_key";
    pub const PROVENANCE_MISSING: &str = "provenance_missing";
    pub const KNOWN_BROKEN: &str = "known_broken";
    pub const BELOW_TESTED: &str = "below_tested";
    pub const BELOW_HANDOFF: &str = "below_handoff";
    pub const TOO_NEW: &str = "too_new";
    pub const PUBLISH_TIME_UNKNOWN: &str = "publish_time_unknown";
    pub const STATION_RESTARTING: &str = "station_restarting";
    pub const STARTS_IN_FLIGHT: &str = "starts_in_flight";
    pub const SESSIONS_RUNNING: &str = "sessions_running";
    pub const ANOTHER_UPDATE: &str = "another_update";
    pub const INSTALL_FAILED: &str = "install_failed";
    pub const TIMEOUT: &str = "timeout";
    pub const VERSION_MISMATCH: &str = "version_mismatch";
    pub const CHECKSUM_MISMATCH: &str = "checksum_mismatch";
    pub const HANDSHAKE_FAILED: &str = "handshake_failed";
    pub const OUTSIDE_FLEET_DECK: &str = "outside_fleet_deck";
    // Revision 2.
    /// A launcher link that the vendor installer does not manage (§3.3).
    pub const CUSTOM_LAUNCHER: &str = "custom_launcher";
    /// The stage and the destination are not on one file system, or the
    /// new package changes its `bin` map (§3.6).
    pub const UNSUPPORTED_LAYOUT: &str = "unsupported_layout";
    /// npm before 7 (no hidden lockfile) or Bun without a text `bun.lock`.
    pub const TOOL_TOO_OLD: &str = "tool_too_old";
    /// The stage holds a package that is not in the verified set.
    pub const UNVERIFIED_PACKAGE: &str = "unverified_package";
    /// A staged package's integrity differs from the signed one.
    pub const INTEGRITY_MISMATCH: &str = "integrity_mismatch";
    /// A process of the attempt survived TERM and KILL (§3.6.3).
    pub const PROCESS_LEFT: &str = "process_left";
    /// A journal row of an interrupted attempt waits for the station start.
    pub const RECOVERY_PENDING: &str = "recovery_pending";
    /// The way back failed; the provider is quarantined.
    pub const ROLLBACK_FAILED: &str = "rollback_failed";
    /// The provider is quarantined; only §3.8.1 or a passing update clears it.
    pub const QUARANTINED: &str = "quarantined";
}

/// A release version: `^\d{1,6}\.\d{1,6}\.\d{1,6}$` (no pre-release, no
/// build). Every version that reaches an argv or a file name passes this
/// check, so a value that looks like an option never gets there.
pub fn is_release(version: &str) -> bool {
    let mut parts = 0;
    for part in version.split('.') {
        parts += 1;
        if part.is_empty() || part.len() > 6 || !part.bytes().all(|b| b.is_ascii_digit()) {
            return false;
        }
    }
    parts == 3
}

/// The TypeScript bindings of this module, for [`crate::ts::export_all`].
pub(crate) fn export_ts(cfg: &ts_rs::Config) -> Result<(), ts_rs::ExportError> {
    InstallMethod::export_all(cfg)?;
    UpdateTrigger::export_all(cfg)?;
    UpdateOutcome::export_all(cfg)?;
    CliUpdateAttempt::export_all(cfg)?;
    ProviderUpdateState::export_all(cfg)?;
    CliUpdateReport::export_all(cfg)?;
    CliUpdateRun::export_all(cfg)?;
    CliUpdateRunAnswer::export_all(cfg)?;
    CliUpdateAttemptsQuery::export_all(cfg)?;
    CliUpdateAttemptsPage::export_all(cfg)
}

#[cfg(test)]
#[path = "cli_update_tests.rs"]
mod tests;
