//! The import of a checked screenshot into the artifact tree and the startup
//! recovery of the captures (R13 P5a task D2, design `omarchy-p5a.md` §5.4,
//! §6.2).
//!
//! The import creates `<artifacts>/<artifact_id>/1/capture.png` from the
//! retained staging descriptor (the staging path is never opened again) and
//! syncs, in this order: the PNG, `1/`, `<artifact_id>/`, `artifacts/`, and
//! `<station data>/` when this import created `artifacts/`. A failed sync is
//! a failed import: the caller commits nothing and removes the partial
//! directory with [`discard`]. Before each write the import checks the free
//! space with the `room` check of the caller: a refusal is `disk_full`, as
//! is a write that fails with `ENOSPC`.
//!
//! The recovery ([`ArtifactService::ensure_recovered`]) runs once, before
//! the first capture: a `running` operation becomes `failed` with
//! `capture_interrupted`, its artifact directory goes away when no version
//! row exists for it, and the staging folder is emptied. A `failed`
//! operation keeps its reservation until its folder is removed, so a removal
//! that failed or was cut by a crash is retried at the next start. It never
//! captures again and never sweeps an ordinary artifact directory.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::os::fd::OwnedFd;
use std::path::{Path, PathBuf};

use fleetdeck_proto::capture::{code, text};
use rustix::fs::{CWD, Mode, OFlags};

use super::{ArtifactError, ArtifactService};
use crate::desktop::capture::{COPY_BUFFER, Staging};
use crate::desktop::{Fail, profile};
use crate::store::capture_ops::{CAPTURE_FILE, CaptureState};

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

const DIR_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);

const FILE_FLAGS: OFlags = OFlags::WRONLY
    .union(OFlags::CREATE)
    .union(OFlags::EXCL)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);

fn publish_failed() -> Fail {
    Fail::new(code::CAPTURE_PUBLISH_FAILED, text::CAPTURE_PUBLISH_FAILED)
}

fn disk_full() -> Fail {
    Fail::new(code::DISK_FULL, text::DISK_FULL)
}

/// The free-space check of one write: `Ok` when the disk of the directory
/// keeps its reserve after `need` more bytes.
pub type Room<'a> = &'a dyn Fn(&OwnedFd, u64) -> Result<(), Fail>;

/// Why the import stopped: an I/O error, or a refusal with its own code.
enum ImportError {
    Io(io::Error),
    Fail(Fail),
}

impl From<io::Error> for ImportError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<rustix::io::Errno> for ImportError {
    fn from(e: rustix::io::Errno) -> Self {
        Self::Io(io::Error::from(e))
    }
}

fn open_dir(parent: &impl rustix::fd::AsFd, name: &Path) -> io::Result<OwnedFd> {
    rustix::fs::openat(parent, name, DIR_FLAGS, Mode::empty()).map_err(io::Error::from)
}

/// Makes and opens the folder `name` of `parent` (mode 0700). It must not
/// exist.
fn make_dir(parent: &OwnedFd, name: &str) -> io::Result<OwnedFd> {
    rustix::fs::mkdirat(parent, name, Mode::from_raw_mode(0o700))?;
    open_dir(parent, Path::new(name))
}

/// Copies `size` bytes of `src`, from offset 0, in reads of at most
/// `COPY_BUFFER` bytes. `room` runs on `artifacts` before each write.
fn copy(
    src: &mut File,
    dest: &mut File,
    size: u64,
    artifacts: &OwnedFd,
    room: Room<'_>,
) -> Result<(), ImportError> {
    src.seek(SeekFrom::Start(0))?;
    let mut reader = src.take(size);
    let mut buf = vec![0_u8; COPY_BUFFER];
    let mut copied = 0_u64;
    loop {
        match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                room(artifacts, n as u64).map_err(ImportError::Fail)?;
                dest.write_all(&buf[..n]).map_err(|e| {
                    if e.raw_os_error() == Some(rustix::io::Errno::NOSPC.raw_os_error()) {
                        ImportError::Fail(disk_full())
                    } else {
                        ImportError::Io(e)
                    }
                })?;
                copied += n as u64;
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e.into()),
        }
    }
    if copied == size {
        Ok(())
    } else {
        Err(io::Error::from(io::ErrorKind::UnexpectedEof).into())
    }
}

fn import(
    root: &Path,
    artifact_id: &str,
    src: &mut File,
    size: u64,
    fs: &dyn DurableFs,
    room: Room<'_>,
) -> Result<(), ImportError> {
    let plain = !artifact_id.is_empty()
        && !artifact_id.contains('/')
        && artifact_id != "."
        && artifact_id != "..";
    let (Some(data_path), Some(name), true) = (root.parent(), root.file_name(), plain) else {
        return Err(io::Error::from(io::ErrorKind::InvalidInput).into());
    };
    let data = open_dir(&CWD, data_path)?;
    let (artifacts, created_root) = match open_dir(&data, Path::new(name)) {
        Ok(fd) => (fd, false),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            rustix::fs::mkdirat(&data, name, Mode::from_raw_mode(0o700))?;
            (open_dir(&data, Path::new(name))?, true)
        }
        Err(e) => return Err(e.into()),
    };
    let id_dir = make_dir(&artifacts, artifact_id)?;
    let version_dir = make_dir(&id_dir, "1")?;
    let fd = rustix::fs::openat(
        &version_dir,
        CAPTURE_FILE,
        FILE_FLAGS,
        Mode::from_raw_mode(0o600),
    )?;
    let mut dest = File::from(fd);
    copy(src, &mut dest, size, &artifacts, room)?;
    fs.sync_file(&dest)?;
    fs.sync_dir(&version_dir)?;
    fs.sync_dir(&id_dir)?;
    fs.sync_dir(&artifacts)?;
    if created_root {
        fs.sync_dir(&data)?;
    }
    Ok(())
}

/// Imports the PNG of `src` (`size` bytes) as `<root>/<artifact_id>/1/capture.png`.
/// Blocking: a caller in an async task uses `spawn_blocking`.
///
/// `room` is called with the descriptor of `<root>` and the size of the next
/// write, before each write.
///
/// # Errors
/// `disk_full` when `room` refuses a write or a write fails with `ENOSPC`;
/// `capture_publish_failed` for every other failure, a failed sync included.
/// The caller removes the partial folder with [`discard`].
pub fn import_png(
    root: &Path,
    artifact_id: &str,
    src: &mut File,
    size: u64,
    fs: &dyn DurableFs,
    room: Room<'_>,
) -> Result<(), Fail> {
    import(root, artifact_id, src, size, fs, room).map_err(|e| match e {
        ImportError::Io(e) => {
            tracing::warn!(kind = ?e.kind(), "cannot import the screenshot");
            publish_failed()
        }
        ImportError::Fail(fail) => {
            tracing::warn!(code = fail.code, "cannot import the screenshot");
            fail
        }
    })
}

/// Removes `<root>/<artifact_id>/` with its content, without following a
/// link. A missing folder is fine.
///
/// # Errors
/// The I/O error of the removal; `InvalidInput` when the folder or `root` is
/// a link or not a folder, or the ID is not a plain name.
pub fn discard(root: &Path, artifact_id: &str) -> io::Result<()> {
    profile::remove(root, artifact_id).map_err(|e| match e {
        profile::ProfileError::NotPlain => io::Error::from(io::ErrorKind::InvalidInput),
        profile::ProfileError::Io(e) => e,
    })
}

fn internal(e: impl std::fmt::Display) -> ArtifactError {
    ArtifactError::new("internal", e.to_string())
}

impl ArtifactService {
    /// `<station data>/captures`: the staging folder, next to the artifacts
    /// root.
    #[must_use]
    pub fn captures_dir(&self) -> PathBuf {
        self.root.with_file_name("captures")
    }

    /// Runs the startup recovery of the captures once. The first caller
    /// runs it; the others wait for it. After an error the next call runs
    /// it again, so a capture never starts before the recovery succeeded.
    ///
    /// # Errors
    /// `internal` when the operations cannot be read or changed.
    pub async fn ensure_recovered(&self) -> Result<(), ArtifactError> {
        self.capture_ready
            .get_or_try_init(|| async {
                // The pending artifact deletions finish first.
                self.finish_pending_deletions().await;
                self.recover_captures().await.inspect_err(|e| {
                    tracing::warn!(error = %e, "the capture recovery failed");
                })
            })
            .await
            .map(|_| ())
    }

    async fn recover_captures(&self) -> Result<(), ArtifactError> {
        let rows = self.store.captures_to_recover().await.map_err(internal)?;
        for op in rows {
            if op.state == CaptureState::Running {
                self.store
                    .capture_fail(
                        &op.deck_fp,
                        &op.capture_id,
                        code::CAPTURE_INTERRUPTED,
                        text::CAPTURE_INTERRUPTED,
                    )
                    .await
                    .map_err(internal)?;
            }
            let committed = self
                .store
                .artifact_has_version(&op.artifact_id)
                .await
                .map_err(internal)?;
            if !committed {
                let (root, id) = (self.root.clone(), op.artifact_id.clone());
                let removed = tokio::task::spawn_blocking(move || discard(&root, &id))
                    .await
                    .map_err(internal)?;
                if let Err(e) = removed {
                    // The reservation stays: the next start retries.
                    tracing::warn!(
                        kind = ?e.kind(),
                        "cannot remove the folder of an interrupted screenshot"
                    );
                    continue;
                }
            }
            self.store
                .capture_cleaned(&op.deck_fp, &op.capture_id)
                .await
                .map_err(internal)?;
        }
        let captures = self.captures_dir();
        tokio::task::spawn_blocking(move || Staging::remove_all(&captures))
            .await
            .map_err(internal)
    }
}

#[cfg(test)]
#[path = "capture_import_tests.rs"]
pub(crate) mod tests;
