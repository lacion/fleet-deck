//! `files.session_diff` (R1.2): the working tree of a session against the
//! commit that it started from (`summary.base`, R1.1).
//!
//! The tracked content comes from the same hardened path as the turn
//! checkpoints: a snapshot tree through a private index (no clean filter,
//! no external diff, no text conversion), then `git diff-tree` of the base
//! commit and that tree, with the private paths left out
//! ([`visible_pathspec`]) and the station's own private paths
//! ([`PrivatePaths`], DL-173): git excludes them from the tracked patch, and
//! an untracked file below one is not listed or read. Each untracked file (not ignored) is read by the
//! file service (no symlink is followed) and rendered as a new file; a
//! binary or a large one is a header line only.
//!
//! Limits: the patch has at most [`MAX_DIFF_BYTES`] bytes. A cut, a left-out
//! untracked file (binary excluded, large, over [`MAX_UNTRACKED`]) sets
//! `truncated`. When the tracked patch is cut, the untracked files are not
//! listed. The summary counts the untracked files that the patch shows.
//!
//! A session without a worktree of its own works in a shared folder, where
//! other people also change files. Its diff then keeps only the paths that
//! the session changed (`scope = session_files`, DL-159): the paths of its
//! `file.changed` events and of its turn checkpoints. A session that changed
//! no file has an empty diff.

use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use fleetdeck_proto::event::DiffSummary;
use fleetdeck_proto::files::{
    DiffScope, FileContent, FileRoot, MAX_DIFF_BYTES, MAX_READ_BYTES, SessionDiff,
    SessionDiffQuery, code, hidden_path,
};

use super::checkpoint::{Limits, Snapshot, Worktree};
use super::files::{self, FileError};
use super::git::nul_fields;
use super::private_paths::PrivatePaths;
use super::restore::visible_pathspec;
use super::{Git, GitOptions};
use crate::manager::policy::Policy;
use crate::store::{RowSide, Store, TurnCheckpointRow};

/// The longest a session diff may take.
pub const DIFF_TIMEOUT: Duration = Duration::from_secs(15);
/// At most two session diffs run at once per station (NV-34): each one can
/// stage up to 512 MiB through `git hash-object -w`. The objects that a diff
/// writes are not removed; Git gc removes the unreachable ones.
static DIFFS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(2);
/// The most untracked files that one diff renders.
pub const MAX_UNTRACKED: usize = 200;
/// The most `git` output that a file listing or a numstat reads.
const LIST_MAX: usize = 4 * 1024 * 1024;
/// The most paths that one `git diff-tree` call gets as a pathspec.
const MAX_PATHSPEC: usize = 500;

/// What the store knows about the files of a session without a worktree.
struct Shared {
    /// The paths of the `file.changed` events, as the provider reported them.
    events: Vec<String>,
    /// The turn checkpoints of the session.
    turns: Vec<TurnCheckpointRow>,
    /// The `cwd` of the session as stored (it can be a link).
    cwd: Option<PathBuf>,
}

/// True for a full hexadecimal commit id (SHA-1 or SHA-256).
fn is_oid(text: &str) -> bool {
    matches!(text.len(), 40 | 64) && text.bytes().all(|b| b.is_ascii_hexdigit())
}

/// `files.session_diff`.
///
/// # Errors
/// [`FileError`]: `unknown_session`, `no_base`, `no_diff`, `outside_roots`,
/// `missing_workspace`, `timeout`.
pub async fn run(
    store: &Store,
    policy: &Policy,
    private: &PrivatePaths,
    git: &GitOptions,
    query: SessionDiffQuery,
) -> Result<SessionDiff, FileError> {
    run_as(store, policy, private, git, query, false).await
}

/// `files.session_diff` for the owner (`member` false) or for a member: the
/// files that the member deny list hides are removed from the patch and
/// from the summary (R11.9, DL-179), as in `files.diff`.
///
/// # Errors
/// As [`run`].
pub async fn run_as(
    store: &Store,
    policy: &Policy,
    private: &PrivatePaths,
    git: &GitOptions,
    query: SessionDiffQuery,
    member: bool,
) -> Result<SessionDiff, FileError> {
    let internal = |e: &dyn std::fmt::Display| FileError::new("internal", e.to_string());
    let summary = store
        .summary(query.session_id.clone())
        .await
        .map_err(|e| internal(&e))?
        .ok_or_else(|| {
            FileError::new(
                code::UNKNOWN_SESSION,
                format!("session `{}` is unknown", query.session_id),
            )
        })?;
    let base = summary
        .base
        .filter(|b| is_oid(&b.oid))
        .ok_or_else(|| FileError::new(code::NO_BASE, "the session has no base commit"))?;
    let root = crate::manager::files::root(
        store,
        policy,
        private,
        &FileRoot::Session(query.session_id.clone()),
    )
    .await?;
    let options = GitOptions {
        timeout: DIFF_TIMEOUT,
        ..git.clone()
    };
    let max = query
        .max_bytes
        .unwrap_or(MAX_DIFF_BYTES)
        .min(MAX_DIFF_BYTES) as usize;
    let shared = if summary.worktree.is_none() {
        let internal = |e: &dyn std::fmt::Display| FileError::new("internal", e.to_string());
        Some(Shared {
            events: store
                .session_changed_paths(query.session_id.clone())
                .await
                .map_err(|e| internal(&e))?,
            turns: store
                .turn_checkpoints(query.session_id.clone())
                .await
                .map_err(|e| internal(&e))?,
            cwd: summary.cwd.as_deref().map(PathBuf::from),
        })
    } else {
        None
    };
    let scope = if shared.is_some() {
        DiffScope::SessionFiles
    } else {
        DiffScope::Worktree
    };
    // The wait for a permit counts against the timeout (NV-34).
    let work = async {
        let _permit = DIFFS
            .acquire()
            .await
            .map_err(|e| FileError::new("internal", e.to_string()))?;
        compute(root, base.oid.clone(), options, max, shared, private).await
    };
    let (mut summary, mut patch, truncated) = tokio::time::timeout(DIFF_TIMEOUT, work)
        .await
        .map_err(|_| FileError::new(code::TIMEOUT, "the diff took too long"))??;
    if member {
        let f = policy.member_deny().filter_patch(&patch);
        summary.files_changed = summary.files_changed.saturating_sub(f.hidden_files);
        summary.insertions = summary.insertions.saturating_sub(f.hidden_insertions);
        summary.deletions = summary.deletions.saturating_sub(f.hidden_deletions);
        patch = f.patch;
    }
    Ok(SessionDiff {
        session_id: query.session_id,
        base_oid: base.oid,
        summary,
        patch,
        truncated,
        scope,
    })
}

async fn compute(
    root: PathBuf,
    base: String,
    options: GitOptions,
    max: usize,
    shared: Option<Shared>,
    private: &PrivatePaths,
) -> Result<(DiffSummary, String, bool), FileError> {
    let internal = |e: &dyn std::fmt::Display| FileError::new("internal", e.to_string());
    let no_diff = |why: String| FileError::new(code::NO_DIFF, why);
    let worktree = Worktree::open(&root, options)
        .await
        .map_err(|_| no_diff("the working directory is not a Git worktree".to_owned()))?;
    let git = worktree.git();
    let mut excludes = private.excludes_for(worktree.root());
    let has_base = git
        .status(
            &["cat-file", "-e", &format!("{base}^{{commit}}")],
            &[],
            None,
        )
        .await
        .map_err(|e| internal(&e))?;
    if has_base.0 != 0 {
        return Err(FileError::new(
            code::NO_BASE,
            "the base commit is not in the repository",
        ));
    }
    let only = match &shared {
        Some(shared) => Some(session_paths(git, &worktree, &root, shared, private).await?),
        None => None,
    };
    if only.as_ref().is_some_and(BTreeSet::is_empty) {
        return Ok((numstat(""), String::new(), false));
    }
    let Snapshot { tree, hidden } = match worktree
        .snapshot_tree(Limits::default(), private)
        .await
        .map_err(|e| internal(&e))?
    {
        Ok(snapshot) => snapshot,
        Err(why) => {
            return Err(no_diff(format!(
                "the working tree is out of the snapshot scope ({})",
                why.code()
            )));
        }
    };
    // A tracked path whose file is private is not in the snapshot, and it is
    // left out of the diff too (FDV2-003). The log has the count only.
    if !hidden.is_empty() {
        tracing::info!("a private file was left out of the session diff");
        excludes.extend(
            hidden
                .iter()
                .map(|path| format!(":(exclude,literal){path}")),
        );
    }
    let (mut summary, mut patch, cut) = match &only {
        Some(only) => tracked_in(git, &base, &tree, only, max, &excludes).await?,
        None => {
            let mut spec = visible_pathspec();
            spec.extend_from_slice(&excludes);
            tracked(git, &base, &tree, &spec, max).await?
        }
    };
    if cut {
        return Ok((summary, patch, true));
    }
    let (mut paths, listing_cut) = untracked(git).await?;
    let top = worktree.root().to_path_buf();
    paths.retain(|path| !private.is_private_canonical(&top.join(path)));
    if let Some(only) = &only {
        paths.retain(|path| only.contains(path));
    }
    let mut truncated = listing_cut || paths.len() > MAX_UNTRACKED;
    let paths: Vec<String> = paths.into_iter().take(MAX_UNTRACKED).collect();
    let remaining = max.saturating_sub(patch.len());
    // `git ls-files` names paths from the worktree root, which is above the
    // session folder when the session started in a subdirectory.
    let private = private.clone();
    let rendered =
        tokio::task::spawn_blocking(move || render_untracked(&top, &paths, remaining, &private))
            .await
            .map_err(|e| internal(&e))?;
    patch.push_str(&rendered.patch);
    summary.files_changed = summary
        .files_changed
        .saturating_add(rendered.summary.files_changed);
    summary.insertions += rendered.summary.insertions;
    truncated |= rendered.truncated;
    Ok((summary, patch, truncated))
}

/// The tracked diff of `base` and `tree` under `spec`: the summary, the patch
/// (at most `max` bytes) and true when the patch was cut.
async fn tracked(
    git: &Git,
    base: &str,
    tree: &str,
    spec: &[String],
    max: usize,
) -> Result<(DiffSummary, String, bool), FileError> {
    let internal = |e: &dyn std::fmt::Display| FileError::new("internal", e.to_string());
    let mut args = vec![
        "diff-tree",
        "-p",
        "-r",
        "-M",
        "--no-ext-diff",
        "--no-textconv",
        "--no-color",
        base,
        tree,
    ];
    args.extend(spec.iter().map(String::as_str));
    let (status, out, cut) = git.bounded(&args, max).await.map_err(|e| internal(&e))?;
    if status != 0 && !cut {
        return Err(internal(&format!("git diff-tree failed ({status})")));
    }
    let mut patch = String::from_utf8_lossy(&out).into_owned();
    let mut args = vec!["diff-tree", "-r", "-M", "--numstat", base, tree];
    args.extend(spec.iter().map(String::as_str));
    let (status, out, _) = git
        .bounded(&args, LIST_MAX)
        .await
        .map_err(|e| internal(&e))?;
    if status != 0 {
        return Err(internal(&format!(
            "git diff-tree --numstat failed ({status})"
        )));
    }
    let summary = numstat(&String::from_utf8_lossy(&out));
    if cut {
        patch.push_str("\n[diff truncated]\n");
    }
    Ok((summary, patch, cut))
}

/// The tracked diff of only the paths in `only` (the files of a session in a
/// shared folder). The paths that really differ go to Git in groups of
/// [`MAX_PATHSPEC`], in path order, so the patch has the order of a whole diff.
async fn tracked_in(
    git: &Git,
    base: &str,
    tree: &str,
    only: &BTreeSet<String>,
    max: usize,
    excludes: &[String],
) -> Result<(DiffSummary, String, bool), FileError> {
    let internal = |e: &dyn std::fmt::Display| FileError::new("internal", e.to_string());
    let mut args = vec![
        "diff-tree",
        "-r",
        "--no-renames",
        "--name-only",
        "-z",
        base,
        tree,
    ];
    let mut spec = visible_pathspec();
    spec.extend_from_slice(excludes);
    args.extend(spec.iter().map(String::as_str));
    let (status, out, _) = git
        .bounded(&args, LIST_MAX)
        .await
        .map_err(|e| internal(&e))?;
    if status != 0 {
        return Err(internal(&format!(
            "git diff-tree --name-only failed ({status})"
        )));
    }
    let chosen: BTreeSet<&str> = nul_fields(&out)
        .into_iter()
        .filter_map(|field| std::str::from_utf8(field).ok())
        .filter(|path| only.contains(*path))
        .collect();
    let chosen: Vec<&str> = chosen.into_iter().collect();
    let mut summary = numstat("");
    let mut patch = String::new();
    for group in chosen.chunks(MAX_PATHSPEC) {
        let spec = literal_pathspec(group, excludes);
        let (part, text, cut) =
            tracked(git, base, tree, &spec, max.saturating_sub(patch.len())).await?;
        summary.files_changed = summary.files_changed.saturating_add(part.files_changed);
        summary.insertions += part.insertions;
        summary.deletions += part.deletions;
        patch.push_str(&text);
        if cut {
            return Ok((summary, patch, true));
        }
    }
    Ok((summary, patch, false))
}

/// [`visible_pathspec`] with the given paths (literal, no pattern) in place
/// of the whole worktree, and the private `excludes`.
fn literal_pathspec(paths: &[&str], excludes: &[String]) -> Vec<String> {
    let mut spec = visible_pathspec();
    spec.splice(1..2, paths.iter().map(|path| format!(":(literal){path}")));
    spec.extend_from_slice(excludes);
    spec
}

/// The paths that a session without a worktree changed, from the top of the
/// worktree: its `file.changed` events and the files that differ between the
/// two sides of each of its turn checkpoints.
async fn session_paths(
    git: &Git,
    worktree: &Worktree,
    session_dir: &Path,
    shared: &Shared,
    private: &PrivatePaths,
) -> Result<BTreeSet<String>, FileError> {
    let mut paths: BTreeSet<String> = shared
        .events
        .iter()
        .filter_map(|raw| repo_path(raw, session_dir, shared.cwd.as_deref(), worktree.root()))
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
            .map_err(|e| FileError::new("internal", e.to_string()))?;
        if status != 0 && !cut {
            tracing::warn!(app_turn = %row.app_turn, status, "the files of a turn checkpoint are not listed");
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
    paths.retain(|path| !private.is_private_canonical(&worktree.root().join(path)));
    Ok(paths)
}

/// The path that a provider reported (absolute, or relative to the session
/// folder) as a path from `top`, the worktree root; `None` for a path outside
/// the worktree, with `..`, or private.
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

/// The sizes of `git diff-tree --numstat` output; a binary file counts as
/// changed with no lines.
fn numstat(text: &str) -> DiffSummary {
    let mut summary = DiffSummary {
        files_changed: 0,
        insertions: 0,
        deletions: 0,
    };
    for line in text.lines().filter(|l| !l.is_empty()) {
        let mut fields = line.splitn(3, '\t');
        let count = |f: Option<&str>| f.and_then(|v| v.parse::<u64>().ok()).unwrap_or(0);
        summary.insertions += count(fields.next());
        summary.deletions += count(fields.next());
        summary.files_changed = summary.files_changed.saturating_add(1);
    }
    summary
}

/// The untracked files that Git lists (not ignored), sorted, once each and
/// without hidden paths; true when the listing was cut.
async fn untracked(git: &Git) -> Result<(Vec<String>, bool), FileError> {
    let (status, out, cut) = git
        .bounded(
            &["ls-files", "-z", "--others", "--exclude-standard"],
            LIST_MAX,
        )
        .await
        .map_err(|e| FileError::new("internal", e.to_string()))?;
    if status != 0 && !cut {
        return Err(FileError::new(
            "internal",
            format!("git ls-files failed ({status})"),
        ));
    }
    let mut paths: Vec<String> = nul_fields(&out)
        .into_iter()
        .filter_map(|field| std::str::from_utf8(field).ok())
        .filter(|path| !hidden_path(path))
        .map(str::to_owned)
        .collect();
    paths.sort_unstable();
    paths.dedup();
    Ok((paths, cut))
}

/// The rendered untracked files.
#[derive(Debug)]
struct Rendered {
    patch: String,
    summary: DiffSummary,
    truncated: bool,
}

/// Renders each untracked path as a new file, in order, until the patch
/// would pass `budget` bytes. A file that cannot be read without following
/// a symlink (a link, a FIFO, a file that vanished) is left out.
fn render_untracked(
    root: &std::path::Path,
    paths: &[String],
    budget: usize,
    private: &PrivatePaths,
) -> Rendered {
    let mut out = Rendered {
        patch: String::new(),
        summary: numstat(""),
        truncated: false,
    };
    for path in paths {
        let Ok(content) = files::read_for(root, path, Some(MAX_READ_BYTES), None, private) else {
            continue;
        };
        let (piece, lines) = new_file_patch(path, &content);
        if out.patch.len() + piece.len() > budget {
            out.patch.push_str("\n[diff truncated]\n");
            out.truncated = true;
            return out;
        }
        out.patch.push_str(&piece);
        out.summary.files_changed = out.summary.files_changed.saturating_add(1);
        out.summary.insertions += lines;
        // A binary or a large file shows its header only.
        out.truncated |= content.text.is_none() || content.truncated;
    }
    out
}

/// The new-file patch of one file, and its number of added lines.
fn new_file_patch(path: &str, content: &FileContent) -> (String, u64) {
    let head = format!("diff --git a/{path} b/{path}\nnew file mode 100644\n");
    let Some(text) = content.text.as_deref().filter(|_| !content.truncated) else {
        return (
            format!("{head}Binary files /dev/null and b/{path} differ\n"),
            0,
        );
    };
    if text.is_empty() {
        return (head, 0);
    }
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let mut patch = format!(
        "{head}--- /dev/null\n+++ b/{path}\n@@ -0,0 +1,{} @@\n",
        lines.len()
    );
    for line in &lines {
        patch.push('+');
        patch.push_str(line);
        if !line.ends_with('\n') {
            patch.push_str("\n\\ No newline at end of file\n");
        }
    }
    (patch, lines.len() as u64)
}

#[cfg(test)]
#[path = "session_diff_tests.rs"]
mod session_diff_tests;
