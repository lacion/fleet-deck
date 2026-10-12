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
use crate::workspace::files::{self, FileError, Found, PER_FILE_HITS, clip, walk_search};
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

static SEARCHES: AtomicUsize = AtomicUsize::new(0);

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
    policy.workspace(&cwd).map_err(|e| match e {
        WorkspaceError::Missing(_) => FileError::new(code::MISSING_WORKSPACE, e.to_string()),
        WorkspaceError::Outside(_) => FileError::new(code::OUTSIDE_ROOTS, e.to_string()),
    })
}

async fn blocking<T: Send + 'static>(
    limit: Duration,
    work: impl FnOnce() -> Result<T, FileError> + Send + 'static,
) -> Result<T, FileError> {
    match tokio::time::timeout(limit, tokio::task::spawn_blocking(work)).await {
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
    blocking(READ_TIMEOUT, move || files::list(&root, &query.path)).await
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
    blocking(READ_TIMEOUT, move || {
        files::read(&root, &query.path, query.max_bytes)
    })
    .await
}

/// Releases a search slot on drop.
struct Slot;

impl Slot {
    fn take() -> Option<Self> {
        let mut now = SEARCHES.load(Ordering::Acquire);
        loop {
            if now >= MAX_SEARCHES {
                return None;
            }
            match SEARCHES.compare_exchange(now, now + 1, Ordering::AcqRel, Ordering::Acquire) {
                Ok(_) => return Some(Self),
                Err(seen) => now = seen,
            }
        }
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        SEARCHES.fetch_sub(1, Ordering::AcqRel);
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
    let Some(_slot) = Slot::take() else {
        return Err(FileError::new(
            code::BUSY,
            format!("{MAX_SEARCHES} searches run on this station; try again"),
        ));
    };
    let content = query.mode == SearchMode::Content;
    let options = GitOptions {
        timeout: SEARCH_TIMEOUT,
        ..git.clone()
    };
    let git = Git::new(&root, options);
    if is_worktree(&git).await {
        let found =
            tokio::time::timeout(SEARCH_TIMEOUT, git_search(&git, &query.query, content, max))
                .await
                .map_err(|_| FileError::new(code::TIMEOUT, "the search took too long"))??;
        return Ok(FileSearch {
            hits: found.hits,
            truncated: found.truncated,
            backend: SearchBackend::Git,
        });
    }
    let needle = query.query.to_lowercase();
    let found = blocking(SEARCH_TIMEOUT, move || {
        Ok(walk_search(&root, &needle, content, max))
    })
    .await?;
    Ok(FileSearch {
        hits: found.hits,
        truncated: found.truncated,
        backend: SearchBackend::Walk,
    })
}

async fn is_worktree(git: &Git) -> bool {
    matches!(
        git.bounded(&["rev-parse", "--is-inside-work-tree"], 64).await,
        Ok((0, out, false)) if out.starts_with(b"true")
    )
}

async fn git_search(git: &Git, text: &str, content: bool, max: usize) -> Result<Found, FileError> {
    let failed = |e: crate::workspace::GitError| FileError::new("internal", e.to_string());
    let mut found = Found::default();
    if !content {
        let (_, out, cut) = git
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
            .map_err(failed)?;
        found.truncated = cut;
        let needle = text.to_lowercase();
        let mut paths: Vec<&str> = crate::workspace::git::nul_fields(&out)
            .into_iter()
            .filter_map(|field| std::str::from_utf8(field).ok())
            .filter(|path| !hidden_path(path) && path.to_lowercase().contains(&needle))
            .collect();
        // Sorted, and `--cached` lists a conflicted path once per stage.
        paths.sort_unstable();
        paths.dedup();
        for path in paths {
            let hit = SearchHit {
                path: path.to_owned(),
                line: None,
                text: None,
            };
            if !found.push(hit, max) {
                break;
            }
        }
        return Ok(found);
    }
    let per_file = format!("--max-count={PER_FILE_HITS}");
    let args = [
        "grep",
        "-I",
        "-n",
        "-i",
        "-F",
        "-z",
        "--no-color",
        "--untracked",
        &per_file,
        "-e",
        text,
        "--",
    ];
    let (status, out, cut) = git
        .bounded(&args, SEARCH_OUTPUT_MAX)
        .await
        .map_err(failed)?;
    // `git grep` exits 1 when nothing matches.
    if status != 0 && status != 1 && !cut {
        return Err(FileError::new(
            "internal",
            format!("git grep failed ({status})"),
        ));
    }
    found.truncated = cut;
    for line in out.split(|b| *b == b'\n') {
        // `-z`: path NUL line NUL text.
        let mut parts = line.splitn(3, |b| *b == 0);
        let (Some(path), Some(number), Some(rest)) = (parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        let (Ok(path), Ok(number)) = (std::str::from_utf8(path), std::str::from_utf8(number))
        else {
            continue;
        };
        if hidden_path(path) {
            continue;
        }
        let hit = SearchHit {
            path: path.to_owned(),
            line: number.parse().ok(),
            text: Some(clip(&String::from_utf8_lossy(rest))),
        };
        if !found.push(hit, max) {
            break;
        }
    }
    Ok(found)
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
