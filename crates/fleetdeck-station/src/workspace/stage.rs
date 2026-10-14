//! Checked staging of the changed tracked paths (FDV2-003).
//!
//! `git add -u` reads every changed tracked path itself, so a tracked path
//! that is replaced by a hard link to a credential file would put the
//! credential bytes into a Git object. This module replaces it:
//!
//! 1. `git diff-files` (private index, no refresh, no content read of a
//!    changed path) lists the tracked paths that differ from the index.
//! 2. The station opens each changed path without following a symlink and
//!    checks the **opened file**: a private file ([`PrivatePaths`], the
//!    inode registry, a link count above 1) is left out. The bytes come
//!    from that same descriptor, so a swap after the check cannot make the
//!    station read another file.
//! 3. Each blob is written with `git hash-object -w --stdin` (the bytes on
//!    stdin, no path), and one `git update-index -z --index-info` call
//!    applies every entry. A path that is left out keeps the blob that the
//!    index had.

use std::io::Read;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;
use std::sync::Arc;

use rustix::fs::OFlags;

use super::checkpoint::{CheckpointError, Limits, PrivateIndex, Result, Unavailable, Worktree};
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

/// The `open` flags of a checked read: the same as the file service.
fn open_flags() -> i32 {
    let flags = OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::NOCTTY;
    i32::try_from(flags.bits()).unwrap_or(0)
}

fn is_loop(error: &std::io::Error) -> bool {
    error.raw_os_error() == Some(rustix::io::Errno::LOOP.raw_os_error())
}

/// True when an ancestor of `rel` below `root` is not a real directory
/// (a symlink, a file, or missing). Git treats such a tracked path as
/// removed, and so does this module.
fn ancestor_is_not_a_directory(root: &Path, rel: &str) -> bool {
    let mut dir = root.to_path_buf();
    let parts: Vec<&str> = rel.split('/').collect();
    for part in parts.iter().take(parts.len().saturating_sub(1)) {
        dir.push(part);
        match std::fs::symlink_metadata(&dir) {
            Ok(meta) if meta.is_dir() => {}
            _ => return true,
        }
    }
    false
}

/// Opens `rel` below `root` without following a symlink, checks the opened
/// file, and reads it. The result is one of [`Change`], or the reason that
/// the snapshot is unavailable.
fn read_change(
    root: &Path,
    rel: &str,
    private: &PrivatePaths,
    limits: Limits,
    after_open: &(dyn Fn(&str) + Send + Sync),
) -> Result<std::result::Result<Change, Unavailable>> {
    if ancestor_is_not_a_directory(root, rel) {
        return Ok(Ok(Change::Delete));
    }
    let path = root.join(rel);
    let meta = match std::fs::symlink_metadata(&path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Ok(Change::Delete)),
        Err(e) => return Err(e.into()),
    };
    if meta.file_type().is_symlink() {
        return match std::fs::read_link(&path) {
            Ok(target) => Ok(Ok(Change::Blob {
                mode: MODE_LINK,
                bytes: target.as_os_str().as_bytes().to_vec(),
            })),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Ok(Change::Delete)),
            Err(e) => Err(e.into()),
        };
    }
    if !meta.is_file() {
        return Ok(Err(Unavailable::NotRegular(rel.to_owned())));
    }
    let file = std::fs::File::options()
        .read(true)
        .custom_flags(open_flags())
        .open(&path);
    let file = match file {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Ok(Change::Delete)),
        Err(e) if is_loop(&e) => return Ok(Err(Unavailable::NotRegular(rel.to_owned()))),
        Err(e) => return Err(e.into()),
    };
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

impl Worktree {
    /// Stages the changed tracked paths into `index` (a private copy of the
    /// index) from checked bytes. Returns the tracked paths that were left
    /// out because the file at the path is private.
    pub(super) async fn stage_changes(
        &self,
        index: &PrivateIndex,
        limits: Limits,
        private: &PrivatePaths,
    ) -> Result<std::result::Result<Vec<String>, Unavailable>> {
        self.stage_changes_with(index, limits, private, Arc::new(|_| {}))
            .await
    }

    /// [`Self::stage_changes`] with a function that runs after each opened
    /// file is checked and before its bytes are read.
    pub(super) async fn stage_changes_with(
        &self,
        index: &PrivateIndex,
        limits: Limits,
        private: &PrivatePaths,
        after_open: AfterOpen,
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
        let mut changed: Vec<String> = Vec::new();
        for pair in fields.chunks(2) {
            let [status, path] = pair else { continue };
            let Ok(path) = std::str::from_utf8(path) else {
                return Ok(Err(Unavailable::NonUtf8Path));
            };
            // `M` modified, `T` type change, `A` intent to add, `D` deleted.
            // The work tree decides below, so `D` needs no special case.
            match *status {
                b"M" | b"T" | b"A" | b"D" => changed.push(path.to_owned()),
                other => {
                    return Ok(Err(Unavailable::UnknownIndexEntry {
                        path: path.to_owned(),
                        tag: String::from_utf8_lossy(other).into_owned(),
                    }));
                }
            }
        }
        let mut hidden = Vec::new();
        let mut records: Vec<u8> = Vec::new();
        let mut zero: Option<String> = None;
        let root = self.root().to_path_buf();
        let shared = Arc::new(private.clone());
        for rel in changed {
            let checked = {
                let rel = rel.clone();
                let root = root.clone();
                let private = Arc::clone(&shared);
                let after_open = Arc::clone(&after_open);
                tokio::task::spawn_blocking(move || {
                    read_change(&root, &rel, &private, limits, &*after_open)
                })
                .await
                .map_err(|e| CheckpointError::Io(std::io::Error::other(e)))??
            };
            let change = match checked {
                Ok(change) => change,
                Err(why) => return Ok(Err(why)),
            };
            match change {
                Change::Hidden => hidden.push(rel),
                Change::Delete => {
                    if zero.is_none() {
                        zero = Some(self.zero_oid().await?);
                    }
                    let zero = zero.as_deref().unwrap_or_default();
                    push_record(&mut records, "0", zero, &rel);
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
