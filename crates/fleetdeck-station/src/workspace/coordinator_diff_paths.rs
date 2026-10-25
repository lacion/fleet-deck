//! The paths of a session in a shared folder (as `files.session_diff`): the
//! files of its `file.changed` events and of its turn checkpoints.

use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};

use fleetdeck_proto::files::hidden_path;

use super::super::files::FileError;
use super::super::git::nul_fields;
use super::super::private_paths::PrivatePaths;
use super::super::{Git, Worktree};
use super::{LIST_MAX, internal};
use crate::store::RowSide;

/// What the store knows about the files of a session without a worktree.
pub(super) struct Shared {
    pub(super) events: Vec<String>,
    pub(super) turns: Vec<crate::store::TurnCheckpointRow>,
    pub(super) cwd: Option<PathBuf>,
}

/// The paths that a session without a worktree changed, from the top of the
/// worktree: its `file.changed` events and the files that differ between the
/// two sides of each of its turn checkpoints (as `files.session_diff`).
pub(super) async fn session_paths(
    git: &Git,
    worktree: &Worktree,
    session_dir: &Path,
    shared: &Shared,
    private: &PrivatePaths,
) -> Result<BTreeSet<String>, FileError> {
    let top = worktree.root();
    let mut paths: BTreeSet<String> = shared
        .events
        .iter()
        .filter_map(|raw| repo_path(raw, session_dir, shared.cwd.as_deref(), top))
        .collect();
    for row in &shared.turns {
        let (RowSide::Available(start), RowSide::Available(end)) = (&row.start, &row.end) else {
            continue;
        };
        if row.worktree_id.as_deref() != Some(worktree.id()) {
            continue;
        }
        let args = [
            "diff-tree",
            "-r",
            "--no-renames",
            "--name-only",
            "-z",
            start.as_str(),
            end.as_str(),
        ];
        let (status, out, cut) = git
            .bounded(&args, LIST_MAX)
            .await
            .map_err(|e| internal(&e))?;
        if status != 0 && !cut {
            continue;
        }
        paths.extend(
            nul_fields(&out)
                .into_iter()
                .filter_map(|field| std::str::from_utf8(field).ok())
                .filter(|path| !hidden_path(path))
                .map(str::to_owned),
        );
    }
    paths.retain(|path| !private.is_private_canonical(&top.join(path)));
    Ok(paths)
}

/// The path that a provider reported (absolute, or relative to the session
/// folder) as a path from `top`, the worktree root; `None` for a path
/// outside the worktree, with `..`, or hidden.
fn repo_path(raw: &str, session_dir: &Path, cwd: Option<&Path>, top: &Path) -> Option<String> {
    let path = Path::new(raw);
    let full = if path.is_absolute() {
        match (path.strip_prefix(top), cwd.map(|c| path.strip_prefix(c))) {
            (Ok(_), _) => path.to_path_buf(),
            (_, Some(Ok(rest))) => session_dir.join(rest),
            _ => return None,
        }
    } else {
        session_dir.join(path)
    };
    let mut parts = Vec::new();
    for part in full.strip_prefix(top).ok()?.components() {
        let Component::Normal(name) = part else {
            return None;
        };
        parts.push(name.to_str()?);
    }
    let joined = parts.join("/");
    (!joined.is_empty() && !hidden_path(&joined)).then_some(joined)
}
