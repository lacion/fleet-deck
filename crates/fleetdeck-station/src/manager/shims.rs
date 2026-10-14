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

use fleetdeck_proto::command::{CommandOrigin, CommandResult};
use fleetdeck_proto::event::ProviderKind;
use fleetdeck_proto::execution::{ExecutionCheck, ProcessInfo};
use fleetdeck_proto::{Event, RejectCode, SessionId};
use fleetdeck_providers::checkpoint;
use fleetdeck_providers::host::shim::attach::AttachFailure;
use fleetdeck_providers::host::shim::host::{AckMode, Attached, ShimHost};
use fleetdeck_providers::host::shim::identity::BootIdReader;
use fleetdeck_providers::host::shim::inspect::{self, Inspection, Proc};
use fleetdeck_providers::host::shim::journal::{Fence, LaunchEvidence, ShimFiles};
use fleetdeck_providers::host::shim::wire::{ExitState, Refusal};
use fleetdeck_providers::host::{HostError, ProcessSpec};

use crate::store::resumes::{describe, effective_reason, proves_end};
use crate::store::{
    DecideError, Decision, Dispatch, NewShim, Result, RunningFacts, ShimRow, ShimState, Store,
    StoreError,
};

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
///
/// DL-182 §1a: a zero count proves the end only when it came from the
/// complete inspection (`inspection >= 1`). An unavailable count (`None`)
/// and a zero count of an older shim are [`reason::EXIT_UNVERIFIED`]: not a
/// proof.
#[must_use]
pub fn exit_reason(exit: Option<&ExitState>) -> &'static str {
    match exit {
        Some(exit) => match exit.descendants_remaining {
            Some(0) if exit.inspection >= 1 => reason::EXITED,
            Some(n) if n > 0 => reason::EXITED_DESCENDANTS_REMAINING,
            _ => reason::EXIT_UNVERIFIED,
        },
        None => reason::EXIT_NOT_REPORTED,
    }
}

/// The inspection version of an exit report, 0 without one.
#[must_use]
pub fn inspection_of(exit: Option<&ExitState>) -> u32 {
    exit.map_or(0, |e| e.inspection)
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
    /// The shim reported the exit, but not by the complete inspection of
    /// DL-182 (an unavailable count, or a shim without it): the end is not
    /// proven.
    pub const EXIT_UNVERIFIED: &str = crate::store::resumes::EXIT_UNVERIFIED;
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
    /// The authority gate refused the launch after the `starting` row: no
    /// shim started and the row is `never_started`.
    #[error("the authority gate refused the launch")]
    Refused(Dispatch),
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
    launch_shim_for(store, host, session_id, spec, mode, None, &|| {
        Box::pin(std::future::ready(None))
    })
    .await
}

/// [`launch_shim`] for a resume or an import: `native_target` is the
/// native id that the new provider continues. The row holds it before the
/// spawn, so a crash after the spawn still names the conversation (DL-182
/// §3). The row also holds the boot id of the launch journal. `gate` (the
/// authority gate, ADR-008 §6.2) runs after the `starting` row and right
/// before the shim starts; a refusal ends the row `never_started` and is
/// [`LaunchError::Refused`].
///
/// # Errors
/// [`LaunchError`].
pub async fn launch_shim_for(
    store: &Store,
    host: &ShimHost,
    session_id: &SessionId,
    spec: &ProcessSpec,
    mode: AckMode,
    native_target: Option<&str>,
    gate: &super::shim_stdin::Gate<'_>,
) -> std::result::Result<Attached, LaunchError> {
    let prepared = host.prepare(spec).map_err(LaunchError::Prepare)?;
    let shim_id = prepared.record.shim_id.clone();
    store
        .shim_starting(NewShim {
            shim_id: shim_id.clone(),
            session_id: session_id.clone(),
            socket_path: prepared.record.socket_path.clone(),
            launch_boot_id: Some(prepared.launch_boot_id.clone()),
            native_target: native_target.map(str::to_owned),
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
    if let Some(refused) = gate().await {
        // No shim started: the launch journal proves it.
        store
            .shim_end(
                shim_id.clone(),
                ShimState::NeverStarted,
                reason::SHIM_NEVER_STARTED.to_owned(),
                0,
            )
            .await?;
        return Err(LaunchError::Refused(refused));
    }
    let record = match host.start(prepared).await {
        Ok(record) => record,
        Err(e) => return settle(e.to_string()).await,
    };
    let Some(identity) = record.provider_identity.clone() else {
        return settle("no provider identity".to_owned()).await;
    };
    let facts = RunningFacts {
        protocol: None,
        shim_pid: record.shim_pid,
    };
    if !store
        .shim_running_with(shim_id.clone(), identity, facts)
        .await?
    {
        return settle("the identity did not commit".to_owned()).await;
    }
    let Some(generation) = store.shim_next_generation(shim_id.clone()).await? else {
        return settle("the row ended".to_owned()).await;
    };
    match host.attach(&record, generation, mode, false).await {
        Ok(attached) => {
            store
                .shim_picked_protocol(shim_id.clone(), attached.protocol)
                .await?;
            Ok(attached)
        }
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
    let inspection = match &outcome {
        Recovered::Exited(exit) => inspection_of(Some(exit)),
        _ => 0,
    };
    store
        .shim_end(row.shim_id.clone(), state, why.to_owned(), inspection)
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
/// The bounded waits of different shims (the `starting` socket wait and the
/// §4.3 in-flight write waits) run concurrently, so they do not add up:
/// every `starting` row settles in one concurrent phase, then every
/// `running` row in a second one (ADR-005 §3.2, §4.3; DL-122). The result
/// keeps the order of [`Store::shims_to_recover`].
///
/// # Errors
/// A database error.
pub async fn recover_shims(
    store: &Store,
    host: &ShimHost,
    mode: AckMode,
    in_flight_wait: Duration,
) -> Result<Vec<(ShimRow, Recovered)>> {
    let rows = store.shims_to_recover().await?;
    let (starting, rest): (Vec<ShimRow>, Vec<ShimRow>) = rows
        .into_iter()
        .partition(|row| row.state == ShimState::Starting);
    let mut done = Vec::new();
    for phase in [starting, rest] {
        let outcomes = futures_util::future::join_all(
            phase
                .iter()
                .map(|row| recover_shim(store, host, row, mode, in_flight_wait)),
        )
        .await;
        for (row, outcome) in phase.into_iter().zip(outcomes) {
            done.push((row, outcome?));
        }
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
                    let facts = RunningFacts {
                        protocol: Some(attached.protocol),
                        shim_pid: None,
                    };
                    if !store
                        .shim_running_with(row.shim_id.clone(), identity, facts)
                        .await?
                    {
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

// ---------------------------------------------------------------------
// The termination evidence after a shim loss (DL-182, design
// `shim-resume.md` §4, ADR-005 §4.6): the verified reboot, the owner's
// execution check and the owner's reconciliation.
//
// - `prove_reboots` runs at every station start, after the shim rows were
//   recovered and before the first command. A row whose end is not proven
//   gets the proof `ended_by_reboot` next to its state when the current
//   boot id differs from the one stored with its execution. A missing,
//   empty or unreadable value gives no proof (fail closed). A row without a
//   provider identity is fenced first (ADR-005 §3.2 step 1) and then
//   compared by its launch boot id.
// - `revalidate` runs at the dispatch of a resume or an import. A reboot
//   proof holds only while the boot id still differs and no live process
//   has the stored identity: a restored VM memory brings the old boot id
//   back.
// - `check_row` is the owner's check (`session.execution_check`): the
//   processes of the previous execution on the same boot. It never kills
//   anything. `decide_reconcile` marks the run as ended; it refuses while
//   the check finds a live process or is incomplete.
// ---------------------------------------------------------------------

/// The text of an incomplete check on a platform without a reader.
pub const NO_INSPECTION: &str = "process inspection is not available on this platform";

/// The boot id of the execution that `row` ran in: its provider's, or the
/// launch journal's for a row without an identity. `None` for neither or an
/// empty value.
fn stored_boot(row: &ShimRow) -> Option<&str> {
    row.provider_identity
        .as_ref()
        .map(|i| i.boot_id.as_str())
        .or(row.launch_boot_id.as_deref())
        .filter(|id| !id.is_empty())
}

/// The current boot id; `None` when it cannot be read or is empty.
fn current_boot(boot: &BootIdReader) -> Option<String> {
    match boot() {
        Ok(id) if !id.is_empty() => Some(id),
        Ok(_) => None,
        Err(e) => {
            tracing::warn!(error = %e, "the boot id cannot be read");
            None
        }
    }
}

/// Records `ended_by_reboot` next to every unresolved row whose boot id
/// differs from the current one. Returns the number of rows proven.
///
/// # Errors
/// A store error.
pub async fn prove_reboots(store: &Store, host: &ShimHost) -> Result<usize> {
    let Some(current) = current_boot(&host.config().boot_id_reader) else {
        return Ok(0);
    };
    let incarnation = store.incarnation().as_str().to_owned();
    let mut proven = 0;
    for row in store.shims_unresolved(None).await? {
        if row.incarnation != incarnation {
            continue;
        }
        let Some(stored) = stored_boot(&row) else {
            continue;
        };
        if stored == current {
            continue;
        }
        if row.provider_identity.is_none() {
            // The launch is fenced before the reboot rule applies: a late
            // shim start of the old boot exits without a provider.
            let files = ShimFiles::new(&host.config().shims_dir(), &row.shim_id);
            match files.fence(&row.incarnation) {
                Ok(Fence::Fenced(_)) => {}
                Ok(Fence::Held) => continue,
                Err(e) => {
                    tracing::warn!(shim = %row.shim_id, error = %e, "cannot fence the launch");
                    continue;
                }
            }
        }
        if store
            .shim_prove_reboot(row.shim_id.clone(), current.clone())
            .await?
        {
            proven += 1;
        }
    }
    Ok(proven)
}

/// Checks the rows whose end rests on a reboot proof at the dispatch of a
/// resume or an import (design §4.1 "Revalidation at launch"). `Some(why)`
/// when one proof does not hold any more.
#[must_use]
pub fn revalidate(boot: &BootIdReader, rows: &[ShimRow]) -> Option<String> {
    if rows.is_empty() {
        return None;
    }
    let Some(current) = current_boot(boot) else {
        return Some("the boot id cannot be read".to_owned());
    };
    for row in rows {
        let Some(stored) = stored_boot(row) else {
            return Some(format!("{}: no boot evidence", describe(row)));
        };
        if stored == current {
            return Some(format!(
                "{}: the boot id is the one of the previous execution",
                describe(row)
            ));
        }
        if let Some(identity) = &row.provider_identity {
            match inspect::identity_alive(identity) {
                Some(false) => {}
                Some(true) => {
                    return Some(format!(
                        "{}: a process with the stored identity is alive",
                        describe(row)
                    ));
                }
                None => {
                    return Some(format!(
                        "{}: the process check is not available",
                        describe(row)
                    ));
                }
            }
        }
    }
    None
}

fn infos(list: &[Proc]) -> Vec<ProcessInfo> {
    list.iter()
        .map(|p| ProcessInfo {
            pid: p.pid,
            command: p.command.clone(),
        })
        .collect()
}

/// The owner's check of one shim row (see the module documentation).
#[must_use]
pub fn check_row(boot: &BootIdReader, row: &ShimRow) -> ExecutionCheck {
    let mut out = ExecutionCheck {
        shim_id: Some(row.shim_id.clone()),
        reason: format!("{} ({})", row.state.as_str(), effective_reason(row)),
        ..ExecutionCheck::nothing_unresolved()
    };
    out.complete = false;
    if !cfg!(target_os = "linux") {
        out.error = Some(NO_INSPECTION.to_owned());
        return out;
    }
    let Some(stored) = stored_boot(row) else {
        out.error = Some("no boot id was recorded for this run".to_owned());
        return out;
    };
    let Some(current) = current_boot(boot) else {
        out.error = Some("the boot id cannot be read".to_owned());
        return out;
    };
    if stored != current {
        // The execution ended with the old boot: the reboot proof covers it.
        out.same_boot = false;
        out.complete = true;
        return out;
    }
    out.same_boot = true;
    let Some(identity) = &row.provider_identity else {
        out.error = Some("no provider identity was recorded for this run".to_owned());
        return out;
    };
    let Inspection {
        identity_alive,
        group,
        session,
        escaped,
        complete,
        error,
    } = inspect::inspect(identity, row.shim_pid);
    out.identity_alive = identity_alive;
    out.group_members = infos(&group);
    out.session_members = infos(&session);
    out.escaped = infos(&escaped);
    out.complete = complete;
    out.error = error;
    out
}

/// The check that answers `session.execution_check`, for the unresolved
/// rows of a session (see `Store::shims_unresolved`).
#[must_use]
pub fn check_unresolved(boot: &BootIdReader, rows: &[ShimRow]) -> ExecutionCheck {
    // `rows` come oldest first: the newest unresolved row is checked.
    match rows.last() {
        Some(row) => check_row(boot, row),
        None => ExecutionCheck::nothing_unresolved(),
    }
}

/// The processes of `check` in words, for a refusal.
fn running(check: &ExecutionCheck) -> String {
    let mut parts = Vec::new();
    if check.identity_alive {
        parts.push("the provider process".to_owned());
    }
    for p in check.group_members.iter().chain(&check.session_members) {
        parts.push(format!("{} ({})", p.pid, p.command));
    }
    parts.join(", ")
}

/// Decides `session.reconcile_execution` (design §4.2). `check` is the
/// check that the manager ran for `shim_id` before the decision; the
/// decision rechecks only the row state. Refusals: `forbidden` (another
/// origin than the board or the local socket), `unknown_session`,
/// `invalid_command` (an unknown shim), `invalid_state` (running, already
/// proven, a live process or an incomplete check).
///
/// # Errors
/// A stored rejection, or a database error.
pub fn decide_reconcile(
    d: &mut Decision<'_>,
    origin: CommandOrigin,
    session_id: &SessionId,
    shim_id: &str,
    check: Option<&ExecutionCheck>,
) -> std::result::Result<CommandResult, DecideError> {
    if !matches!(origin, CommandOrigin::Board | CommandOrigin::Local) {
        return Err(super::decide::reject(
            RejectCode::Forbidden,
            "only the owner can mark a run as ended",
        ));
    }
    if d.summary(session_id)?.is_none() {
        return Err(super::decide::reject(
            RejectCode::UnknownSession,
            "no such session",
        ));
    }
    let row = d
        .shim_row(shim_id)?
        .filter(|row| &row.session_id == session_id)
        .ok_or_else(|| super::decide::reject(RejectCode::InvalidCommand, "unknown shim"))?;
    if matches!(row.state, ShimState::Starting | ShimState::Running) {
        return Err(super::decide::reject(
            RejectCode::InvalidState,
            "the shim is running",
        ));
    }
    if proves_end(&row) {
        return Err(super::decide::reject(
            RejectCode::InvalidState,
            "the previous run is already proven ended",
        ));
    }
    let Some(check) = check.filter(|c| c.shim_id.as_deref() == Some(shim_id)) else {
        return Err(super::decide::reject(
            RejectCode::InvalidState,
            "the process check is incomplete: it was not made",
        ));
    };
    if !check.complete {
        return Err(super::decide::reject(
            RejectCode::InvalidState,
            format!(
                "the process check is incomplete: {}",
                check.error.as_deref().unwrap_or("unknown reason")
            ),
        ));
    }
    if check.still_runs() {
        return Err(super::decide::reject(
            RejectCode::InvalidState,
            format!("the previous run still has processes: {}", running(check)),
        ));
    }
    if !d.set_owner_proof(shim_id)? {
        return Err(super::decide::reject(
            RejectCode::InvalidState,
            "the previous run is already proven ended",
        ));
    }
    d.append(
        session_id,
        Event::ExecutionReconciled {
            shim_id: shim_id.to_owned(),
        }
        .into(),
    )?;
    Ok(super::decide::result(session_id, None))
}
