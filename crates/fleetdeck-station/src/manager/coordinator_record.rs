//! The grant record at a process start and at a renewal (MCP P4b §5): a
//! grant with an `attached` record belongs to the session, so every process
//! start binds it at a higher generation with a new credential, and the
//! owner's renewal moves the generation of the record.

use fleetdeck_proto::command::{CommandResult, SessionSpec};
use fleetdeck_proto::event::ProviderKind;
use fleetdeck_proto::{CommandId, Event, RejectCode, SessionId};
use fleetdeck_providers::coordinator::CoordinatorLaunch;

use crate::coordinator::{BindingState, Bindings};
use crate::manager::coordinator_start::{BridgeStart, RecordStart, Tools};
use crate::manager::decide::{reject, result};
use crate::manager::worker_grant::{WorkerSkip, tools_for_start};
use crate::store::session_grants::{GrantRecord, RecordState};
use crate::store::shim_bindings::RowState;
use crate::store::{DecideError, Decision, Store};

/// The reason of `coordinator.ended` for a record past its expiry.
const EXPIRED: &str = "expired";

/// The tools of one start: the host form, a skipped worker grant, and
/// whether the start is a record start.
pub(in crate::manager) struct StartTools {
    /// The bridge start of the shim host.
    pub bridge: Option<BridgeStart>,
    /// The handoff of the direct host.
    pub launch: Option<CoordinatorLaunch>,
    /// The worker grant that the launch skipped, if any.
    pub worker_skip: Option<WorkerSkip>,
    /// The start binds a grant with a record (a resume gets the grant tools).
    pub record_start: bool,
}

/// The tools of a start or a resume (DL-165, DL-194): [`plan`] first, then
/// the tools of the effective spec.
///
/// # Errors
/// The reason that the start fails with.
// The start needs the store, the bindings, the provider, the session, its
// spec, the host, the resume and the start command.
#[allow(clippy::too_many_arguments)]
pub(in crate::manager) async fn start_tools(
    store: &Store,
    bindings: &Bindings,
    provider: ProviderKind,
    session_id: &SessionId,
    spec: &SessionSpec,
    shim_host: bool,
    resume: Option<&str>,
    command_id: Option<CommandId>,
) -> Result<StartTools, String> {
    let planned = plan(store, session_id, spec, resume).await?;
    let (effective, record) = match &planned {
        Some((effective, record)) => (effective, Some(record)),
        None => (spec, None),
    };
    let (tools, worker_skip) = tools_for_start(
        bindings,
        provider,
        session_id,
        effective,
        shim_host,
        resume.is_some(),
        command_id,
        record,
    )?;
    let (bridge, launch) = match tools {
        Tools::Direct(launch) => (None, Some(launch)),
        Tools::Bridge(start) => (Some(start), None),
        Tools::None => (None, None),
    };
    Ok(StartTools {
        bridge,
        launch,
        worker_skip,
        record_start: record.is_some(),
    })
}

/// What the process start of a session does with the record of its grant
/// (MCP P4b §5).
///
/// - An `attached` record that is not past `spec.expires_at`, for a resume or
///   for a start of the same grant: the **record start**. It stores
///   `generation + 1` first and returns the spec of the record (with the
///   other facts of `spec`) and the [`RecordStart`] for the grant binding.
/// - An `attached` record past its expiry ends (`expired`, with its
///   `coordinator.ended`); the start goes on with the rules of today.
/// - Otherwise (no record, an `ended` record, another grant): `None`, the
///   rules of today.
///
/// # Errors
/// The reason that the start fails with: the record cannot be read or
/// written.
pub(in crate::manager) async fn plan(
    store: &Store,
    session_id: &SessionId,
    spec: &SessionSpec,
    resume: Option<&str>,
) -> Result<Option<(SessionSpec, RecordStart)>, String> {
    let unreadable = |error: crate::store::StoreError| {
        tracing::error!(%error, session = %session_id, "the grant record was not read or written at a start");
        format!("the grant record of the session cannot be read: {error}")
    };
    let Some(record) = store
        .session_grant(session_id.clone())
        .await
        .map_err(unreadable)?
        .filter(|r| r.state == RecordState::Attached)
    else {
        return Ok(None);
    };
    let Some(granted) = record.spec.clone() else {
        tracing::warn!(session = %session_id, "an attached grant record has no spec; the start uses the rules of a grant without a record");
        return Ok(None);
    };
    if granted.expires_at <= store.now_ms() {
        store
            .session_grant_end(session_id.clone(), EXPIRED.to_owned())
            .await
            .map_err(unreadable)?;
        return Ok(None);
    }
    let names_the_grant = spec
        .coordinator
        .as_deref()
        .is_some_and(|c| c.grant_id == record.grant_id);
    if resume.is_none() && !names_the_grant {
        return Ok(None);
    }
    let generation = record
        .generation
        .checked_add(1)
        .ok_or_else(|| "the grant generation is out of range".to_owned())?;
    store
        .session_grant_generation(session_id.clone(), generation)
        .await
        .map_err(unreadable)?;
    let mut effective = spec.clone();
    effective.coordinator = Some(Box::new(granted));
    Ok(Some((
        effective,
        RecordStart {
            generation,
            expected_native: resume.map(str::to_owned),
        },
    )))
}

/// The renewal of a grant with an `attached` record (MCP P4b §5). With
/// `p = generation` of the command, `g = max(p, record.generation + 1)`:
///
/// - A live binding of the grant that is `Active` or `Expired` gets
///   generation `g` and the state `Active` (memory, and the shim row), a
///   `coordinator.bound { g }` is appended, and the record takes `g`.
/// - Without a live binding (the process is gone) the record takes `g - 1`:
///   the next process start binds at `g`.
/// - A `Pending` binding is not bound yet and is refused.
///
/// # Errors
/// A stored rejection, or a database error.
pub(in crate::manager) fn renew(
    d: &mut Decision<'_>,
    bindings: &Bindings,
    session_id: &SessionId,
    record: &GrantRecord,
    generation: u32,
) -> Result<CommandResult, DecideError> {
    let g = record
        .generation
        .checked_add(1)
        .map_or(generation, |next| generation.max(next));
    let mut next = record.clone();
    next.updated_at = d.now();
    let live = bindings
        .find(session_id, &record.grant_id)
        .filter(|b| b.state != BindingState::Revoked);
    let digest = live
        .as_ref()
        .and_then(|b| bindings.digest_of(&b.binding_id));
    let row = match &digest {
        Some(digest) => d.shim_binding_row(digest)?,
        None => None,
    };
    // A binding whose shim row ended or is suspended is dead: the process is
    // gone (DL-194).
    let live = live.filter(|_| {
        row.as_ref()
            .is_none_or(|r| matches!(r.state, RowState::Pending | RowState::Active))
    });
    let Some(binding) = live else {
        next.generation = g.saturating_sub(1);
        d.session_grant_put(&next)?;
        return Ok(result(session_id, None));
    };
    let Some(native_id) = binding
        .native_id
        .clone()
        .filter(|_| matches!(binding.state, BindingState::Active | BindingState::Expired))
    else {
        return Err(reject(
            RejectCode::InvalidState,
            "the binding is not bound yet",
        ));
    };
    bindings.update(&binding.binding_id, |b| {
        b.state = BindingState::Active;
        b.generation = g;
    });
    if let (Some(digest), Some(row), Some(renewed)) =
        (&digest, &row, bindings.get(&binding.binding_id))
    {
        d.shim_binding_rewrite(digest, &renewed, row.expires_at)?;
    }
    d.append(
        session_id,
        Event::CoordinatorBound {
            binding_id: binding.binding_id.clone(),
            grant_id: record.grant_id.clone(),
            generation: g,
            provider_instance: binding.provider_instance.clone(),
            native_id,
        }
        .into(),
    )?;
    next.generation = g;
    d.session_grant_put(&next)?;
    Ok(result(session_id, None))
}

#[cfg(test)]
#[path = "coordinator_epochs_tests.rs"]
mod epochs_tests;

#[cfg(test)]
#[path = "coordinator_renew_tests.rs"]
mod renew_tests;
