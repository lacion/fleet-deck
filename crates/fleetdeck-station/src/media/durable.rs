//! The syncs of an import and the durable removal of a folder tree (R13
//! P5b, design `omarchy-p5b.md` §5.3, decision O12-7).
//!
//! A removal is finished only when it is durable: every file is unlinked,
//! each emptied folder is synced and removed, and the parent is synced at
//! the end. A failed sync is a failed removal: the caller keeps its
//! reservation and its cleanup state, and the start retries it.

use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io;
use std::os::fd::OwnedFd;
use std::os::unix::ffi::OsStrExt;

use rustix::fs::{AtFlags, Dir, FileType, Mode, OFlags};

/// The deepest folder level that the removal follows down.
const MAX_DEPTH: u32 = 64;

const DIR_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);

/// The syncs of the import. The production value calls `fsync`; a test
/// records the calls or fails one of them.
pub trait DurableFs: Send + Sync {
    /// Makes the data of the file durable.
    ///
    /// # Errors
    /// The I/O error of the sync.
    fn sync_file(&self, f: &File) -> io::Result<()>;

    /// Makes the entries of the directory durable.
    ///
    /// # Errors
    /// The I/O error of the sync.
    fn sync_dir(&self, d: &OwnedFd) -> io::Result<()>;
}

/// The production [`DurableFs`]: `fsync`.
#[derive(Debug, Clone, Copy, Default)]
pub struct SyncFs;

impl DurableFs for SyncFs {
    fn sync_file(&self, f: &File) -> io::Result<()> {
        rustix::fs::fsync(f).map_err(io::Error::from)
    }

    fn sync_dir(&self, d: &OwnedFd) -> io::Result<()> {
        rustix::fs::fsync(d).map_err(io::Error::from)
    }
}

/// One entry of a folder: its name and what it is.
struct Entry {
    name: OsString,
    is_dir: bool,
}

/// Removes the folder `name` of `parent` with everything inside, durably.
///
/// The traversal is relative to descriptors and never follows a link: each
/// folder is opened with `O_NOFOLLOW | O_DIRECTORY`. Files are unlinked
/// depth-first; each emptied folder is synced, then removed; at the end
/// `parent` is synced. A missing `name` is already removed: `parent` is
/// synced and the call returns `Ok`.
///
/// # Errors
/// `InvalidInput` when `name` is not one plain file name, when the tree is
/// deeper than 64 levels, or when an entry is neither a regular file nor a
/// folder (a link included): the entries of that folder are then not
/// touched. Any other I/O error, a failed sync included. After an `Err` the
/// removal is not finished and the caller must not treat it as done.
pub fn remove_tree_durable(parent: &OwnedFd, name: &str, fs: &dyn DurableFs) -> io::Result<()> {
    if name.is_empty() || name.contains(['/', '\0']) || name == "." || name == ".." {
        return Err(io::Error::from(io::ErrorKind::InvalidInput));
    }
    match remove_at(parent, OsStr::new(name), fs, 0) {
        Ok(()) => {}
        Err(e) if e.raw_os_error() == Some(rustix::io::Errno::NOENT.raw_os_error()) => {}
        Err(e) => return Err(e),
    }
    fs.sync_dir(parent)
}

/// Removes the folder `name` of `parent`: the content first, a sync of the
/// emptied folder, then the folder itself.
fn remove_at(parent: &OwnedFd, name: &OsStr, fs: &dyn DurableFs, depth: u32) -> io::Result<()> {
    if depth > MAX_DEPTH {
        return Err(io::Error::from(io::ErrorKind::InvalidInput));
    }
    let child = rustix::fs::openat(parent, name, DIR_FLAGS, Mode::empty())?;
    // Read and check every entry first: the folder changes while it is
    // emptied, and an entry of a wrong type stops the removal before this
    // folder loses anything.
    let mut entries: Vec<Entry> = Vec::new();
    for entry in Dir::read_from(&child)? {
        let entry = entry?;
        let raw = entry.file_name().to_bytes();
        if raw == b"." || raw == b".." {
            continue;
        }
        let entry_name = OsStr::from_bytes(raw).to_owned();
        let kind = match entry.file_type() {
            FileType::Unknown => {
                let stat = rustix::fs::statat(&child, &entry_name, AtFlags::SYMLINK_NOFOLLOW)?;
                FileType::from_raw_mode(stat.st_mode)
            }
            kind => kind,
        };
        match kind {
            FileType::RegularFile => entries.push(Entry {
                name: entry_name,
                is_dir: false,
            }),
            FileType::Directory => entries.push(Entry {
                name: entry_name,
                is_dir: true,
            }),
            _ => return Err(io::Error::from(io::ErrorKind::InvalidInput)),
        }
    }
    for entry in entries {
        if entry.is_dir {
            remove_at(&child, &entry.name, fs, depth + 1)?;
        } else {
            rustix::fs::unlinkat(&child, &entry.name, AtFlags::empty())?;
        }
    }
    fs.sync_dir(&child)?;
    rustix::fs::unlinkat(parent, name, AtFlags::REMOVEDIR)?;
    Ok(())
}

#[cfg(test)]
#[path = "durable_tests.rs"]
mod tests;
