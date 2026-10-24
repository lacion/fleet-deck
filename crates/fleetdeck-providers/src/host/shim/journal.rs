//! The persistent launch evidence of a shim (ADR-005 §3 "Launch journal",
//! §3.2 step 1, §4.5): the private directory, the launch journal, the
//! launch lock, the attach key file and the exit file.
//!
//! Every file lives in `<station dir>/shims/` (mode 0700, files 0600),
//! never in a runtime or temporary directory: a reboot or a cleaner must
//! not remove launch evidence. Each journal record is one JSON line; each
//! append is written, then the file and the directory are synced.

use std::fs::{File, OpenOptions};
use std::io::{self, Write as _};
use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _, OpenOptionsExt as _};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::proof::{AttachKey, KEY_BYTES};
use super::wire::ExitState;

/// One record of a launch journal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "record", rename_all = "snake_case", deny_unknown_fields)]
pub enum Record {
    /// The station wrote this before it started the shim.
    Created {
        shim_id: String,
        incarnation: String,
        /// The boot id that the station read before it started the shim
        /// (DL-182 §4.1). The shim refuses to spawn when its own boot id
        /// differs. Absent in a journal of an older station: the shim then
        /// spawns as before. An older station reads a journal with this
        /// field as unknown evidence and fails closed.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        boot_id: Option<String>,
    },
    /// The shim wrote this before it started the provider.
    SpawnAttempted,
    /// Recovery wrote this while it held the launch lock; a later shim
    /// start exits without a provider.
    Fenced,
}

/// What a journal proves about a launch (ADR-005 §3.2 step 1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaunchEvidence {
    /// An intact journal of this shim and incarnation without
    /// `spawn_attempted`, then `fenced`: no provider ever started.
    NeverStarted,
    /// A missing, unreadable, truncated or foreign journal, or one with
    /// `spawn_attempted`: a provider may have started.
    Unknown,
}

/// The result of fencing a launch at recovery.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fence {
    /// A live shim holds the launch lock: attach to it.
    Held,
    /// The lock was free; `fenced` is in the journal and the evidence is
    /// as given.
    Fenced(LaunchEvidence),
}

fn own_uid() -> u32 {
    rustix::process::getuid().as_raw()
}

/// Creates `dir` with mode 0700, or checks an existing one: it must be a
/// directory owned by this user with no group or other permission
/// (ADR-005 §5.1, test 17). A failed check is `PermissionDenied`.
pub fn prepare_dir(dir: &Path) -> io::Result<()> {
    match std::fs::DirBuilder::new().mode(0o700).create(dir) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            if let Some(parent) = dir.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::DirBuilder::new().mode(0o700).create(dir)?;
        }
        Err(e) => return Err(e),
    }
    check_dir(dir)
}

/// The owner and mode check of [`prepare_dir`], without creating.
pub fn check_dir(dir: &Path) -> io::Result<()> {
    let meta = std::fs::symlink_metadata(dir)?;
    if !meta.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("{} is not a directory", dir.display()),
        ));
    }
    if meta.uid() != own_uid() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("{} belongs to another user", dir.display()),
        ));
    }
    if meta.mode() & 0o077 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "{} is open to other users (mode {:o})",
                dir.display(),
                meta.mode() & 0o777
            ),
        ));
    }
    Ok(())
}

/// The paths of one shim's files.
#[derive(Debug, Clone)]
pub struct ShimFiles {
    dir: PathBuf,
    shim_id: String,
}

impl ShimFiles {
    #[must_use]
    pub fn new(dir: &Path, shim_id: &str) -> Self {
        Self {
            dir: dir.to_owned(),
            shim_id: shim_id.to_owned(),
        }
    }

    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    #[must_use]
    pub fn shim_id(&self) -> &str {
        &self.shim_id
    }

    fn path(&self, ext: &str) -> PathBuf {
        self.dir.join(format!("{}.{ext}", self.shim_id))
    }

    #[must_use]
    pub fn journal(&self) -> PathBuf {
        self.path("journal")
    }

    #[must_use]
    pub fn lock(&self) -> PathBuf {
        self.path("lock")
    }

    #[must_use]
    pub fn key(&self) -> PathBuf {
        self.path("key")
    }

    #[must_use]
    pub fn exit(&self) -> PathBuf {
        self.path("exit")
    }

    /// Writes the first record. Fails if a journal exists. `boot_id` is
    /// the station's boot evidence of the launch (DL-182 §4.1).
    pub fn create_journal(&self, incarnation: &str, boot_id: Option<&str>) -> io::Result<()> {
        let record = Record::Created {
            shim_id: self.shim_id.clone(),
            incarnation: incarnation.to_owned(),
            boot_id: boot_id.map(str::to_owned),
        };
        self.write_record(&record, true)
    }

    /// Appends one record durably.
    pub fn append(&self, record: &Record) -> io::Result<()> {
        self.write_record(record, false)
    }

    fn write_record(&self, record: &Record, create_new: bool) -> io::Result<()> {
        let mut line = serde_json::to_vec(record).map_err(io::Error::other)?;
        line.push(b'\n');
        let mut options = OpenOptions::new();
        options.mode(0o600);
        if create_new {
            options.write(true).create_new(true);
        } else {
            options.append(true);
        }
        let mut file = options.open(self.journal())?;
        file.write_all(&line)?;
        file.sync_all()?;
        sync_dir(&self.dir)
    }

    /// The records of the journal. `None` for a missing, unreadable or
    /// malformed journal, or one whose last line is cut.
    #[must_use]
    pub fn records(&self) -> Option<Vec<Record>> {
        let text = std::fs::read_to_string(self.journal()).ok()?;
        if !text.ends_with('\n') {
            return None;
        }
        text.lines()
            .map(|line| serde_json::from_str(line).ok())
            .collect()
    }

    /// True when the journal holds `fenced`.
    #[must_use]
    pub fn is_fenced(&self) -> bool {
        self.records()
            .is_some_and(|records| records.contains(&Record::Fenced))
    }

    /// The launch evidence (ADR-005 §3.2 step 1). Only an intact journal of
    /// this shim and incarnation that starts with `created`, holds no
    /// `spawn_attempted` and holds `fenced` proves that no provider started.
    #[must_use]
    pub fn evidence(&self, incarnation: &str) -> LaunchEvidence {
        let Some(records) = self.records() else {
            return LaunchEvidence::Unknown;
        };
        let created = matches!(
            records.first(),
            Some(Record::Created { shim_id, incarnation: inc, .. })
                if shim_id == &self.shim_id && inc == incarnation
        );
        let spawned = records.contains(&Record::SpawnAttempted);
        let fenced = records.contains(&Record::Fenced);
        if created && !spawned && fenced {
            LaunchEvidence::NeverStarted
        } else {
            LaunchEvidence::Unknown
        }
    }

    /// Takes the launch lock without waiting. `None` while another process
    /// holds it.
    pub fn try_lock(&self) -> io::Result<Option<LaunchLock>> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(self.lock())?;
        match rustix::fs::flock(&file, rustix::fs::FlockOperation::NonBlockingLockExclusive) {
            Ok(()) => Ok(Some(LaunchLock(file))),
            Err(rustix::io::Errno::WOULDBLOCK) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Recovery's fence of a launch: with the lock free, append `fenced`
    /// while holding it, then classify (ADR-005 §3.2 step 1).
    pub fn fence(&self, incarnation: &str) -> io::Result<Fence> {
        let Some(lock) = self.try_lock()? else {
            return Ok(Fence::Held);
        };
        // A missing journal stays missing: the evidence is `unknown`.
        if self.journal().exists() {
            self.append(&Record::Fenced)?;
        }
        let evidence = self.evidence(incarnation);
        drop(lock);
        Ok(Fence::Fenced(evidence))
    }

    /// Publishes the attach key atomically (written, synced, renamed, the
    /// directory synced).
    pub fn write_key(&self, key: &AttachKey) -> io::Result<()> {
        publish(&self.dir, &self.key(), key.expose())
    }

    /// The attach key, if the key file is intact.
    #[must_use]
    pub fn read_key(&self) -> Option<AttachKey> {
        let bytes = std::fs::read(self.key()).ok()?;
        (bytes.len() == KEY_BYTES)
            .then(|| AttachKey::from_bytes(&bytes))
            .flatten()
    }

    /// Publishes the exit state atomically to `<shim_id>.exit` (ADR-005
    /// §4.5).
    pub fn write_exit(&self, exit: &ExitState) -> io::Result<()> {
        let bytes = serde_json::to_vec(exit).map_err(io::Error::other)?;
        publish(&self.dir, &self.exit(), &bytes)
    }

    /// The published exit state, if the exit file is intact.
    #[must_use]
    pub fn read_exit(&self) -> Option<ExitState> {
        let bytes = std::fs::read(self.exit()).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    /// Removes the files of an ended shim after `ack_exit`: the key and the
    /// lock. The journal and the exit file stay as evidence for recovery.
    pub fn remove_after_exit(&self) {
        for path in [self.key(), self.lock()] {
            if let Err(e) = std::fs::remove_file(&path)
                && e.kind() != io::ErrorKind::NotFound
            {
                tracing::warn!(path = %path.display(), error = %e, "cannot remove a shim file");
            }
        }
    }
}

/// The launch lock; the shim holds it for its whole life.
#[derive(Debug)]
pub struct LaunchLock(#[allow(dead_code)] File);

fn sync_dir(dir: &Path) -> io::Result<()> {
    File::open(dir)?.sync_all()
}

/// Writes `bytes` to a temporary file (0600), syncs it, renames it to
/// `path` and syncs the directory.
fn publish(dir: &Path, path: &Path, bytes: &[u8]) -> io::Result<()> {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tmp = dir.join(format!(".{name}.tmp"));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        std::fs::rename(&tmp, path)?;
        sync_dir(dir)
    })();
    if result.is_err() {
        drop(std::fs::remove_file(&tmp));
    }
    result
}

/// Socket name limits of `sun_path` (ADR-005 §5.1).
#[cfg(target_os = "linux")]
pub const SUN_PATH_MAX: usize = 108;
#[cfg(not(target_os = "linux"))]
pub const SUN_PATH_MAX: usize = 104;

/// The socket path of a shim (ADR-005 §5.1): `$XDG_RUNTIME_DIR/fleetdeck/
/// <incarnation>/<shim_id>.s`, else `<station dir>/run/shims/<shim_id>.s`;
/// when the path is too long for `sun_path`, `${TMPDIR:-/tmp}/fd-<uid>-
/// <incarnation>/<shim_id>.s`. `None` when every candidate is too long
/// (`socket_path_too_long`). The full ULID is never shortened.
#[must_use]
pub fn socket_path(
    runtime_dir: Option<&Path>,
    station_dir: &Path,
    tmp_dir: &Path,
    incarnation: &str,
    shim_id: &str,
) -> Option<PathBuf> {
    let name = format!("{shim_id}.s");
    let first = match runtime_dir {
        Some(run) => run.join("fleetdeck").join(incarnation),
        None => station_dir.join("run").join("shims"),
    };
    let fallback = tmp_dir.join(format!("fd-{}-{incarnation}", own_uid()));
    [first, fallback]
        .into_iter()
        .map(|dir| dir.join(&name))
        .find(|path| path.as_os_str().len() < SUN_PATH_MAX)
}
