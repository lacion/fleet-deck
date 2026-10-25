//! Wire types of the secret files of a project (Fleet Sync FS-3, design
//! `docs/v2/design/fleet-sync.md` §3): the three station methods
//! `project.secret_scan`, `project.secret_read` and `project.secret_write`,
//! and the report types of the board.
//!
//! `project.secret_browse` lists one folder of a home, with the files that can
//! be secret files marked.
//!
//! A secret file is an ignored file (`.env` and similar) that the owner
//! lists. The content crosses the deck as base64 in a read answer and a write
//! request only. The `Debug` of both types hides it.

use std::fmt;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::member_deny::MemberDeny;

/// The most secret files of one project.
pub const MAX_SECRET_FILES: usize = 20;
/// The longest path of a secret file, in bytes.
pub const MAX_SECRET_PATH: usize = 256;
/// The largest secret file, in bytes (512 KiB).
pub const MAX_SECRET_BYTES: u64 = 512 * 1024;
/// The text of a path with a bad shape.
pub const SECRET_PATH_TEXT: &str =
    "A secret file is a relative path inside the home, without `..`.";
/// The text of a path that members can read.
pub const SECRET_HIDDEN_TEXT: &str = "Fleet Deck syncs only files that members cannot read, such as .env*, *.pem, *.key, secrets* and credentials*.json.";

/// Why [`check_secret_path`] refuses a path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecretPathError {
    /// The shape is bad, or a segment is `.git`.
    Invalid,
    /// The default member deny list does not hide the path.
    NotMemberHidden,
}

/// Checks the path of a secret file: 1 to 256 bytes, no NUL, no `\`, not
/// absolute, no empty segment, no `.`, `..` or `.git` segment (any case), and
/// hidden from members by the default member deny list.
///
/// # Errors
/// [`SecretPathError::Invalid`] for the shape rules, then
/// [`SecretPathError::NotMemberHidden`].
pub fn check_secret_path(path: &str) -> Result<(), SecretPathError> {
    let shape = (1..=MAX_SECRET_PATH).contains(&path.len())
        && !path.contains(['\0', '\\'])
        && !path.starts_with('/')
        && path
            .split('/')
            .all(|s| !s.is_empty() && s != "." && s != ".." && !s.eq_ignore_ascii_case(".git"));
    if !shape {
        return Err(SecretPathError::Invalid);
    }
    if MemberDeny::default().denies(path) {
        Ok(())
    } else {
        Err(SecretPathError::NotMemberHidden)
    }
}

/// One listed file in a station request: its path and the salt of its digest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "fleet_git/")]
pub struct SecretFileKey {
    pub path: String,
    /// 64 lower-case hex characters (32 bytes).
    pub salt: String,
}

/// Params of `project.secret_scan`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "fleet_git/")]
pub struct SecretScanParams {
    pub project_id: String,
    pub generation: u64,
    /// The `remote_dest` of the project.
    pub dest: String,
    /// The `push_remote` of the project settings.
    pub remote_name: String,
    pub files: Vec<SecretFileKey>,
}

/// Whether a listed file can be synced on a station.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "fleet_git/")]
pub enum SecretPresence {
    Present,
    Missing,
    Refused,
    /// A state of a newer station.
    #[serde(other)]
    Unknown,
}

/// The state of one listed file on one station.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "fleet_git/")]
pub struct SecretFileState {
    pub path: String,
    pub state: SecretPresence,
    /// A refusal: `not_ignored`, `symlink`, `not_regular`, `too_large`,
    /// `invalid_path`, `private_file` or `unreadable`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub code: Option<String>,
    /// Present only: the keyed digest of the content.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub digest: Option<String>,
    /// Present only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub size: Option<u64>,
    /// Present only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub mtime_ms: Option<i64>,
}

/// The answer of `project.secret_scan`: one state for each requested file, in
/// the order of the request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "fleet_git/")]
pub struct SecretScanAnswer {
    pub files: Vec<SecretFileState>,
}

/// Params of `project.secret_read`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "fleet_git/")]
pub struct SecretReadParams {
    pub project_id: String,
    pub generation: u64,
    pub dest: String,
    pub remote_name: String,
    pub path: String,
    pub salt: String,
    /// The digest that the scan reported; the station refuses a changed file.
    pub digest: String,
}

/// The answer of `project.secret_read`. Its `Debug` hides the content.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "fleet_git/")]
pub struct SecretReadAnswer {
    /// Standard base64 with padding.
    pub content_b64: String,
}

impl fmt::Debug for SecretReadAnswer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SecretReadAnswer")
            .field("content_b64", &"<redacted>")
            .finish()
    }
}

/// Params of `project.secret_write`. Its `Debug` hides the content.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "fleet_git/")]
pub struct SecretWriteParams {
    pub project_id: String,
    pub generation: u64,
    pub dest: String,
    pub remote_name: String,
    pub path: String,
    pub salt: String,
    /// The keyed digest of the content.
    pub digest: String,
    /// Standard base64 with padding.
    pub content_b64: String,
    /// `None`: the file must be missing. `Some(d)`: its digest must be `d`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub expect: Option<String>,
}

impl fmt::Debug for SecretWriteParams {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SecretWriteParams")
            .field("project_id", &self.project_id)
            .field("generation", &self.generation)
            .field("dest", &self.dest)
            .field("remote_name", &self.remote_name)
            .field("path", &self.path)
            .field("salt", &"<redacted>")
            .field("digest", &"<redacted>")
            .field("content_b64", &"<redacted>")
            .field("expect", &self.expect.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

/// The answer of `project.secret_write`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "fleet_git/")]
pub struct SecretWriteAnswer {
    /// The station kept a copy of the replaced file.
    pub backup: bool,
}

/// What a sync run did with one file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "fleet_git/")]
pub enum SecretOutcome {
    Same,
    Copied,
    Partial,
    Nothing,
    NotRun,
    #[serde(other)]
    Unknown,
}

/// The state of one file on one station after a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "fleet_git/")]
pub enum SecretStationState {
    Same,
    Written,
    Missing,
    Refused,
    Offline,
    Failed,
    #[serde(other)]
    Unknown,
}

/// One station in the report of one file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "fleet_git/")]
pub struct SecretStationReport {
    pub station_id: String,
    pub state: SecretStationState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub mtime_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub size: Option<u64>,
}

/// The report of one file after a run. It holds no content and no digest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "fleet_git/")]
pub struct SecretFileReport {
    pub path: String,
    /// Unix ms; only for a complete run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub synced_at: Option<i64>,
    /// The station id of the winning copy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub source: Option<String>,
    pub conflict: bool,
    pub outcome: SecretOutcome,
    pub stations: Vec<SecretStationReport>,
}

/// The answer of `GET` and `PUT /api/projects/{id}/secret-files`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "fleet_git/")]
pub struct SecretFilesView {
    pub paths: Vec<String>,
    pub files: Vec<SecretFileReport>,
}

/// The answer of `POST /api/projects/{id}/secret-files/sync`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "fleet_git/")]
pub struct SecretSyncReport {
    pub files: Vec<SecretFileReport>,
}

/// Params of `project.secret_browse`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "fleet_git/")]
pub struct SecretBrowseParams {
    pub project_id: String,
    pub generation: u64,
    pub dest: String,
    pub remote_name: String,
    /// `""` (the home folder) or a relative folder path below it.
    pub dir: String,
}

/// What one entry of a browsed folder is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "fleet_git/")]
pub enum SecretEntryKind {
    Dir,
    File,
    Other,
    /// A kind of a newer station.
    #[serde(other)]
    Unknown,
}

/// One entry of a browsed folder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "fleet_git/")]
pub struct SecretBrowseEntry {
    pub name: String,
    pub kind: SecretEntryKind,
    /// A file that can be a secret file.
    pub eligible: bool,
    /// Not eligible: `not_ignored`, `not_member_hidden`, `too_large`,
    /// `symlink`, `private_file`, `not_regular`, `invalid_path` or
    /// `unreadable`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub reason: Option<String>,
    /// Regular files only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub size: Option<u64>,
}

/// The answer of `project.secret_browse`: the folders first, then the rest,
/// each group by name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "fleet_git/")]
pub struct SecretBrowseAnswer {
    pub dir: String,
    pub entries: Vec<SecretBrowseEntry>,
    /// More than 500 entries: only the first 500 names are shown.
    pub truncated: bool,
}

#[cfg(test)]
#[path = "fleet_git_secrets_tests.rs"]
mod tests;
