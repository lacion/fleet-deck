//! `files.coordinator_diff` (ADR-008 §4.4): the diff of a session for a
//! coordinator. **No byte of the old side leaves the station.**
//!
//! The board's session diff is built from a snapshot tree and shows the
//! committed old side. A coordinator reads third-party text and may send it
//! on, and committed history has no provenance (a credential that was
//! committed and later replaced is still in it). So this diff is built per
//! file, and privacy is decided before any byte is read:
//!
//! 1. The changed paths come from `git diff --name-status -z -M <base>` on
//!    the work tree, plus the untracked files that Git lists.
//! 2. A path is dropped when its old or its new name is private (the hidden
//!    names, the private paths of the station) or is on the member deny
//!    list.
//! 3. The new side is read from one open descriptor: the work-tree file is
//!    opened without following a link, the identity of that open file is
//!    checked (the private registry, the hard-link rule of DL-173), and the
//!    bytes come from the same descriptor. A private file is shown as
//!    `(hidden: private file)`.
//! 4. The old side (`<base>:<path>`) only feeds the diff computation. It is
//!    never output.
//! 5. `git diff --no-index` renders each pair from two copies in a private
//!    temporary directory (mode 0700, removed at the end). Every run of
//!    removed lines becomes one line `-[<n> lines removed]`; the added lines
//!    and the context lines come from the checked new side; every `@@`
//!    header is rebuilt from the numbers only; no other Git metadata is
//!    kept. The result is a review aid, not a patch that applies.
//! 6. An identity check that cannot run fails the whole query with
//!    `privacy_unknown`.
//!
//! The file shows `diff --coordinator <path>` and then exactly one of:
//! `(renamed) <old> -> <new>` and its hunks, `(deleted file)` and
//! `-[<n> lines removed]`, `(binary file)`, `(hidden: private file)`,
//! `(hidden: diff unavailable)`, or the hunks. The size limits and the
//! `session_files` scope of a session without a worktree are those of
//! `files.session_diff`.

use std::collections::BTreeSet;
use std::fs::File;
use std::io::Read;
use std::os::fd::OwnedFd;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use fleetdeck_proto::event::DiffSummary;
use fleetdeck_proto::files::{
    CoordinatorDiffQuery, DiffScope, FileRoot, MAX_DIFF_BYTES, MAX_READ_BYTES, SessionDiff, code,
    hidden_path,
};
use ring::rand::{SecureRandom, SystemRandom};
use rustix::fs::Mode;
use rustix::io::Errno;

use super::files::{BINARY_SNIFF_BYTES, FileError, descend, file_flags, open_root};
use super::git::nul_fields;
use super::private_paths::PrivatePaths;
use super::session_diff::{DIFF_TIMEOUT, MAX_UNTRACKED};
use super::{Git, GitOptions, Worktree};
use crate::manager::policy::Policy;
use crate::store::Store;

/// The most `git` output that a listing reads.
const LIST_MAX: usize = 4 * 1024 * 1024;
/// The marker line that ends a cut patch.
const CUT: &str = "\n[diff truncated]\n";

/// A function that a test runs for a changed path at one point of its read.
pub(crate) type Hook = Arc<dyn Fn(&str) + Send + Sync>;

/// The points where a test holds the read of a file (tests only; the
/// station runs with no-op hooks).
#[derive(Clone)]
pub(crate) struct Hooks {
    /// Right before the work-tree file is opened.
    pub(crate) before_open: Hook,
    /// After the open file is checked, before its bytes are read.
    pub(crate) after_open: Hook,
}

impl Default for Hooks {
    fn default() -> Self {
        Self {
            before_open: Arc::new(|_| {}),
            after_open: Arc::new(|_| {}),
        }
    }
}

/// True for a full hexadecimal commit id (SHA-1 or SHA-256).
fn is_oid(text: &str) -> bool {
    matches!(text.len(), 40 | 64) && text.bytes().all(|b| b.is_ascii_hexdigit())
}

fn internal(e: &dyn std::fmt::Display) -> FileError {
    FileError::new("internal", e.to_string())
}

fn unknown() -> FileError {
    FileError::new(
        code::PRIVACY_UNKNOWN,
        "a privacy check of the diff could not run",
    )
}

/// `files.coordinator_diff`.
///
/// # Errors
/// [`FileError`]: `unknown_session`, `no_base`, `no_diff`, `outside_roots`,
/// `missing_workspace`, `timeout` and `privacy_unknown`.
pub async fn run(
    store: &Store,
    policy: &Policy,
    private: &PrivatePaths,
    git: &GitOptions,
    query: CoordinatorDiffQuery,
) -> Result<SessionDiff, FileError> {
    run_in(
        store,
        policy,
        private,
        git,
        query,
        &std::env::temp_dir(),
        Hooks::default(),
    )
    .await
}

/// [`run`] with the directory that holds the private temporary directory,
/// and the hooks of the tests.
pub(crate) async fn run_in(
    store: &Store,
    policy: &Policy,
    private: &PrivatePaths,
    git: &GitOptions,
    query: CoordinatorDiffQuery,
    temp_parent: &Path,
    hooks: Hooks,
) -> Result<SessionDiff, FileError> {
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
    let shared = if summary.worktree.is_none() {
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
    let job = Job {
        base: base.oid.clone(),
        options,
        shared,
        policy,
        private,
        temp_parent,
        hooks,
    };
    let (summary, patch, truncated) = tokio::time::timeout(DIFF_TIMEOUT, job.compute(root))
        .await
        .map_err(|_| FileError::new(code::TIMEOUT, "the diff took too long"))??;
    Ok(SessionDiff {
        session_id: query.session_id,
        base_oid: base.oid,
        summary,
        patch,
        truncated,
        scope,
    })
}

/// One changed file.
struct Entry {
    old: Option<String>,
    path: String,
    kind: Kind,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Added,
    Modified,
    Deleted,
    Renamed,
    /// A type change, an unmerged path or a status that is not known.
    Other,
}

/// The new side of a file after the identity check.
enum NewSide {
    Bytes(Vec<u8>),
    Private,
    /// A link, a special file, a file that vanished or is too large.
    Unavailable {
        too_large: bool,
    },
}

/// The rendered part of a file.
#[derive(Default)]
struct Piece {
    text: String,
    added: u64,
    removed: u64,
    /// The file shows lines (it counts in `files_changed`).
    shown: bool,
    /// A limit hid part of the file.
    cut: bool,
}

struct Job<'a> {
    base: String,
    options: GitOptions,
    shared: Option<Shared>,
    policy: &'a Policy,
    private: &'a PrivatePaths,
    temp_parent: &'a Path,
    hooks: Hooks,
}

/// A private temporary directory (mode 0700) that is removed on drop.
struct TempDir(PathBuf);

impl TempDir {
    fn new(parent: &Path) -> std::io::Result<Self> {
        for _ in 0..8 {
            let mut bytes = [0u8; 8];
            SystemRandom::new()
                .fill(&mut bytes)
                .map_err(|_| std::io::Error::other("no randomness"))?;
            let name: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
            let path = parent.join(format!("fd-coord-diff-{name}"));
            match std::fs::DirBuilder::new().mode(0o700).create(&path) {
                Ok(()) => return Ok(Self(path)),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(e),
            }
        }
        Err(std::io::Error::other("no free temporary directory name"))
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        if let Err(e) = std::fs::remove_dir_all(&self.0) {
            tracing::warn!(error = %e, "the temporary directory of a coordinator diff stays");
        }
    }
}

impl Job<'_> {
    async fn compute(&self, root: PathBuf) -> Result<(DiffSummary, String, bool), FileError> {
        let no_diff = |why: &str| FileError::new(code::NO_DIFF, why);
        let worktree = Worktree::open(&root, self.options.clone())
            .await
            .map_err(|_| no_diff("the working directory is not a Git worktree"))?;
        let git = worktree.git();
        let top = worktree.root().to_path_buf();
        let has_base = git
            .status(
                &["cat-file", "-e", &format!("{}^{{commit}}", self.base)],
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
        let only = match &self.shared {
            Some(shared) => Some(session_paths(git, &worktree, &root, shared, self.private).await?),
            None => None,
        };
        let mut summary = DiffSummary {
            files_changed: 0,
            insertions: 0,
            deletions: 0,
        };
        if only.as_ref().is_some_and(BTreeSet::is_empty) {
            return Ok((summary, String::new(), false));
        }
        // No clean or smudge filter may run while Git compares the files.
        let tracked = self.tracked_paths(git).await?;
        if worktree
            .filtered(&tracked)
            .await
            .map_err(|e| internal(&e))?
            .is_some()
        {
            return Err(no_diff("the working tree is out of the diff scope"));
        }
        let (mut entries, mut truncated) = self.entries(git, &top, only.as_ref()).await?;
        let temp = TempDir::new(self.temp_parent).map_err(|e| internal(&e))?;
        let tmp_git = Git::new(&temp.0, self.options.clone());
        let root_fd = Arc::new(open_root(&top).map_err(|_| unknown())?);
        let mut patch = String::new();
        for entry in entries.drain(..) {
            let piece = self.piece(git, &tmp_git, &temp.0, &root_fd, &entry).await?;
            if patch.len() + piece.text.len() > MAX_DIFF_BYTES as usize {
                patch.push_str(CUT);
                truncated = true;
                break;
            }
            patch.push_str(&piece.text);
            summary.files_changed = summary.files_changed.saturating_add(u32::from(piece.shown));
            summary.insertions += piece.added;
            summary.deletions += piece.removed;
            truncated |= piece.cut;
        }
        Ok((summary, patch, truncated))
    }

    /// Every tracked path of the work tree.
    async fn tracked_paths(&self, git: &Git) -> Result<Vec<String>, FileError> {
        let (status, out, _) = git
            .bounded(&["ls-files", "-z"], LIST_MAX)
            .await
            .map_err(|e| internal(&e))?;
        if status != 0 {
            return Err(internal(&format!("git ls-files failed ({status})")));
        }
        Ok(nul_fields(&out)
            .into_iter()
            .filter_map(|f| std::str::from_utf8(f).ok().map(str::to_owned))
            .collect())
    }

    /// Step 1 and 2: the changed paths against the base, without the paths
    /// that have a private or a denied name, old or new. True when a limit
    /// left files out.
    async fn entries(
        &self,
        git: &Git,
        top: &Path,
        only: Option<&BTreeSet<String>>,
    ) -> Result<(Vec<Entry>, bool), FileError> {
        let (status, out, cut) = git
            .bounded(
                &[
                    "diff",
                    "--name-status",
                    "-z",
                    "-M",
                    "--no-ext-diff",
                    "--no-textconv",
                    "--no-color",
                    &self.base,
                ],
                LIST_MAX,
            )
            .await
            .map_err(|e| internal(&e))?;
        if status != 0 && !cut {
            return Err(internal(&format!(
                "git diff --name-status failed ({status})"
            )));
        }
        let mut entries = Vec::new();
        let mut fields = nul_fields(&out).into_iter();
        while let Some(tag) = fields.next() {
            let tag = String::from_utf8_lossy(tag).into_owned();
            let first = fields.next().and_then(|f| std::str::from_utf8(f).ok());
            let Some(first) = first.map(str::to_owned) else {
                break;
            };
            let (old, path, kind) = match tag.chars().next() {
                Some('R' | 'C') => {
                    let Some(new) = fields.next().and_then(|f| std::str::from_utf8(f).ok()) else {
                        break;
                    };
                    let kind = if tag.starts_with('R') {
                        Kind::Renamed
                    } else {
                        Kind::Other
                    };
                    (Some(first), new.to_owned(), kind)
                }
                Some('A') => (None, first, Kind::Added),
                Some('M') => (Some(first.clone()), first, Kind::Modified),
                Some('D') => (Some(first.clone()), first, Kind::Deleted),
                _ => (Some(first.clone()), first, Kind::Other),
            };
            entries.push(Entry { old, path, kind });
        }
        let (untracked, listing_cut) = self.untracked(git).await?;
        let mut truncated = cut || listing_cut;
        let over = untracked.len() > MAX_UNTRACKED;
        truncated |= over;
        entries.extend(untracked.into_iter().take(MAX_UNTRACKED).map(|path| Entry {
            old: None,
            path,
            kind: Kind::Added,
        }));
        entries.retain(|e| {
            only.is_none_or(|only| only.contains(&e.path))
                && [Some(&e.path), e.old.as_ref()]
                    .into_iter()
                    .flatten()
                    .all(|name| self.name_visible(top, name))
        });
        Ok((entries, truncated))
    }

    /// The name rules of step 2.
    fn name_visible(&self, top: &Path, path: &str) -> bool {
        !hidden_path(path)
            && !self.private.is_private_canonical(&top.join(path))
            && !self.policy.member_deny().denies(path)
    }

    /// The untracked files that Git lists (not ignored), sorted and once.
    async fn untracked(&self, git: &Git) -> Result<(Vec<String>, bool), FileError> {
        let (status, out, cut) = git
            .bounded(
                &["ls-files", "-z", "--others", "--exclude-standard"],
                LIST_MAX,
            )
            .await
            .map_err(|e| internal(&e))?;
        if status != 0 && !cut {
            return Err(internal(&format!("git ls-files failed ({status})")));
        }
        let mut paths: Vec<String> = nul_fields(&out)
            .into_iter()
            .filter_map(|f| std::str::from_utf8(f).ok().map(str::to_owned))
            .collect();
        paths.sort_unstable();
        paths.dedup();
        Ok((paths, cut))
    }

    /// The text of one file (steps 3 to 5).
    async fn piece(
        &self,
        git: &Git,
        tmp_git: &Git,
        temp: &Path,
        root_fd: &Arc<OwnedFd>,
        entry: &Entry,
    ) -> Result<Piece, FileError> {
        let head = format!("diff --coordinator {}\n", entry.path);
        let hidden = |why: &str, cut: bool| Piece {
            text: format!("{head}(hidden: {why})\n"),
            cut,
            ..Piece::default()
        };
        if entry.kind == Kind::Other {
            return Ok(hidden("diff unavailable", false));
        }
        if entry.kind == Kind::Deleted {
            return self.deleted(git, entry, &head).await;
        }
        let new = {
            let (root_fd, path) = (Arc::clone(root_fd), entry.path.clone());
            let (private, hooks) = (self.private.clone(), self.hooks.clone());
            tokio::task::spawn_blocking(move || new_side(&root_fd, &path, &private, &hooks))
                .await
                .map_err(|e| internal(&e))??
        };
        let bytes = match new {
            NewSide::Private => return Ok(hidden("private file", false)),
            NewSide::Unavailable { too_large } => {
                return Ok(hidden("diff unavailable", too_large));
            }
            NewSide::Bytes(bytes) => bytes,
        };
        if is_binary(&bytes) {
            return Ok(Piece {
                text: format!("{head}(binary file)\n"),
                shown: true,
                // A binary file shows no lines: the view says so (`truncated`).
                cut: true,
                ..Piece::default()
            });
        }
        let old = match &entry.old {
            None => Vec::new(),
            Some(old) => match self.old_side(git, old).await? {
                Some(bytes) => bytes,
                None => return Ok(hidden("diff unavailable", false)),
            },
        };
        let mut text = head;
        if entry.kind == Kind::Renamed {
            let from = entry.old.as_deref().unwrap_or_default();
            text.push_str(&format!("(renamed) {from} -> {}\n", entry.path));
        }
        let rendered = render(tmp_git, temp, &old, &bytes).await?;
        text.push_str(&rendered.text);
        Ok(Piece {
            text,
            added: rendered.added,
            removed: rendered.removed,
            shown: true,
            cut: false,
        })
    }

    /// A deleted file: the count of its lines, or the marker for a binary.
    async fn deleted(&self, git: &Git, entry: &Entry, head: &str) -> Result<Piece, FileError> {
        let old = entry.old.as_deref().unwrap_or(&entry.path);
        let Some(bytes) = self.old_side(git, old).await? else {
            return Ok(Piece {
                text: format!("{head}(hidden: diff unavailable)\n"),
                cut: true,
                ..Piece::default()
            });
        };
        let mut text = format!("{head}(deleted file)\n");
        let mut removed = 0;
        if !is_binary(&bytes) {
            removed = line_count(&bytes);
            if removed > 0 {
                text.push_str(&format!("-[{removed} lines removed]\n"));
            }
        }
        Ok(Piece {
            text,
            removed,
            shown: true,
            ..Piece::default()
        })
    }

    /// Step 4: the blob of `<base>:<path>`. It never leaves the station.
    /// `None` when it cannot be read or is over the read limit.
    async fn old_side(&self, git: &Git, path: &str) -> Result<Option<Vec<u8>>, FileError> {
        let spec = format!("{}:{path}", self.base);
        let limit = MAX_READ_BYTES as usize;
        let (status, out, cut) = git
            .bounded(&["cat-file", "blob", &spec], limit)
            .await
            .map_err(|e| internal(&e))?;
        Ok((status == 0 && !cut).then_some(out))
    }
}

/// True when the first bytes hold a NUL.
fn is_binary(bytes: &[u8]) -> bool {
    bytes[..bytes.len().min(BINARY_SNIFF_BYTES)].contains(&0)
}

/// The number of lines of a text: the last line counts without its newline.
fn line_count(bytes: &[u8]) -> u64 {
    let newlines = bytes.iter().filter(|b| **b == b'\n').count() as u64;
    newlines + u64::from(bytes.last().is_some_and(|b| *b != b'\n'))
}

/// The errors that mean "the path is not there (any more)".
fn is_gone(errno: Errno) -> bool {
    matches!(errno, Errno::NOENT | Errno::NOTDIR | Errno::LOOP)
}

/// Step 3: walks to `rel` from the pinned root with directory descriptors,
/// opens the file without following a link, checks that open file and reads
/// its bytes from the same descriptor. A check that cannot run is
/// `privacy_unknown`.
fn new_side(
    root: &OwnedFd,
    rel: &str,
    private: &PrivatePaths,
    hooks: &Hooks,
) -> Result<NewSide, FileError> {
    let parts: Vec<&str> = rel.split('/').collect();
    let Some((last, parents)) = parts.split_last() else {
        return Ok(NewSide::Unavailable { too_large: false });
    };
    (hooks.before_open)(rel);
    let parent = match descend(root, parents) {
        Ok(dir) => dir,
        Err(e) if is_gone(e) => return Ok(NewSide::Unavailable { too_large: false }),
        Err(_) => return Err(unknown()),
    };
    let fd = match rustix::fs::openat(&parent, *last, file_flags(), Mode::empty()) {
        Ok(fd) => fd,
        Err(e) if is_gone(e) => return Ok(NewSide::Unavailable { too_large: false }),
        Err(_) => return Err(unknown()),
    };
    let file = File::from(fd);
    let meta = file.metadata().map_err(|_| unknown())?;
    if !meta.is_file() {
        return Ok(NewSide::Unavailable { too_large: false });
    }
    if private.is_private_opened(&meta, rel) {
        return Ok(NewSide::Private);
    }
    (hooks.after_open)(rel);
    let limit = MAX_READ_BYTES;
    let mut bytes = Vec::new();
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| unknown())?;
    if bytes.len() as u64 > limit {
        return Ok(NewSide::Unavailable { too_large: true });
    }
    Ok(NewSide::Bytes(bytes))
}

#[path = "coordinator_diff_paths.rs"]
mod paths;
#[path = "coordinator_diff_render.rs"]
mod render;
use paths::{Shared, session_paths};
use render::render;
#[cfg(test)]
use render::rewrite;

#[cfg(test)]
#[path = "coordinator_diff_tests.rs"]
mod coordinator_diff_tests;
