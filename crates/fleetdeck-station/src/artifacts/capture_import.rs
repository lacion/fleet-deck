//! The import of a checked capture into the artifact tree and the startup
//! recovery of the captures (R13 P5a task D2, design `omarchy-p5a.md` §5.4,
//! §6.2; R13 P5b task S2, design `omarchy-p5b.md` §5.3, §5.4, §9.4).
//!
//! The import creates `<artifacts>/<artifact_id>/1/capture.png` (a
//! screenshot) or `.../1/capture.mp4` (a recording) from the retained staging
//! descriptor (the staging path is never opened again) and syncs, in this
//! order: the file, `1/`, `<artifact_id>/`, `artifacts/`, and `<station data>/`
//! when this import created `artifacts/`. A failed sync is a failed import: the
//! caller commits nothing and removes the partial directory with
//! [`discard_durable`]. Before each write the import checks the free space
//! with the `room` check of the caller: a refusal is `disk_full`, as is a write
//! that fails with `ENOSPC`.
//!
//! The recovery ([`ArtifactService::ensure_recovered`]) runs once, before the
//! first capture and the first artifact read (§5.4), in this order:
//!
//! 1. the pending artifact deletions finish;
//! 2. a `running` operation becomes `failed`: `capture_interrupted` for a
//!    screenshot, `recording_interrupted` (also as the stop reason) for a
//!    recording;
//! 3. the staging folder and the uncommitted destination folder of every
//!    `failed` operation are removed durably, then its reservation is freed;
//! 4. the staging copy of a `committed` operation that still holds a
//!    reservation is removed durably, then its reservation is freed;
//! 5. the orphan scan: an entry of the staging folder that belongs to no
//!    `running` operation and no operation in cleanup, and an
//!    `<artifacts>/<name>` that a capture operation named and that has no
//!    version row and no such operation, is garbage. It is counted in
//!    `capture_garbage` with its size, removed durably, and then its row is
//!    deleted. An ordinary artifact folder is never touched.
//!
//! A removal that fails leaves its operation (or its garbage row) in place,
//! with its bytes counted in the quota; the next start retries it. The
//! recovery never captures again.

use std::collections::HashSet;
use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::os::fd::OwnedFd;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use fleetdeck_proto::capture::{CaptureKind, code, text};
use fleetdeck_proto::recording::{RECORDING_FILE, StopReason};
use rustix::fs::{AtFlags, CWD, Dir, FileType, Mode, OFlags};

use super::{ArtifactError, ArtifactService};
use crate::desktop::capture::{COPY_BUFFER, Staging};
use crate::desktop::{Fail, profile};
use crate::media::durable::remove_tree_durable;
use crate::store::capture_ops::{CAPTURE_FILE, CaptureOp, CaptureState, GarbageArea, ScanSets};

pub use crate::media::durable::{DurableFs, SyncFs};

/// The deepest folder level that the size of a garbage entry follows down.
const MAX_DEPTH: u32 = 64;

const DIR_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);

const FILE_FLAGS: OFlags = OFlags::WRONLY
    .union(OFlags::CREATE)
    .union(OFlags::EXCL)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);

/// What a capture imports: the trusted kind of the operation decides the file
/// name; the caller never chooses it (design §9.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureFileKind {
    /// A screenshot: `capture.png`.
    Png,
    /// A recording: `capture.mp4`.
    Mp4,
}

impl CaptureFileKind {
    /// The name of the one file of the version.
    #[must_use]
    pub fn file_name(self) -> &'static str {
        match self {
            Self::Png => CAPTURE_FILE,
            Self::Mp4 => RECORDING_FILE,
        }
    }

    fn publish_failed(self) -> Fail {
        match self {
            Self::Png => Fail::new(code::CAPTURE_PUBLISH_FAILED, text::CAPTURE_PUBLISH_FAILED),
            Self::Mp4 => Fail::new(
                code::RECORDING_PUBLISH_FAILED,
                text::RECORDING_PUBLISH_FAILED,
            ),
        }
    }
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
    kind: CaptureFileKind,
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
        kind.file_name(),
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

/// Imports the file of `src` (`size` bytes) as `<root>/<artifact_id>/1/<file>`,
/// where `<file>` is `capture.png` for [`CaptureFileKind::Png`] and
/// `capture.mp4` for [`CaptureFileKind::Mp4`]. Blocking: a caller in an async
/// task uses `spawn_blocking`.
///
/// `room` is called with the descriptor of `<root>` and the size of the next
/// write, before each write.
///
/// # Errors
/// `disk_full` when `room` refuses a write or a write fails with `ENOSPC`;
/// `capture_publish_failed` (a screenshot) or `recording_publish_failed` (a
/// recording) for every other failure, a failed sync included. The caller
/// removes the partial folder with [`discard_durable`].
pub fn import_capture(
    root: &Path,
    artifact_id: &str,
    kind: CaptureFileKind,
    src: &mut File,
    size: u64,
    fs: &dyn DurableFs,
    room: Room<'_>,
) -> Result<(), Fail> {
    import(root, artifact_id, kind, src, size, fs, room).map_err(|e| match e {
        ImportError::Io(e) => {
            tracing::warn!(kind = ?e.kind(), "cannot import the capture");
            kind.publish_failed()
        }
        ImportError::Fail(fail) => {
            tracing::warn!(code = fail.code, "cannot import the capture");
            fail
        }
    })
}

/// [`import_capture`] for a screenshot.
///
/// # Errors
/// As [`import_capture`] with [`CaptureFileKind::Png`].
pub fn import_png(
    root: &Path,
    artifact_id: &str,
    src: &mut File,
    size: u64,
    fs: &dyn DurableFs,
    room: Room<'_>,
) -> Result<(), Fail> {
    import_capture(root, artifact_id, CaptureFileKind::Png, src, size, fs, room)
}

/// Removes `<root>/<artifact_id>/` with its content, without following a
/// link. A missing folder is fine. The removal is not durable: the recovery
/// and the cleanup of a capture use [`discard_durable`].
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

/// Removes the uncommitted destination tree `<root>/<artifact_id>/` durably
/// (design §5.3): every file is unlinked, each emptied folder is synced and
/// removed, and `root` is synced at the end. A missing `root` or folder is
/// already removed.
///
/// # Errors
/// The I/O error of the removal or of a sync; `InvalidInput` for an ID that
/// is not a plain name, a link, or a tree of more than 64 levels. After an
/// `Err` the removal is not finished.
pub fn discard_durable(root: &Path, artifact_id: &str, fs: &dyn DurableFs) -> io::Result<()> {
    let dir = match open_dir(&CWD, root) {
        Ok(dir) => dir,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    remove_tree_durable(&dir, artifact_id, fs)
}

fn internal(e: impl std::fmt::Display) -> ArtifactError {
    ArtifactError::new("internal", e.to_string())
}

fn warn_kind(what: &str, e: &io::Error) {
    tracing::warn!(kind = ?e.kind(), "{what}");
}

/// One entry of a folder and what it is; a link is not followed.
struct Entry {
    name: OsString,
    kind: FileType,
}

fn list_entries(dir: &OwnedFd) -> io::Result<Vec<Entry>> {
    let mut out = Vec::new();
    for entry in Dir::read_from(dir)? {
        let entry = entry?;
        let raw = entry.file_name().to_bytes();
        if raw == b"." || raw == b".." {
            continue;
        }
        let name = OsStr::from_bytes(raw).to_owned();
        let kind = match entry.file_type() {
            FileType::Unknown => FileType::from_raw_mode(
                rustix::fs::statat(dir, &name, AtFlags::SYMLINK_NOFOLLOW)?.st_mode,
            ),
            kind => kind,
        };
        out.push(Entry { name, kind });
    }
    Ok(out)
}

/// The sum of the sizes of the regular files of `dir`, below it too.
fn tree_bytes(dir: &OwnedFd, depth: u32) -> io::Result<u64> {
    if depth > MAX_DEPTH {
        return Err(io::Error::from(io::ErrorKind::InvalidInput));
    }
    let mut total = 0_u64;
    for entry in list_entries(dir)? {
        total = total.saturating_add(entry_bytes(dir, &entry, depth)?);
    }
    Ok(total)
}

/// The size of one entry: a regular file, or the files of a folder; 0 for
/// anything else.
fn entry_bytes(dir: &OwnedFd, entry: &Entry, depth: u32) -> io::Result<u64> {
    match entry.kind {
        FileType::RegularFile => {
            let stat = rustix::fs::statat(dir, &entry.name, AtFlags::SYMLINK_NOFOLLOW)?;
            Ok(u64::try_from(stat.st_size).unwrap_or(0))
        }
        FileType::Directory => {
            let child = rustix::fs::openat(dir, &entry.name, DIR_FLAGS, Mode::empty())?;
            tree_bytes(&child, depth + 1)
        }
        _ => Ok(0),
    }
}

/// Removes the entry `name` of the folder `area` durably: a folder as a tree,
/// anything else (a regular file, a link) with one unlink; then `area` is
/// synced. A missing entry is removed already.
fn remove_entry(area: &Path, name: &str, fs: &dyn DurableFs) -> io::Result<()> {
    let dir = open_dir(&CWD, area)?;
    let stat = match rustix::fs::statat(&dir, name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(stat) => stat,
        Err(e) if e == rustix::io::Errno::NOENT => return fs.sync_dir(&dir),
        Err(e) => return Err(e.into()),
    };
    if FileType::from_raw_mode(stat.st_mode) == FileType::Directory {
        remove_tree_durable(&dir, name, fs)
    } else {
        rustix::fs::unlinkat(&dir, name, AtFlags::empty())?;
        fs.sync_dir(&dir)
    }
}

/// What the listing of the two folders found.
#[derive(Default)]
struct Found {
    /// The areas that were listed (a missing folder counts as listed).
    listed: Vec<GarbageArea>,
    /// Every entry that exists, by area and name.
    present: HashSet<(GarbageArea, String)>,
    /// The entries to remove, with their size.
    garbage: Vec<(GarbageArea, String, u64)>,
}

/// Lists the staging folder and `<artifacts>/` (blocking) and picks the
/// garbage (design §5.4 step 6).
fn scan(captures: &Path, artifacts: &Path, sets: &ScanSets) -> Found {
    let mut found = Found::default();
    for (area, path) in [
        (GarbageArea::Staging, captures),
        (GarbageArea::Artifacts, artifacts),
    ] {
        let dir = match open_dir(&CWD, path) {
            Ok(dir) => dir,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                found.listed.push(area);
                continue;
            }
            Err(e) => {
                warn_kind("cannot open a capture folder for the orphan scan", &e);
                continue;
            }
        };
        let entries = match list_entries(&dir) {
            Ok(entries) => entries,
            Err(e) => {
                warn_kind("cannot list a capture folder for the orphan scan", &e);
                continue;
            }
        };
        found.listed.push(area);
        for entry in entries {
            let Some(name) = entry.name.to_str() else {
                tracing::warn!("a capture folder entry has a name that is not text");
                continue;
            };
            found.present.insert((area, name.to_owned()));
            let garbage = match area {
                GarbageArea::Staging => !sets.staging_keep.contains(name),
                GarbageArea::Artifacts => sets.artifact_orphans.contains(name),
            };
            if !garbage {
                continue;
            }
            match entry_bytes(&dir, &entry, 0) {
                Ok(bytes) => found.garbage.push((area, name.to_owned(), bytes)),
                Err(e) => warn_kind("cannot measure a garbage entry", &e),
            }
        }
    }
    found
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
    /// it again, so a capture or an artifact read never starts before the
    /// recovery succeeded.
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
        self.recover_captures_with(Arc::new(SyncFs)).await
    }

    /// The recovery of the module text, with the syncs of `fs`.
    async fn recover_captures_with(&self, fs: Arc<dyn DurableFs>) -> Result<(), ArtifactError> {
        let rows = self.store.captures_to_recover().await.map_err(internal)?;
        for op in rows {
            match op.state {
                CaptureState::Committed => self.finish_staging(&op, &fs).await?,
                CaptureState::Running | CaptureState::Failed => {
                    if op.state == CaptureState::Running {
                        self.interrupt(&op).await?;
                    }
                    self.clean_failed(&op, &fs).await?;
                }
            }
        }
        self.scan_orphans(&fs).await
    }

    /// A `running` operation found at the start becomes `failed`.
    async fn interrupt(&self, op: &CaptureOp) -> Result<(), ArtifactError> {
        let result = match op.kind {
            CaptureKind::Screenshot => {
                self.store
                    .capture_fail(
                        &op.deck_fp,
                        &op.capture_id,
                        code::CAPTURE_INTERRUPTED,
                        text::CAPTURE_INTERRUPTED,
                    )
                    .await
            }
            CaptureKind::Recording => {
                self.store
                    .recording_fail(
                        &op.deck_fp,
                        &op.capture_id,
                        code::RECORDING_INTERRUPTED,
                        text::RECORDING_INTERRUPTED,
                        StopReason::RecordingInterrupted,
                    )
                    .await
            }
        };
        result.map_err(internal)
    }

    /// Removes the staging folder and the uncommitted destination folder of a
    /// `failed` operation durably; the reservation is freed only when both
    /// removals succeeded. A destination with a version row is never removed.
    async fn clean_failed(
        &self,
        op: &CaptureOp,
        fs: &Arc<dyn DurableFs>,
    ) -> Result<(), ArtifactError> {
        let has_version = self
            .store
            .artifact_has_version(&op.artifact_id)
            .await
            .map_err(internal)?;
        let (captures, capture_id) = (self.captures_dir(), op.capture_id.clone());
        let (root, artifact_id, fs) = (self.root.clone(), op.artifact_id.clone(), Arc::clone(fs));
        let removed = tokio::task::spawn_blocking(move || {
            let staging = Staging::remove(&captures, &capture_id, &*fs);
            let folder = if has_version {
                Ok(())
            } else {
                discard_durable(&root, &artifact_id, &*fs)
            };
            (staging, folder)
        })
        .await
        .map_err(internal)?;
        match removed {
            (Ok(()), Ok(())) => self
                .store
                .capture_cleaned(&op.deck_fp, &op.capture_id)
                .await
                .map_err(internal),
            (staging, folder) => {
                // The reservation stays: the next start retries.
                tracing::warn!(
                    staging = ?staging.err().map(|e| e.code),
                    folder = ?folder.err().map(|e| e.kind()),
                    "cannot remove the files of an interrupted capture"
                );
                Ok(())
            }
        }
    }

    /// Removes the staging copy of a `committed` operation durably and frees
    /// its reservation.
    async fn finish_staging(
        &self,
        op: &CaptureOp,
        fs: &Arc<dyn DurableFs>,
    ) -> Result<(), ArtifactError> {
        let (captures, capture_id, fs) =
            (self.captures_dir(), op.capture_id.clone(), Arc::clone(fs));
        let removed =
            tokio::task::spawn_blocking(move || Staging::remove(&captures, &capture_id, &*fs))
                .await
                .map_err(internal)?;
        match removed {
            Ok(()) => self
                .store
                .capture_staging_cleaned(&op.deck_fp, &op.capture_id)
                .await
                .map_err(internal),
            Err(e) => {
                tracing::warn!(
                    code = e.code,
                    "cannot remove the staging copy of a recording"
                );
                Ok(())
            }
        }
    }

    /// The orphan scan (design §5.4 step 6): counts each garbage entry,
    /// removes it durably, then deletes its row.
    async fn scan_orphans(&self, fs: &Arc<dyn DurableFs>) -> Result<(), ArtifactError> {
        let sets = self.store.capture_scan_sets().await.map_err(internal)?;
        let (captures, artifacts) = (self.captures_dir(), self.root.clone());
        let found = tokio::task::spawn_blocking(move || scan(&captures, &artifacts, &sets))
            .await
            .map_err(internal)?;
        // A row of an earlier start whose entry is gone counts nothing.
        for (area, name, _) in self.store.garbage_list().await.map_err(internal)? {
            let gone =
                found.listed.contains(&area) && !found.present.contains(&(area, name.clone()));
            if gone {
                self.store
                    .garbage_delete(area, &name)
                    .await
                    .map_err(internal)?;
            }
        }
        for (area, name, bytes) in found.garbage {
            self.store
                .garbage_record(area, &name, bytes)
                .await
                .map_err(internal)?;
            let folder = match area {
                GarbageArea::Staging => self.captures_dir(),
                GarbageArea::Artifacts => self.root.clone(),
            };
            let (entry, fs) = (name.clone(), Arc::clone(fs));
            let removed = tokio::task::spawn_blocking(move || remove_entry(&folder, &entry, &*fs))
                .await
                .map_err(internal)?;
            match removed {
                Ok(()) => self
                    .store
                    .garbage_delete(area, &name)
                    .await
                    .map_err(internal)?,
                Err(e) => {
                    // The row stays and counts its bytes: the next start retries.
                    tracing::warn!(
                        kind = ?e.kind(),
                        area = ?area,
                        "cannot remove a garbage entry of the captures"
                    );
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "capture_import_tests.rs"]
pub(crate) mod tests;

#[cfg(test)]
#[path = "capture_recovery_tests.rs"]
mod recovery_tests;
