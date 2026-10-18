//! The private staging file of one screenshot (design §5.2, §5.3 items 3
//! and 4):
//!
//! ```text
//! <captures>/                 0700
//! <captures>/<capture_id>/    0700
//! <captures>/<capture_id>/capture.png   0600
//! ```
//!
//! Every step works relative to a directory descriptor and follows no link.
//! The station keeps the descriptor of the file and never reopens the PNG by
//! its path.

use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::os::fd::OwnedFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

use fleetdeck_proto::capture::{code, text, valid_capture_id};
use ring::digest;
use rustix::fs::{AtFlags, CWD, Dir, FileType, Mode, OFlags};

use super::COPY_BUFFER;
use crate::desktop::{Fail, code as desktop_code, profile};

/// The name of the staging file.
const FILE_NAME: &str = "capture.png";

const DIR_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);

const FILE_FLAGS: OFlags = OFlags::RDWR
    .union(OFlags::CREATE)
    .union(OFlags::EXCL)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);

/// The identity of the staging file, taken right after it was created.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Ident {
    dev: u64,
    ino: u64,
    uid: u32,
    mode: u32,
}

/// One staging directory and its open file.
#[derive(Debug)]
pub(crate) struct Staging {
    dir: OwnedFd,
    file: File,
    ident: Ident,
}

fn failed() -> Fail {
    Fail::new(code::CAPTURE_FAILED, text::CAPTURE_FAILED)
}

fn open_dir(parent: &impl rustix::fd::AsFd, name: &Path) -> rustix::io::Result<OwnedFd> {
    rustix::fs::openat(parent, name, DIR_FLAGS, Mode::empty())
}

/// Opens `captures_root` without following a link, and creates it (0700)
/// when `create` is set and it is missing. `Ok(None)`: it is missing.
fn open_root(captures_root: &Path, create: bool) -> Result<Option<OwnedFd>, rustix::io::Errno> {
    match open_dir(&CWD, captures_root) {
        Ok(fd) => Ok(Some(fd)),
        Err(e) if e == rustix::io::Errno::NOENT && create => {
            match rustix::fs::mkdirat(CWD, captures_root, Mode::from_raw_mode(0o700)) {
                Ok(()) => {}
                Err(e) if e == rustix::io::Errno::EXIST => {}
                Err(e) => return Err(e),
            }
            open_dir(&CWD, captures_root).map(Some)
        }
        Err(e) if e == rustix::io::Errno::NOENT => Ok(None),
        Err(e) => Err(e),
    }
}

impl Staging {
    /// Makes `<captures_root>/<capture_id>/capture.png`.
    ///
    /// # Errors
    /// `invalid_params` for a capture ID that is not a ULID; `capture_failed`
    /// when `captures_root` or the ID directory is a link, a non-directory or
    /// exists, or when the file is not a fresh regular file of this user with
    /// one link.
    pub(crate) fn create(captures_root: &Path, capture_id: &str) -> Result<Self, Fail> {
        if !valid_capture_id(capture_id) {
            return Err(Fail::new(
                desktop_code::INVALID_PARAMS,
                "The capture ID is not valid.",
            ));
        }
        let root = open_root(captures_root, true)
            .map_err(|_| failed())?
            .ok_or_else(failed)?;
        rustix::fs::mkdirat(&root, capture_id, Mode::from_raw_mode(0o700)).map_err(|_| failed())?;
        match Self::make_file(&root, capture_id) {
            Ok(staging) => Ok(staging),
            Err(fail) => {
                if profile::remove_in(&root, capture_id).is_err() {
                    tracing::warn!(
                        code = fail.code,
                        "cannot remove the half-made capture directory"
                    );
                }
                Err(fail)
            }
        }
    }

    fn make_file(root: &OwnedFd, capture_id: &str) -> Result<Self, Fail> {
        let dir = open_dir(root, Path::new(capture_id)).map_err(|_| failed())?;
        let fd = rustix::fs::openat(&dir, FILE_NAME, FILE_FLAGS, Mode::from_raw_mode(0o600))
            .map_err(|_| failed())?;
        let file = File::from(fd);
        let meta = file.metadata().map_err(|_| failed())?;
        if !meta.is_file() || meta.nlink() != 1 || meta.uid() != rustix::process::geteuid().as_raw()
        {
            return Err(failed());
        }
        let ident = Ident {
            dev: meta.dev(),
            ino: meta.ino(),
            uid: meta.uid(),
            mode: meta.mode(),
        };
        Ok(Self { dir, file, ident })
    }

    /// The descriptor of `<captures>/<capture_id>/`.
    pub(crate) fn dir(&self) -> &OwnedFd {
        &self.dir
    }

    /// The retained descriptor of the PNG.
    pub(crate) fn file(&mut self) -> &mut File {
        &mut self.file
    }

    /// The SHA-256 of the file as lowercase hex. It rewinds the retained
    /// descriptor first.
    ///
    /// # Errors
    /// `capture_failed` when the file cannot be read.
    pub(crate) fn sha256(&mut self) -> Result<String, Fail> {
        self.file.seek(SeekFrom::Start(0)).map_err(|_| failed())?;
        let mut ctx = digest::Context::new(&digest::SHA256);
        let mut buf = vec![0_u8; COPY_BUFFER];
        loop {
            match self.file.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => ctx.update(&buf[..n]),
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => return Err(failed()),
            }
        }
        let mut hex = String::with_capacity(64);
        for byte in ctx.finish().as_ref() {
            hex.push_str(&format!("{byte:02x}"));
        }
        Ok(hex)
    }

    /// Checks the retained descriptor again: the same device and inode, `len`
    /// bytes, mode 0600, the same owner and one link.
    ///
    /// # Errors
    /// `capture_failed` when one of them changed.
    pub(crate) fn recheck(&self, len: u64) -> Result<(), Fail> {
        let meta = self.file.metadata().map_err(|_| failed())?;
        let same = meta.is_file()
            && meta.dev() == self.ident.dev
            && meta.ino() == self.ident.ino
            && meta.uid() == self.ident.uid
            && meta.mode() == self.ident.mode
            && meta.mode() & 0o7777 == 0o600
            && meta.size() == len
            && meta.nlink() == 1;
        if same { Ok(()) } else { Err(failed()) }
    }

    /// Removes `<captures_root>/<capture_id>/` with its content. A missing
    /// directory is fine.
    ///
    /// # Errors
    /// `capture_failed` when `captures_root` or the entry is a link or not a
    /// directory, or the removal fails.
    pub(crate) fn remove(captures_root: &Path, capture_id: &str) -> Result<(), Fail> {
        profile::remove(captures_root, capture_id).map_err(|_| failed())
    }

    /// Removes every entry below `captures_root` (the startup recovery). A
    /// link is removed itself and never followed. A failure writes a warning
    /// with the error kind and the other entries go on. A missing
    /// `captures_root` is silent.
    pub(crate) fn remove_all(captures_root: &Path) {
        let root = match open_root(captures_root, false) {
            Ok(Some(root)) => root,
            Ok(None) => return,
            Err(e) => {
                warn_kind(&std::io::Error::from(e));
                return;
            }
        };
        let entries = match list(&root) {
            Ok(entries) => entries,
            Err(e) => {
                warn_kind(&e);
                return;
            }
        };
        for (name, is_dir) in entries {
            let result = if is_dir {
                match name.to_str() {
                    Some(plain) => profile::remove_in(&root, plain).map_err(|e| match e {
                        profile::ProfileError::NotPlain => {
                            std::io::Error::from(std::io::ErrorKind::InvalidInput)
                        }
                        profile::ProfileError::Io(e) => e,
                    }),
                    None => Err(std::io::Error::from(std::io::ErrorKind::InvalidInput)),
                }
            } else {
                rustix::fs::unlinkat(&root, &name, AtFlags::empty()).map_err(std::io::Error::from)
            };
            if let Err(e) = result {
                warn_kind(&e);
            }
        }
    }
}

fn warn_kind(e: &std::io::Error) {
    tracing::warn!(kind = ?e.kind(), "cannot remove a capture staging entry");
}

/// The entries of `root` with a flag for a directory; a link is not one.
fn list(root: &OwnedFd) -> std::io::Result<Vec<(OsString, bool)>> {
    let mut entries = Vec::new();
    for entry in Dir::read_from(root)? {
        let entry = entry?;
        let raw = entry.file_name().to_bytes();
        if raw == b"." || raw == b".." {
            continue;
        }
        let name = OsStr::from_bytes(raw).to_owned();
        let is_dir = match entry.file_type() {
            FileType::Directory => true,
            FileType::Unknown => {
                let stat = rustix::fs::statat(root, &name, AtFlags::SYMLINK_NOFOLLOW)?;
                FileType::from_raw_mode(stat.st_mode) == FileType::Directory
            }
            _ => false,
        };
        entries.push((name, is_dir));
    }
    Ok(entries)
}
