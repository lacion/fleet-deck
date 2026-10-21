//! The station side of the default worker grant (MCP P4a §4, §6.1, §6.2).
//!
//! A start whose spec carries a `CoordinatorSpec` of the kind `worker` never
//! fails because of that grant. Where an owner grant would refuse the start,
//! the station drops the grant and the session starts without it:
//!
//! - at admission, a handoff refusal gives [`WorkerAdmission::Skip`] and the
//!   decision removes the grant from the spec;
//! - at launch, [`tools_for_start`] prepares the fleet-only tools instead of
//!   the grant (the same gate as a start that asked for no grant).
//!
//! Every skip is one durable station event `coordinator.worker_outcome`
//! (outcome `skipped`), so the deck can end the grant at once. A used worker
//! grant has no outcome event: its `coordinator.bound` shows the use.

use fleetdeck_proto::command::{CoordinatorSpec, GrantKind, SessionSpec};
use fleetdeck_proto::event::{ProviderKind, RunOrigin, WorkerOutcome};
use fleetdeck_proto::{CommandId, Event, SessionId};

use super::coordinator_command::check_structure;
use super::coordinator_start::{
    RecordStart, Tools, bridge_verified, prepare, prepare_bridge, tools_generic,
};
use crate::coordinator::Bindings;
use crate::store::{DecideError, NewEvent, Store};

/// The provider version has no verified handoff.
pub(super) const HANDOFF_UNVERIFIED: &str = "handoff_unverified";
/// The shim bridge is not verified or does not listen.
pub(super) const BRIDGE_UNVERIFIED: &str = "bridge_unverified";
/// A resumed start never gets a grant.
pub(super) const RESUMED: &str = "resumed";
/// Any other refusal of the launch preparation.
pub(super) const PREPARE_FAILED: &str = "prepare_failed";
/// The binding row of the shim host was not stored.
pub(super) const SHIM_ROW_FAILED: &str = "shim_row_failed";

/// The admission result of a worker spec.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum WorkerAdmission {
    /// The grant goes on to the launch.
    Keep,
    /// The start goes on without the grant.
    Skip { reason: &'static str },
}

/// One worker grant whose credential does not reach a provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WorkerSkip {
    pub grant_id: String,
    /// The start command that carried the grant; `None` when the effect has
    /// no command (no event is appended then).
    pub start_command_id: Option<CommandId>,
    pub reason: &'static str,
}

impl WorkerSkip {
    /// The station event of the skip. `None` (and a warning) without a start
    /// command id: the idle expiry of the deck ends such a grant.
    pub(crate) fn event(&self) -> Option<NewEvent> {
        let Some(start_command_id) = self.start_command_id.clone() else {
            tracing::warn!(
                grant = %self.grant_id,
                reason = self.reason,
                "a worker grant was skipped, but the start has no command id: no outcome event"
            );
            return None;
        };
        Some(NewEvent::from(Event::CoordinatorWorkerOutcome {
            grant_id: self.grant_id.clone(),
            start_command_id,
            outcome: WorkerOutcome::Skipped,
            reason: self.reason.to_owned(),
        }))
    }
}

/// The worker `CoordinatorSpec` of a start, if it has one.
fn worker_of(spec: &SessionSpec) -> Option<&CoordinatorSpec> {
    spec.coordinator
        .as_deref()
        .filter(|c| c.kind == GrantKind::Worker)
}

/// The admission check of a worker spec: every origin, import and structure
/// refusal of an owner spec stays; a handoff refusal is a skip.
///
/// # Errors
/// [`DecideError::Reject`] for a refused spec (not a handoff refusal).
pub(super) fn check_worker_spec(
    origin: RunOrigin,
    provider: ProviderKind,
    spec: &CoordinatorSpec,
    import: bool,
    bindings: &Bindings,
) -> Result<WorkerAdmission, DecideError> {
    check_structure(origin, spec, import)?;
    Ok(match bindings.handoff(provider, None) {
        Ok(()) => WorkerAdmission::Keep,
        Err(_) => WorkerAdmission::Skip {
            reason: HANDOFF_UNVERIFIED,
        },
    })
}

/// The admission of the coordinator spec of a start, if any. An owner spec
/// keeps the refusals of `check_spec`; a worker spec that skips is removed
/// from `spec` and returned as the skip to record.
///
/// # Errors
/// [`DecideError::Reject`] for a refused spec.
pub(super) fn admit(
    origin: RunOrigin,
    provider: ProviderKind,
    spec: &mut SessionSpec,
    import: bool,
    bindings: &Bindings,
    start_command_id: &CommandId,
) -> Result<Option<WorkerSkip>, DecideError> {
    let Some(coordinator) = spec.coordinator.as_deref() else {
        return Ok(None);
    };
    if coordinator.kind != GrantKind::Worker {
        super::coordinator_command::check_spec(origin, provider, coordinator, import, bindings)?;
        return Ok(None);
    }
    match check_worker_spec(origin, provider, coordinator, import, bindings)? {
        WorkerAdmission::Keep => Ok(None),
        WorkerAdmission::Skip { reason } => {
            let grant_id = coordinator.grant_id.clone();
            spec.coordinator = None;
            Ok(Some(WorkerSkip {
                grant_id,
                start_command_id: Some(start_command_id.clone()),
                reason,
            }))
        }
    }
}

/// [`tools_generic`] for a start whose spec may carry a worker grant. A
/// worker grant never fails the start: every refusal that would fail an
/// owner grant drops the grant and prepares the fleet-only tools (only when
/// the fleet handoff is verified, else none), and returns the skip to
/// record. `start_command_id` is the command of the start effect. `record`
/// is the start of a grant with a record (MCP P4b §5); its spec is an owner
/// grant, never a worker grant.
///
/// # Errors
/// The reason that an owner grant start fails with.
// The start needs the host, the session, its spec, the resume flag, the
// start command and the record.
#[allow(clippy::too_many_arguments)]
pub(super) fn tools_for_start(
    bindings: &Bindings,
    provider: ProviderKind,
    session_id: &SessionId,
    spec: &SessionSpec,
    shim_host: bool,
    resumed: bool,
    start_command_id: Option<CommandId>,
    record: Option<&RecordStart>,
) -> Result<(Tools, Option<WorkerSkip>), String> {
    let Some(worker) = worker_of(spec) else {
        return tools_generic(
            bindings, provider, session_id, spec, shim_host, resumed, record,
        )
        .map(|tools| (tools, None));
    };
    match grant_tools(
        bindings, provider, session_id, spec, worker, shim_host, resumed,
    ) {
        Ok(mut tools) => {
            if let Tools::Bridge(start) = &mut tools {
                start.grant_id = Some(worker.grant_id.clone());
                start.start_command_id = start_command_id;
            }
            Ok((tools, None))
        }
        Err(reason) => {
            tracing::info!(session = %session_id, reason, "the worker grant is skipped at launch");
            let mut fleet = spec.clone();
            fleet.coordinator = None;
            let tools = tools_generic(
                bindings, provider, session_id, &fleet, shim_host, resumed, None,
            )?;
            let skip = WorkerSkip {
                grant_id: worker.grant_id.clone(),
                start_command_id,
                reason,
            };
            Ok((tools, Some(skip)))
        }
    }
}

/// The grant tools of a worker start, or the reason that the grant is
/// skipped.
fn grant_tools(
    bindings: &Bindings,
    provider: ProviderKind,
    session_id: &SessionId,
    spec: &SessionSpec,
    worker: &CoordinatorSpec,
    shim_host: bool,
    resumed: bool,
) -> Result<Tools, &'static str> {
    if resumed {
        return Err(RESUMED);
    }
    if bindings.handoff(provider, None).is_err() {
        return Err(HANDOFF_UNVERIFIED);
    }
    if !shim_host {
        return prepare(bindings, provider, session_id, spec, worker, false, None)
            .map(Tools::Direct)
            .map_err(|_| PREPARE_FAILED);
    }
    if !bridge_verified(bindings, provider) || bindings.bridge().is_none() {
        return Err(BRIDGE_UNVERIFIED);
    }
    prepare_bridge(bindings, provider, session_id, spec, worker, None)
        .map(Tools::Bridge)
        .map_err(|_| PREPARE_FAILED)
}

/// Appends the outcome event of `skip`, if any, to the session. A failure
/// is logged: the deck's idle expiry then ends the grant.
pub(super) async fn record(store: &Store, session_id: &SessionId, skip: Option<&WorkerSkip>) {
    let Some(event) = skip.and_then(WorkerSkip::event) else {
        return;
    };
    if let Err(error) = store.append(session_id.clone(), vec![event]).await {
        tracing::error!(%error, session = %session_id, "the worker outcome event was not stored");
    }
}
