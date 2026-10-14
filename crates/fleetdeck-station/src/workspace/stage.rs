//! Checked staging of the changed tracked paths (FDV2-003).
//!
//! `git add -u` reads every changed tracked path itself, so a tracked path
//! that is replaced by a hard link to a credential file would put the
//! credential bytes into a Git object. This module replaces it:
//!
//! 1. `git diff-files` (private index, after a refresh, no content read of
//!    a changed path) lists the tracked paths that differ from the index.
//!    For an [`IndexTrust::Untrusted`] index (a copy of the real index that
//!    the agent could write) `git diff-index --cached` also lists the
//!    entries whose blob or mode differs from HEAD: such a blob may be one
//!    that the agent staged (`git add -u` after a credential hard link), so
//!    its path is checked too.
//! 2. The station walks to each such path from a pinned root with
//!    directory descriptors (no symlink is followed at any level), opens
//!    the last component without following a symlink and checks the
//!    **opened file**: a private file ([`PrivatePaths`], the inode
//!    registry, a link count above 1) is left out. The bytes come from that
//!    same descriptor, so a swap after the check cannot make the station
//!    read another file.
//! 3. Each blob is written with `git hash-object -w --stdin` (the bytes on
//!    stdin, no path), and one `git update-index -z --index-info` call
//!    applies every entry. A path that is left out keeps the blob that the
//!    index had; an untrusted entry that differs from HEAD is reset to the
//!    HEAD blob and mode (or removed when HEAD has none).

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::os::fd::OwnedFd;
use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;

use rustix::fs::{AtFlags, FileType, Mode};
use rustix::io::Errno;

use super::checkpoint::{CheckpointError, Limits, PrivateIndex, Result, Unavailable, Worktree};
use super::files::{descend, file_flags, open_root};
use super::git::{GitError, nul_fields};
use super::private_paths::PrivatePaths;

/// The most changed paths that one snapshot stages.
const MAX_CHANGED: usize = 5000;

/// Git file modes of a staged entry.
const MODE_FILE: &str = "100644";
const MODE_EXEC: &str = "100755";
const MODE_LINK: &str = "120000";

/// A function that a test runs after a changed file is opened and checked
/// and before its bytes are read (it gets the path below the root).
pub(super) type AfterOpen = Arc<dyn Fn(&str) + Send + Sync>;

/// A function that a test runs for each changed path right before the
/// directory walk to it (it gets the path below the root).
pub(super) type BeforeWalk = Arc<dyn Fn(&str) + Send + Sync>;

/// Whether the entries of the index that is staged into are trusted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum IndexTrust {
    /// A copy of the real index: the agent could have staged any blob.
    /// The entries that differ from HEAD are checked from the work tree.
    Untrusted,
    /// An index that the station made itself (`read-tree` of a checkpoint
    /// tree). Its entries are committed content.
    Trusted,
}

/// `Err` when `n` changed paths are more than one snapshot stages.
pub(super) fn check_change_count(n: usize) -> std::result::Result<(), Unavailable> {
    if n > MAX_CHANGED {
        Err(Unavailable::TooManyChanges(n))
    } else {
        Ok(())
    }
}

/// What the station decided for one changed path.
enum Change {
    /// The path is gone (or lies below something that is not a directory).
    Delete,
    /// The path is private: it is not staged.
    Hidden,
    /// The blob that the path has now.
    Blob { mode: &'static str, bytes: Vec<u8> },
}

/// The mode and the object of a path in HEAD.
#[derive(Debug, Clone)]
struct HeadEntry {
    mode: String,
    oid: String,
}

fn io_error(errno: Errno) -> CheckpointError {
    CheckpointError::Io(errno.into())
}

/// The errors of a walk step that mean "this path is not there": the name
/// is missing, or a component is not a directory (a file, or a symlink that
/// the no-follow open refused). Git treats such a tracked path as removed,
/// and so does this module.
fn is_gone(errno: Errno) -> bool {
    matches!(errno, Errno::NOENT | Errno::NOTDIR | Errno::LOOP)
}

/// Walks to `rel` from the pinned `root` with directory descriptors,
/// opens the last component without following a symlink, checks the opened
/// file, and reads it. The result is one of [`Change`], or the reason that
/// the snapshot is unavailable.
fn read_change(
    root: &OwnedFd,
    rel: &str,
    private: &PrivatePaths,
    limits: Limits,
    before_walk: &(dyn Fn(&str) + Send + Sync),
    after_open: &(dyn Fn(&str) + Send + Sync),
) -> Result<std::result::Result<Change, Unavailable>> {
    before_walk(rel);
    let parts: Vec<&str> = rel.split('/').collect();
    let Some((last, parents)) = parts.split_last() else {
        return Ok(Ok(Change::Delete));
    };
    let parent = match descend(root, parents) {
        Ok(dir) => dir,
        Err(e) if is_gone(e) => return Ok(Ok(Change::Delete)),
        Err(e) => return Err(io_error(e)),
    };
    let stat = match rustix::fs::statat(&parent, *last, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(stat) => stat,
        Err(e) if is_gone(e) => return Ok(Ok(Change::Delete)),
        Err(e) => return Err(io_error(e)),
    };
    match FileType::from_raw_mode(stat.st_mode) {
        FileType::Symlink => {
            return match rustix::fs::readlinkat(&parent, *last, Vec::new()) {
                Ok(target) => Ok(Ok(Change::Blob {
                    mode: MODE_LINK,
                    bytes: target.into_bytes(),
                })),
                Err(e) if is_gone(e) => Ok(Ok(Change::Delete)),
                Err(e) => Err(io_error(e)),
            };
        }
        FileType::RegularFile => {}
        _ => return Ok(Err(Unavailable::NotRegular(rel.to_owned()))),
    }
    let fd = match rustix::fs::openat(&parent, *last, file_flags(), Mode::empty()) {
        Ok(fd) => fd,
        Err(Errno::LOOP) => return Ok(Err(Unavailable::NotRegular(rel.to_owned()))),
        Err(e) if is_gone(e) => return Ok(Ok(Change::Delete)),
        Err(e) => return Err(io_error(e)),
    };
    let file = std::fs::File::from(fd);
    // The identity of the opened file, not of its name.
    let opened = file.metadata()?;
    if !opened.is_file() {
        return Ok(Err(Unavailable::NotRegular(rel.to_owned())));
    }
    if private.is_private_opened(&opened, rel) {
        return Ok(Ok(Change::Hidden));
    }
    after_open(rel);
    if opened.len() > limits.max_file_bytes {
        return Ok(Err(Unavailable::FileTooLarge {
            path: rel.to_owned(),
            bytes: opened.len(),
        }));
    }
    let mut bytes = Vec::new();
    file.take(limits.max_file_bytes.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limits.max_file_bytes {
        return Ok(Err(Unavailable::FileTooLarge {
            path: rel.to_owned(),
            bytes: bytes.len() as u64,
        }));
    }
    let mode = if opened.permissions().mode() & 0o100 != 0 {
        MODE_EXEC
    } else {
        MODE_FILE
    };
    Ok(Ok(Change::Blob { mode, bytes }))
}

/// `Ok` with the text of a path field, or `NonUtf8Path`.
fn utf8_path(field: &[u8]) -> std::result::Result<&str, Unavailable> {
    std::str::from_utf8(field).map_err(|_| Unavailable::NonUtf8Path)
}

impl Worktree {
    /// Stages the changed tracked paths into `index` (a private copy of the
    /// index) from checked bytes. Returns the tracked paths that were left
    /// out because the file at the path is private.
    pub(super) async fn stage_changes(
        &self,
        index: &PrivateIndex,
        limits: Limits,
        private: &PrivatePaths,
        trust: IndexTrust,
    ) -> Result<std::result::Result<Vec<String>, Unavailable>> {
        self.stage_changes_with(
            index,
            limits,
            private,
            trust,
            Arc::new(|_| {}),
            Arc::new(|_| {}),
        )
        .await
    }

    /// [`Self::stage_changes`] with a function that runs after each opened
    /// file is checked and before its bytes are read, and a function that
    /// runs for each path right before the directory walk to it.
    pub(super) async fn stage_changes_with(
        &self,
        index: &PrivateIndex,
        limits: Limits,
        private: &PrivatePaths,
        trust: IndexTrust,
        after_open: AfterOpen,
        before_walk: BeforeWalk,
    ) -> Result<std::result::Result<Vec<String>, Unavailable>> {
        let env = [("GIT_INDEX_FILE", index.path().as_os_str())];
        // A path that was only touched (new file times, same content) is not
        // a change: the refresh marks it clean, so `diff-files` lists the
        // paths that differ in content, as `git add -u` staged them. A
        // `read-tree` index has no file times at all. Exit code 1 means that
        // some path still needs an update.
        let (code, _, stderr) = self
            .git()
            .status(&["update-index", "-q", "--refresh"], &env, None)
            .await?;
        if code != 0 && code != 1 {
            return Err(GitError::Failed {
                args: "update-index -q --refresh".to_owned(),
                code: code.to_string(),
                stderr,
            }
            .into());
        }
        let listed = self
            .git()
            .call(
                &["diff-files", "-z", "--name-status", "--no-renames"],
                &env,
                None,
            )
            .await?;
        let fields = nul_fields(&listed);
        if let Err(why) = check_change_count(fields.len() / 2) {
            return Ok(Err(why));
        }
        let mut changed: BTreeSet<String> = BTreeSet::new();
        for pair in fields.chunks(2) {
            let [status, path] = pair else { continue };
            let path = match utf8_path(path) {
                Ok(path) => path,
                Err(why) => return Ok(Err(why)),
            };
            // `M` modified, `T` type change, `A` intent to add, `D` deleted.
            // The work tree decides below, so `D` needs no special case.
            match *status {
                b"M" | b"T" | b"A" | b"D" => {
                    changed.insert(path.to_owned());
                }
                other => {
                    return Ok(Err(Unavailable::UnknownIndexEntry {
                        path: path.to_owned(),
                        tag: String::from_utf8_lossy(other).into_owned(),
                    }));
                }
            }
        }
        // The entries of an untrusted index that differ from HEAD.
        let mut head: BTreeMap<String, Option<HeadEntry>> = BTreeMap::new();
        if trust == IndexTrust::Untrusted {
            head = match self.index_against_head(&env).await? {
                Ok(head) => head,
                Err(why) => return Ok(Err(why)),
            };
            changed.extend(head.keys().cloned());
            if let Err(why) = check_change_count(changed.len()) {
                return Ok(Err(why));
            }
        }
        let mut hidden = Vec::new();
        let mut records: Vec<u8> = Vec::new();
        let mut zero: Option<String> = None;
        let root_fd = open_root(self.root()).map_err(io_error)?;
        let root_fd = Arc::new(root_fd);
        let shared = Arc::new(private.clone());
        for rel in changed {
            let checked = {
                let rel = rel.clone();
                let root_fd = Arc::clone(&root_fd);
                let private = Arc::clone(&shared);
                let after_open = Arc::clone(&after_open);
                let before_walk = Arc::clone(&before_walk);
                tokio::task::spawn_blocking(move || {
                    read_change(
                        &root_fd,
                        &rel,
                        &private,
                        limits,
                        &*before_walk,
                        &*after_open,
                    )
                })
                .await
                .map_err(|e| CheckpointError::Io(std::io::Error::other(e)))??
            };
            let change = match checked {
                Ok(change) => change,
                Err(why) => return Ok(Err(why)),
            };
            match change {
                Change::Hidden => {
                    // An untrusted entry that differs from HEAD goes back
                    // to the HEAD blob; any other entry keeps the blob that
                    // the index had (it is HEAD's).
                    match head.get(&rel) {
                        Some(Some(entry)) => {
                            push_record(&mut records, &entry.mode, &entry.oid, &rel);
                        }
                        Some(None) => {
                            let zero = self.zero_cached(&mut zero).await?;
                            push_record(&mut records, "0", &zero, &rel);
                        }
                        None => {}
                    }
                    hidden.push(rel);
                }
                Change::Delete => {
                    let zero = self.zero_cached(&mut zero).await?;
                    push_record(&mut records, "0", &zero, &rel);
                }
                Change::Blob { mode, bytes } => {
                    let oid = self
                        .git()
                        .call(&["hash-object", "-w", "--stdin"], &[], Some(bytes))
                        .await?;
                    let oid = String::from_utf8_lossy(&oid).trim().to_owned();
                    push_record(&mut records, mode, &oid, &rel);
                }
            }
        }
        if !records.is_empty() {
            self.git()
                .call(&["update-index", "-z", "--index-info"], &env, Some(records))
                .await?;
        }
        Ok(Ok(hidden))
    }

    /// The entries of the index in `env` whose blob or mode differs from
    /// HEAD, each with its HEAD entry (`None`: HEAD has no such path). For
    /// a repository without HEAD every entry of the index is listed.
    async fn index_against_head(
        &self,
        env: &[(&str, &std::ffi::OsStr)],
    ) -> Result<std::result::Result<BTreeMap<String, Option<HeadEntry>>, Unavailable>> {
        let mut out: BTreeMap<String, Option<HeadEntry>> = BTreeMap::new();
        let (code, _, stderr) = self
            .git()
            .status(&["rev-parse", "--verify", "-q", "HEAD"], &[], None)
            .await?;
        match code {
            0 => {}
            1 => {
                let all = self.git().call(&["ls-files", "-z"], env, None).await?;
                for path in nul_fields(&all) {
                    match utf8_path(path) {
                        Ok(path) => out.insert(path.to_owned(), None),
                        Err(why) => return Ok(Err(why)),
                    };
                }
                return Ok(Ok(out));
            }
            other => {
                return Err(GitError::Failed {
                    args: "rev-parse --verify -q HEAD".to_owned(),
                    code: other.to_string(),
                    stderr,
                }
                .into());
            }
        }
        let listed = self
            .git()
            .call(
                &[
                    "diff-index",
                    "--cached",
                    "-z",
                    "--raw",
                    "--no-renames",
                    "HEAD",
                ],
                env,
                None,
            )
            .await?;
        for pair in nul_fields(&listed).chunks(2) {
            let [meta, path] = pair else { continue };
            let path = match utf8_path(path) {
                Ok(path) => path,
                Err(why) => return Ok(Err(why)),
            };
            // `:<src mode> <dst mode> <src oid> <dst oid> <status>`
            let meta = String::from_utf8_lossy(meta);
            let parts: Vec<&str> = meta.trim_start_matches(':').split(' ').collect();
            let [src_mode, _, src_oid, _, status] = parts.as_slice() else {
                return Ok(Err(Unavailable::UnknownIndexEntry {
                    path: path.to_owned(),
                    tag: meta.into_owned(),
                }));
            };
            match *status {
                // The path is not in the index: there is no blob to trust.
                "D" => {}
                "M" | "T" | "A" => {
                    let in_head = src_oid.bytes().any(|b| b != b'0');
                    let entry = in_head.then(|| HeadEntry {
                        mode: (*src_mode).to_owned(),
                        oid: (*src_oid).to_owned(),
                    });
                    out.insert(path.to_owned(), entry);
                }
                other => {
                    return Ok(Err(Unavailable::UnknownIndexEntry {
                        path: path.to_owned(),
                        tag: other.to_owned(),
                    }));
                }
            }
        }
        Ok(Ok(out))
    }

    /// The all-zero object id, asked once.
    async fn zero_cached(&self, cache: &mut Option<String>) -> Result<String> {
        if cache.is_none() {
            *cache = Some(self.zero_oid().await?);
        }
        Ok(cache.clone().unwrap_or_default())
    }

    /// The all-zero object id of the repository's hash algorithm.
    async fn zero_oid(&self) -> Result<String> {
        let format = self
            .git()
            .text(&["rev-parse", "--show-object-format"])
            .await?;
        let len = if format.trim() == "sha256" { 64 } else { 40 };
        Ok("0".repeat(len))
    }
}

/// One `--index-info` record: `<mode> <object>\t<path>` and a NUL.
fn push_record(records: &mut Vec<u8>, mode: &str, oid: &str, path: &str) {
    records.extend_from_slice(mode.as_bytes());
    records.push(b' ');
    records.extend_from_slice(oid.as_bytes());
    records.push(b'\t');
    records.extend_from_slice(path.as_bytes());
    records.push(0);
}
