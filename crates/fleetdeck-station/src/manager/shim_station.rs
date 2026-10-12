//! Station start with shim sessions (ADR-005 §3.2, §4.3; DL-112).
//!
//! [`recover_station`] is the order of a station start when the station
//! hosts sessions under shims:
//!
//! 1. Every open `shims` row of this incarnation is recovered
//!    ([`super::shims::recover_shims`]): a running shim is reattached with a
//!    newly committed generation, and its claimed stdin writes are settled
//!    from the shim's records before anything else (§4.3).
//! 2. The durable driver of each reattached shim is restored from its
//!    committed adapter checkpoint and transitions, without spawn,
//!    `initialize`, `thread/start` or a native resume (§2.1). A checkpoint
//!    that cannot be restored ends the row `lost` with its reason; the shim
//!    is left alone, and no fresh adapter replaces it.
//! 3. Only then does the generic store recovery run, with the processes of
//!    the restored drivers as the live processes: every other process is
//!    dead, its remaining claims become `outcome_unknown`, its requests
//!    expire, and its session is `lost`. Commands are admitted after this
//!    step (`Store::recover` opens the decision path).

use std::sync::Arc;
use std::time::Duration;

use fleetdeck_proto::event::ProviderKind;
use fleetdeck_proto::{SessionId, SessionSummary};
use fleetdeck_providers::host::shim::host::{AckMode, Attached, ShimHost, ShimProcess};
use fleetdeck_providers::{claude, codex};

use super::durable::DriverError;
use super::shim_claude::DurableClaude;
use super::shim_codex::DurableCodex;
use super::shims::{Recovered, recover_shims};
use crate::store::{ProcessIncarnation, RecoveryReport, ShimState, Store, StoreError};

/// The reason of a reattached shim whose session has no live process row.
pub const REASON_NO_PROCESS: &str = "no_live_process";
/// The reason of a reattached shim of a session that the store no longer
/// knows or whose provider has no durable driver.
pub const REASON_NO_DRIVER: &str = "no_durable_driver";

/// A restored durable driver of either provider.
#[derive(Clone)]
pub enum Durable {
    Claude(Arc<DurableClaude>),
    Codex(Arc<DurableCodex>),
}

impl std::fmt::Debug for Durable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Claude(_) => "Durable::Claude",
            Self::Codex(_) => "Durable::Codex",
        })
    }
}

impl Durable {
    /// Applies one claimed effect through the driver.
    pub async fn apply(&self, effect: &crate::store::Effect) -> crate::store::Dispatch {
        match self {
            Self::Claude(d) => d.apply(effect).await,
            Self::Codex(d) => d.apply(effect).await,
        }
    }

    /// Writes the first input of a launch (the prompt of `start`) as
    /// adapter-generated frames: the provider's `session.started` already
    /// confirmed the start effect, so the input cannot be claimed under it.
    /// The frames are committed with the adapter state before they leave
    /// and settle like every generated frame after a restart (DL-113).
    ///
    /// # Errors
    /// The core refused the input, or a store error.
    pub async fn first_input(
        &self,
        effect: &crate::store::Effect,
    ) -> Result<(), super::durable::DriverError> {
        use super::durable::DurableCore as _;
        match self {
            Self::Claude(d) => d.internal(|c| c.command(effect)).await,
            Self::Codex(d) => d.internal(|c| c.command(effect)).await,
        }
    }

    /// The shim process of the driver.
    #[must_use]
    pub fn process(&self) -> Arc<ShimProcess> {
        match self {
            Self::Claude(d) => Arc::clone(d.process()),
            Self::Codex(d) => Arc::clone(d.process()),
        }
    }

    /// Stops the driver's output pump without touching the shim (tests and
    /// station shutdown).
    pub async fn abandon(&self) {
        match self {
            Self::Claude(d) => d.abandon().await,
            Self::Codex(d) => d.abandon().await,
        }
    }
}

/// Builds the core config of a restored session from its summary.
pub type ConfigOf<T> = Box<dyn Fn(&SessionSummary) -> Option<T> + Send + Sync>;

/// The adapter configs of restored sessions, built from their summaries;
/// `None` when the station has no durable driver for the session's
/// provider instance (the row then ends `lost`, [`REASON_NO_DRIVER`]).
pub struct Configs {
    pub claude: ConfigOf<claude::CoreConfig>,
    pub codex: ConfigOf<codex::CoreConfig>,
}

impl std::fmt::Debug for Configs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Configs")
    }
}

/// One restored session.
#[derive(Debug, Clone)]
pub struct Restored {
    pub session_id: SessionId,
    pub shim_id: String,
    pub process: ProcessIncarnation,
    pub driver: Durable,
}

/// The result of a station start with shim sessions.
#[derive(Debug)]
pub struct StationRecovery {
    pub restored: Vec<Restored>,
    /// Every recovered row that did not end with a restored driver: shim
    /// id and the outcome name or reason.
    pub ended: Vec<(String, String)>,
    pub report: RecoveryReport,
}

/// Runs steps 1–3 of the module documentation.
///
/// # Errors
/// A store error; the store then stays closed for commands.
pub async fn recover_station(
    store: &Store,
    host: &ShimHost,
    configs: &Configs,
    in_flight_wait: Duration,
) -> Result<StationRecovery, StoreError> {
    let mut restored = Vec::new();
    let mut ended = Vec::new();
    for (row, outcome) in recover_shims(store, host, AckMode::Manual, in_flight_wait).await? {
        let Recovered::Reattached(attached) = outcome else {
            if let Recovered::Exited(exit) = &outcome {
                // A valid exit report ends the durable session before the
                // generic recovery would call it lost (§3.2 step 3, §4.5).
                let events = super::shim_exit::exit_events(store, &row, configs, exit).await?;
                store
                    .shim_record_exit(row.shim_id.clone(), exit.final_offset, events)
                    .await?;
            }
            if matches!(outcome, Recovered::NeverStarted) {
                // The intact journal proves that no provider was spawned:
                // the start effect was not dispatched (§3.2 step 1). Every
                // other outcome leaves it to the generic recovery
                // (`outcome_unknown`).
                store
                    .shim_retire_start(
                        row.session_id.clone(),
                        super::shims::reason::SHIM_NEVER_STARTED,
                    )
                    .await?;
            }
            ended.push((row.shim_id.clone(), outcome.name().to_owned()));
            continue;
        };
        match restore_one(store, &row.session_id, attached, configs, in_flight_wait).await? {
            Ok((process, driver)) => restored.push(Restored {
                session_id: row.session_id.clone(),
                shim_id: row.shim_id.clone(),
                process,
                driver,
            }),
            Err(reason) => {
                store
                    .shim_end(row.shim_id.clone(), ShimState::Lost, reason.clone())
                    .await?;
                ended.push((row.shim_id.clone(), reason));
            }
        }
    }
    let live = restored.iter().map(|r| r.process.clone()).collect();
    let report = store.recover(live).await?;
    Ok(StationRecovery {
        restored,
        ended,
        report,
    })
}

/// Restores the driver of one reattached shim. `Ok(Err(reason))` leaves the
/// shim alone and ends its row `lost` with `reason` (the caller commits it).
async fn restore_one(
    store: &Store,
    session_id: &SessionId,
    attached: Attached,
    configs: &Configs,
    wait: Duration,
) -> Result<Result<(ProcessIncarnation, Durable), String>, StoreError> {
    let Some(process) = store.live_process(session_id.clone()).await? else {
        return Ok(Err(REASON_NO_PROCESS.to_owned()));
    };
    if store
        .shim_input_blocked(session_id.clone())
        .await?
        .as_deref()
        == Some(crate::store::shim_gaps::REASON_ADAPTER_STATE_GAP)
    {
        // The stored adapter state was invalidated by an output gap
        // (§4.2): no driver is restored from it.
        return Ok(Err(
            crate::store::shim_gaps::REASON_ADAPTER_STATE_GAP.to_owned()
        ));
    }
    let Some(summary) = store.summary(session_id.clone()).await? else {
        return Ok(Err(REASON_NO_DRIVER.to_owned()));
    };
    let driver = match summary.provider {
        Some(ProviderKind::Claude) => {
            let Some(config) = (configs.claude)(&summary) else {
                return Ok(Err(REASON_NO_DRIVER.to_owned()));
            };
            DurableClaude::restore(store, attached, config, wait)
                .await
                .map(Durable::Claude)
        }
        Some(ProviderKind::Codex) => {
            let Some(config) = (configs.codex)(&summary) else {
                return Ok(Err(REASON_NO_DRIVER.to_owned()));
            };
            DurableCodex::restore(store, attached, config, wait)
                .await
                .map(Durable::Codex)
        }
        _ => return Ok(Err(REASON_NO_DRIVER.to_owned())),
    };
    match driver {
        Ok(driver) => Ok(Ok((process, driver))),
        Err(DriverError::Store(e)) => Err(e),
        Err(DriverError::Checkpoint(reason)) => Ok(Err(format!(
            "{}: {reason}",
            fleetdeck_providers::checkpoint::REASON_CORRUPT
        ))),
        Err(other) => Ok(Err(other.to_string())),
    }
}
