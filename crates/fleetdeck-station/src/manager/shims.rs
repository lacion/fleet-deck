//! Station start recovery of shim rows (ADR-005 §3.2 steps 1–5; slice 4,
//! DL-104). It runs before any command is admitted, `starting` rows first.
//!
//! - `starting`, lock free: recovery fences the launch. Only an intact
//!   journal without `spawn_attempted` gives `never_started`; any other
//!   evidence reads the exit file, else the row is `lost`
//!   (`execution_unknown`). Missing runtime files prove nothing.
//! - `starting`, lock held: a shim lives. Recovery commits the next
//!   generation, attaches (waiting at most [`STARTING_WAIT`] for the
//!   socket) and commits the first reported identity before input.
//! - `running`: recovery commits the next generation, then attaches with
//!   the stored identity. A socket that accepts no connection means the
//!   shim is gone: a valid exit file reports the exit, otherwise the row is
//!   `lost` (`execution_unknown`); the stale socket file goes only after
//!   that check. A refused attach is `attach_refused` and the shim is left
//!   alone; an incompatible shim is `shim_incompatible`.

use std::path::Path;
use std::time::Duration;

use fleetdeck_proto::SessionId;
use fleetdeck_proto::event::ProviderKind;
use fleetdeck_providers::checkpoint;
use fleetdeck_providers::host::shim::attach::AttachFailure;
use fleetdeck_providers::host::shim::host::{AckMode, Attached, ShimHost};
use fleetdeck_providers::host::shim::journal::{Fence, LaunchEvidence, ShimFiles};
use fleetdeck_providers::host::shim::wire::{ExitState, Refusal};
use fleetdeck_providers::host::{HostError, ProcessSpec};

use crate::store::{NewShim, Result, ShimRow, ShimState, Store, StoreError};

/// How long recovery waits for the socket of a live `starting` shim.
pub const STARTING_WAIT: Duration = Duration::from_secs(30);
/// How long recovery asks about a stdin write in flight (ADR-005 §4.3).
pub const IN_FLIGHT_WAIT: Duration =
    Duration::from_millis(fleetdeck_providers::host::shim::settle::IN_FLIGHT_WAIT_MS);

/// Why recovery ended a row as `lost`.
/// The `exited` row reason of a shim exit report (ADR-005 §4.6; DL-119,
/// DL-120): [`reason::EXITED`] only when no member of the provider's
/// process group or session outlived the exit. A member that was still
/// alive, or a missing report, is not a proven end.
#[must_use]
pub fn exit_reason(exit: Option<&ExitState>) -> &'static str {
    match exit {
        Some(exit) if exit.descendants_remaining == 0 => reason::EXITED,
        Some(_) => reason::EXITED_DESCENDANTS_REMAINING,
        None => reason::EXIT_NOT_REPORTED,
    }
}

pub mod reason {
    /// No evidence proves that the provider stopped (ADR-005 §4.6).
    pub const EXECUTION_UNKNOWN: &str = "execution_unknown";
    /// The shim refused this station (`bad_proof`, `foreign_station`,
    /// `stale_owner`) or the socket belongs to another shim.
    pub const ATTACH_REFUSED: &str = "attach_refused";
    /// The shim speaks no protocol version of this build (§5.3).
    pub const SHIM_INCOMPATIBLE: &str = "shim_incompatible";
    /// The shim reports another provider process (§3.1).
    pub const IDENTITY_MISMATCH: &str = "identity_mismatch";
    /// An intact journal proves that no provider started (§3.2 step 1).
    pub const SHIM_NEVER_STARTED: &str = "shim_never_started";
    /// The provider exited while the station was down.
    pub const EXITED: &str = "exited";
    /// The provider exited, and members of its process group or session
    /// were still alive (the end is not proven, DL-119).
    pub const EXITED_DESCENDANTS_REMAINING: &str = "exited_descendants_remaining";
    /// The session ended, and no exit report of the shim arrived (the end
    /// is not proven, DL-120).
    pub const EXIT_NOT_REPORTED: &str = "exit_not_reported";
}

/// What recovery did with one row.
#[derive(Debug)]
pub enum Recovered {
    /// Attached with a newly committed generation; the replay starts at the
    /// committed cursor.
    Reattached(Attached),
    /// The shim is gone and its exit file reports the exit.
    Exited(ExitState),
    /// No provider ever started.
    NeverStarted,
    /// The row is `lost` with this reason.
    Lost(&'static str),
}

impl Recovered {
    /// A short name for logs and tests.
    #[must_use]
    pub fn name(&self) -> &'static str {
        match self {
            Self::Reattached(_) => "reattached",
            Self::Exited(_) => "exited",
            Self::NeverStarted => reason::SHIM_NEVER_STARTED,
            Self::Lost(why) => why,
        }
    }
}

/// Why [`launch_shim`] has no attachment.
#[derive(Debug, thiserror::Error)]
pub enum LaunchError {
    /// The journal, the key or the socket path failed before any process
    /// started; no row exists.
    #[error("the shim was not prepared: {0}")]
    Prepare(HostError),
    #[error(transparent)]
    Store(#[from] StoreError),
    /// The launch failed and recovery settled the row with this outcome.
    #[error("the shim launch failed ({error}); the row is {settled}")]
    Settled {
        error: String,
        settled: &'static str,
    },
}

/// Launches a shim for `session_id` with the commits of ADR-005 §3: the
/// `starting` row before the shim starts, the reported identity, then
/// generation 1 before the attach. A failed start is settled at once by
/// [`recover_shim`], so no evidence is guessed.
///
/// # Errors
/// [`LaunchError`].
pub async fn launch_shim(
    store: &Store,
    host: &ShimHost,
    session_id: &SessionId,
    spec: &ProcessSpec,
    mode: AckMode,
) -> std::result::Result<Attached, LaunchError> {
    let prepared = host.prepare(spec).map_err(LaunchError::Prepare)?;
    let shim_id = prepared.record.shim_id.clone();
    store
        .shim_starting(NewShim {
            shim_id: shim_id.clone(),
            session_id: session_id.clone(),
            socket_path: prepared.record.socket_path.clone(),
        })
        .await?;
    let settle = async |error: String| -> std::result::Result<Attached, LaunchError> {
        let Some(row) = store.shim(shim_id.clone()).await? else {
            return Err(LaunchError::Settled {
                error,
                settled: reason::EXECUTION_UNKNOWN,
            });
        };
        match recover_shim(store, host, &row, mode, IN_FLIGHT_WAIT).await? {
            Recovered::Reattached(attached) => Ok(attached),
            other => Err(LaunchError::Settled {
                error,
                settled: other.name(),
            }),
        }
    };
    let record = match host.start(prepared).await {
        Ok(record) => record,
        Err(e) => return settle(e.to_string()).await,
    };
    let Some(identity) = record.provider_identity.clone() else {
        return settle("no provider identity".to_owned()).await;
    };
    if !store.shim_running(shim_id.clone(), identity).await? {
        return settle("the identity did not commit".to_owned()).await;
    }
    let Some(generation) = store.shim_next_generation(shim_id.clone()).await? else {
        return settle("the row ended".to_owned()).await;
    };
    match host.attach(&record, generation, mode, false).await {
        Ok(attached) => Ok(attached),
        Err(e) => settle(e.to_string()).await,
    }
}

/// Settles one open row and commits its new state.
///
/// # Errors
/// A database error; the row then stays open for the next start.
pub async fn recover_shim(
    store: &Store,
    host: &ShimHost,
    row: &ShimRow,
    mode: AckMode,
    in_flight_wait: Duration,
) -> Result<Recovered> {
    let files = ShimFiles::new(&host.config().shims_dir(), &row.shim_id);
    let outcome = match row.state {
        ShimState::Starting => match files.fence(&row.incarnation) {
            Ok(Fence::Held) => attach(store, host, row, mode, &files, true).await?,
            Ok(Fence::Fenced(LaunchEvidence::NeverStarted)) => Recovered::NeverStarted,
            // An i/o error while fencing is missing evidence (§4.6).
            Ok(Fence::Fenced(LaunchEvidence::Unknown)) | Err(_) => gone(&files, &row.socket_path),
        },
        ShimState::Running => attach(store, host, row, mode, &files, false).await?,
        ShimState::Exited | ShimState::NeverStarted | ShimState::Lost => {
            return Ok(Recovered::Lost(reason::EXECUTION_UNKNOWN));
        }
    };
    let (state, why) = match &outcome {
        Recovered::Reattached(attached) => {
            // §4.3 settlement order: the claimed stdin writes of this shim
            // settle before the generic recovery sees them.
            super::shim_stdin::settle_writes(
                store,
                &attached.process,
                &row.shim_id,
                in_flight_wait,
            )
            .await?;
            // §2.1: a stored adapter checkpoint that this build cannot read
            // makes the session `not_resumable`; the station never
            // substitutes a fresh adapter, and the shim stays alive for a
            // compatible build (dropping the attachment only detaches).
            match unreadable_checkpoint(store, row).await? {
                None => return Ok(outcome),
                Some(why) => (ShimState::Lost, why),
            }
        }
        Recovered::Exited(exit) => (ShimState::Exited, exit_reason(Some(exit))),
        Recovered::NeverStarted => (ShimState::NeverStarted, reason::SHIM_NEVER_STARTED),
        Recovered::Lost(why) => (ShimState::Lost, *why),
    };
    store
        .shim_end(row.shim_id.clone(), state, why.to_owned())
        .await?;
    if matches!(outcome, Recovered::Reattached(_)) {
        return Ok(Recovered::Lost(why));
    }
    Ok(outcome)
}

/// The reason why the stored adapter checkpoint of `row` cannot be
/// restored, or `None` when it can (or none is stored yet: a shim whose
/// adapter never committed a transition has nothing to restore).
async fn unreadable_checkpoint(store: &Store, row: &ShimRow) -> Result<Option<&'static str>> {
    let Some(text) = store.shim_checkpoint(row.shim_id.clone()).await? else {
        return Ok(None);
    };
    let provider = store
        .summary(row.session_id.clone())
        .await?
        .and_then(|s| s.provider)
        .unwrap_or(ProviderKind::Unknown);
    Ok(checkpoint::readable(provider, &text)
        .err()
        .map(|e| e.reason()))
}

/// Settles every open row of this incarnation, `starting` rows first.
///
/// # Errors
/// A database error.
pub async fn recover_shims(
    store: &Store,
    host: &ShimHost,
    mode: AckMode,
    in_flight_wait: Duration,
) -> Result<Vec<(ShimRow, Recovered)>> {
    let mut done = Vec::new();
    for row in store.shims_to_recover().await? {
        let outcome = recover_shim(store, host, &row, mode, in_flight_wait).await?;
        done.push((row, outcome));
    }
    Ok(done)
}

/// The shim is gone (§3.2 step 3): a valid exit file reports the exit;
/// otherwise nothing proves that the provider stopped. The stale socket
/// file goes only after the exit file was read.
fn gone(files: &ShimFiles, socket: &Path) -> Recovered {
    let outcome = files.read_exit().map_or(
        Recovered::Lost(reason::EXECUTION_UNKNOWN),
        Recovered::Exited,
    );
    if std::fs::symlink_metadata(socket).is_ok_and(|m| !m.is_dir()) {
        let _ = std::fs::remove_file(socket);
    }
    outcome
}

async fn attach(
    store: &Store,
    host: &ShimHost,
    row: &ShimRow,
    mode: AckMode,
    files: &ShimFiles,
    starting: bool,
) -> Result<Recovered> {
    let deadline = tokio::time::Instant::now() + STARTING_WAIT;
    loop {
        // The generation commits before every attach attempt (§3 step 3).
        let Some(generation) = store.shim_next_generation(row.shim_id.clone()).await? else {
            return Ok(Recovered::Lost(reason::EXECUTION_UNKNOWN));
        };
        let failure = match host.attach(&row.record(), generation, mode, starting).await {
            Ok(attached) => {
                if starting {
                    let Some(identity) = attached.record.provider_identity.clone() else {
                        return Ok(Recovered::Lost(reason::IDENTITY_MISMATCH));
                    };
                    if !store.shim_running(row.shim_id.clone(), identity).await? {
                        return Ok(Recovered::Lost(reason::IDENTITY_MISMATCH));
                    }
                }
                return Ok(Recovered::Reattached(attached));
            }
            Err(failure) => failure,
        };
        // A live `starting` shim may not have bound its socket yet.
        if starting && failure.is_unreachable() && tokio::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(100)).await;
            continue;
        }
        return Ok(classify(&failure, files, &row.socket_path));
    }
}

fn classify(failure: &AttachFailure, files: &ShimFiles, socket: &Path) -> Recovered {
    match failure {
        AttachFailure::Unreachable(_) => gone(files, socket),
        AttachFailure::Incompatible => Recovered::Lost(reason::SHIM_INCOMPATIBLE),
        AttachFailure::IdentityMismatch => Recovered::Lost(reason::IDENTITY_MISMATCH),
        AttachFailure::WrongShim
        | AttachFailure::Refused(
            Refusal::BadProof | Refusal::ForeignStation | Refusal::StaleOwner,
        ) => Recovered::Lost(reason::ATTACH_REFUSED),
        AttachFailure::Refused(Refusal::Incompatible) => Recovered::Lost(reason::SHIM_INCOMPATIBLE),
        AttachFailure::NoKey
        | AttachFailure::Protocol(_)
        | AttachFailure::NoAttachedFrame
        | AttachFailure::Send(_) => Recovered::Lost(reason::EXECUTION_UNKNOWN),
    }
}
