//! A safe no-replace writer and a private reader of small files (P8,
//! design `omarchy-p8.md` 2.3).
//!
//! [`write_new_private`] creates a file that did not exist, with mode 0600,
//! in a private directory. It writes a temporary leaf, publishes it with an
//! atomic no-replace `linkat`, and never replaces an existing destination (a
//! dangling symlink counts as existing). [`read_private`] reads such a file
//! and refuses a link, a file that other users can read, and a file over a
//! limit.

use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path};

use ring::rand::{SecureRandom, SystemRandom};
use rustix::fs::{
    AtFlags, FileType, FlockOperation, Mode, OFlags, flock, fstat, fsync, linkat, open, openat,
    renameat, unlinkat,
};
use rustix::io::Errno;
use rustix::process::geteuid;

/// Why a new private file was refused or failed.
#[derive(Debug, thiserror::Error)]
pub enum PrivateWriteError {
    /// The destination exists (any type; a dangling symlink too).
    #[error("the output path already exists")]
    Exists,
    /// The path or one of its directories is not private or is a link.
    #[error("the output path is not private or contains a link")]
    Unsafe,
    #[error("the output file could not be written: {0}")]
    Io(#[from] io::Error),
}

/// Why a private file was not read.
#[derive(Debug, thiserror::Error)]
pub enum PrivateReadError {
    /// A link, a file that is not private, or a parent that is not safe.
    #[error("the file is not private or is a link")]
    Unsafe,
    #[error("the file is larger than the limit")]
    TooLarge,
    #[error("the file could not be read: {0}")]
    Io(#[from] io::Error),
}

/// Checks that [`write_new_private`] may create `path`.
///
/// 1. `path` is absolute and its last component is a normal name.
/// 2. Every directory from `/` to the parent is a directory and not a
///    symlink. Every one except the parent is owned by root or by the
///    effective user and not writable by group or others (a sticky
///    world-writable directory such as `/tmp` is refused).
/// 3. The parent is owned by the effective user and has mode 0700.
/// 4. Nothing exists at `path` (not even a dangling symlink).
///
/// # Errors
/// [`PrivateWriteError::Unsafe`], [`PrivateWriteError::Exists`] or
/// [`PrivateWriteError::Io`] (a directory that cannot be read).
pub fn check_new_private(path: &Path) -> Result<(), PrivateWriteError> {
    check_new_below(Path::new("/"), path)
}

/// [`check_new_private`] that checks only the directories from `root` down
/// (a test gives its own private root).
fn check_new_below(root: &Path, path: &Path) -> Result<(), PrivateWriteError> {
    check_new_rule(root, path, false)
}

/// [`check_new_below`] with the parent rule: `shared` allows a parent that
/// others can read (mode 0755), else the parent must be 0700.
fn check_new_rule(root: &Path, path: &Path, shared: bool) -> Result<(), PrivateWriteError> {
    check_parent_rule(root, path, shared)?;
    match fs::symlink_metadata(path) {
        Ok(_) => Err(PrivateWriteError::Exists),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(PrivateWriteError::Io(e)),
    }
}

/// Rules 1 to 3 of [`check_new_private`]: the path and its directories, from
/// `root` down. It does not look at `path` itself.
fn check_parent_below(root: &Path, path: &Path) -> Result<(), PrivateWriteError> {
    check_parent_rule(root, path, false)
}

/// [`check_parent_below`] with the parent rule of [`check_new_rule`].
fn check_parent_rule(root: &Path, path: &Path, shared: bool) -> Result<(), PrivateWriteError> {
    let parent = parent_of(path).ok_or(PrivateWriteError::Unsafe)?;
    if !parent.starts_with(root) {
        return Err(PrivateWriteError::Unsafe);
    }
    let euid = geteuid().as_raw();
    for dir in parent.ancestors().take_while(|d| d.starts_with(root)) {
        let meta = fs::symlink_metadata(dir)?;
        if !meta.is_dir() {
            return Err(PrivateWriteError::Unsafe);
        }
        let ok = if dir == parent && shared {
            meta.uid() == euid && meta.mode() & 0o022 == 0
        } else if dir == parent {
            meta.uid() == euid && meta.mode() & 0o777 == 0o700
        } else {
            (meta.uid() == 0 || meta.uid() == euid) && meta.mode() & 0o022 == 0
        };
        if !ok {
            return Err(PrivateWriteError::Unsafe);
        }
    }
    Ok(())
}

/// The parent of an absolute path whose components after the root are all
/// normal names (no `.` or `..`: the ancestor walk is lexical).
fn parent_of(path: &Path) -> Option<&Path> {
    let mut parts = path.components();
    if parts.next() != Some(Component::RootDir)
        || parts.clone().next().is_none()
        || !parts.all(|c| matches!(c, Component::Normal(_)))
    {
        return None;
    }
    path.parent()
}

/// Creates the file `path` with mode 0600 and the content `bytes`; it never
/// replaces an existing path.
///
/// After [`check_new_private`], it opens the parent directory without
/// following links, creates the temporary leaf
/// `<temp_prefix><32 lowercase hex>.tmp` exclusively, writes and syncs it,
/// publishes it with `linkat` (no replace), removes the temporary leaf and
/// syncs the parent. After the temporary leaf exists, every error path
/// removes it (best effort) and never touches `path`.
///
/// # Errors
/// [`PrivateWriteError`]: the refusals of [`check_new_private`], `Exists`
/// when the destination appears before the publish, and `Io`.
pub fn write_new_private(
    path: &Path,
    bytes: &[u8],
    temp_prefix: &str,
) -> Result<(), PrivateWriteError> {
    write_new_below(Path::new("/"), path, bytes, temp_prefix)
}

fn write_new_below(
    root: &Path,
    path: &Path,
    bytes: &[u8],
    temp_prefix: &str,
) -> Result<(), PrivateWriteError> {
    write_new_rule(root, path, bytes, temp_prefix, false)
}

/// Creates the file `path` with mode 0600 and the content `bytes`, like
/// [`write_new_private`], in a parent that other users can read: the parent
/// is owned by the effective user and has no group or other write bit (mode
/// 0755 passes). The ancestors, the temporary leaf, the no-replace publish
/// and the syncs are the same.
///
/// # Errors
/// As [`write_new_private`], with the parent rule above.
pub fn write_new_shared_parent(
    path: &Path,
    bytes: &[u8],
    temp_prefix: &str,
) -> Result<(), PrivateWriteError> {
    write_new_shared_below(Path::new("/"), path, bytes, temp_prefix)
}

/// [`write_new_shared_parent`] that checks only the directories from `root`
/// down (a test gives its own root).
///
/// # Errors
/// As [`write_new_shared_parent`].
pub fn write_new_shared_below(
    root: &Path,
    path: &Path,
    bytes: &[u8],
    temp_prefix: &str,
) -> Result<(), PrivateWriteError> {
    write_new_rule(root, path, bytes, temp_prefix, true)
}

fn write_new_rule(
    root: &Path,
    path: &Path,
    bytes: &[u8],
    temp_prefix: &str,
    shared: bool,
) -> Result<(), PrivateWriteError> {
    check_new_rule(root, path, shared)?;
    let (Some(parent), Some(leaf)) = (path.parent(), path.file_name()) else {
        return Err(PrivateWriteError::Unsafe);
    };
    let dir = open(
        parent,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(io::Error::from)?;
    let temp = format!("{temp_prefix}{}.tmp", random_hex()?);
    let fd = openat(
        &dir,
        temp.as_str(),
        OFlags::CREATE | OFlags::EXCL | OFlags::WRONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::RUSR | Mode::WUSR,
    )
    .map_err(io::Error::from)?;
    let published: Result<(), PrivateWriteError> = (|| {
        let mut file = File::from(fd);
        file.write_all(bytes)?;
        file.sync_all()?;
        match linkat(&dir, temp.as_str(), &dir, leaf, AtFlags::empty()) {
            Ok(()) => Ok(()),
            Err(Errno::EXIST) => Err(PrivateWriteError::Exists),
            Err(e) => Err(PrivateWriteError::Io(e.into())),
        }
    })();
    let removed = unlinkat(&dir, temp.as_str(), AtFlags::empty());
    published?;
    removed.map_err(io::Error::from)?;
    fsync(&dir).map_err(io::Error::from)?;
    Ok(())
}

/// Replaces the private file `path` with `bytes` (mode 0600), or creates it.
///
/// The checks of [`check_new_private`] on the directories apply (the parent
/// is owned by the effective user and has mode 0700). An existing
/// destination must be a regular file, not a link, owned by the effective
/// user, with no group or other bits; else `Unsafe`. The new content goes
/// into the exclusive 0600 temporary leaf `<temp_prefix><32 hex>.tmp`, is
/// synced, replaces the destination with `renameat`, and the parent is
/// synced. Every error path removes the temporary leaf (best effort).
///
/// # Errors
/// [`PrivateWriteError`]: `Unsafe` for a refused path or destination, `Io`.
pub fn replace_private(
    path: &Path,
    bytes: &[u8],
    temp_prefix: &str,
) -> Result<(), PrivateWriteError> {
    replace_private_below(Path::new("/"), path, bytes, temp_prefix)
}

/// [`replace_private`] that checks only the directories from `root` down
/// (a caller that owns its private root, such as a test, gives it).
///
/// # Errors
/// As [`replace_private`].
pub fn replace_private_below(
    root: &Path,
    path: &Path,
    bytes: &[u8],
    temp_prefix: &str,
) -> Result<(), PrivateWriteError> {
    check_parent_below(root, path)?;
    let (Some(parent), Some(leaf)) = (path.parent(), path.file_name()) else {
        return Err(PrivateWriteError::Unsafe);
    };
    check_destination(path)?;
    let dir = open(
        parent,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(io::Error::from)?;
    let temp = format!("{temp_prefix}{}.tmp", random_hex()?);
    let fd = openat(
        &dir,
        temp.as_str(),
        OFlags::CREATE | OFlags::EXCL | OFlags::WRONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::RUSR | Mode::WUSR,
    )
    .map_err(io::Error::from)?;
    let published: Result<(), PrivateWriteError> = (|| {
        let mut file = File::from(fd);
        file.write_all(bytes)?;
        file.sync_all()?;
        renameat(&dir, temp.as_str(), &dir, leaf).map_err(io::Error::from)?;
        Ok(())
    })();
    if published.is_err() {
        // After a successful rename the temporary name no longer exists.
        let _ = unlinkat(&dir, temp.as_str(), AtFlags::empty());
    }
    published?;
    fsync(&dir).map_err(io::Error::from)?;
    Ok(())
}

/// An existing destination of [`replace_private`] is a regular file of the
/// effective user with no group or other bits, and not a link.
fn check_destination(path: &Path) -> Result<(), PrivateWriteError> {
    match fs::symlink_metadata(path) {
        Ok(meta) => {
            if !meta.file_type().is_file()
                || meta.uid() != geteuid().as_raw()
                || meta.mode() & 0o077 != 0
            {
                return Err(PrivateWriteError::Unsafe);
            }
            Ok(())
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(PrivateWriteError::Io(e)),
    }
}

/// Takes the exclusive non-blocking lock of the private file `path`.
///
/// The file is created with mode 0600 when it is absent. The directory rules
/// of [`replace_private`] apply. An existing file must be a regular file of
/// the effective user with no group or other bits (it is opened with
/// `O_NOFOLLOW`). `Ok(None)` means that another holder has the lock. The
/// returned file holds the lock until it is dropped.
///
/// # Errors
/// [`PrivateWriteError`]: `Unsafe` for a refused path or file, `Io`.
pub fn try_lock_private(path: &Path) -> Result<Option<File>, PrivateWriteError> {
    try_lock_private_below(Path::new("/"), path)
}

/// [`try_lock_private`] that checks only the directories from `root` down.
///
/// # Errors
/// As [`try_lock_private`].
pub fn try_lock_private_below(root: &Path, path: &Path) -> Result<Option<File>, PrivateWriteError> {
    check_parent_below(root, path)?;
    let (Some(parent), Some(leaf)) = (path.parent(), path.file_name()) else {
        return Err(PrivateWriteError::Unsafe);
    };
    let dir = open(
        parent,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(io::Error::from)?;
    let fd = match openat(
        &dir,
        leaf,
        OFlags::CREATE | OFlags::RDWR | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::RUSR | Mode::WUSR,
    ) {
        Ok(fd) => fd,
        Err(Errno::LOOP) => return Err(PrivateWriteError::Unsafe),
        Err(e) => return Err(PrivateWriteError::Io(e.into())),
    };
    let st = fstat(&fd).map_err(io::Error::from)?;
    if FileType::from_raw_mode(st.st_mode) != FileType::RegularFile
        || st.st_uid != geteuid().as_raw()
        || st.st_mode & 0o077 != 0
    {
        return Err(PrivateWriteError::Unsafe);
    }
    match flock(&fd, FlockOperation::NonBlockingLockExclusive) {
        Ok(()) => Ok(Some(File::from(fd))),
        Err(Errno::WOULDBLOCK) => Ok(None),
        Err(e) => Err(PrivateWriteError::Io(e.into())),
    }
}

/// 32 lowercase hex digits of 16 random bytes.
fn random_hex() -> io::Result<String> {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut bytes = [0u8; 16];
    SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| io::Error::other("the random source failed"))?;
    let mut text = String::with_capacity(32);
    for b in bytes {
        text.push(char::from(DIGITS[usize::from(b >> 4)]));
        text.push(char::from(DIGITS[usize::from(b & 0x0f)]));
    }
    Ok(text)
}

/// Reads the private file `path`, at most `max` bytes.
///
/// `path` is absolute. The parent is a directory (not a symlink) owned by
/// the effective user, not writable by group or others (mode 0755 passes).
/// The file is opened without following a symlink; it is a regular file
/// owned by the effective user with no group or other permission bits and a
/// size of at most `max`.
///
/// # Errors
/// [`PrivateReadError`]: `Unsafe` for a path, parent, link, type, owner or
/// mode that fails the rules, `TooLarge` over `max` bytes, `Io` otherwise.
pub fn read_private(path: &Path, max: u64) -> Result<Vec<u8>, PrivateReadError> {
    let parent = parent_of(path).ok_or(PrivateReadError::Unsafe)?;
    let euid = geteuid().as_raw();
    let meta = fs::symlink_metadata(parent)?;
    if !meta.is_dir() || meta.uid() != euid || meta.mode() & 0o022 != 0 {
        return Err(PrivateReadError::Unsafe);
    }
    let fd = match open(
        path,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    ) {
        Ok(fd) => fd,
        Err(Errno::LOOP) => return Err(PrivateReadError::Unsafe),
        Err(e) => return Err(PrivateReadError::Io(e.into())),
    };
    let st = fstat(&fd).map_err(io::Error::from)?;
    if FileType::from_raw_mode(st.st_mode) != FileType::RegularFile
        || st.st_uid != euid
        || st.st_mode & 0o077 != 0
    {
        return Err(PrivateReadError::Unsafe);
    }
    let size = u64::try_from(st.st_size).map_err(|_| PrivateReadError::Unsafe)?;
    if size > max {
        return Err(PrivateReadError::TooLarge);
    }
    let mut bytes = Vec::new();
    File::from(fd)
        .take(max.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if !u64::try_from(bytes.len()).is_ok_and(|n| n <= max) {
        return Err(PrivateReadError::TooLarge);
    }
    Ok(bytes)
}

#[cfg(test)]
#[path = "private_file_shared_tests.rs"]
mod shared_tests;

#[cfg(test)]
mod tests {
    use std::os::unix::fs::{PermissionsExt, symlink};
    use std::path::PathBuf;

    use super::*;

    const PREFIX: &str = ".provision.";

    fn chmod(path: &Path, mode: u32) {
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
    }

    /// A private (0700) directory that is the root of the ancestor walk.
    fn private_root() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        chmod(dir.path(), 0o700);
        dir
    }

    fn write(root: &Path, path: &Path, bytes: &[u8]) -> Result<(), PrivateWriteError> {
        write_new_below(root, path, bytes, PREFIX)
    }

    fn temps(dir: &Path) -> Vec<PathBuf> {
        fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with(PREFIX) && n.ends_with(".tmp"))
            })
            .collect()
    }

    #[test]
    fn a_new_file_is_written_with_mode_0600_and_the_exact_bytes() {
        let root = private_root();
        let path = root.path().join("out.toml");
        write(root.path(), &path, b"hello\n").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"hello\n");
        assert_eq!(fs::metadata(&path).unwrap().mode() & 0o777, 0o600);
        assert!(temps(root.path()).is_empty());
    }

    #[test]
    fn an_existing_file_is_not_replaced() {
        let root = private_root();
        let path = root.path().join("out.toml");
        fs::write(&path, b"first").unwrap();
        let r = write(root.path(), &path, b"second");
        assert!(matches!(r, Err(PrivateWriteError::Exists)), "{r:?}");
        assert_eq!(fs::read(&path).unwrap(), b"first");
        assert!(temps(root.path()).is_empty());
    }

    #[test]
    fn a_dangling_symlink_counts_as_existing() {
        let root = private_root();
        let path = root.path().join("out.toml");
        let target = root.path().join("nowhere");
        symlink(&target, &path).unwrap();
        let r = write(root.path(), &path, b"data");
        assert!(matches!(r, Err(PrivateWriteError::Exists)), "{r:?}");
        assert_eq!(fs::read_link(&path).unwrap(), target);
        assert!(!target.exists());
        assert!(temps(root.path()).is_empty());
    }

    #[test]
    fn a_parent_with_mode_0755_is_unsafe() {
        let root = private_root();
        chmod(root.path(), 0o755);
        let path = root.path().join("out.toml");
        let r = write(root.path(), &path, b"data");
        assert!(matches!(r, Err(PrivateWriteError::Unsafe)), "{r:?}");
        assert!(!path.exists());
    }

    #[test]
    fn a_symlinked_parent_is_unsafe() {
        let root = private_root();
        let real = root.path().join("real");
        fs::create_dir(&real).unwrap();
        chmod(&real, 0o700);
        let link = root.path().join("link");
        symlink(&real, &link).unwrap();
        let r = write(root.path(), &link.join("out.toml"), b"data");
        assert!(matches!(r, Err(PrivateWriteError::Unsafe)), "{r:?}");
        assert!(!real.join("out.toml").exists());
    }

    #[test]
    fn an_ancestor_that_group_can_write_is_unsafe() {
        let root = private_root();
        let middle = root.path().join("middle");
        let inner = middle.join("inner");
        fs::create_dir_all(&inner).unwrap();
        chmod(&inner, 0o700);
        chmod(&middle, 0o770);
        let r = check_new_below(root.path(), &inner.join("out.toml"));
        assert!(matches!(r, Err(PrivateWriteError::Unsafe)), "{r:?}");
        chmod(&middle, 0o700);
        check_new_below(root.path(), &inner.join("out.toml")).unwrap();
    }

    #[test]
    fn a_relative_path_and_a_path_without_a_name_are_unsafe() {
        for path in ["out.toml", "/", "/tmp/..", "/home/../etc/out.toml"] {
            let r = check_new_private(Path::new(path));
            assert!(matches!(r, Err(PrivateWriteError::Unsafe)), "{path}: {r:?}");
        }
    }

    #[test]
    fn a_path_outside_the_root_is_unsafe() {
        let root = private_root();
        let other = private_root();
        let r = check_new_below(root.path(), &other.path().join("out.toml"));
        assert!(matches!(r, Err(PrivateWriteError::Unsafe)), "{r:?}");
    }

    #[test]
    fn the_public_check_refuses_a_path_below_tmp() {
        let dir = tempfile::Builder::new().tempdir_in("/tmp").unwrap();
        chmod(dir.path(), 0o700);
        let r = check_new_private(&dir.path().join("out.toml"));
        assert!(matches!(r, Err(PrivateWriteError::Unsafe)), "{r:?}");
        let r = write_new_private(&dir.path().join("out.toml"), b"data", PREFIX);
        assert!(matches!(r, Err(PrivateWriteError::Unsafe)), "{r:?}");
        assert!(!dir.path().join("out.toml").exists());
    }

    #[test]
    fn a_private_file_is_read() {
        let root = private_root();
        let path = root.path().join("in.toml");
        fs::write(&path, b"abc").unwrap();
        chmod(&path, 0o600);
        assert_eq!(read_private(&path, 16).unwrap(), b"abc");
        // The limit itself is accepted.
        assert_eq!(read_private(&path, 3).unwrap(), b"abc");
    }

    #[test]
    fn a_file_that_others_can_read_is_unsafe() {
        let root = private_root();
        let path = root.path().join("in.toml");
        fs::write(&path, b"abc").unwrap();
        chmod(&path, 0o644);
        let r = read_private(&path, 16);
        assert!(matches!(r, Err(PrivateReadError::Unsafe)), "{r:?}");
    }

    #[test]
    fn a_symlink_is_unsafe_to_read() {
        let root = private_root();
        let real = root.path().join("real.toml");
        fs::write(&real, b"abc").unwrap();
        chmod(&real, 0o600);
        let link = root.path().join("in.toml");
        symlink(&real, &link).unwrap();
        let r = read_private(&link, 16);
        assert!(matches!(r, Err(PrivateReadError::Unsafe)), "{r:?}");
    }

    #[test]
    fn a_file_over_the_limit_is_too_large() {
        let root = private_root();
        let path = root.path().join("in.toml");
        fs::write(&path, vec![b'x'; 17]).unwrap();
        chmod(&path, 0o600);
        let r = read_private(&path, 16);
        assert!(matches!(r, Err(PrivateReadError::TooLarge)), "{r:?}");
    }

    #[test]
    fn a_parent_with_mode_0755_is_accepted_for_reading() {
        let root = private_root();
        let path = root.path().join("in.toml");
        fs::write(&path, b"abc").unwrap();
        chmod(&path, 0o600);
        chmod(root.path(), 0o755);
        assert_eq!(read_private(&path, 16).unwrap(), b"abc");
        chmod(root.path(), 0o775);
        let r = read_private(&path, 16);
        assert!(matches!(r, Err(PrivateReadError::Unsafe)), "{r:?}");
    }

    #[test]
    fn a_symlinked_parent_or_a_relative_path_is_unsafe_to_read() {
        let root = private_root();
        let real = root.path().join("real");
        fs::create_dir(&real).unwrap();
        let file = real.join("in.toml");
        fs::write(&file, b"abc").unwrap();
        chmod(&file, 0o600);
        let link = root.path().join("link");
        symlink(&real, &link).unwrap();
        let r = read_private(&link.join("in.toml"), 16);
        assert!(matches!(r, Err(PrivateReadError::Unsafe)), "{r:?}");
        let r = read_private(Path::new("in.toml"), 16);
        assert!(matches!(r, Err(PrivateReadError::Unsafe)), "{r:?}");
    }

    fn replace(root: &Path, path: &Path, bytes: &[u8]) -> Result<(), PrivateWriteError> {
        replace_private_below(root, path, bytes, ".state.")
    }

    #[test]
    fn replace_creates_a_new_file_with_mode_0600() {
        let root = private_root();
        let path = root.path().join("state.json");
        replace(root.path(), &path, b"one").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"one");
        assert_eq!(fs::metadata(&path).unwrap().mode() & 0o777, 0o600);
    }

    #[test]
    fn replace_replaces_a_private_file_and_leaves_no_temp() {
        let root = private_root();
        let path = root.path().join("state.json");
        replace(root.path(), &path, b"one").unwrap();
        replace(root.path(), &path, b"two").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"two");
        assert_eq!(fs::metadata(&path).unwrap().mode() & 0o777, 0o600);
        let names: Vec<_> = fs::read_dir(root.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names.len(), 1, "{names:?}");
    }

    #[test]
    fn replace_refuses_a_destination_that_others_can_read() {
        let root = private_root();
        let path = root.path().join("state.json");
        fs::write(&path, b"old").unwrap();
        chmod(&path, 0o644);
        let r = replace(root.path(), &path, b"new");
        assert!(matches!(r, Err(PrivateWriteError::Unsafe)), "{r:?}");
        assert_eq!(fs::read(&path).unwrap(), b"old");
    }

    #[test]
    fn replace_refuses_a_symlink_and_leaves_its_target() {
        let root = private_root();
        let target = root.path().join("target.json");
        fs::write(&target, b"old").unwrap();
        chmod(&target, 0o600);
        let path = root.path().join("state.json");
        symlink(&target, &path).unwrap();
        let r = replace(root.path(), &path, b"new");
        assert!(matches!(r, Err(PrivateWriteError::Unsafe)), "{r:?}");
        assert_eq!(fs::read(&target).unwrap(), b"old");
        assert_eq!(fs::read_link(&path).unwrap(), target);
        let dangling = root.path().join("dangling.json");
        symlink(root.path().join("nowhere"), &dangling).unwrap();
        let r = replace(root.path(), &dangling, b"new");
        assert!(matches!(r, Err(PrivateWriteError::Unsafe)), "{r:?}");
    }

    #[test]
    fn replace_refuses_a_directory_as_destination() {
        let root = private_root();
        let path = root.path().join("state.json");
        fs::create_dir(&path).unwrap();
        let r = replace(root.path(), &path, b"new");
        assert!(matches!(r, Err(PrivateWriteError::Unsafe)), "{r:?}");
    }

    #[test]
    fn replace_refuses_a_parent_that_is_not_private() {
        let root = private_root();
        chmod(root.path(), 0o755);
        let path = root.path().join("state.json");
        let r = replace(root.path(), &path, b"new");
        assert!(matches!(r, Err(PrivateWriteError::Unsafe)), "{r:?}");
        assert!(!path.exists());
    }

    #[test]
    fn the_public_replace_refuses_a_path_below_tmp() {
        let dir = tempfile::Builder::new().tempdir_in("/tmp").unwrap();
        chmod(dir.path(), 0o700);
        let r = replace_private(&dir.path().join("state.json"), b"data", ".state.");
        assert!(matches!(r, Err(PrivateWriteError::Unsafe)), "{r:?}");
    }

    #[test]
    fn a_lock_is_taken_once_and_released_on_drop() {
        let root = private_root();
        let path = root.path().join("x.lock");
        let first = try_lock_private_below(root.path(), &path).unwrap();
        assert!(first.is_some());
        assert_eq!(fs::metadata(&path).unwrap().mode() & 0o777, 0o600);
        let second = try_lock_private_below(root.path(), &path).unwrap();
        assert!(second.is_none());
        drop(first);
        let third = try_lock_private_below(root.path(), &path).unwrap();
        assert!(third.is_some());
    }

    #[test]
    fn a_lock_file_that_others_can_read_is_unsafe() {
        let root = private_root();
        let path = root.path().join("x.lock");
        fs::write(&path, b"").unwrap();
        chmod(&path, 0o644);
        let r = try_lock_private_below(root.path(), &path);
        assert!(matches!(r, Err(PrivateWriteError::Unsafe)), "{r:?}");
    }

    #[test]
    fn a_lock_file_that_is_a_symlink_is_unsafe() {
        let root = private_root();
        let real = root.path().join("real");
        fs::write(&real, b"").unwrap();
        chmod(&real, 0o600);
        let path = root.path().join("x.lock");
        symlink(&real, &path).unwrap();
        let r = try_lock_private_below(root.path(), &path);
        assert!(matches!(r, Err(PrivateWriteError::Unsafe)), "{r:?}");
    }

    #[test]
    fn a_lock_in_a_parent_that_is_not_private_is_unsafe() {
        let root = private_root();
        chmod(root.path(), 0o755);
        let r = try_lock_private_below(root.path(), &root.path().join("x.lock"));
        assert!(matches!(r, Err(PrivateWriteError::Unsafe)), "{r:?}");
        let tmp = tempfile::Builder::new().tempdir_in("/tmp").unwrap();
        chmod(tmp.path(), 0o700);
        let r = try_lock_private(&tmp.path().join("x.lock"));
        assert!(matches!(r, Err(PrivateWriteError::Unsafe)), "{r:?}");
    }

    #[test]
    fn the_temporary_name_is_32_lowercase_hex() {
        let text = random_hex().unwrap();
        assert_eq!(text.len(), 32);
        assert!(
            text.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        );
        assert_ne!(text, random_hex().unwrap());
    }
}
