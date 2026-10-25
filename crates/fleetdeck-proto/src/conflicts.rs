//! The conflict radar across stations (M3 6-7, DL-306; PLAN §16, §10
//! `file_touches`).
//!
//! The deck records each file that a session changes. Two sessions that
//! touch the same file of the same repository within
//! [`CONFLICT_WINDOW_MS`] are a conflict. Sessions on two stations share a
//! repository only through the normalized origin (a heuristic, PLAN §6.6);
//! without one origin, a repository is its work tree on one station.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::ids::{SessionId, StationId};
use crate::session::RepoInfo;

/// Touches older than this are not a conflict (v0 parity: 30 minutes).
pub const CONFLICT_WINDOW_MS: i64 = 30 * 60 * 1000;
/// The most conflicts in one answer.
pub const MAX_CONFLICTS: usize = 200;

/// One session's latest touch of a file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "conflicts/")]
pub struct FileTouch {
    pub station_id: StationId,
    pub session_id: SessionId,
    pub session_title: Option<String>,
    /// The path on the station.
    pub path: String,
    pub touched_at: i64,
}

/// One file that two or more sessions touched.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "conflicts/")]
pub struct FileConflict {
    /// The origin, or the work tree root, or `None` for a file outside a
    /// repository (then `file` is the absolute path on one station).
    pub repo: Option<String>,
    /// The path inside the repository.
    pub file: String,
    /// Newest first; at least two sessions.
    pub touches: Vec<FileTouch>,
}

/// The grouping key and the repository-relative path of a touched file.
/// `path` may be relative to `cwd` (a provider can report either form).
#[must_use]
pub fn touch_key(
    station: &StationId,
    cwd: Option<&str>,
    repo: Option<&RepoInfo>,
    path: &str,
) -> (String, Option<String>, String) {
    let abs = match cwd {
        Some(cwd) if !path.starts_with('/') => format!("{}/{path}", cwd.trim_end_matches('/')),
        _ => path.to_owned(),
    };
    if let Some(repo) = repo {
        let root = repo.root.trim_end_matches('/');
        if let Some(rel) = abs.strip_prefix(root).and_then(|r| r.strip_prefix('/')) {
            return match &repo.origin {
                Some(origin) => (
                    format!("origin:{origin}"),
                    Some(origin.clone()),
                    rel.to_owned(),
                ),
                None => (
                    format!("root:{station}:{root}"),
                    Some(root.to_owned()),
                    rel.to_owned(),
                ),
            };
        }
    }
    (format!("station:{station}"), None, abs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_in_a_repo_with_one_origin_groups_across_stations() {
        let repo = |root: &str, origin: Option<&str>| RepoInfo {
            root: root.to_owned(),
            origin: origin.map(str::to_owned),
        };
        let a = StationId::from("a");
        let b = StationId::from("b");
        let on_a = repo("/home/x/app", Some("github.com/o/app"));
        let on_b = repo("/srv/app", Some("github.com/o/app"));
        let ka = touch_key(
            &a,
            Some("/home/x/app/src"),
            Some(&on_a),
            "/home/x/app/src/lib.rs",
        );
        let kb = touch_key(&b, Some("/srv/app"), Some(&on_b), "src/lib.rs");
        assert_eq!(ka.0, kb.0);
        assert_eq!(ka.2, "src/lib.rs");
        assert_eq!(kb.2, "src/lib.rs");
        // Without an origin the work tree stays on its station.
        let local = repo("/srv/app", None);
        let kc = touch_key(&b, None, Some(&local), "/srv/app/src/lib.rs");
        let kd = touch_key(&a, None, Some(&local), "/srv/app/src/lib.rs");
        assert_ne!(kc.0, kd.0);
        // A file outside the repository keeps its absolute path.
        let ke = touch_key(&a, None, Some(&on_a), "/tmp/notes.md");
        assert_eq!(
            (ke.0.as_str(), ke.1, ke.2.as_str()),
            ("station:a", None, "/tmp/notes.md")
        );
    }
}
