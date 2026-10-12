//! The link side of the read-only file service (PLAN §6.6, M3 DL-088):
//! `files.list`, `files.read`, `files.search` and `files.diff`.
//!
//! The station resolves every root itself: a session's working directory
//! (from the session summary) or a workspace directory, and both pass the
//! allowed-roots policy again on every call (fail closed). Each call has a
//! deadline; searches are capped per station.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use fleetdeck_proto::files::{
    FileContent, FileListQuery, FileListing, FileReadQuery, FileRoot, FileSearch, FileSearchQuery,
    MAX_DIFF_BYTES, MAX_QUERY_CHARS, MAX_SEARCH_HITS, MIN_QUERY_CHARS, SearchBackend, SearchHit,
    SearchMode, TurnDiff, TurnDiffQuery, code, hidden_path,
};

use super::policy::{Policy, WorkspaceError};
use crate::store::{RowSide, RowState, Store};
use crate::workspace::files::{self, FileError, Found, walk_search};
use crate::workspace::{CheckpointKey, Git, GitOptions, Phase, Worktree};

/// The longest a list or read may take.
pub const READ_TIMEOUT: Duration = Duration::from_secs(5);
/// The longest a search may take.
pub const SEARCH_TIMEOUT: Duration = Duration::from_secs(10);
/// The longest a diff may take.
pub const DIFF_TIMEOUT: Duration = Duration::from_secs(15);
/// The most searches that run at the same time on one station.
pub const MAX_SEARCHES: usize = 4;
/// The most `git` output that one search reads.
pub const SEARCH_OUTPUT_MAX: usize = 4 * 1024 * 1024;

/// The most lists and reads that run at the same time on one station.
pub const MAX_READS: usize = 16;

static SEARCHES: AtomicUsize = AtomicUsize::new(0);
static READS: AtomicUsize = AtomicUsize::new(0);

/// The canonical root of a query.
///
/// # Errors
/// `unknown_session`, `missing_workspace` or `outside_roots`.
pub async fn root(store: &Store, policy: &Policy, root: &FileRoot) -> Result<PathBuf, FileError> {
    let cwd = match root {
        FileRoot::Workspace(path) => path.clone(),
        FileRoot::Session(id) => {
            let summary = store
                .summary(id.clone())
                .await
                .map_err(|e| FileError::new("internal", e.to_string()))?;
            summary.and_then(|s| s.cwd).ok_or_else(|| {
                FileError::new(
                    code::UNKNOWN_SESSION,
                    format!("session `{id}` is unknown or has no working directory"),
                )
            })?
        }
    };
    let canonical = policy.workspace(&cwd).map_err(|e| match e {
        WorkspaceError::Missing(_) => FileError::new(code::MISSING_WORKSPACE, e.to_string()),
        WorkspaceError::Outside(_) => FileError::new(code::OUTSIDE_ROOTS, e.to_string()),
    })?;
    refuse_private(policy, &canonical)?;
    Ok(canonical)
}

/// Refuses a root that is a private directory (`.git` or a credential
/// directory) or lies inside one, for both root forms (finding 2 of
/// sa-13931f53).
fn refuse_private(policy: &Policy, root: &Path) -> Result<(), FileError> {
    if files::private_root(root, policy.allowed_roots()) {
        return Err(FileError::new(
            code::OUTSIDE_ROOTS,
            "the root is a private directory or lies inside one",
        ));
    }
    Ok(())
}

/// Runs `work` on the blocking pool with a deadline. `slot` moves into
/// the worker and is released only when the worker ends: a timeout
/// answers the caller, but file-system calls cannot be cancelled, so the
/// worker may still run until its own bounds stop it, and its slot stays
/// taken until then (finding 5 of sa-13931f53). The slots therefore bound
/// the blocking workers that a slow file system can pile up.
async fn blocking<T: Send + 'static>(
    limit: Duration,
    slot: Slot,
    work: impl FnOnce() -> Result<T, FileError> + Send + 'static,
) -> Result<T, FileError> {
    let task = tokio::task::spawn_blocking(move || {
        let _slot = slot;
        work()
    });
    match tokio::time::timeout(limit, task).await {
        Ok(Ok(result)) => result,
        Ok(Err(e)) => Err(FileError::new("internal", e.to_string())),
        Err(_) => Err(FileError::new(
            code::TIMEOUT,
            "the file system took too long",
        )),
    }
}

/// `files.list`.
///
/// # Errors
/// [`FileError`].
pub async fn list(
    store: &Store,
    policy: &Policy,
    query: FileListQuery,
) -> Result<FileListing, FileError> {
    let root = root(store, policy, &query.root).await?;
    let slot = Slot::take(&READS, MAX_READS)?;
    blocking(READ_TIMEOUT, slot, move || files::list(&root, &query.path)).await
}

/// `files.read`.
///
/// # Errors
/// [`FileError`].
pub async fn read(
    store: &Store,
    policy: &Policy,
    query: FileReadQuery,
) -> Result<FileContent, FileError> {
    let root = root(store, policy, &query.root).await?;
    let slot = Slot::take(&READS, MAX_READS)?;
    blocking(READ_TIMEOUT, slot, move || {
        files::read(&root, &query.path, query.max_bytes)
    })
    .await
}

/// One taken worker slot of a counter; released on drop.
struct Slot(&'static AtomicUsize);

impl Slot {
    fn take(counter: &'static AtomicUsize, max: usize) -> Result<Self, FileError> {
        let mut now = counter.load(Ordering::Acquire);
        loop {
            if now >= max {
                return Err(FileError::new(
                    code::BUSY,
                    format!("{max} file operations of this kind run on this station; try again"),
                ));
            }
            match counter.compare_exchange(now, now + 1, Ordering::AcqRel, Ordering::Acquire) {
                Ok(_) => return Ok(Self(counter)),
                Err(seen) => now = seen,
            }
        }
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

/// `files.search`: case-insensitive fixed text in names or content. A Git
/// worktree is searched with `git` (tracked and untracked files, not
/// ignored ones); any other directory with a bounded walk.
///
/// # Errors
/// [`FileError`].
pub async fn search(
    store: &Store,
    policy: &Policy,
    git: &GitOptions,
    query: FileSearchQuery,
) -> Result<FileSearch, FileError> {
    let chars = query.query.chars().count();
    if !(MIN_QUERY_CHARS..=MAX_QUERY_CHARS).contains(&chars) || query.query.contains('\0') {
        return Err(FileError::new(
            code::INVALID_QUERY,
            format!("a search text has {MIN_QUERY_CHARS} to {MAX_QUERY_CHARS} characters"),
        ));
    }
    let root = root(store, policy, &query.root).await?;
    let max = query
        .max_hits
        .unwrap_or(MAX_SEARCH_HITS)
        .clamp(1, MAX_SEARCH_HITS) as usize;
    let slot = Slot::take(&SEARCHES, MAX_SEARCHES)?;
    let content = query.mode == SearchMode::Content;
    let needle = query.query.to_lowercase();
    let options = GitOptions {
        timeout: SEARCH_TIMEOUT,
        ..git.clone()
    };
    // Git runs in the directory that the root descriptor holds, never in
    // the root by name: a rename of the root or an ancestor during the
    // search cannot move it (DL-092). Git only names the candidate files;
    // their content is read through the same descriptor, one component at
    // a time without following a symlink, so a directory swapped for a
    // symlink below the root cannot redirect the search (DL-098). Without
    // a pinned path (macOS) the search uses the descriptor walk.
    let base = files::open_root(&root)
        .map_err(|_| FileError::new(code::NOT_FOUND, "the root is not found"))?;
    let listed = match files::pinned_dir(&base) {
        Some(pinned) => {
            let git = Git::new(pinned, options);
            if is_worktree(&git).await {
                Some(
                    tokio::time::timeout(SEARCH_TIMEOUT, candidates(&git))
                        .await
                        .map_err(|_| FileError::new(code::TIMEOUT, "the search took too long"))??,
                )
            } else {
                None
            }
        }
        None => None,
    };
    let Some((paths, cut)) = listed else {
        drop(base);
        let found = blocking(SEARCH_TIMEOUT, slot, move || {
            Ok(walk_search(&root, &needle, content, max))
        })
        .await?;
        return Ok(FileSearch {
            hits: found.hits,
            truncated: found.truncated,
            backend: SearchBackend::Walk,
        });
    };
    let mut found = if content {
        blocking(SEARCH_TIMEOUT, slot, move || {
            Ok(files::grep_paths(&base, &paths, &needle, max))
        })
        .await?
    } else {
        drop(base);
        let mut found = Found::default();
        for path in paths
            .into_iter()
            .filter(|p| p.to_lowercase().contains(&needle))
        {
            let hit = SearchHit {
                path,
                line: None,
                text: None,
            };
            if !found.push(hit, max) {
                break;
            }
        }
        found
    };
    found.truncated |= cut;
    Ok(FileSearch {
        hits: found.hits,
        truncated: found.truncated,
        backend: SearchBackend::Git,
    })
}

async fn is_worktree(git: &Git) -> bool {
    matches!(
        git.bounded(&["rev-parse", "--is-inside-work-tree"], 64).await,
        Ok((0, out, false)) if out.starts_with(b"true")
    )
}

/// The tracked and untracked (not ignored) files that Git lists, sorted,
/// once each and without hidden paths; true when the listing was cut.
async fn candidates(git: &Git) -> Result<(Vec<String>, bool), FileError> {
    let (status, out, cut) = git
        .bounded(
            &[
                "ls-files",
                "-z",
                "--cached",
                "--others",
                "--exclude-standard",
            ],
            SEARCH_OUTPUT_MAX,
        )
        .await
        .map_err(|e| FileError::new("internal", e.to_string()))?;
    if status != 0 && !cut {
        return Err(FileError::new(
            "internal",
            format!("git ls-files failed ({status})"),
        ));
    }
    let mut paths: Vec<String> = crate::workspace::git::nul_fields(&out)
        .into_iter()
        .filter_map(|field| std::str::from_utf8(field).ok())
        .filter(|path| !hidden_path(path))
        .map(str::to_owned)
        .collect();
    // Sorted, and `--cached` lists a conflicted path once per stage.
    paths.sort_unstable();
    paths.dedup();
    Ok((paths, cut))
}

/// `files.diff`: the patch of one finalized turn from its start checkpoint
/// to its completion checkpoint. Provider turn ids restart in each process;
/// the newest finalized turn with that id is used.
///
/// # Errors
/// [`FileError`]: `unknown_session`, `no_diff`, `outside_roots`, `timeout`.
pub async fn diff(
    store: &Store,
    policy: &Policy,
    git: &GitOptions,
    query: TurnDiffQuery,
) -> Result<TurnDiff, FileError> {
    let internal = |e: &dyn std::fmt::Display| FileError::new("internal", e.to_string());
    if store
        .summary(query.session_id.clone())
        .await
        .map_err(|e| internal(&e))?
        .is_none()
    {
        return Err(FileError::new(
            code::UNKNOWN_SESSION,
            format!("session `{}` is unknown", query.session_id),
        ));
    }
    let rows = store
        .turn_checkpoints(query.session_id.clone())
        .await
        .map_err(|e| internal(&e))?;
    let no_diff = |why: &str| FileError::new(code::NO_DIFF, why.to_owned());
    let row = rows
        .iter()
        .rev()
        .find(|r| r.state == RowState::Finalized && r.turn_id.as_ref() == Some(&query.turn_id))
        .ok_or_else(|| no_diff("no finalized turn of this session has that id"))?;
    let (RowSide::Available(from), RowSide::Available(to)) = (&row.start, &row.end) else {
        return Err(no_diff("the turn has no pair of available checkpoints"));
    };
    let Some(worktree_root) = row.worktree_root.as_deref() else {
        return Err(no_diff("the turn has no worktree"));
    };
    let canonical = policy.workspace(worktree_root).map_err(|e| match e {
        WorkspaceError::Missing(_) => no_diff("the worktree is gone"),
        WorkspaceError::Outside(_) => FileError::new(code::OUTSIDE_ROOTS, e.to_string()),
    })?;
    refuse_private(policy, &canonical)?;
    let options = GitOptions {
        timeout: DIFF_TIMEOUT,
        ..git.clone()
    };
    let max = query
        .max_bytes
        .unwrap_or(MAX_DIFF_BYTES)
        .min(MAX_DIFF_BYTES) as usize;
    let key = |phase| CheckpointKey {
        incarnation: store.incarnation().as_str().to_owned(),
        session: query.session_id.as_str().to_owned(),
        turn: row.app_turn.as_str().to_owned(),
        phase,
    };
    let (start_key, end_key) = (key(Phase::Start), key(Phase::Completion));
    let work = async {
        let worktree = Worktree::open(Path::new(&canonical), options)
            .await
            .map_err(|_| no_diff("the worktree cannot be opened"))?;
        let start = worktree.read(&start_key).await.ok().flatten();
        let end = worktree.read(&end_key).await.ok().flatten();
        let (Some(start), Some(end)) = (start, end) else {
            return Err(no_diff("a checkpoint ref is missing"));
        };
        if &start.commit != from || &end.commit != to {
            return Err(no_diff("a checkpoint ref changed"));
        }
        let summary = worktree
            .diff_summary(&start, &end)
            .await
            .map_err(|e| internal(&e))?;
        let patch = worktree
            .diff(&start, &end, max)
            .await
            .map_err(|e| internal(&e))?;
        Ok(TurnDiff {
            session_id: query.session_id.clone(),
            turn_id: query.turn_id.clone(),
            from_commit: start.commit,
            to_commit: end.commit,
            summary,
            patch: patch.patch,
            truncated: patch.truncated,
        })
    };
    tokio::time::timeout(DIFF_TIMEOUT, work)
        .await
        .map_err(|_| FileError::new(code::TIMEOUT, "the diff took too long"))?
}

#[cfg(test)]
#[path = "files_tests.rs"]
mod files_tests;

#[cfg(test)]
#[path = "files_review_tests.rs"]
mod files_review_tests;

#[cfg(test)]
#[path = "files_review2_tests.rs"]
mod files_review2_tests;

#[cfg(test)]
#[path = "files_review3_tests.rs"]
mod files_review3_tests;
