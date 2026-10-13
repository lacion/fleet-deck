//! `files.session_diff` (R1.2): the working tree of a session against the
//! commit that it started from (`summary.base`, R1.1).
//!
//! The tracked content comes from the same hardened path as the turn
//! checkpoints: a snapshot tree through a private index (no clean filter,
//! no external diff, no text conversion), then `git diff-tree` of the base
//! commit and that tree, with the private paths left out
//! ([`visible_pathspec`]). Each untracked file (not ignored) is read by the
//! file service (no symlink is followed) and rendered as a new file; a
//! binary or a large one is a header line only.
//!
//! Limits: the patch has at most [`MAX_DIFF_BYTES`] bytes. A cut, a left-out
//! untracked file (binary excluded, large, over [`MAX_UNTRACKED`]) sets
//! `truncated`. When the tracked patch is cut, the untracked files are not
//! listed. The summary counts the untracked files that the patch shows.

use std::time::Duration;

use fleetdeck_proto::event::DiffSummary;
use fleetdeck_proto::files::{
    FileContent, FileRoot, MAX_DIFF_BYTES, MAX_READ_BYTES, SessionDiff, SessionDiffQuery, code,
    hidden_path,
};

use super::checkpoint::{Limits, Worktree};
use super::files::{self, FileError};
use super::git::nul_fields;
use super::restore::visible_pathspec;
use super::{Git, GitOptions};
use crate::manager::policy::Policy;
use crate::store::Store;

/// The longest a session diff may take.
pub const DIFF_TIMEOUT: Duration = Duration::from_secs(15);
/// The most untracked files that one diff renders.
pub const MAX_UNTRACKED: usize = 200;
/// The most `git` output that a file listing or a numstat reads.
const LIST_MAX: usize = 4 * 1024 * 1024;

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
    git: &GitOptions,
    query: SessionDiffQuery,
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
    let root =
        crate::manager::files::root(store, policy, &FileRoot::Session(query.session_id.clone()))
            .await?;
    let options = GitOptions {
        timeout: DIFF_TIMEOUT,
        ..git.clone()
    };
    let max = query
        .max_bytes
        .unwrap_or(MAX_DIFF_BYTES)
        .min(MAX_DIFF_BYTES) as usize;
    let work = compute(root, base.oid.clone(), options, max);
    let (summary, patch, truncated) = tokio::time::timeout(DIFF_TIMEOUT, work)
        .await
        .map_err(|_| FileError::new(code::TIMEOUT, "the diff took too long"))??;
    Ok(SessionDiff {
        session_id: query.session_id,
        base_oid: base.oid,
        summary,
        patch,
        truncated,
    })
}

async fn compute(
    root: std::path::PathBuf,
    base: String,
    options: GitOptions,
    max: usize,
) -> Result<(DiffSummary, String, bool), FileError> {
    let internal = |e: &dyn std::fmt::Display| FileError::new("internal", e.to_string());
    let no_diff = |why: String| FileError::new(code::NO_DIFF, why);
    let worktree = Worktree::open(&root, options)
        .await
        .map_err(|_| no_diff("the working directory is not a Git worktree".to_owned()))?;
    let git = worktree.git();
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
    let tree = match worktree
        .snapshot_tree(Limits::default())
        .await
        .map_err(|e| internal(&e))?
    {
        Ok(tree) => tree,
        Err(why) => {
            return Err(no_diff(format!(
                "the working tree is out of the snapshot scope ({})",
                why.code()
            )));
        }
    };
    let spec = visible_pathspec();
    let mut args = vec![
        "diff-tree",
        "-p",
        "-r",
        "-M",
        "--no-ext-diff",
        "--no-textconv",
        "--no-color",
        base.as_str(),
        tree.as_str(),
    ];
    args.extend(spec.iter().map(String::as_str));
    let (status, out, cut) = git.bounded(&args, max).await.map_err(|e| internal(&e))?;
    if status != 0 && !cut {
        return Err(internal(&format!("git diff-tree failed ({status})")));
    }
    let mut patch = String::from_utf8_lossy(&out).into_owned();
    let mut args = vec![
        "diff-tree",
        "-r",
        "-M",
        "--numstat",
        base.as_str(),
        tree.as_str(),
    ];
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
    let mut summary = numstat(&String::from_utf8_lossy(&out));
    if cut {
        patch.push_str("\n[diff truncated]\n");
        return Ok((summary, patch, true));
    }
    let (paths, listing_cut) = untracked(git).await?;
    let mut truncated = listing_cut || paths.len() > MAX_UNTRACKED;
    let paths: Vec<String> = paths.into_iter().take(MAX_UNTRACKED).collect();
    let remaining = max.saturating_sub(patch.len());
    let rendered = tokio::task::spawn_blocking(move || render_untracked(&root, &paths, remaining))
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
fn render_untracked(root: &std::path::Path, paths: &[String], budget: usize) -> Rendered {
    let mut out = Rendered {
        patch: String::new(),
        summary: numstat(""),
        truncated: false,
    };
    for path in paths {
        let Ok(content) = files::read(root, path, Some(MAX_READ_BYTES)) else {
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
