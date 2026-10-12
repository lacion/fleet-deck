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

use super::client;
use super::host::{ShimHost, ShimRecord};
use super::journal::{self, ShimFiles, prepare_dir};
use super::launch::{EXIT_PUBLISH_AFTER_MS, EXIT_RETRY_MS, LaunchReply, LaunchSpec};
use super::proof::AttachKey;
use crate::host::{HostError, ProcessSpec};

/// A shim whose journal and key exist and whose process has not started.
pub struct Prepared {
    /// The row to commit: no provider identity, generation 0, cursor 0.
    pub record: ShimRecord,
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
        files.create_journal(&c.incarnation).map_err(spawn_error)?;
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
            record: ShimRecord {
                shim_id,
                socket_path,
                provider_identity: None,
                generation: 0,
                cursor: 0,
                next_write_seq: 1,
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
        // The shim outlives this handle: tokio never kills it on drop.
        drop(launched.child);
        record.provider_identity = Some(identity);
        Ok(record)
    }
}
