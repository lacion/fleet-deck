//! A verified launch directory (B9, DL-200): the station opens the session
//! folder once, below an allowed root and without following a link, and every
//! process that it starts for the session starts in exactly that directory
//! object, or does not start.
//!
//! The allowed roots are a launch policy, not a sandbox (design section 2): a
//! process that can write outside the roots is out of the threat model, and a
//! descriptor pins an object, not its ancestry.

use std::os::fd::{BorrowedFd, OwnedFd};
use std::path::{Component, Path, PathBuf};

use fleetdeck_proto::launch::LaunchIdentity;
use rustix::fs::{Mode, OFlags};

/// The text of [`LaunchDirError::Changed`] and of any open failure.
pub const CHANGED: &str = "The session folder changed: a link is in its path.";
/// The text of [`LaunchDirError::Unavailable`].
pub const UNAVAILABLE: &str = "The station cannot hold the session folder open (no /proc).";
/// The text of [`LaunchDirError::Unsupported`].
pub const UNSUPPORTED: &str = "This system cannot hold the session folder open.";

/// Why a launch directory cannot be used.
#[derive(Debug, thiserror::Error)]
pub enum LaunchDirError {
    #[error("{CHANGED}")]
    Changed,
    #[error("{UNAVAILABLE}")]
    Unavailable,
    #[error("{UNSUPPORTED}")]
    Unsupported,
}

impl LaunchDirError {
    /// The stable error code.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::Changed => "launch_dir_changed",
            Self::Unavailable => "launch_dir_unavailable",
            Self::Unsupported => "launch_dir_unsupported",
        }
    }
}

fn dir_flags() -> OFlags {
    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC
}

/// Opens the canonical, absolute `root` as a directory descriptor without
/// following a symlink at any level: the walk starts at `/` and opens each
/// component relative to its parent's descriptor with `O_NOFOLLOW`, so an
/// ancestor of the root swapped for a symlink is refused too (M3 round 13,
/// DL-092). A root that is not absolute or has `.`/`..` components is
/// refused; callers pass the canonical root from the allowed-roots policy.
///
/// # Errors
/// The `openat` error of the first component that is missing, not a
/// directory or a symlink; `EINVAL` for a root that is not canonical.
pub fn open_root(root: &Path) -> rustix::io::Result<OwnedFd> {
    if !root.is_absolute() {
        return Err(rustix::io::Errno::INVAL);
    }
    let mut dir = rustix::fs::open("/", dir_flags(), Mode::empty())?;
    for component in root.components() {
        match component {
            Component::RootDir => {}
            Component::Normal(name) => {
                dir = rustix::fs::openat(&dir, name, dir_flags(), Mode::empty())?;
            }
            Component::Prefix(_) | Component::CurDir | Component::ParentDir => {
                return Err(rustix::io::Errno::INVAL);
            }
        }
    }
    Ok(dir)
}

/// A path that names exactly the directory `dir` for a child process's
/// working directory: `/proc/self/fd/<n>`. The child resolves it before
/// `exec`, while it still holds the inherited descriptor (close-on-exec
/// closes it only at `exec`), so a rename of the root or of one of its
/// ancestors cannot move the child elsewhere. The caller keeps `dir` open
/// until the child has started. `None` where no such path exists (macOS):
/// the caller then must not run a child in the directory by name.
#[must_use]
pub fn pinned_dir(dir: &OwnedFd) -> Option<PathBuf> {
    #[cfg(target_os = "linux")]
    {
        use std::os::fd::AsRawFd;
        Some(PathBuf::from(format!("/proc/self/fd/{}", dir.as_raw_fd())))
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = dir;
        None
    }
}

/// The directory object that the station checked, held open.
#[derive(Debug)]
pub struct LaunchDir {
    fd: OwnedFd,
    identity: LaunchIdentity,
}

/// The normal components of `rel`, or `Changed` for anything else.
fn normal_components(rel: &Path) -> Result<Vec<&std::ffi::OsStr>, LaunchDirError> {
    rel.components()
        .map(|c| match c {
            Component::Normal(name) => Ok(name),
            _ => Err(LaunchDirError::Changed),
        })
        .collect()
}

/// Opens `names` below `root_fd`, one component at a time, each relative to
/// its parent and without following a link. Weaker than `openat2` against a
/// concurrent rename inside the roots (B9, `ENOSYS` fallback).
fn walk(root_fd: &OwnedFd, names: &[&std::ffi::OsStr]) -> rustix::io::Result<OwnedFd> {
    let mut dir = rustix::fs::openat(root_fd, c".", dir_flags(), Mode::empty())?;
    for name in names {
        dir = rustix::fs::openat(&dir, *name, dir_flags(), Mode::empty())?;
    }
    Ok(dir)
}

#[cfg(target_os = "linux")]
fn open_below(
    root_fd: &OwnedFd,
    rel: &Path,
    names: &[&std::ffi::OsStr],
) -> rustix::io::Result<OwnedFd> {
    use rustix::fs::{ResolveFlags, openat2};
    let path = if names.is_empty() {
        Path::new(".")
    } else {
        rel
    };
    match openat2(
        root_fd,
        path,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
        ResolveFlags::BENEATH | ResolveFlags::NO_SYMLINKS | ResolveFlags::NO_MAGICLINKS,
    ) {
        Err(rustix::io::Errno::NOSYS) => walk(root_fd, names),
        other => other,
    }
}

#[cfg(not(target_os = "linux"))]
fn open_below(
    root_fd: &OwnedFd,
    _rel: &Path,
    names: &[&std::ffi::OsStr],
) -> rustix::io::Result<OwnedFd> {
    walk(root_fd, names)
}

impl LaunchDir {
    /// Opens `rel` below the canonical, absolute `root`.
    ///
    /// # Errors
    /// [`LaunchDirError::Changed`] for every failure: a link, a missing
    /// folder, a `rel` that is not made of normal components.
    pub fn open(root: &Path, rel: &Path) -> Result<Self, LaunchDirError> {
        let names = normal_components(rel)?;
        let root_text = root.to_str().ok_or(LaunchDirError::Changed)?;
        let rel_text = names
            .iter()
            .map(|n| n.to_str())
            .collect::<Option<Vec<_>>>()
            .ok_or(LaunchDirError::Changed)?
            .join("/");
        let root_fd = open_root(root).map_err(|_| LaunchDirError::Changed)?;
        let fd = open_below(&root_fd, rel, &names).map_err(|_| LaunchDirError::Changed)?;
        let stat = rustix::fs::fstat(&fd).map_err(|_| LaunchDirError::Changed)?;
        #[allow(
            clippy::useless_conversion,
            reason = "st_dev and st_ino differ by platform"
        )]
        let identity = LaunchIdentity {
            root: root_text.to_owned(),
            rel: rel_text,
            dev: u64::from(stat.st_dev),
            ino: u64::from(stat.st_ino),
        };
        Ok(Self { fd, identity })
    }

    /// Opens `path` below the longest of `roots` that it starts with
    /// (lexically; `path` is absolute with normal components only).
    ///
    /// # Errors
    /// [`LaunchDirError::Changed`] when no root holds `path`, and as [`Self::open`].
    pub fn open_in(roots: &[PathBuf], path: &Path) -> Result<Self, LaunchDirError> {
        let root = roots
            .iter()
            .filter(|r| path.starts_with(r))
            .max_by_key(|r| r.components().count())
            .ok_or(LaunchDirError::Changed)?;
        let rel = path
            .strip_prefix(root)
            .map_err(|_| LaunchDirError::Changed)?;
        Self::open(root, rel)
    }

    /// Opens the folder of `identity` again; `(dev, ino)` must be the same.
    ///
    /// # Errors
    /// [`LaunchDirError::Changed`] when the folder cannot be opened or is a
    /// different object.
    pub fn reopen(identity: &LaunchIdentity) -> Result<Self, LaunchDirError> {
        let dir = Self::open(Path::new(&identity.root), Path::new(&identity.rel))?;
        if dir.identity.dev == identity.dev && dir.identity.ino == identity.ino {
            Ok(dir)
        } else {
            Err(LaunchDirError::Changed)
        }
    }

    #[must_use]
    pub fn identity(&self) -> &LaunchIdentity {
        &self.identity
    }

    /// `root.join(rel)`: for texts and for the provider's `cwd` string. The
    /// root itself has no trailing `/` (`join("")` would add one).
    #[must_use]
    pub fn path(&self) -> PathBuf {
        if self.identity.rel.is_empty() {
            PathBuf::from(&self.identity.root)
        } else {
            Path::new(&self.identity.root).join(&self.identity.rel)
        }
    }

    #[must_use]
    pub fn fd(&self) -> BorrowedFd<'_> {
        use std::os::fd::AsFd;
        self.fd.as_fd()
    }

    /// The working directory for a child: Linux `/proc/self/fd/<n>`, which
    /// must be a directory. The caller keeps `self` alive until the child
    /// has started.
    ///
    /// # Errors
    /// [`LaunchDirError::Unavailable`] when the path is not a directory (no
    /// `/proc`); [`LaunchDirError::Unsupported`] off Linux.
    pub fn cwd(&self) -> Result<PathBuf, LaunchDirError> {
        #[cfg(target_os = "linux")]
        {
            pinned_dir(&self.fd)
                .filter(|p| std::fs::metadata(p).is_ok_and(|m| m.is_dir()))
                .ok_or(LaunchDirError::Unavailable)
        }
        #[cfg(not(target_os = "linux"))]
        {
            Err(LaunchDirError::Unsupported)
        }
    }
}

#[cfg(all(test, target_os = "linux"))]
#[path = "launch_dir_tests.rs"]
mod tests;
