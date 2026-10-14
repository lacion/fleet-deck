//! The two launch steps of [`ShimHost`] (ADR-005 §3 "Launch"; slice 4,
//! DL-104), so that the station commits its `starting` row between them:
//!
//! 1. [`ShimHost::prepare`] picks the shim id and the socket path and
//!    durably writes the launch journal (`created`) and the key file. It
//!    starts no process.
//! 2. The station commits the `starting` row of [`Prepared::record`].
//! 3. [`ShimHost::start`] starts `fleetdeck shim`, which takes the launch
//!    lock, binds the socket, appends `spawn_attempted`, spawns the
//!    provider and reports `ready`. The station then commits the identity
//!    and generation 1 before it attaches.
//!
//! A crash at any point leaves evidence that recovery classifies (§3.2
//! step 1); a failed [`ShimHost::start`] is settled the same way.

use std::fmt;
use std::io;
use std::path::PathBuf;
use std::time::Duration;

use super::client;
use super::host::{ShimHost, ShimRecord};
use super::identity::{BootIdReader, real_boot_id_reader};
use super::journal::{self, ShimFiles, prepare_dir};
use super::launch::{EXIT_PUBLISH_AFTER_MS, EXIT_RETRY_MS, LaunchReply, LaunchSpec};
use super::output::DEFAULT_CAPACITY;
use super::proof::AttachKey;
use crate::host::{HostError, ProcessSpec};

/// Where and how a [`ShimHost`] launches shims.
#[derive(Clone)]
pub struct ShimHostConfig {
    /// The `fleetdeck` binary (it has the hidden `shim` subcommand).
    pub exe: PathBuf,
    /// The persistent station directory; shim files go to `<dir>/shims`.
    pub station_dir: PathBuf,
    /// `$XDG_RUNTIME_DIR`, if set (ADR-005 §5.1).
    pub runtime_dir: Option<PathBuf>,
    /// `${TMPDIR:-/tmp}` for the short-path fallback.
    pub tmp_dir: PathBuf,
    /// The station database incarnation.
    pub incarnation: String,
    /// The shim output buffer (ADR-005 §4.1).
    pub buffer_bytes: usize,
    /// The bound of each launch, connect and attach step.
    pub wait: Duration,
    /// How the station reads the boot id (DL-182 §4.1). The real reader
    /// unless a test installs another.
    pub boot_id_reader: BootIdReader,
}

impl fmt::Debug for ShimHostConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ShimHostConfig")
            .field("exe", &self.exe)
            .field("station_dir", &self.station_dir)
            .field("runtime_dir", &self.runtime_dir)
            .field("tmp_dir", &self.tmp_dir)
            .field("incarnation", &self.incarnation)
            .field("buffer_bytes", &self.buffer_bytes)
            .field("wait", &self.wait)
            .finish_non_exhaustive()
    }
}

impl ShimHostConfig {
    #[must_use]
    pub fn new(exe: PathBuf, station_dir: PathBuf, incarnation: String) -> Self {
        Self {
            exe,
            station_dir,
            runtime_dir: std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from),
            tmp_dir: std::env::temp_dir(),
            incarnation,
            buffer_bytes: DEFAULT_CAPACITY,
            wait: Duration::from_secs(10),
            boot_id_reader: real_boot_id_reader(),
        }
    }

    /// The boot id as the station reads it now.
    ///
    /// # Errors
    /// The reader fails (an unreadable boot id fails closed).
    pub fn boot_id(&self) -> io::Result<String> {
        (self.boot_id_reader)()
    }

    #[must_use]
    pub fn shims_dir(&self) -> PathBuf {
        self.station_dir.join("shims")
    }
}

/// A shim whose journal and key exist and whose process has not started.
pub struct Prepared {
    /// The row to commit: no provider identity, generation 0, cursor 0.
    pub record: ShimRecord,
    /// The boot id that the launch journal records (DL-182 §4.1). The
    /// station stores it on the `starting` row.
    pub launch_boot_id: String,
    spec: LaunchSpec,
    key: AttachKey,
    program: String,
}

impl fmt::Debug for Prepared {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Prepared")
            .field("record", &self.record)
            .finish_non_exhaustive()
    }
}

impl ShimHost {
    /// Step 1: the shim id, the socket path, the journal and the key file.
    ///
    /// # Errors
    /// The shims directory, the journal or the key file cannot be written,
    /// or the socket path is too long.
    pub fn prepare(&self, spec: &ProcessSpec) -> Result<Prepared, HostError> {
        let c = self.config();
        let program = spec.program.display().to_string();
        let spawn_error = |source: io::Error| HostError::Spawn {
            program: program.clone(),
            source,
        };
        let shims = c.shims_dir();
        prepare_dir(&shims).map_err(spawn_error)?;
        let shim_id = ulid::Ulid::new().to_string();
        let files = ShimFiles::new(&shims, &shim_id);
        // The boot evidence comes before the shim and the provider exist
        // (DL-182 §4.1). Without it no launch starts (fail closed): the
        // row could never get a reboot proof.
        let launch_boot_id = c.boot_id().map_err(|e| {
            spawn_error(io::Error::other(format!("the boot id cannot be read: {e}")))
        })?;
        if launch_boot_id.is_empty() {
            return Err(spawn_error(io::Error::other(
                "the boot id cannot be read: it is empty",
            )));
        }
        files
            .create_journal(&c.incarnation, Some(&launch_boot_id))
            .map_err(spawn_error)?;
        let key = AttachKey::generate()
            .map_err(|_| spawn_error(io::Error::other("no random bytes for the attach key")))?;
        files.write_key(&key).map_err(spawn_error)?;
        let socket_path = journal::socket_path(
            c.runtime_dir.as_deref(),
            &c.station_dir,
            &c.tmp_dir,
            &c.incarnation,
            &shim_id,
        )
        .ok_or_else(|| spawn_error(io::Error::other("socket_path_too_long")))?;
        let launch = LaunchSpec {
            shim_id: shim_id.clone(),
            incarnation: c.incarnation.clone(),
            shims_dir: shims,
            socket_path: socket_path.clone(),
            buffer_bytes: c.buffer_bytes,
            exit_publish_after_ms: EXIT_PUBLISH_AFTER_MS,
            exit_retry_ms: EXIT_RETRY_MS,
            program: Vec::new(),
            args: Vec::new(),
            cwd: Vec::new(),
            env: Vec::new(),
        }
        .provider_from(spec);
        Ok(Prepared {
            launch_boot_id,
            record: ShimRecord {
                shim_id,
                socket_path,
                provider_identity: None,
                generation: 0,
                cursor: 0,
                next_write_seq: 1,
                shim_pid: None,
            },
            spec: launch,
            key,
            program,
        })
    }

    /// Step 3: starts the shim and waits for `ready`. The returned record
    /// holds the reported provider identity and generation 0; the caller
    /// commits both identity and the next generation before it attaches.
    ///
    /// # Errors
    /// The shim did not start or did not report `ready`. The provider may
    /// still have started: the caller settles the row by recovery.
    pub async fn start(&self, prepared: Prepared) -> Result<ShimRecord, HostError> {
        let c = self.config();
        let Prepared {
            mut record,
            spec,
            key,
            program,
            launch_boot_id: _,
        } = prepared;
        let spawn_error = |source: io::Error| HostError::Spawn {
            program: program.clone(),
            source,
        };
        let launched = client::launch(&c.exe, &spec, &key, c.wait)
            .await
            .map_err(|e| spawn_error(io::Error::other(e.to_string())))?;
        let identity = match launched.reply {
            LaunchReply::Ready { provider_identity } => provider_identity,
            other => return Err(spawn_error(io::Error::other(format!("{other:?}")))),
        };
        record.shim_pid = launched.child.id();
        // The shim outlives this handle: tokio never kills it on drop.
        drop(launched.child);
        record.provider_identity = Some(identity);
        Ok(record)
    }
}
