//! The private staging file of one capture (design §5.2, §5.3 items 3 and 4;
//! R13 P5b §9.2):
//!
//! ```text
//! <captures>/                 0700
//! <captures>/<capture_id>/    0700
//! <captures>/<capture_id>/<file name>   0600   (capture.png or capture.mp4)
//! ```
//!
//! Every step works relative to a directory descriptor and follows no link.
//! The station keeps the descriptor of the file and never reopens it by its
//! path. The removal is durable ([`Staging::remove`]).

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::os::fd::OwnedFd;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

use fleetdeck_proto::capture::{code, text, valid_capture_id};
use ring::digest;
use rustix::fs::{CWD, Mode, OFlags};

use super::COPY_BUFFER;
use crate::desktop::{Fail, code as desktop_code, profile};
use crate::media::durable::{DurableFs, remove_tree_durable};

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
    /// Makes `<captures_root>/<capture_id>/<file_name>`; `file_name` is one
    /// plain name (`capture.png` or `capture.mp4`).
    ///
    /// # Errors
    /// `invalid_params` for a capture ID that is not a ULID; `capture_failed`
    /// when `file_name` is not one plain name, when `captures_root` or the ID
    /// directory is a link, a non-directory or exists, or when the file is
    /// not a fresh regular file of this user with one link.
    pub(crate) fn create(
        captures_root: &Path,
        capture_id: &str,
        file_name: &str,
    ) -> Result<Self, Fail> {
        if !valid_capture_id(capture_id) {
            return Err(Fail::new(
                desktop_code::INVALID_PARAMS,
                "The capture ID is not valid.",
            ));
        }
        let plain = !file_name.is_empty()
            && !file_name.contains(['/', '\0'])
            && file_name != "."
            && file_name != "..";
        if !plain {
            return Err(failed());
        }
        let root = open_root(captures_root, true)
            .map_err(|_| failed())?
            .ok_or_else(failed)?;
        rustix::fs::mkdirat(&root, capture_id, Mode::from_raw_mode(0o700)).map_err(|_| failed())?;
        match Self::make_file(&root, capture_id, file_name) {
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

    fn make_file(root: &OwnedFd, capture_id: &str, file_name: &str) -> Result<Self, Fail> {
        let dir = open_dir(root, Path::new(capture_id)).map_err(|_| failed())?;
        let fd = rustix::fs::openat(&dir, file_name, FILE_FLAGS, Mode::from_raw_mode(0o600))
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

    /// The retained descriptor of the staging file.
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

    /// Removes `<captures_root>/<capture_id>/` with its content, durably: the
    /// files are unlinked, each emptied folder is synced and removed, and
    /// `captures_root` is synced at the end (design §5.3). A missing folder or
    /// `captures_root` is already removed.
    ///
    /// # Errors
    /// `capture_failed` when `captures_root` or the entry is a link or not a
    /// directory, or the removal or a sync fails. After an error the removal
    /// is not finished.
    pub(crate) fn remove(
        captures_root: &Path,
        capture_id: &str,
        fs: &dyn DurableFs,
    ) -> Result<(), Fail> {
        let root = match open_root(captures_root, false) {
            Ok(Some(root)) => root,
            Ok(None) => return Ok(()),
            Err(_) => return Err(failed()),
        };
        remove_tree_durable(&root, capture_id, fs).map_err(|e| {
            tracing::warn!(kind = ?e.kind(), "cannot remove a capture staging folder");
            failed()
        })
    }
}
