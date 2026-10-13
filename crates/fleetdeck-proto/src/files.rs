//! The read-only file service of a station (PLAN §6.6 "File browse, search,
//! read; diffs", M3 DL-088): the params and answers of the link methods
//! `files.list`, `files.read`, `files.search` and `files.diff` (feature
//! `files.v1`, deck → station, scope `drive`, not mutating) and
//! `files.session_diff` (feature `files.session_diff.v1`, scope `read`).
//!
//! Every path in a query is relative to a root that the station resolves
//! itself: the working directory of a session, or a workspace directory
//! inside the allowed roots. The station never follows a symlink, never
//! answers for `.git` or a credential directory, and reports every cut
//! (`truncated`) instead of hiding it.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::event::DiffSummary;
use crate::ids::{SessionId, TurnId};

/// The largest `files.read` answer in bytes of file content.
pub const MAX_READ_BYTES: u64 = 256 * 1024;
/// The largest `files.diff` patch in bytes.
pub const MAX_DIFF_BYTES: u64 = 256 * 1024;
/// The most entries in one `files.list` answer.
pub const MAX_LIST_ENTRIES: usize = 1000;
/// The most hits in one `files.search` answer.
pub const MAX_SEARCH_HITS: u32 = 200;
/// The longest relative path or search text in characters.
pub const MAX_PATH_CHARS: usize = 4096;
/// The shortest search text in characters.
pub const MIN_QUERY_CHARS: usize = 2;
/// The longest search text in characters.
pub const MAX_QUERY_CHARS: usize = 256;
/// The longest hit text in characters; a longer line is cut.
pub const MAX_HIT_CHARS: usize = 400;

/// Wire error codes of the file methods.
pub mod code {
    /// The path is absolute, has `..`, NUL, or is too long.
    pub const INVALID_PATH: &str = "invalid_path";
    /// The search text is too short or too long.
    pub const INVALID_QUERY: &str = "invalid_query";
    /// The path does not exist, is hidden (`.git`, a credential directory),
    /// or is a symlink.
    pub const NOT_FOUND: &str = "not_found";
    /// `files.list` of a file or `files.read` of a directory.
    pub const WRONG_KIND: &str = "wrong_kind";
    /// The session is unknown or has no working directory.
    pub const UNKNOWN_SESSION: &str = "unknown_session";
    /// The root is not a directory inside the allowed roots.
    pub const OUTSIDE_ROOTS: &str = "outside_roots";
    /// The root directory does not exist.
    pub const MISSING_WORKSPACE: &str = "missing_workspace";
    /// The turn has no finalized pair of available checkpoints.
    pub const NO_DIFF: &str = "no_diff";
    /// The session has no recorded base commit (`session.base`).
    pub const NO_BASE: &str = "no_base";
    /// Too many searches run on this station.
    pub const BUSY: &str = "busy";
    /// The file system or `git` took too long.
    pub const TIMEOUT: &str = "timeout";
}

/// The directory that a query's paths are relative to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
#[ts(export_to = "files/")]
pub enum FileRoot {
    /// The working directory of this session.
    Session(SessionId),
    /// A workspace directory; it must be inside the allowed roots.
    Workspace(String),
}

/// `files.list` params.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "files/")]
pub struct FileListQuery {
    pub root: FileRoot,
    /// Relative to the root; empty for the root itself.
    #[serde(default)]
    pub path: String,
}

/// The kind of one directory entry. A symlink is shown, never followed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "files/")]
pub enum FileKind {
    File,
    Dir,
    Symlink,
    Other,
}

/// One entry of a [`FileListing`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "files/")]
pub struct FileEntry {
    pub name: String,
    pub kind: FileKind,
    /// Bytes, for a file.
    #[ts(type = "number | null")]
    pub size: Option<u64>,
}

/// The `files.list` answer: directories first, then by name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "files/")]
pub struct FileListing {
    /// The canonical root.
    pub root: String,
    /// The listed directory, relative to the root.
    pub path: String,
    pub entries: Vec<FileEntry>,
    /// More entries exist than the answer holds.
    pub truncated: bool,
}

/// `files.read` params.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "files/")]
pub struct FileReadQuery {
    pub root: FileRoot,
    pub path: String,
    /// At most [`MAX_READ_BYTES`]; that is also the default.
    #[serde(default)]
    #[ts(type = "number | null")]
    pub max_bytes: Option<u64>,
}

/// The `files.read` answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "files/")]
pub struct FileContent {
    pub path: String,
    /// The size of the whole file.
    #[ts(type = "number")]
    pub size: u64,
    /// The first bytes hold a NUL; `text` is then `None`.
    pub binary: bool,
    /// The content (lossy UTF-8) up to the byte limit.
    pub text: Option<String>,
    /// The file is longer than the answer.
    pub truncated: bool,
}

/// What `files.search` compares with the search text (case-insensitive,
/// fixed text, no pattern).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "files/")]
pub enum SearchMode {
    /// The relative path of each file.
    Name,
    /// Each line of each text file.
    Content,
}

/// `files.search` params.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "files/")]
pub struct FileSearchQuery {
    pub root: FileRoot,
    pub query: String,
    pub mode: SearchMode,
    /// At most [`MAX_SEARCH_HITS`]; that is also the default.
    #[serde(default)]
    pub max_hits: Option<u32>,
}

/// How the station searched.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "files/")]
pub enum SearchBackend {
    /// `git ls-files` / `git grep` (tracked and untracked, not ignored).
    Git,
    /// A bounded walk of a directory that is not a Git worktree.
    Walk,
}

/// One search hit; `line` and `text` only in [`SearchMode::Content`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "files/")]
pub struct SearchHit {
    pub path: String,
    pub line: Option<u32>,
    pub text: Option<String>,
}

/// The `files.search` answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "files/")]
pub struct FileSearch {
    pub hits: Vec<SearchHit>,
    /// More hits exist, or a limit (time, output, walk size) cut the search.
    pub truncated: bool,
    pub backend: SearchBackend,
}

/// `files.diff` params: the diff of one finalized turn, from its start
/// checkpoint to its completion checkpoint (L13: from checkpoint state, not
/// from provider diff events).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "files/")]
pub struct TurnDiffQuery {
    pub session_id: SessionId,
    pub turn_id: TurnId,
    /// At most [`MAX_DIFF_BYTES`]; that is also the default.
    #[serde(default)]
    #[ts(type = "number | null")]
    pub max_bytes: Option<u64>,
}

/// The `files.diff` answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "files/")]
pub struct TurnDiff {
    pub session_id: SessionId,
    pub turn_id: TurnId,
    /// The start checkpoint commit.
    pub from_commit: String,
    /// The completion checkpoint commit.
    pub to_commit: String,
    pub summary: DiffSummary,
    /// The patch (`git diff-tree -p --binary`), lossy UTF-8.
    pub patch: String,
    /// The patch was cut at the byte limit.
    pub truncated: bool,
}

/// `files.session_diff` params: the working tree of a session against the
/// commit that it started from (R1.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "files/")]
pub struct SessionDiffQuery {
    pub session_id: SessionId,
    /// At most [`MAX_DIFF_BYTES`]; that is also the default.
    #[serde(default)]
    #[ts(type = "number | null")]
    pub max_bytes: Option<u64>,
}

/// The `files.session_diff` answer: the same parts as [`TurnDiff`], against
/// the base commit of the session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "files/")]
pub struct SessionDiff {
    pub session_id: SessionId,
    /// The base commit (`session.base`).
    pub base_oid: String,
    pub summary: DiffSummary,
    /// The patch: tracked files against the base (`git diff`), then each
    /// untracked file as a new file. Lossy UTF-8.
    pub patch: String,
    /// The patch was cut at the byte limit, or a binary, a large or an
    /// excess untracked file was left out.
    pub truncated: bool,
}

/// Validates a relative path of a query and returns its segments.
///
/// # Errors
/// [`code::INVALID_PATH`] for an absolute path, `..`, `.`, an empty segment
/// inside the path, a backslash, NUL, or more than [`MAX_PATH_CHARS`]
/// characters; [`code::NOT_FOUND`] for a hidden segment (`.git` or a
/// credential directory).
pub fn path_segments(path: &str) -> Result<Vec<&str>, &'static str> {
    if path.chars().count() > MAX_PATH_CHARS
        || path.contains('\0')
        || path.contains('\\')
        || path.starts_with('/')
    {
        return Err(code::INVALID_PATH);
    }
    if path.is_empty() {
        return Ok(Vec::new());
    }
    let segments: Vec<&str> = path.trim_end_matches('/').split('/').collect();
    if segments
        .iter()
        .any(|s| s.is_empty() || *s == "." || *s == "..")
    {
        return Err(code::INVALID_PATH);
    }
    if segments.iter().any(|s| hidden_name(s)) {
        return Err(code::NOT_FOUND);
    }
    Ok(segments)
}

/// Directory names that the file service never shows or opens: the Git
/// store (it can hold remote credentials) and credential directories.
pub const HIDDEN_NAMES: &[&str] = &[
    ".git", ".ssh", ".aws", ".gnupg", ".netrc", ".kube", ".docker",
];

/// True when `name` is one of [`HIDDEN_NAMES`] (case-insensitive: macOS
/// file systems fold case).
#[must_use]
pub fn hidden_name(name: &str) -> bool {
    HIDDEN_NAMES.iter().any(|h| h.eq_ignore_ascii_case(name))
}

/// True when any segment of a relative `path` is hidden.
#[must_use]
pub fn hidden_path(path: &str) -> bool {
    path.split('/').any(hidden_name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_are_relative_without_dot_segments_or_hidden_names() {
        assert_eq!(path_segments(""), Ok(vec![]));
        assert_eq!(path_segments("src/main.rs"), Ok(vec!["src", "main.rs"]));
        assert_eq!(path_segments("src/"), Ok(vec!["src"]));
        for bad in [
            "/etc/passwd",
            "../x",
            "a/../b",
            "a//b",
            "./a",
            "a\\b",
            "a\0b",
        ] {
            assert_eq!(path_segments(bad), Err(code::INVALID_PATH), "{bad:?}");
        }
        let long = "a".repeat(MAX_PATH_CHARS + 1);
        assert_eq!(path_segments(&long), Err(code::INVALID_PATH));
        for hidden in [".git/config", "x/.SSH/id_ed25519", ".aws", "a/.Git"] {
            assert_eq!(path_segments(hidden), Err(code::NOT_FOUND), "{hidden:?}");
        }
        assert!(hidden_path("deep/.gnupg/key"));
        assert!(!hidden_path(".github/workflows/ci.yml"));
    }

    #[test]
    fn queries_refuse_unknown_fields_and_take_defaults() {
        let q: FileListQuery =
            serde_json::from_value(serde_json::json!({ "root": { "session": "s1" } })).unwrap();
        assert_eq!(q.path, "");
        assert_eq!(q.root, FileRoot::Session(SessionId::from("s1".to_owned())));
        let w: FileReadQuery = serde_json::from_value(
            serde_json::json!({ "root": { "workspace": "/w" }, "path": "a" }),
        )
        .unwrap();
        assert_eq!(w.max_bytes, None);
        assert!(
            serde_json::from_value::<FileListQuery>(
                serde_json::json!({ "root": { "session": "s1" }, "extra": 1 })
            )
            .is_err()
        );
        assert!(
            serde_json::from_value::<FileListQuery>(serde_json::json!({ "root": "/etc" })).is_err()
        );
    }
}
