//! The writer of the installer (R13 P4 task C, `omarchy-p4.md` §3.4).
//!
//! Every path is relative to the config home, which is opened once (it may
//! be a link: the owner chose it). Every component below it is opened with
//! `O_NOFOLLOW` through the directory descriptor of its parent, and must be
//! owned by the effective user. A link, or an owner that is not you, is
//! [`FsError::Unsafe`]. Directories are created with mode 0755. A write is
//! atomic: an exclusive temporary file in the same directory (`O_CREAT |
//! O_EXCL | O_NOFOLLOW`, the target mode), `fsync`, `rename`, `fsync` of the
//! directory. The private writers of `fleetdeck_core::private_file` demand
//! 0700 parents and 0600 files, so the installer has its own.

use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::OwnedFd;
use std::path::{Component, Path, PathBuf};

use ring::rand::{SecureRandom, SystemRandom};
use rustix::fs::{
    AtFlags, FileType, Mode, OFlags, fchmod, fstat, fsync, mkdirat, open, openat, renameat, statat,
    unlinkat,
};
use rustix::io::Errno;
use rustix::process::geteuid;

/// The most bytes that the installer reads from one file.
const MAX_READ: u64 = 8 * 1024 * 1024;

/// Why a path was refused or failed.
#[derive(Debug)]
pub enum FsError {
    /// A link, a non-directory in the way, or an owner that is not you.
    Unsafe(PathBuf),
    Io(io::Error),
}

impl std::fmt::Display for FsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unsafe(path) => write!(
                f,
                "{} is a link or is not yours; change nothing.",
                path.display()
            ),
            Self::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for FsError {}

impl From<io::Error> for FsError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<Errno> for FsError {
    fn from(e: Errno) -> Self {
        Self::Io(e.into())
    }
}

/// What a read found.
#[derive(Debug)]
pub struct Data {
    pub bytes: Vec<u8>,
    /// The permission bits.
    pub mode: u32,
}

/// The config home, opened.
pub struct Home {
    fd: OwnedFd,
    path: PathBuf,
}

const DIR_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);

fn normal_components(rel: &Path) -> Result<Vec<&std::ffi::OsStr>, FsError> {
    let mut parts = Vec::new();
    for c in rel.components() {
        match c {
            Component::Normal(name) => parts.push(name),
            _ => {
                return Err(FsError::Io(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "a relative path with normal components only is required",
                )));
            }
        }
    }
    Ok(parts)
}

fn mode_of(bits: u32) -> Mode {
    Mode::from_raw_mode(bits)
}

fn random_hex() -> io::Result<String> {
    let mut raw = [0_u8; 8];
    SystemRandom::new()
        .fill(&mut raw)
        .map_err(|_| io::Error::other("no random bytes"))?;
    Ok(raw.iter().map(|b| format!("{b:02x}")).collect())
}

impl Home {
    /// Opens the config home (a link is allowed here).
    ///
    /// # Errors
    /// The directory cannot be opened.
    pub fn open(path: &Path) -> Result<Self, FsError> {
        let fd = open(
            path,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
        )?;
        Ok(Self {
            fd,
            path: path.to_owned(),
        })
    }

    /// The absolute path of `rel`.
    #[must_use]
    pub fn abs(&self, rel: &Path) -> PathBuf {
        self.path.join(rel)
    }

    fn unsafe_at(&self, rel: &Path) -> FsError {
        FsError::Unsafe(self.abs(rel))
    }

    /// Opens the directory `rel` below the home. `create` makes the missing
    /// directories (0755). `Ok(None)` when a directory is missing and
    /// `create` is false.
    fn dir(&self, rel: &Path, create: bool) -> Result<Option<OwnedFd>, FsError> {
        let mut current = openat(&self.fd, ".", DIR_FLAGS, Mode::empty())?;
        let mut so_far = PathBuf::new();
        for name in normal_components(rel)? {
            so_far.push(name);
            let next = match openat(&current, name, DIR_FLAGS, Mode::empty()) {
                Ok(fd) => fd,
                Err(Errno::NOENT) if create => {
                    match mkdirat(&current, name, mode_of(0o755)) {
                        Ok(()) | Err(Errno::EXIST) => {}
                        Err(e) => return Err(e.into()),
                    }
                    let fd = openat(&current, name, DIR_FLAGS, Mode::empty())
                        .map_err(|e| self.map_open(e, &so_far))?;
                    if fstat(&fd)?.st_uid == geteuid().as_raw() {
                        fchmod(&fd, mode_of(0o755))?;
                    }
                    fd
                }
                Err(Errno::NOENT) => return Ok(None),
                Err(e) => return Err(self.map_open(e, &so_far)),
            };
            if fstat(&next)?.st_uid != geteuid().as_raw() {
                return Err(self.unsafe_at(&so_far));
            }
            current = next;
        }
        Ok(Some(current))
    }

    fn map_open(&self, e: Errno, so_far: &Path) -> FsError {
        if matches!(e, Errno::LOOP | Errno::NOTDIR) {
            self.unsafe_at(so_far)
        } else {
            e.into()
        }
    }

    fn split(rel: &Path) -> Result<(&Path, &std::ffi::OsStr), FsError> {
        match (rel.parent(), rel.file_name()) {
            (Some(parent), Some(leaf)) => Ok((parent, leaf)),
            _ => Err(FsError::Io(io::Error::new(
                io::ErrorKind::InvalidInput,
                "the path has no file name",
            ))),
        }
    }

    /// Reads the regular file `rel`. `Ok(None)` when it (or a directory
    /// above it) does not exist.
    ///
    /// # Errors
    /// [`FsError::Unsafe`] for a link, a file that is not regular or not
    /// yours, and a link above it; `Io` for the rest (also a file over 8 MiB).
    pub fn read(&self, rel: &Path) -> Result<Option<Data>, FsError> {
        let (parent, leaf) = Self::split(rel)?;
        let Some(dir) = self.dir(parent, false)? else {
            return Ok(None);
        };
        let fd = match openat(
            &dir,
            leaf,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        ) {
            Ok(fd) => fd,
            Err(Errno::NOENT) => return Ok(None),
            Err(e) => return Err(self.map_open(e, rel)),
        };
        let stat = fstat(&fd)?;
        if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile
            || stat.st_uid != geteuid().as_raw()
        {
            return Err(self.unsafe_at(rel));
        }
        let mut bytes = Vec::new();
        File::from(fd).take(MAX_READ + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_READ {
            return Err(FsError::Io(io::Error::other("the file is too large")));
        }
        Ok(Some(Data {
            bytes,
            mode: stat.st_mode & 0o777,
        }))
    }

    /// Writes `bytes` to `rel` atomically with the permission bits `mode`,
    /// and makes the missing directories.
    ///
    /// # Errors
    /// [`FsError::Unsafe`] for a link or a foreign owner on the path (also an
    /// existing target that is not a regular file of yours); `Io`.
    pub fn write(&self, rel: &Path, bytes: &[u8], mode: u32) -> Result<(), FsError> {
        let (parent, leaf) = Self::split(rel)?;
        let dir = self
            .dir(parent, true)?
            .ok_or_else(|| self.unsafe_at(parent))?;
        match statat(&dir, leaf, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(stat) => {
                if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile
                    || stat.st_uid != geteuid().as_raw()
                {
                    return Err(self.unsafe_at(rel));
                }
            }
            Err(Errno::NOENT) => {}
            Err(e) => return Err(e.into()),
        }
        let temp = format!(
            ".{}.fleetdeck-{}.tmp",
            leaf.to_string_lossy(),
            random_hex()?
        );
        let fd = openat(
            &dir,
            temp.as_str(),
            OFlags::CREATE | OFlags::EXCL | OFlags::WRONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            mode_of(mode),
        )?;
        let published: Result<(), FsError> = (|| {
            fchmod(&fd, mode_of(mode))?;
            let mut file = File::from(fd);
            file.write_all(bytes)?;
            file.sync_all()?;
            renameat(&dir, temp.as_str(), &dir, leaf)?;
            Ok(())
        })();
        if published.is_err() {
            // After a successful rename the temporary name no longer exists.
            let _ = unlinkat(&dir, temp.as_str(), AtFlags::empty());
        }
        published?;
        fsync(&dir)?;
        Ok(())
    }

    /// Removes the file `rel`. `Ok(false)` when it does not exist. A link is
    /// removed as a link (never followed).
    ///
    /// # Errors
    /// [`FsError::Unsafe`] for a link above the file; `Io`.
    pub fn remove(&self, rel: &Path) -> Result<bool, FsError> {
        let (parent, leaf) = Self::split(rel)?;
        let Some(dir) = self.dir(parent, false)? else {
            return Ok(false);
        };
        match unlinkat(&dir, leaf, AtFlags::empty()) {
            Ok(()) => {}
            Err(Errno::NOENT) => return Ok(false),
            Err(e) => return Err(e.into()),
        }
        fsync(&dir)?;
        Ok(true)
    }

    /// Removes the directory `rel` when it is empty. `false` for a directory
    /// that is missing, not empty, or not removable.
    pub fn remove_empty_dir(&self, rel: &Path) -> bool {
        let Ok((parent, leaf)) = Self::split(rel) else {
            return false;
        };
        let Ok(Some(dir)) = self.dir(parent, false) else {
            return false;
        };
        unlinkat(&dir, leaf, AtFlags::REMOVEDIR).is_ok()
    }
}
