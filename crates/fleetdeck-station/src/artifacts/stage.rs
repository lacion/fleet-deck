//! Copies the bytes of an artifact into a staging directory (R12, DL-165).
//!
//! The source root is canonical and inside the allowed roots (the caller
//! resolves it). It opens once as a directory descriptor, and every
//! component below it opens relative to its parent's descriptor with
//! `O_NOFOLLOW`, as the file service does: a symlink is never read, also
//! when one is swapped in while the copy runs. A file is streamed in chunks
//! with its SHA-256; there is no size cap.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use fleetdeck_proto::artifacts::{ArtifactKind, code};
use fleetdeck_proto::files::path_segments;
use ring::digest::{Context, SHA256};
use rustix::fs::{FileType, Mode, OFlags};

use super::ArtifactError;
use super::bundle;
use crate::store::artifacts::{Staged, StagedFile};
use crate::workspace::PrivatePaths;
use crate::workspace::files::open_root;

const CHUNK: usize = 64 * 1024;

fn dir_flags() -> OFlags {
    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC
}

fn file_flags() -> OFlags {
    OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC | OFlags::NOCTTY
}

fn not_found(path: &str) -> ArtifactError {
    ArtifactError::new(
        code::NOT_FOUND,
        format!("`{path}` is not a regular file in the workspace"),
    )
}

fn io_error(what: &str, e: impl std::fmt::Display) -> ArtifactError {
    ArtifactError::new("internal", format!("{what}: {e}"))
}

/// Opens the directory `segments` below `base`, one component at a time,
/// without following a symlink.
fn descend(base: &OwnedFd, segments: &[&str]) -> rustix::io::Result<OwnedFd> {
    let mut dir = rustix::fs::openat(base, c".", dir_flags(), Mode::empty())?;
    for segment in segments {
        dir = rustix::fs::openat(&dir, *segment, dir_flags(), Mode::empty())?;
    }
    Ok(dir)
}

/// Opens the regular file `name` of `dir`. `None` for anything else: a
/// missing name, a symlink, a directory, a FIFO, a device, or a hard link to
/// a private file of the station.
fn open_regular(dir: &OwnedFd, name: &str, private: &PrivatePaths) -> Option<File> {
    let fd = rustix::fs::openat(dir, name, file_flags(), Mode::empty()).ok()?;
    let stat = rustix::fs::fstat(fd.as_fd()).ok()?;
    if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile {
        return None;
    }
    let file = File::from(fd);
    (!file.metadata().is_ok_and(|m| private.is_private_file(&m))).then_some(file)
}

/// Streams `source` into `target` and returns its size and SHA-256.
fn copy_hashed(mut source: File, target: &Path) -> Result<(u64, String), ArtifactError> {
    if let Some(parent) = target.parent() {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(parent)
            .map_err(|e| io_error("staging directory", e))?;
    }
    let mut out = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(target)
        .map_err(|e| io_error("staging file", e))?;
    let mut ctx = Context::new(&SHA256);
    let mut buf = vec![0u8; CHUNK];
    let mut size = 0u64;
    loop {
        let n = match source.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(io_error("read", e)),
        };
        ctx.update(&buf[..n]);
        out.write_all(&buf[..n])
            .map_err(|e| io_error("staging write", e))?;
        size += n as u64;
    }
    out.flush().map_err(|e| io_error("staging write", e))?;
    let hex = ctx
        .finish()
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    Ok((size, hex))
}

/// Copies the file `rel` below `root` and, for an HTML page, the local
/// files that it names, into `staging`. The main file comes first. A private
/// path of the station ([`PrivatePaths`]) is never copied: the main file is
/// not found, and a bundled file that is private is left out like a missing
/// one.
///
/// # Errors
/// `invalid_path`, `not_found` or `internal`.
pub fn stage(
    root: &Path,
    rel: &str,
    staging: &Path,
    private: &PrivatePaths,
) -> Result<Staged, ArtifactError> {
    let segments = path_segments(rel).map_err(|c| {
        if c == code::NOT_FOUND {
            not_found(rel)
        } else {
            ArtifactError::new(code::INVALID_PATH, "a path has no `..`, NUL or `\\`")
        }
    })?;
    let Some((name, parents)) = segments.split_last() else {
        return Err(ArtifactError::new(code::INVALID_PATH, "the path is empty"));
    };
    let page_path = parents.iter().fold(root.to_path_buf(), |p, s| p.join(s));
    if private.is_private_canonical(&page_path.join(name)) {
        return Err(not_found(rel));
    }
    let base = open_root(root).map_err(|_| not_found(rel))?;
    let page_dir = descend(&base, parents).map_err(|_| not_found(rel))?;
    let main = open_regular(&page_dir, name, private).ok_or_else(|| not_found(rel))?;
    fs::create_dir_all(staging).map_err(|e| io_error("staging directory", e))?;
    let (size, sha256) = copy_hashed(main, &staging.join(name))?;
    let mut files = vec![StagedFile {
        path: (*name).to_owned(),
        size,
        sha256,
    }];
    if ArtifactKind::of_path(name) != ArtifactKind::Html {
        return Ok(Staged {
            dir: staging.to_path_buf(),
            files,
        });
    }
    let html = fs::read(staging.join(name)).map_err(|e| io_error("staging read", e))?;
    let html = String::from_utf8_lossy(&html);
    for reference in bundle::references(&html) {
        if reference == *name {
            continue;
        }
        let Ok(parts) = path_segments(&reference) else {
            continue;
        };
        let Some((last, dirs)) = parts.split_last() else {
            continue;
        };
        // A missing file, a symlink, a directory, a private file or a file
        // that cannot be opened is not part of the bundle: the page keeps
        // its link.
        if private.is_private_canonical(&page_path.join(&reference)) {
            continue;
        }
        let Ok(dir) = descend(&page_dir, dirs) else {
            continue;
        };
        let Some(file) = open_regular(&dir, last, private) else {
            continue;
        };
        let (size, sha256) = copy_hashed(file, &staging.join(&reference))?;
        files.push(StagedFile {
            path: reference,
            size,
            sha256,
        });
    }
    Ok(Staged {
        dir: staging.to_path_buf(),
        files,
    })
}

/// A fresh staging directory path below `root/.staging`.
#[must_use]
pub fn staging_dir(root: &Path, unique: &str) -> PathBuf {
    root.join(".staging").join(unique)
}
