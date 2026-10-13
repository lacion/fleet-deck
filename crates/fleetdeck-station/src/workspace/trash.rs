//! The file work of the worktree trash (DL-150): move a worktree into the
//! trash folder beside it, move it back, or delete it for good.
//!
//! Every run starts with the **claim guard**: it reads the `worktree_ops`
//! row and goes on only when the row is in the state that the action
//! expects (`trashing`, `restoring`, `purging`). A stale effect (a release,
//! a restart re-run, a second command) ends `superseded` and changes
//! nothing. The folder is identified by its device and inode number from
//! the trash decision, never by its path alone: a replaced folder is never
//! moved or deleted. Branches are never deleted.
//!
//! The run ends `Confirmed` when it settled, also when it set the row to
//! `failed`: the row and the inventory carry the outcome.

use std::os::unix::fs::{DirBuilderExt, MetadataExt};
use std::path::{Path, PathBuf};

use crate::manager::policy::Policy;
use crate::manager::trash::{TRASH_DIR, shell_in};
use crate::shell::Shells;
use crate::store::worktree_ops::{
    PHASE_PURGE, PHASE_RESTORE, PHASE_TRASH, PURGED, RESTORED, TRASHED, expected_state,
};
use crate::store::{Dispatch, Store, StoreError, WorktreeAction, WorktreeOp};
use crate::workspace::worktrees::scan;
use crate::workspace::{Git, GitOptions};

/// The reason of an effect whose row moved on.
pub const SUPERSEDED: &str = "superseded";

/// What a step decided.
enum Step {
    /// Go on.
    Go,
    /// The row ends `failed` in this phase, with this message.
    Fail(&'static str, String),
}

type Res<T> = Result<T, StoreError>;

/// Runs one worktree effect (see the module documentation).
pub async fn run(
    store: &Store,
    policy: &Policy,
    options: &GitOptions,
    shells: Option<&Shells>,
    op_id: &str,
    action: WorktreeAction,
) -> Dispatch {
    let from = expected_state(action);
    let row = match store.worktree_op_claim(op_id.to_owned(), from).await {
        Ok(Some(row)) => row,
        Ok(None) => return Dispatch::NotDispatched(SUPERSEDED.into()),
        Err(e) => return Dispatch::Unknown(format!("store: {e}")),
    };
    let step = match action {
        WorktreeAction::Trash => trash(store, policy, options, shells, &row).await,
        WorktreeAction::Restore => restore(options, &row).await,
        WorktreeAction::Purge => purge(options, shells, &row).await,
    };
    let settled = match step {
        Ok(Step::Go) => {
            let to = match action {
                WorktreeAction::Trash => TRASHED,
                WorktreeAction::Restore => RESTORED,
                WorktreeAction::Purge => PURGED,
            };
            store
                .worktree_op_finish(op_id.to_owned(), from, to)
                .await
                .map(|_| ())
        }
        Ok(Step::Fail(phase, message)) => {
            tracing::warn!(op = op_id, phase, %message, "worktree operation failed");
            store
                .worktree_op_fail(op_id.to_owned(), from, phase, message)
                .await
                .map(|_| ())
        }
        Err(e) => Err(e),
    };
    match settled {
        Ok(()) => Dispatch::Confirmed,
        Err(e) => Dispatch::Unknown(format!("store: {e}")),
    }
}

/// The device and inode of `path` (without following a link); `None` when
/// nothing is there.
fn identity(path: &str) -> Result<Option<(u64, u64)>, String> {
    match std::fs::symlink_metadata(path) {
        Ok(m) => Ok(Some((m.dev(), m.ino()))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("`{path}`: {e}")),
    }
}

fn changed(row: &WorktreeOp) -> String {
    format!(
        "the worktree folder changed during the move; check {} and {}",
        row.original_path, row.trash_path
    )
}

/// One entry of `git worktree list --porcelain`.
#[derive(Debug, PartialEq, Eq)]
struct Entry {
    path: String,
    locked: bool,
}

fn parse_list(text: &str) -> Vec<Entry> {
    let mut out: Vec<Entry> = Vec::new();
    for line in text.lines() {
        if let Some(path) = line.strip_prefix("worktree ") {
            out.push(Entry {
                path: path.to_owned(),
                locked: false,
            });
        } else if (line == "locked" || line.starts_with("locked "))
            && let Some(last) = out.last_mut()
        {
            last.locked = true;
        }
    }
    out
}

fn same(a: &str, b: &str) -> bool {
    let (pa, pb) = (PathBuf::from(a), PathBuf::from(b));
    pa.canonicalize().unwrap_or(pa) == pb.canonicalize().unwrap_or(pb)
}

fn os_error(path: &str, e: &std::io::Error) -> String {
    format!("`{path}`: {e}")
}

/// What step 0 reads from Git.
struct GitFacts {
    main: String,
    admin_dir: String,
    branch: Option<String>,
}

/// Step 0 of the trash: the Git facts of the worktree. `at` is the folder
/// that has the recorded identity (the original place, or the trash after
/// an interrupted move).
async fn git_facts(options: &GitOptions, row: &WorktreeOp, at: &str) -> Result<GitFacts, String> {
    let git = Git::new(at, options.clone());
    let common = scan::text(&git, &["rev-parse", "--git-common-dir"])
        .await
        .map_err(|e| e.to_string())?;
    let common = Path::new(common.trim());
    let common = if common.is_absolute() {
        common.to_path_buf()
    } else {
        Path::new(at).join(common)
    };
    let common = common
        .canonicalize()
        .map_err(|e| os_error(&common.to_string_lossy(), &e))?;
    let list = scan::text(&git, &["worktree", "list", "--porcelain"])
        .await
        .map_err(|e| e.to_string())?;
    let entries = parse_list(&list);
    let main = entries
        .first()
        .ok_or_else(|| "git lists no worktree".to_owned())?;
    if same(&main.path, &row.original_path) || same(&main.path, &row.trash_path) {
        return Err("this is the main checkout, not a linked worktree".to_owned());
    }
    let listed = entries
        .iter()
        .find(|e| same(&e.path, &row.original_path) || same(&e.path, &row.trash_path))
        .ok_or_else(|| "git worktree list does not name this folder".to_owned())?;
    if listed.locked {
        return Err("the worktree is locked in Git".to_owned());
    }
    let dotgit = Path::new(at).join(".git");
    let text =
        std::fs::read_to_string(&dotgit).map_err(|e| os_error(&dotgit.to_string_lossy(), &e))?;
    let target = text
        .lines()
        .find_map(|l| l.strip_prefix("gitdir:"))
        .map(str::trim)
        .ok_or_else(|| "the .git file of the worktree names no gitdir".to_owned())?;
    let target = Path::new(target);
    let target = if target.is_absolute() {
        target.to_path_buf()
    } else {
        Path::new(at).join(target)
    };
    let admin = target
        .canonicalize()
        .map_err(|e| os_error(&target.to_string_lossy(), &e))?;
    if !admin.starts_with(common.join("worktrees")) {
        return Err("the Git record of the worktree is outside the repository".to_owned());
    }
    let branch = scan::text(&git, &["rev-parse", "--abbrev-ref", "HEAD"])
        .await
        .ok()
        .map(|t| t.trim().to_owned())
        .filter(|b| !b.is_empty() && b != "HEAD");
    Ok(GitFacts {
        main: main.path.clone(),
        admin_dir: admin.to_string_lossy().into_owned(),
        branch,
    })
}

/// `git -C <main> worktree repair -- <path>`.
async fn repair(options: &GitOptions, main: &str, path: &str) -> Result<(), String> {
    let git = Git::new(main, options.clone());
    scan::run(&git, &["worktree", "repair", "--", path])
        .await
        .map(|_| ())
        .map_err(|e| e.to_string())
}

async fn trash(
    store: &Store,
    policy: &Policy,
    options: &GitOptions,
    shells: Option<&Shells>,
    row: &WorktreeOp,
) -> Res<Step> {
    let rec = (row.dev, row.ino);
    let (orig, moved) = match (identity(&row.original_path), identity(&row.trash_path)) {
        (Ok(o), Ok(t)) => (o, t),
        (Err(m), _) | (_, Err(m)) => return Ok(Step::Fail(PHASE_TRASH, m)),
    };
    if row.main.is_empty() {
        // Step 0: nothing has moved yet, or an earlier run moved the folder
        // before the facts were stored (the same code reads either place).
        if shell_in(shells, &row.original_path) || shell_in(shells, &row.trash_path) {
            let m = "a shell is open in this worktree; close it first";
            return Ok(Step::Fail(PHASE_TRASH, m.to_owned()));
        }
        if policy.workspace(&row.original_path).is_err() && orig.is_some() {
            let m = "the worktree is outside the allowed roots";
            return Ok(Step::Fail(PHASE_TRASH, m.to_owned()));
        }
        let at = match (orig, moved) {
            (Some(o), None) if o == rec => &row.original_path,
            (None, Some(t)) if t == rec => &row.trash_path,
            _ => return Ok(Step::Fail(PHASE_TRASH, changed(row))),
        };
        let facts = match git_facts(options, row, at).await {
            Ok(f) => f,
            Err(m) => return Ok(Step::Fail(PHASE_TRASH, m)),
        };
        let stored = store
            .worktree_op_set_git(row.op_id.clone(), facts.main, facts.admin_dir, facts.branch)
            .await?;
        if !stored {
            return Ok(Step::Fail(PHASE_TRASH, "the operation changed".to_owned()));
        }
    }
    let Some(row) = store.worktree_op(row.op_id.clone()).await? else {
        return Ok(Step::Fail(PHASE_TRASH, "the operation is gone".to_owned()));
    };
    // Step 1: the trash folder.
    if let Err(m) = ensure_trash_dir(&row.trash_path) {
        return Ok(Step::Fail(PHASE_TRASH, m));
    }
    // Step 2: reconcile by identity, then move.
    match (orig, moved) {
        (Some(o), None) if o == rec => {
            if let Err(e) = std::fs::rename(&row.original_path, &row.trash_path) {
                let m = format!(
                    "the worktree was not moved ({e}); the trash folder must be on the same disk"
                );
                return Ok(Step::Fail(PHASE_TRASH, m));
            }
        }
        (None, Some(t)) if t == rec => {}
        _ => return Ok(Step::Fail(PHASE_TRASH, changed(&row))),
    }
    // Step 3: Git learns the new place.
    match repair(options, &row.main, &row.trash_path).await {
        Ok(()) => Ok(Step::Go),
        Err(m) => Ok(Step::Fail(PHASE_TRASH, format!("git worktree repair: {m}"))),
    }
}

/// The trash folder beside the worktree: created (mode 0700) when absent;
/// a link or a file in its place is refused.
fn ensure_trash_dir(trash_path: &str) -> Result<(), String> {
    let dir = Path::new(trash_path)
        .parent()
        .filter(|p| p.file_name().is_some_and(|n| n == TRASH_DIR))
        .ok_or_else(|| "the trash path is not inside a .fleetdeck-trash folder".to_owned())?;
    match std::fs::symlink_metadata(dir) {
        Ok(m) if m.file_type().is_dir() => Ok(()),
        Ok(_) => Err(format!(
            "`{}` exists and is not a folder",
            dir.to_string_lossy()
        )),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => std::fs::DirBuilder::new()
            .mode(0o700)
            .create(dir)
            .map_err(|e| os_error(&dir.to_string_lossy(), &e)),
        Err(e) => Err(os_error(&dir.to_string_lossy(), &e)),
    }
}

async fn restore(options: &GitOptions, row: &WorktreeOp) -> Res<Step> {
    let rec = (row.dev, row.ino);
    let (orig, moved) = match (identity(&row.original_path), identity(&row.trash_path)) {
        (Ok(o), Ok(t)) => (o, t),
        (Err(m), _) | (_, Err(m)) => return Ok(Step::Fail(PHASE_RESTORE, m)),
    };
    match (orig, moved) {
        (Some(o), None) if o == rec => {}
        (None, Some(t)) if t == rec => {
            if let Err(e) = std::fs::rename(&row.trash_path, &row.original_path) {
                let m = format!("the worktree was not moved back ({e})");
                return Ok(Step::Fail(PHASE_RESTORE, m));
            }
        }
        _ => {
            let m = format!(
                "the worktree folder is not where Fleet Deck left it; check {} and {}",
                row.original_path, row.trash_path
            );
            return Ok(Step::Fail(PHASE_RESTORE, m));
        }
    }
    // A move that failed before Git was read left the folder in place and
    // Git untouched: nothing to repair.
    if row.main.is_empty() {
        return Ok(Step::Go);
    }
    match repair(options, &row.main, &row.original_path).await {
        Ok(()) => Ok(Step::Go),
        Err(m) => Ok(Step::Fail(
            PHASE_RESTORE,
            format!("git worktree repair: {m}"),
        )),
    }
}

/// The common Git directory of the main checkout.
async fn common_dir(options: &GitOptions, main: &str) -> Result<PathBuf, String> {
    let dotgit = Path::new(main).join(".git");
    let dir = if dotgit.is_dir() {
        dotgit
    } else {
        let git = Git::new(main, options.clone());
        let text = scan::text(&git, &["rev-parse", "--git-common-dir"])
            .await
            .map_err(|e| e.to_string())?;
        let p = PathBuf::from(text.trim());
        if p.is_absolute() {
            p
        } else {
            Path::new(main).join(p)
        }
    };
    dir.canonicalize()
        .map_err(|e| os_error(&dir.to_string_lossy(), &e))
}

async fn purge(options: &GitOptions, shells: Option<&Shells>, row: &WorktreeOp) -> Res<Step> {
    if shell_in(shells, &row.trash_path) || shell_in(shells, &row.original_path) {
        let m = "a shell is open in this worktree; close it first";
        return Ok(Step::Fail(PHASE_PURGE, m.to_owned()));
    }
    if Path::new(&row.trash_path)
        .parent()
        .and_then(Path::file_name)
        .is_none_or(|n| n != TRASH_DIR)
    {
        let m = "the trash path is not inside a .fleetdeck-trash folder";
        return Ok(Step::Fail(PHASE_PURGE, m.to_owned()));
    }
    // (1) The folder: only the one with the recorded identity.
    match identity(&row.trash_path) {
        Err(m) => return Ok(Step::Fail(PHASE_PURGE, m)),
        Ok(Some(id)) if id == (row.dev, row.ino) => {
            let path = row.trash_path.clone();
            let removed = tokio::task::spawn_blocking(move || std::fs::remove_dir_all(&path))
                .await
                .map_err(|e| std::io::Error::other(e.to_string()))
                .and_then(|r| r);
            if let Err(e) = removed {
                let m = format!("the folder was not fully deleted: {e}");
                return Ok(Step::Fail(PHASE_PURGE, m));
            }
        }
        Ok(Some(_)) => {
            let m = "the trash folder changed; nothing was deleted";
            return Ok(Step::Fail(PHASE_PURGE, m.to_owned()));
        }
        Ok(None) => {}
    }
    // (2) The Git record: only when it is still the one of this folder.
    if !row.admin_dir.is_empty() && Path::new(&row.admin_dir).exists() {
        let record_changed = || {
            Ok(Step::Fail(
                PHASE_PURGE,
                "the Git worktree record changed".to_owned(),
            ))
        };
        let Ok(common) = common_dir(options, &row.main).await else {
            return record_changed();
        };
        let Ok(admin) = Path::new(&row.admin_dir).canonicalize() else {
            return record_changed();
        };
        if !admin.starts_with(common.join("worktrees")) {
            return record_changed();
        }
        let names = std::fs::read_to_string(admin.join("gitdir"))
            .ok()
            .map(|t| PathBuf::from(t.trim()))
            .and_then(|p| p.parent().map(Path::to_path_buf));
        if names.as_deref() != Some(Path::new(&row.trash_path)) {
            return record_changed();
        }
        let dir = row.admin_dir.clone();
        let removed = tokio::task::spawn_blocking(move || std::fs::remove_dir_all(&dir))
            .await
            .map_err(|e| std::io::Error::other(e.to_string()))
            .and_then(|r| r);
        if let Err(e) = removed {
            let m = format!("the Git worktree record was not deleted: {e}");
            return Ok(Step::Fail(PHASE_PURGE, m));
        }
    }
    Ok(Step::Go)
}

#[cfg(test)]
#[path = "trash_tests.rs"]
mod tests;
