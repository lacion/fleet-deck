//! Diffs between checkpoints and the restore of a checkpoint's tracked
//! content (PLAN §6.6, L13; Revision 3 "Snapshot and restore limits").
//!
//! A restore has two steps, so that a rejection changes nothing:
//!
//! 1. [`Worktree::plan_restore`] checks everything without a mutation: the
//!    checkpoint belongs to this worktree; the target and the current
//!    content are both in scope (no submodule, filter or size limit); no
//!    path to create is an untracked or ignored file or lies below one; no
//!    path to change lies below a symlink. A failed check is a
//!    [`Rejected`] reason.
//! 2. [`Worktree::apply_restore`] removes the tracked files that the target
//!    does not have and writes the target content of the other changed
//!    paths through a private index (`checkout-index`). The real index is
//!    never written; untracked and ignored files are never removed.
//!
//! The session manager calls step 2 only under the revert fence, after the
//! provider confirmed its conversation rollback (PLAN §6.6 "Mixed
//! outcomes"). This module never claims a conversation rollback.

use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::path::Path;

use super::checkpoint::{
    CAPTURE_VERIFIED, Checkpoint, CheckpointError, Limits, PrivateIndex, Result, Snapshot,
    Unavailable, Worktree,
};
use super::git::nul_fields;
use super::private_paths::PrivatePaths;
use super::stage::IndexTrust;
use fleetdeck_proto::event::DiffSummary;
use fleetdeck_proto::files::HIDDEN_NAMES;

/// The pathspec of every path that a diff may show: everything except the
/// private names of [`HIDDEN_NAMES`] (`.git` and credential files and
/// directories) at any depth, case-insensitive. Renames are not detected,
/// so each side of a rename into or out of a private directory is a
/// separate path, and the private side is left out (finding 3 of
/// sa-13931f53).
#[must_use]
pub fn visible_pathspec() -> Vec<String> {
    let mut spec = vec!["--".to_owned(), ".".to_owned()];
    for name in HIDDEN_NAMES {
        spec.push(format!(":(exclude,icase,glob)**/{name}"));
        spec.push(format!(":(exclude,icase,glob)**/{name}/**"));
    }
    spec
}

/// Why a restore was rejected before any mutation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rejected {
    /// The current content or the target is out of the snapshot scope.
    Unavailable(Unavailable),
    /// The target creates a path where an untracked or ignored file is.
    UntrackedConflict(String),
    /// A changed path lies below a symlink.
    SymlinkAncestor(String),
    /// The target was captured before the checked staging of FDV2-003
    /// (`capture` below 2): it can hold the bytes of a private file.
    OldCapture,
}

impl Rejected {
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::Unavailable(why) => why.code(),
            Self::UntrackedConflict(_) => "untracked_conflict",
            Self::SymlinkAncestor(_) => "symlink_ancestor",
            Self::OldCapture => "checkpoint_unverified",
        }
    }
}

/// The checked changes of one restore.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestorePlan {
    pub target: Checkpoint,
    /// The tree of the current tracked content when the plan was made.
    pub current_tree: String,
    /// Paths written with the target content.
    pub write: Vec<String>,
    /// Tracked paths that the target does not have.
    pub remove: Vec<String>,
    /// Tracked paths whose file is private now: a restore never writes or
    /// removes them, and never reads them from the work tree.
    pub hidden: Vec<String>,
}

/// The result of an applied restore.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoreReport {
    pub written: usize,
    pub removed: usize,
    /// True when the tracked content now equals the target exactly.
    pub matches: bool,
}

/// A diff between two checkpoints.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diff {
    pub patch: String,
    /// True when the patch was cut at the byte limit.
    pub truncated: bool,
}

impl Worktree {
    /// The patch from `from` to `to`, at most `max_bytes` long. Both
    /// checkpoints must belong to this worktree. Private paths are left
    /// out ([`visible_pathspec`]) and the pathspecs of `extra_excludes` (the
    /// station's private paths, DL-173) are left out too. Git's output is
    /// read up to the limit only; with more output the process is killed
    /// (finding 4 of sa-13931f53).
    pub async fn diff(
        &self,
        from: &Checkpoint,
        to: &Checkpoint,
        max_bytes: usize,
        extra_excludes: &[String],
    ) -> Result<Diff> {
        for c in [from, to] {
            self.own(c)?;
        }
        let mut spec = visible_pathspec();
        spec.extend_from_slice(extra_excludes);
        let mut args = vec![
            "diff-tree",
            "-p",
            "-r",
            "--no-renames",
            "--no-ext-diff",
            "--no-textconv",
            "--no-color",
            "--binary",
            &from.tree,
            &to.tree,
        ];
        args.extend(spec.iter().map(String::as_str));
        let (status, out, truncated) = self.git().bounded(&args, max_bytes).await?;
        if status != 0 && !truncated {
            return Err(CheckpointError::Io(std::io::Error::other(format!(
                "git diff-tree failed ({status})"
            ))));
        }
        let mut patch = String::from_utf8_lossy(&out).into_owned();
        if truncated {
            // A lossy cut may end inside a character; it is marked anyway.
            patch.push_str("\n[diff truncated]\n");
        }
        Ok(Diff { patch, truncated })
    }

    /// The size of the diff from `from` to `to` (`diff-tree --numstat`);
    /// a binary file counts as changed with no lines. Private paths are
    /// left out ([`visible_pathspec`]), and so are the pathspecs of
    /// `extra_excludes`.
    pub async fn diff_summary(
        &self,
        from: &Checkpoint,
        to: &Checkpoint,
        extra_excludes: &[String],
    ) -> Result<DiffSummary> {
        for c in [from, to] {
            self.own(c)?;
        }
        let mut spec = visible_pathspec();
        spec.extend_from_slice(extra_excludes);
        let mut args = vec![
            "diff-tree",
            "-r",
            "--numstat",
            "--no-renames",
            &from.tree,
            &to.tree,
        ];
        args.extend(spec.iter().map(String::as_str));
        let out = self.git().text(&args).await?;
        let mut summary = DiffSummary {
            files_changed: 0,
            insertions: 0,
            deletions: 0,
        };
        for line in out.lines().filter(|l| !l.is_empty()) {
            let mut fields = line.splitn(3, '\t');
            let count = |f: Option<&str>| f.and_then(|v| v.parse::<u64>().ok()).unwrap_or(0);
            summary.insertions += count(fields.next());
            summary.deletions += count(fields.next());
            summary.files_changed = summary.files_changed.saturating_add(1);
        }
        Ok(summary)
    }

    /// Step 1 of the module docs: checks a restore of `target` without any
    /// mutation.
    pub async fn plan_restore(
        &self,
        target: &Checkpoint,
        limits: Limits,
        private: &PrivatePaths,
    ) -> Result<std::result::Result<RestorePlan, Rejected>> {
        self.own(target)?;
        if target.capture < CAPTURE_VERIFIED {
            return Ok(Err(Rejected::OldCapture));
        }
        if let Some(why) = self.target_out_of_scope(target, limits).await? {
            return Ok(Err(Rejected::Unavailable(why)));
        }
        let Snapshot {
            tree: current_tree,
            hidden,
        } = match self.snapshot_tree(limits, private).await? {
            Ok(snapshot) => snapshot,
            Err(why) => return Ok(Err(Rejected::Unavailable(why))),
        };
        let out = self
            .git()
            .run(&[
                "diff-tree",
                "-r",
                "-z",
                "--no-renames",
                "--raw",
                &current_tree,
                &target.tree,
            ])
            .await?;
        let mut write = Vec::new();
        let mut remove = Vec::new();
        let fields = nul_fields(&out);
        for pair in fields.chunks(2) {
            let [head, path] = pair else { continue };
            let Ok(path) = std::str::from_utf8(path) else {
                return Ok(Err(Rejected::Unavailable(Unavailable::NonUtf8Path)));
            };
            // A private file is never restored over (FDV2-003).
            if hidden.iter().any(|h| h == path) {
                continue;
            }
            // `:<old mode> <new mode> <old> <new> <status>`
            if head.last() == Some(&b'D') {
                remove.push(path.to_owned());
            } else {
                write.push((path.to_owned(), head.last() == Some(&b'A')));
            }
        }
        let removed: BTreeSet<&str> = remove.iter().map(String::as_str).collect();
        for path in &remove {
            if let Some(reason) = self.check_ancestors(path, &removed) {
                return Ok(Err(reason));
            }
            // A tracked file whose place now holds a directory: the
            // directory is untracked content, which restore never removes.
            if std::fs::symlink_metadata(self.root().join(path)).is_ok_and(|m| m.is_dir()) {
                return Ok(Err(Rejected::UntrackedConflict(path.clone())));
            }
        }
        for (path, added) in &write {
            if let Some(reason) = self.check_ancestors(path, &removed) {
                return Ok(Err(reason));
            }
            let here = std::fs::symlink_metadata(self.root().join(path));
            if *added && here.is_ok() {
                return Ok(Err(Rejected::UntrackedConflict(path.clone())));
            }
            if !*added && here.is_ok_and(|m| m.is_dir()) {
                return Ok(Err(Rejected::UntrackedConflict(path.clone())));
            }
        }
        Ok(Ok(RestorePlan {
            target: target.clone(),
            current_tree,
            write: write.into_iter().map(|(p, _)| p).collect(),
            remove,
            hidden,
        }))
    }

    /// Step 2 of the module docs. The caller holds the revert fence and has
    /// just made the plan; a concurrent writer is reported by `matches`.
    pub async fn apply_restore(
        &self,
        plan: &RestorePlan,
        private: &PrivatePaths,
    ) -> Result<RestoreReport> {
        self.own(&plan.target)?;
        if !plan.hidden.is_empty() {
            tracing::info!(
                count = plan.hidden.len(),
                "private files were left unchanged by a restore"
            );
        }
        for path in &plan.remove {
            match std::fs::remove_file(self.root().join(path)) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
        }
        if !plan.write.is_empty() {
            let index = PrivateIndex::empty(self.git_dir());
            let env = [("GIT_INDEX_FILE", index.path().as_os_str())];
            self.git()
                .call(&["read-tree", &plan.target.tree], &env, None)
                .await?;
            let mut input = Vec::new();
            for path in &plan.write {
                input.extend_from_slice(path.as_bytes());
                input.push(0);
            }
            self.git()
                .call(
                    &["checkout-index", "-f", "-z", "--stdin"],
                    &env,
                    Some(input),
                )
                .await?;
        }
        let matches = self.matches_target(plan, private).await?;
        Ok(RestoreReport {
            written: plan.write.len(),
            removed: plan.remove.len(),
            matches,
        })
    }

    /// True when every target path holds the target content and no removed
    /// path exists. A restored path that the real index does not track is
    /// untracked for Git afterwards (the index is preserved), so the check
    /// reads the target tree into a private index instead of copying the
    /// real one.
    async fn matches_target(&self, plan: &RestorePlan, private: &PrivatePaths) -> Result<bool> {
        if plan
            .remove
            .iter()
            .any(|p| std::fs::symlink_metadata(self.root().join(p)).is_ok_and(|m| !m.is_dir()))
        {
            return Ok(false);
        }
        let index = PrivateIndex::empty(self.git_dir());
        let env = [("GIT_INDEX_FILE", index.path().as_os_str())];
        self.git()
            .call(&["read-tree", &plan.target.tree], &env, None)
            .await?;
        if let Err(why) = self
            .stage_changes(&index, Limits::default(), private, IndexTrust::Trusted)
            .await?
        {
            tracing::warn!(code = why.code(), "the restored content cannot be checked");
            return Ok(false);
        }
        let tree = self.git().call(&["write-tree"], &env, None).await?;
        Ok(String::from_utf8_lossy(&tree).trim() == plan.target.tree)
    }

    /// Rejects a checkpoint of another worktree.
    fn own(&self, c: &Checkpoint) -> Result<()> {
        if c.worktree_id == self.id() && c.ref_name.contains(&format!("/{}/", self.id())) {
            Ok(())
        } else {
            Err(CheckpointError::OtherWorktree(c.ref_name.clone()))
        }
    }

    /// Submodules, filters and sizes inside the target tree.
    async fn target_out_of_scope(
        &self,
        target: &Checkpoint,
        limits: Limits,
    ) -> Result<Option<Unavailable>> {
        let out = self
            .git()
            .run(&["ls-tree", "-r", "-l", "-z", "--full-tree", &target.tree])
            .await?;
        let mut paths = Vec::new();
        let mut total: u64 = 0;
        for entry in nul_fields(&out) {
            // `<mode> <type> <object> <size>\t<path>`
            let Some(tab) = entry.iter().position(|b| *b == b'\t') else {
                continue;
            };
            let Ok(path) = std::str::from_utf8(&entry[tab + 1..]) else {
                return Ok(Some(Unavailable::NonUtf8Path));
            };
            let head = String::from_utf8_lossy(&entry[..tab]);
            let parts: Vec<&str> = head.split_whitespace().collect();
            if parts.first() == Some(&"160000") {
                return Ok(Some(Unavailable::Submodule(path.to_owned())));
            }
            let size: u64 = parts.get(3).and_then(|s| s.parse().ok()).unwrap_or(0);
            if size > limits.max_file_bytes {
                return Ok(Some(Unavailable::FileTooLarge {
                    path: path.to_owned(),
                    bytes: size,
                }));
            }
            total = total.saturating_add(size);
            paths.push(path.to_owned());
        }
        if paths.len() > limits.max_files {
            return Ok(Some(Unavailable::TooManyFiles(paths.len())));
        }
        if total > limits.max_total_bytes {
            return Ok(Some(Unavailable::TooLarge { bytes: total }));
        }
        self.filtered(&paths).await
    }

    /// Every existing ancestor of `path` must be a real directory, or a
    /// tracked file that this restore removes first.
    fn check_ancestors(&self, path: &str, removed: &BTreeSet<&str>) -> Option<Rejected> {
        let rel = Path::new(path);
        let mut prefix = self.root().to_path_buf();
        let mut rel_prefix = String::new();
        let parts: Vec<&OsStr> = rel.iter().collect();
        for part in parts.iter().take(parts.len().saturating_sub(1)) {
            prefix.push(part);
            if !rel_prefix.is_empty() {
                rel_prefix.push('/');
            }
            rel_prefix.push_str(&part.to_string_lossy());
            match std::fs::symlink_metadata(&prefix) {
                Err(_) => return None, // nothing below exists either
                Ok(m) if m.file_type().is_symlink() => {
                    return Some(Rejected::SymlinkAncestor(path.to_owned()));
                }
                Ok(m) if m.is_dir() => {}
                Ok(_) if removed.contains(rel_prefix.as_str()) => return None,
                Ok(_) => return Some(Rejected::UntrackedConflict(path.to_owned())),
            }
        }
        None
    }
}
