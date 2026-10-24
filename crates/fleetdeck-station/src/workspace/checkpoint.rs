//! Turn checkpoints as hidden Git refs (PLAN §6.6, L13; Revision 3).
//!
//! - **Namespace.** `refs/fleetdeck/checkpoints/<incarnation>/<worktree id>/
//!   <app session>/<app turn>/<start|end>`, outside `refs/heads` and
//!   `refs/tags`. The worktree id is derived from the canonical worktree
//!   path and the canonical common Git directory, so two worktrees of one
//!   repository never share a checkpoint.
//! - **Scope.** A checkpoint holds the tracked working-tree content,
//!   including tracked edits that are not staged. Untracked and ignored
//!   files are out of scope. The snapshot uses a private copy of the index;
//!   the real index is never written. Git never reads a changed tracked
//!   path itself: the station opens each one without following a symlink,
//!   refuses a private file by the identity of the opened file, and hands
//!   Git the checked bytes ([`super::stage`], FDV2-003). A private file is
//!   left out of the snapshot: the tree keeps the blob that the index had.
//! - **Unavailable.** Submodules, unmerged, skip-worktree or assume-unchanged
//!   entries (Git would silently leave out their unstaged edits), clean or
//!   encoding filters, non-UTF-8 paths and the size limits make a checkpoint
//!   unavailable with an explicit reason ([`Unavailable`]); nothing is
//!   published then.
//! - **Idempotent publication.** The ref is created only if it does not
//!   exist; a second capture of the same key returns the first checkpoint.
//!
//! This module is the Git layer only. The turn lifecycle (`turn.finalized`,
//! the fenced boundary) and revert admission live in the session manager.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use super::git::{Git, GitError, GitOptions, nul_fields};
use super::private_paths::PrivatePaths;
use super::stage::IndexTrust;

/// The root of every checkpoint ref.
pub const REF_ROOT: &str = "refs/fleetdeck/checkpoints";

/// The snapshot scope recorded in every checkpoint.
pub const SCOPE_TRACKED: &str = "tracked";

/// The capture version that new checkpoints record (`capture: 2`). A
/// checkpoint without the line (version 1) was captured before the checked
/// staging of FDV2-003 and is unverified: it can hold the bytes of a
/// private file.
pub const CAPTURE_VERIFIED: u32 = 2;

/// Git file mode of a submodule (gitlink) entry.
const GITLINK: &str = "160000";

#[derive(Debug, thiserror::Error)]
pub enum CheckpointError {
    #[error(transparent)]
    Git(#[from] GitError),
    #[error("`{0}` is not inside a Git worktree")]
    NotAWorktree(PathBuf),
    #[error("invalid checkpoint key part `{0}` (allowed: A-Z a-z 0-9 _ -)")]
    BadKey(String),
    #[error("checkpoint {0} belongs to another worktree")]
    OtherWorktree(String),
    #[error("checkpoint {0} is not a FleetDeck checkpoint")]
    Foreign(String),
    #[error("file system: {0}")]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, CheckpointError>;

/// Why a checkpoint cannot be captured or restored. The reason is shown;
/// a revert of an unavailable checkpoint is disabled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unavailable {
    Submodule(String),
    Unmerged(String),
    SkipWorktree(String),
    /// `git update-index --assume-unchanged` (or `core.ignoreStat`): Git
    /// does not look at the working-tree file, so its edits would be left out.
    AssumeUnchanged(String),
    /// An index entry with a status tag that this code does not know.
    UnknownIndexEntry {
        path: String,
        tag: String,
    },
    Filter {
        path: String,
        attribute: String,
    },
    NonUtf8Path,
    FileTooLarge {
        path: String,
        bytes: u64,
    },
    TooLarge {
        bytes: u64,
    },
    TooManyFiles(usize),
    /// More tracked paths changed than one snapshot stages.
    TooManyChanges(usize),
    /// A changed tracked path is a directory, a FIFO, a socket, a device,
    /// or a symlink where a regular file was opened.
    NotRegular(String),
}

impl Unavailable {
    /// The stable reason code (events and the board use it).
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::Submodule(_) => "submodule",
            Self::Unmerged(_) => "unmerged",
            Self::SkipWorktree(_) => "skip_worktree",
            Self::AssumeUnchanged(_) => "assume_unchanged",
            Self::UnknownIndexEntry { .. } => "unknown_index_entry",
            Self::Filter { .. } => "filter",
            Self::NonUtf8Path => "non_utf8_path",
            Self::FileTooLarge { .. } => "file_too_large",
            Self::TooLarge { .. } => "too_large",
            Self::TooManyFiles(_) => "too_many_files",
            Self::TooManyChanges(_) => "too_many_changes",
            Self::NotRegular(_) => "not_regular",
        }
    }
}

/// The size limits of a snapshot.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub max_file_bytes: u64,
    pub max_total_bytes: u64,
    pub max_files: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_file_bytes: 50 * 1024 * 1024,
            max_total_bytes: 512 * 1024 * 1024,
            max_files: 100_000,
        }
    }
}

/// Which checkpoint of a turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Start,
    Completion,
}

impl Phase {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::Completion => "end",
        }
    }
}

/// The identity of one checkpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckpointKey {
    pub incarnation: String,
    pub session: String,
    pub turn: String,
    pub phase: Phase,
}

/// One worktree, identified by its canonical paths.
#[derive(Debug, Clone)]
pub struct Worktree {
    root: PathBuf,
    git_dir: PathBuf,
    id: String,
    git: Git,
}

/// A published checkpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Checkpoint {
    pub ref_name: String,
    pub commit: String,
    pub tree: String,
    pub worktree_id: String,
    pub scope: String,
    /// The capture version: 2 for a checkpoint of the checked staging
    /// ([`CAPTURE_VERIFIED`]); 1 when the commit has no `capture:` line.
    pub capture: u32,
}

/// A snapshot of the tracked content: the tree, and the tracked paths that
/// the snapshot left out because the file at the path is private.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    pub tree: String,
    pub hidden: Vec<String>,
}

/// The result of a capture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Captured {
    /// This call created the ref.
    Published(Checkpoint),
    /// The ref already existed (idempotent per key); it is unchanged.
    Existing(Checkpoint),
    /// Nothing was published.
    Unavailable(Unavailable),
}

impl Worktree {
    /// Opens the worktree that contains `path`.
    pub async fn open(path: &Path, options: GitOptions) -> Result<Self> {
        Self::open_in_roots(path, options, None).await
    }

    /// Opens the worktree that contains `path`. With `roots` (B9), the
    /// toplevel that Git reports must be a folder below one of them without
    /// a link in its path, else there is no worktree (checkpoints are off).
    /// Later checkpoint Git runs by the retained toplevel path: a swap after
    /// this start is a detection-only case (design launch-dir §2 item 2).
    pub async fn open_in_roots(
        path: &Path,
        options: GitOptions,
        roots: Option<&[PathBuf]>,
    ) -> Result<Self> {
        let probe = Git::new(path, options.clone());
        let out = probe
            .text(&[
                "rev-parse",
                "--path-format=absolute",
                "--show-toplevel",
                "--git-dir",
                "--git-common-dir",
            ])
            .await
            .map_err(|_| CheckpointError::NotAWorktree(path.to_owned()))?;
        let mut lines = out.lines();
        let (Some(top), Some(git_dir), Some(common)) = (lines.next(), lines.next(), lines.next())
        else {
            return Err(CheckpointError::NotAWorktree(path.to_owned()));
        };
        let root = std::fs::canonicalize(top)?;
        if let Some(roots) = roots
            && fleetdeck_core::launch_dir::LaunchDir::open_in(roots, &root).is_err()
        {
            return Err(CheckpointError::NotAWorktree(path.to_owned()));
        }
        let git_dir = std::fs::canonicalize(git_dir)?;
        let common = std::fs::canonicalize(common)?;
        let id = worktree_id(&root, &common);
        Ok(Self {
            git: Git::new(&root, options),
            root,
            git_dir,
            id,
        })
    }

    /// The canonical worktree root.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The canonical worktree identity used in the ref namespace.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    pub(super) fn git(&self) -> &Git {
        &self.git
    }

    /// The ref name of `key` in this worktree.
    pub fn ref_name(&self, key: &CheckpointKey) -> Result<String> {
        for part in [&key.incarnation, &key.session, &key.turn] {
            check_part(part)?;
        }
        Ok(format!(
            "{REF_ROOT}/{}/{}/{}/{}/{}",
            key.incarnation,
            self.id,
            key.session,
            key.turn,
            key.phase.as_str()
        ))
    }

    /// Captures the tracked content as the checkpoint `key`.
    pub async fn capture(
        &self,
        key: &CheckpointKey,
        limits: Limits,
        private: &PrivatePaths,
    ) -> Result<Captured> {
        let ref_name = self.ref_name(key)?;
        if let Some(found) = self.read_ref(&ref_name).await? {
            return Ok(Captured::Existing(found));
        }
        let Snapshot { tree, hidden } = match self.snapshot_tree(limits, private).await? {
            Ok(snapshot) => snapshot,
            Err(why) => return Ok(Captured::Unavailable(why)),
        };
        if !hidden.is_empty() {
            tracing::info!(
                count = hidden.len(),
                "private files were left out of a turn checkpoint"
            );
        }
        let message = format!(
            "fleetdeck checkpoint\n\nworktree-id: {}\nincarnation: {}\nsession: {}\nturn: {}\nphase: {}\nscope: {SCOPE_TRACKED}\ncapture: {CAPTURE_VERIFIED}\n",
            self.id,
            key.incarnation,
            key.session,
            key.turn,
            key.phase.as_str()
        );
        let mut args = vec!["commit-tree", "--no-gpg-sign", tree.as_str()];
        let head = self
            .git
            .status(&["rev-parse", "-q", "--verify", "HEAD^{commit}"], &[], None)
            .await?;
        let parent = String::from_utf8_lossy(&head.1).trim().to_owned();
        if head.0 == 0 && !parent.is_empty() {
            args.extend(["-p", parent.as_str()]);
        }
        let env = identity_env();
        let env_refs: Vec<(&str, &OsStr)> = env.iter().map(|(k, v)| (*k, OsStr::new(v))).collect();
        let commit = self
            .git
            .call(&args, &env_refs, Some(message.into_bytes()))
            .await?;
        let commit = String::from_utf8_lossy(&commit).trim().to_owned();
        // An empty old value: create only. A concurrent capture of the same
        // key wins or loses as a whole; the loser reads the winner.
        let created = self
            .git
            .status(&["update-ref", &ref_name, &commit, ""], &[], None)
            .await?;
        if created.0 != 0 {
            return match self.read_ref(&ref_name).await? {
                Some(found) => Ok(Captured::Existing(found)),
                None => Err(GitError::Failed {
                    args: format!("update-ref {ref_name}"),
                    code: created.0.to_string(),
                    stderr: created.2,
                }
                .into()),
            };
        }
        Ok(Captured::Published(Checkpoint {
            ref_name,
            commit,
            tree,
            worktree_id: self.id.clone(),
            scope: SCOPE_TRACKED.to_owned(),
            capture: CAPTURE_VERIFIED,
        }))
    }

    /// The checkpoint `key`, if it was published.
    pub async fn read(&self, key: &CheckpointKey) -> Result<Option<Checkpoint>> {
        let ref_name = self.ref_name(key)?;
        self.read_ref(&ref_name).await
    }

    /// Reads a checkpoint ref and checks that it belongs to this worktree.
    pub async fn read_ref(&self, ref_name: &str) -> Result<Option<Checkpoint>> {
        let (code, out, _) = self
            .git
            .status(
                &[
                    "rev-parse",
                    "-q",
                    "--verify",
                    &format!("{ref_name}^{{commit}}"),
                ],
                &[],
                None,
            )
            .await?;
        if code != 0 {
            return Ok(None);
        }
        let commit = String::from_utf8_lossy(&out).trim().to_owned();
        let body = self.git.text(&["cat-file", "commit", &commit]).await?;
        let tree = body
            .lines()
            .find_map(|l| l.strip_prefix("tree "))
            .map(str::to_owned)
            .ok_or_else(|| CheckpointError::Foreign(ref_name.to_owned()))?;
        let field = |name: &str| {
            body.lines()
                .find_map(|l| l.strip_prefix(name))
                .map(str::to_owned)
        };
        if !body.contains("\n\nfleetdeck checkpoint\n") {
            return Err(CheckpointError::Foreign(ref_name.to_owned()));
        }
        let worktree_id =
            field("worktree-id: ").ok_or_else(|| CheckpointError::Foreign(ref_name.to_owned()))?;
        if worktree_id != self.id || !ref_name.contains(&format!("/{}/", self.id)) {
            return Err(CheckpointError::OtherWorktree(ref_name.to_owned()));
        }
        Ok(Some(Checkpoint {
            ref_name: ref_name.to_owned(),
            commit,
            tree,
            worktree_id,
            scope: field("scope: ").unwrap_or_default(),
            capture: field("capture: ")
                .and_then(|v| v.trim().parse().ok())
                .unwrap_or(1),
        }))
    }

    /// Writes the tracked working-tree content to a tree object through a
    /// private index. The real index is not written.
    pub(super) async fn snapshot_tree(
        &self,
        limits: Limits,
        private: &PrivatePaths,
    ) -> Result<std::result::Result<Snapshot, Unavailable>> {
        if let Err(why) = self.check_tracked(limits).await? {
            return Ok(Err(why));
        }
        let index = PrivateIndex::copy_of(&self.git_dir)?;
        let hidden = match self
            .stage_changes(&index, limits, private, IndexTrust::Untrusted)
            .await?
        {
            Ok(hidden) => hidden,
            Err(why) => return Ok(Err(why)),
        };
        let env = [("GIT_INDEX_FILE", index.path().as_os_str())];
        let tree = self.git.call(&["write-tree"], &env, None).await?;
        Ok(Ok(Snapshot {
            tree: String::from_utf8_lossy(&tree).trim().to_owned(),
            hidden,
        }))
    }

    /// The pre-capture checks of the module docs.
    async fn check_tracked(&self, limits: Limits) -> Result<std::result::Result<(), Unavailable>> {
        let out = self.git.run(&["ls-files", "-s", "-v", "-z"]).await?;
        let mut paths: Vec<String> = Vec::new();
        for entry in nul_fields(&out) {
            // `<tag> <mode> <object> <stage>\t<path>`
            let Some(tab) = entry.iter().position(|b| *b == b'\t') else {
                continue;
            };
            let Ok(path) = std::str::from_utf8(&entry[tab + 1..]) else {
                return Ok(Err(Unavailable::NonUtf8Path));
            };
            let head = String::from_utf8_lossy(&entry[..tab]);
            let parts: Vec<&str> = head.split(' ').collect();
            let (tag, mode, stage) = match parts.as_slice() {
                [tag, mode, _, stage] => (*tag, *mode, *stage),
                _ => continue,
            };
            if mode == GITLINK {
                return Ok(Err(Unavailable::Submodule(path.to_owned())));
            }
            if stage != "0" {
                return Ok(Err(Unavailable::Unmerged(path.to_owned())));
            }
            // `ls-files -v` tags: `H` an ordinary cached entry, `S`
            // skip-worktree; a lower-case tag marks assume-unchanged (`s` is
            // both). Any other tag fails closed.
            match tag {
                "H" => {}
                "S" | "s" => return Ok(Err(Unavailable::SkipWorktree(path.to_owned()))),
                t if t.chars().any(|c| c.is_ascii_lowercase()) => {
                    return Ok(Err(Unavailable::AssumeUnchanged(path.to_owned())));
                }
                t => {
                    return Ok(Err(Unavailable::UnknownIndexEntry {
                        path: path.to_owned(),
                        tag: t.to_owned(),
                    }));
                }
            }
            paths.push(path.to_owned());
        }
        if paths.len() > limits.max_files {
            return Ok(Err(Unavailable::TooManyFiles(paths.len())));
        }
        let mut total: u64 = 0;
        for path in &paths {
            let Ok(meta) = std::fs::symlink_metadata(self.root.join(path)) else {
                continue; // a deleted tracked file
            };
            if meta.is_file() {
                if meta.len() > limits.max_file_bytes {
                    return Ok(Err(Unavailable::FileTooLarge {
                        path: path.clone(),
                        bytes: meta.len(),
                    }));
                }
                total = total.saturating_add(meta.len());
            }
        }
        if total > limits.max_total_bytes {
            return Ok(Err(Unavailable::TooLarge { bytes: total }));
        }
        if let Some(why) = self.filtered(&paths).await? {
            return Ok(Err(why));
        }
        Ok(Ok(()))
    }

    /// The first path with a clean/smudge filter or a working-tree encoding.
    pub(super) async fn filtered(&self, paths: &[String]) -> Result<Option<Unavailable>> {
        if paths.is_empty() {
            return Ok(None);
        }
        let mut input = Vec::new();
        for path in paths {
            input.extend_from_slice(path.as_bytes());
            input.push(0);
        }
        let out = self
            .git
            .call(
                &[
                    "check-attr",
                    "-z",
                    "--stdin",
                    "filter",
                    "working-tree-encoding",
                ],
                &[],
                Some(input),
            )
            .await?;
        let fields = nul_fields(&out);
        for triple in fields.chunks(3) {
            if let [path, attribute, value] = triple
                && *value != b"unspecified"
                && *value != b"unset"
            {
                return Ok(Some(Unavailable::Filter {
                    path: String::from_utf8_lossy(path).into_owned(),
                    attribute: String::from_utf8_lossy(attribute).into_owned(),
                }));
            }
        }
        Ok(None)
    }
}

/// A private index file in the worktree's Git directory (next to a split
/// index's shared file), removed on drop.
pub(super) struct PrivateIndex(PathBuf);

impl PrivateIndex {
    /// A copy of the real index, or an empty private index when the
    /// repository has none yet.
    ///
    /// The copy keeps the modification time of the real index. Git compares
    /// file times in whole seconds (it is usually built without
    /// `USE_NSEC`), and its "racy entry" protection rechecks the content of
    /// every entry that is not older than the index file. A copy with a new
    /// time would lose that protection: a same-size edit in the second of
    /// the entry's last stat would look clean, and the snapshot would miss
    /// it (DL-094). The time is read before the copy, so a concurrent
    /// rewrite of the real index can only make the copy older (more
    /// rechecks), never newer.
    pub(super) fn copy_of(git_dir: &Path) -> Result<Self> {
        let index = Self(Self::fresh(git_dir));
        let real = git_dir.join("index");
        if real.exists() {
            let modified = std::fs::metadata(&real)?.modified()?;
            std::fs::copy(&real, &index.0)?;
            std::fs::File::options()
                .write(true)
                .open(&index.0)?
                .set_modified(modified)?;
        }
        Ok(index)
    }

    /// An empty private index.
    pub(super) fn empty(git_dir: &Path) -> Self {
        Self(Self::fresh(git_dir))
    }

    fn fresh(git_dir: &Path) -> PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        git_dir.join(format!("fleetdeck-ckpt-{}-{n}.index", std::process::id()))
    }

    pub(super) fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for PrivateIndex {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
        let _ = std::fs::remove_file(self.0.with_extension("index.lock"));
    }
}

impl Worktree {
    pub(super) fn git_dir(&self) -> &Path {
        &self.git_dir
    }
}

fn check_part(part: &str) -> Result<()> {
    let ok = !part.is_empty()
        && part.len() <= 64
        && part
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
    if ok {
        Ok(())
    } else {
        Err(CheckpointError::BadKey(part.to_owned()))
    }
}

/// The first 16 bytes (hex) of SHA-256 over the canonical root and the
/// canonical common Git directory.
fn worktree_id(root: &Path, common: &Path) -> String {
    use std::os::unix::ffi::OsStrExt;
    let mut ctx = ring::digest::Context::new(&ring::digest::SHA256);
    ctx.update(root.as_os_str().as_bytes());
    ctx.update(&[0]);
    ctx.update(common.as_os_str().as_bytes());
    ctx.finish().as_ref()[..16]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// A fixed author and committer, so a capture needs no user configuration.
fn identity_env() -> [(&'static str, String); 4] {
    [
        ("GIT_AUTHOR_NAME", "FleetDeck".to_owned()),
        (
            "GIT_AUTHOR_EMAIL",
            "checkpoint@fleetdeck.invalid".to_owned(),
        ),
        ("GIT_COMMITTER_NAME", "FleetDeck".to_owned()),
        (
            "GIT_COMMITTER_EMAIL",
            "checkpoint@fleetdeck.invalid".to_owned(),
        ),
    ]
}

#[cfg(test)]
#[path = "snapshot_private_tests.rs"]
mod snapshot_private_tests;
