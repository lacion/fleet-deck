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
    AtFlags, Dir, FileType, Mode, OFlags, RawMode, RenameFlags, Stat, fchmod, fstat, fsync,
    mkdirat, open, openat, renameat, renameat_with, statat, unlinkat,
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
    /// The device of the file.
    pub dev: u64,
    /// The inode of the file.
    pub ino: u64,
    /// The size in bytes.
    pub size: u64,
    /// The permission bits with the special bits (`st_mode & 0o7777`).
    pub full_mode: u32,
}

/// The kind of a file, without following a link.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Regular,
    Directory,
    Link,
    Other,
}

/// What `lstat` found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Meta {
    pub kind: Kind,
    pub uid: u32,
    /// True when the file has exactly one name.
    pub single_link: bool,
    pub dev: u64,
    pub ino: u64,
    /// `st_mode & 0o7777`.
    pub mode: u32,
    pub size: u64,
}

impl Meta {
    /// True when the file is owned by the effective user.
    #[must_use]
    pub fn is_mine(&self) -> bool {
        self.uid == geteuid().as_raw()
    }
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

/// `mode_t` is `u16` on macOS; every mode here is at most `0o7777`.
#[allow(clippy::unnecessary_cast, clippy::cast_possible_truncation)]
fn mode_of(bits: u32) -> Mode {
    Mode::from_raw_mode(bits as RawMode)
}

/// The `st_mode` of `stat` (`mode_t` is `u16` on macOS).
#[allow(clippy::useless_conversion)]
fn mode_bits(stat: &Stat) -> u32 {
    u32::from(stat.st_mode)
}

/// The device and the inode of `stat` (`dev_t` is `i32` on macOS).
#[allow(clippy::unnecessary_cast, clippy::cast_sign_loss)]
fn id_of(stat: &Stat) -> (u64, u64) {
    (stat.st_dev as u64, stat.st_ino as u64)
}

fn random_hex() -> io::Result<String> {
    random_bytes_hex(8)
}

/// `count` random bytes as lower-case hex (`2 * count` characters).
///
/// # Errors
/// The system has no random bytes.
pub fn random_bytes_hex(count: usize) -> io::Result<String> {
    let mut raw = vec![0_u8; count];
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
        self.dir_modes(rel, create, &mut Vec::new())
    }

    /// [`Self::dir`], and `modes` gets the permission bits (`st_mode &
    /// 0o7777`, from `fstat` of the opened descriptor) of each directory of
    /// `rel`, from the first component down.
    fn dir_modes(
        &self,
        rel: &Path,
        create: bool,
        modes: &mut Vec<u32>,
    ) -> Result<Option<OwnedFd>, FsError> {
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
            let stat = fstat(&next)?;
            if stat.st_uid != geteuid().as_raw() {
                return Err(self.unsafe_at(&so_far));
            }
            modes.push(mode_bits(&stat) & 0o7777);
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
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        ) {
            Ok(fd) => fd,
            Err(Errno::NOENT) => return Ok(None),
            Err(e) => return Err(self.map_open(e, rel)),
        };
        let stat = fstat(&fd)?;
        if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile
            || stat.st_uid != geteuid().as_raw()
            || stat.st_nlink != 1
        {
            return Err(self.unsafe_at(rel));
        }
        let mut bytes = Vec::new();
        File::from(fd).take(MAX_READ + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_READ {
            return Err(FsError::Io(io::Error::other("the file is too large")));
        }
        let (dev, ino) = id_of(&stat);
        Ok(Some(Data {
            bytes,
            mode: mode_bits(&stat) & 0o777,
            dev,
            ino,
            size: stat.st_size.unsigned_abs(),
            full_mode: mode_bits(&stat) & 0o7777,
        }))
    }

    /// What is at `rel`, without following a link. `Ok(None)` when it (or a
    /// directory above it) does not exist.
    ///
    /// # Errors
    /// [`FsError::Unsafe`] for a link or a foreign owner above it; `Io`.
    pub fn lstat(&self, rel: &Path) -> Result<Option<Meta>, FsError> {
        let (parent, leaf) = Self::split(rel)?;
        let Some(dir) = self.dir(parent, false)? else {
            return Ok(None);
        };
        let stat = match statat(&dir, leaf, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(stat) => stat,
            Err(Errno::NOENT) => return Ok(None),
            Err(e) => return Err(self.map_open(e, rel)),
        };
        let kind = match FileType::from_raw_mode(stat.st_mode) {
            FileType::RegularFile => Kind::Regular,
            FileType::Directory => Kind::Directory,
            FileType::Symlink => Kind::Link,
            _ => Kind::Other,
        };
        let (dev, ino) = id_of(&stat);
        Ok(Some(Meta {
            kind,
            uid: stat.st_uid,
            single_link: stat.st_nlink == 1,
            dev,
            ino,
            mode: mode_bits(&stat) & 0o7777,
            size: stat.st_size.unsigned_abs(),
        }))
    }

    /// True when the file `rel` or a directory of its path has a group or
    /// other write bit (`mode & 0o022`). The directories checked are the
    /// ones from the first component of `rel` (the base folder) down to the
    /// parent; the config home is not. A file or a directory that does not
    /// exist adds nothing.
    ///
    /// # Errors
    /// [`FsError::Unsafe`] for a link or a foreign owner on the path; `Io`.
    pub fn others_can_write(&self, rel: &Path) -> Result<bool, FsError> {
        let (parent, leaf) = Self::split(rel)?;
        let mut modes = Vec::new();
        let Some(dir) = self.dir_modes(parent, false, &mut modes)? else {
            return Ok(modes.iter().any(|m| m & 0o022 != 0));
        };
        let file = match statat(&dir, leaf, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(stat) => Some(mode_bits(&stat) & 0o7777),
            Err(Errno::NOENT) => None,
            Err(e) => return Err(self.map_open(e, rel)),
        };
        Ok(modes.iter().chain(file.iter()).any(|m| m & 0o022 != 0))
    }

    /// The names in the directory `rel`, sorted. `Ok(None)` when the
    /// directory does not exist. A name that is not UTF-8 is left out.
    ///
    /// # Errors
    /// [`FsError::Unsafe`] for a link or a foreign owner on the path; `Io`.
    pub fn list(&self, rel: &Path) -> Result<Option<Vec<String>>, FsError> {
        let Some(fd) = self.dir(rel, false)? else {
            return Ok(None);
        };
        let mut names = Vec::new();
        for entry in Dir::read_from(&fd)? {
            let entry = entry?;
            let name = entry.file_name().to_bytes();
            if name == b"." || name == b".." {
                continue;
            }
            if let Ok(text) = std::str::from_utf8(name) {
                names.push(text.to_owned());
            }
        }
        names.sort();
        Ok(Some(names))
    }

    /// Makes the directory `rel` with the mode 0700 (the missing ones above
    /// it get 0755). An existing directory of yours is set to 0700.
    ///
    /// # Errors
    /// [`FsError::Unsafe`] for a link, a non-directory or a foreign owner on
    /// the path; `Io`.
    pub fn private_dir(&self, rel: &Path) -> Result<(), FsError> {
        let (parent, leaf) = Self::split(rel)?;
        let dir = self
            .dir(parent, true)?
            .ok_or_else(|| self.unsafe_at(parent))?;
        match mkdirat(&dir, leaf, mode_of(0o700)) {
            Ok(()) | Err(Errno::EXIST) => {}
            Err(e) => return Err(e.into()),
        }
        let fd = openat(&dir, leaf, DIR_FLAGS, Mode::empty()).map_err(|e| self.map_open(e, rel))?;
        if fstat(&fd)?.st_uid != geteuid().as_raw() {
            return Err(self.unsafe_at(rel));
        }
        fchmod(&fd, mode_of(0o700))?;
        fsync(&dir)?;
        Ok(())
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

    /// Creates `rel` with `bytes` and the permission bits `mode`, only when
    /// it does not exist (`O_CREAT | O_EXCL | O_NOFOLLOW`), and makes the
    /// missing directories. `Ok(false)` when the name is taken (also by a
    /// link): nothing is changed then.
    ///
    /// # Errors
    /// [`FsError::Unsafe`] for a link or a foreign owner above the file;
    /// `Io`. A failed write removes the file that this call created.
    pub fn create_new(&self, rel: &Path, bytes: &[u8], mode: u32) -> Result<bool, FsError> {
        Ok(self.create_new_held(rel, bytes, mode)?.is_some())
    }

    /// [`Self::create_new`], and the open descriptor of the new file (write
    /// only) instead of `true`: `None` when the name is taken.
    ///
    /// # Errors
    /// The same as [`Self::create_new`].
    pub fn create_new_held(
        &self,
        rel: &Path,
        bytes: &[u8],
        mode: u32,
    ) -> Result<Option<OwnedFd>, FsError> {
        let (parent, leaf) = Self::split(rel)?;
        let dir = self
            .dir(parent, true)?
            .ok_or_else(|| self.unsafe_at(parent))?;
        let fd = match openat(
            &dir,
            leaf,
            OFlags::CREATE | OFlags::EXCL | OFlags::WRONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            mode_of(mode),
        ) {
            Ok(fd) => fd,
            Err(Errno::EXIST) => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let written: Result<File, FsError> = (|| {
            fchmod(&fd, mode_of(mode))?;
            let mut file = File::from(fd);
            file.write_all(bytes)?;
            file.sync_all()?;
            Ok(file)
        })();
        let file = match written {
            Ok(file) => file,
            Err(e) => {
                let _ = unlinkat(&dir, leaf, AtFlags::empty());
                return Err(e);
            }
        };
        fsync(&dir)?;
        Ok(Some(OwnedFd::from(file)))
    }

    /// Opens the file `rel` by name (`O_NOFOLLOW`) and returns what it holds
    /// only when it is the file of the open descriptor `held` (the same
    /// device and inode), a regular file of yours with one name, and with
    /// no group or other write bit. `Ok(None)` for any other file, or none.
    ///
    /// # Errors
    /// [`FsError::Unsafe`] for a link or a foreign owner on the path; `Io`.
    pub fn read_same(&self, rel: &Path, held: &OwnedFd) -> Result<Option<Data>, FsError> {
        let stat = fstat(held)?;
        let data = match self.read(rel) {
            Ok(Some(data)) => data,
            Ok(None) | Err(FsError::Unsafe(_)) => return Ok(None),
            Err(e) => return Err(e),
        };
        let same = (data.dev, data.ino) == id_of(&stat);
        Ok((same && data.full_mode & 0o022 == 0).then_some(data))
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

    /// Opens the lock file `rel` without following a link, and returns its
    /// descriptor. With `create`, the missing directories (0755) and the
    /// file (0600) are made. `Ok(None)` when the directory or the file is
    /// missing and `create` is false.
    ///
    /// # Errors
    /// [`FsError::Unsafe`] for a link, a file that is not regular or not
    /// yours, and a link above it; `Io`.
    pub fn open_lock(&self, rel: &Path, create: bool) -> Result<Option<OwnedFd>, FsError> {
        let (parent, leaf) = Self::split(rel)?;
        let Some(dir) = self.dir(parent, create)? else {
            return Ok(None);
        };
        let mut flags = OFlags::RDWR | OFlags::NOFOLLOW | OFlags::CLOEXEC;
        if create {
            flags |= OFlags::CREATE;
        }
        let fd = match openat(&dir, leaf, flags, mode_of(0o600)) {
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
        Ok(Some(fd))
    }

    /// Renames the file `from` to `to` in the same directory, and never
    /// replaces a file (`RENAME_NOREPLACE`). `Ok(false)` when `to` exists:
    /// nothing is changed then.
    ///
    /// # Errors
    /// [`FsError::Unsafe`] for a link above the files; `Io` (also a missing
    /// `from`, and a file system without `RENAME_NOREPLACE`).
    pub fn rename_new(&self, from: &Path, to: &Path) -> Result<bool, FsError> {
        let (parent, from_leaf) = Self::split(from)?;
        let (to_parent, to_leaf) = Self::split(to)?;
        if parent != to_parent {
            return Err(FsError::Io(io::Error::new(
                io::ErrorKind::InvalidInput,
                "both names must be in the same directory",
            )));
        }
        let dir = self
            .dir(parent, false)?
            .ok_or_else(|| self.unsafe_at(parent))?;
        match renameat_with(&dir, from_leaf, &dir, to_leaf, RenameFlags::NOREPLACE) {
            Ok(()) => {}
            Err(Errno::EXIST) => return Ok(false),
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
