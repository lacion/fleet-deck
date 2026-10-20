//! `session.coordinator_renew` and `session.coordinator_revoke` (ADR-006
//! §6.1 and §7, DL-212): the owner's binding lifecycle commands from the
//! deck. Only a paired deck sends them; the station-local socket and every
//! coordinator operation are refused. The receipt store makes a duplicate
//! return its stored receipt, so each command applies once.
//!
//! - Revoke: the binding of the session under the grant becomes `revoked`
//!   (the ingress answers `401` from then on) and the session records
//!   `coordinator.ended { reason: "revoked" }`.
//! - Renew: only an idle-`expired` binding at `generation - 1` becomes
//!   `active` at `generation`; the session records `coordinator.bound`
//!   with the new generation, which the deck awaits before it commits the
//!   renewal.

use fleetdeck_proto::command::{CommandOrigin, CommandResult, CoordinatorSpec, SessionCommand};
use fleetdeck_proto::event::{ProviderKind, RunOrigin};
use fleetdeck_proto::{Event, RejectCode, SessionId};

use super::decide::{reject, result};
use crate::coordinator::{Binding, BindingState, Bindings};
use crate::store::shim_bindings::RowState;
use crate::store::{Caller, DecideError, Decision};

fn forbidden() -> DecideError {
    reject(
        RejectCode::Forbidden,
        "only the owner's paired deck can renew or revoke a coordinator binding (ADR-006 §7)",
    )
}

/// The caller check: the station-local socket never renews or revokes,
/// and never starts a session with a coordinator grant (§8.2 test 13).
///
/// # Errors
/// The `forbidden` rejection.
pub fn guard(command: &SessionCommand, caller: &Caller) -> Result<(), DecideError> {
    let lifecycle = matches!(
        command,
        SessionCommand::CoordinatorRenew { .. }
            | SessionCommand::CoordinatorRevoke { .. }
            | SessionCommand::CoordinatorSet { .. }
    );
    if lifecycle && !caller.is_peer() {
        return Err(forbidden());
    }
    let grant = match command {
        SessionCommand::Start { spec, .. } | SessionCommand::Import { spec, .. } => {
            spec.coordinator.is_some()
        }
        _ => false,
    };
    if grant && !caller.is_peer() {
        return Err(reject(
            RejectCode::InvalidCommand,
            "a station-local start cannot carry a coordinator grant (ADR-006 §4)",
        ));
    }
    Ok(())
}

fn owner(origin: CommandOrigin) -> Result<(), DecideError> {
    if origin == CommandOrigin::Board {
        Ok(())
    } else {
        Err(forbidden())
    }
}

/// # Errors
/// A stored rejection, or a database error.
pub fn revoke(
    d: &mut Decision<'_>,
    bindings: &Bindings,
    origin: CommandOrigin,
    session_id: &SessionId,
    grant_id: &str,
) -> Result<CommandResult, DecideError> {
    owner(origin)?;
    d.summary(session_id)?
        .ok_or_else(|| reject(RejectCode::UnknownSession, "no such session"))?;
    let binding = bindings.find(session_id, grant_id).ok_or_else(|| {
        reject(
            RejectCode::InvalidCommand,
            "the session has no coordinator binding under this grant",
        )
    })?;
    if let Some(done) = revoke_row(d, bindings, &binding, session_id)? {
        return Ok(done);
    }
    if binding.state != BindingState::Revoked {
        bindings.update(&binding.binding_id, |b| b.state = BindingState::Revoked);
        d.append(
            session_id,
            Event::CoordinatorEnded {
                binding_id: binding.binding_id.clone(),
                generation: binding.generation,
                reason: "revoked".to_owned(),
            }
            .into(),
        )?;
    }
    Ok(result(session_id, None))
}

/// The revoke of a binding with a `shim_bindings` row on the shim host
/// (DL-194 §2.3, §9.13). The durable row decides, not memory (memory may be
/// gated already). In order: the memory gate, the fence entry, the row end
/// and the event. `None`: the binding has no row or the station has no
/// fence, so the caller takes the direct path.
fn revoke_row(
    d: &mut Decision<'_>,
    bindings: &Bindings,
    binding: &Binding,
    session_id: &SessionId,
) -> Result<Option<CommandResult>, DecideError> {
    let Some(fence) = bindings.fence() else {
        return Ok(None);
    };
    let Some(digest) = bindings.digest_of(&binding.binding_id) else {
        return Ok(None);
    };
    let Some(row) = d.shim_binding_row(&digest)? else {
        return Ok(None);
    };
    if !matches!(row.state, RowState::Pending | RowState::Active) {
        // Ended or suspended already: no event, but memory is gated too, so
        // an ended grant never stays usable (DL-194).
        bindings.update(&binding.binding_id, |b| b.state = BindingState::Revoked);
        return Ok(Some(result(session_id, None)));
    }
    bindings.update(&binding.binding_id, |b| b.state = BindingState::Revoked);
    let entry = (row.shim_id.clone(), digest.clone());
    fence
        .add(std::slice::from_ref(&entry), "revoke")
        .map_err(|error| {
            tracing::error!(%error, "the revoke fence entry was not written");
            reject(
                RejectCode::InvalidCommand,
                "The revoke was not saved. The old token is blocked until Fleet Deck restarts. Try again.",
            )
        })?;
    fence.mark_settle(entry);
    if d.shim_binding_end(&digest, "revoked")?.is_none() {
        return Ok(Some(result(session_id, None)));
    }
    crate::fault::hit(
        crate::fault::FaultPoint::AfterBindingEnd,
        "session.coordinator_revoke",
    );
    d.append(
        session_id,
        Event::CoordinatorEnded {
            binding_id: binding.binding_id.clone(),
            generation: binding.generation,
            reason: "revoked".to_owned(),
        }
        .into(),
    )?;
    Ok(Some(result(session_id, None)))
}

/// # Errors
/// A stored rejection, or a database error.
pub fn renew(
    d: &mut Decision<'_>,
    bindings: &Bindings,
    origin: CommandOrigin,
    session_id: &SessionId,
    grant_id: &str,
    generation: u32,
) -> Result<CommandResult, DecideError> {
    owner(origin)?;
    d.summary(session_id)?
        .ok_or_else(|| reject(RejectCode::UnknownSession, "no such session"))?;
    let binding = bindings.find(session_id, grant_id).ok_or_else(|| {
        reject(
            RejectCode::InvalidCommand,
            "the session has no coordinator binding under this grant",
        )
    })?;
    // On the shim host the durable row decides: an ended or suspended row
    // never renews (DL-194).
    if bindings.fence().is_some()
        && let Some(digest) = bindings.digest_of(&binding.binding_id)
        && let Some(row) = d.shim_binding_row(&digest)?
        && !matches!(row.state, RowState::Pending | RowState::Active)
    {
        return Err(reject(RejectCode::InvalidState, "the binding has ended"));
    }
    let Some(native_id) = binding.native_id.clone() else {
        return Err(reject(
            RejectCode::InvalidState,
            "the binding is not bound yet",
        ));
    };
    if binding.state != BindingState::Expired
        || binding.generation.checked_add(1) != Some(generation)
    {
        return Err(reject(
            RejectCode::InvalidState,
            "only an idle-expired binding at the previous generation can renew",
        ));
    }
    bindings.update(&binding.binding_id, |b| {
        b.state = BindingState::Active;
        b.generation = generation;
    });
    d.append(
        session_id,
        Event::CoordinatorBound {
            binding_id: binding.binding_id.clone(),
            grant_id: grant_id.to_owned(),
            generation,
            provider_instance: binding.provider_instance.clone(),
            native_id,
        }
        .into(),
    )?;
    Ok(result(session_id, None))
}

/// The start check of a coordinator spec (ADR-006 §4, §5.4; DL-215). Only
/// an owner start from the paired deck may carry a grant: the
/// station-local socket gets `invalid_command` (§8.2 test 13), a
/// coordinator or mail start gets `forbidden`, an import gets
/// `invalid_command`. A provider without a verified credential handoff
/// gets `unsupported` with the capability reason (§8.2 test 12).
///
/// # Errors
/// [`DecideError::Reject`] for a refused spec.
pub(super) fn check_spec(
    origin: RunOrigin,
    provider: ProviderKind,
    spec: &CoordinatorSpec,
    import: bool,
    bindings: &crate::coordinator::Bindings,
) -> Result<(), DecideError> {
    check_structure(origin, spec, import)?;
    bindings
        .handoff(provider, None)
        .map_err(|reason| reject(RejectCode::Unsupported, reason))
}

/// The origin, import and structure checks of a coordinator spec, without the
/// handoff check. A worker spec (MCP P4a) keeps all of them.
///
/// # Errors
/// [`DecideError::Reject`] for a refused spec.
pub(super) fn check_structure(
    origin: RunOrigin,
    spec: &CoordinatorSpec,
    import: bool,
) -> Result<(), DecideError> {
    match origin {
        RunOrigin::Board => {}
        RunOrigin::Local => {
            return Err(reject(
                RejectCode::InvalidCommand,
                "a station-local start cannot carry a coordinator grant (ADR-006 §4)",
            ));
        }
        _ => {
            return Err(reject(
                RejectCode::Forbidden,
                "only an owner start from the deck can carry a coordinator grant (ADR-006 §4)",
            ));
        }
    }
    if import {
        return Err(reject(
            RejectCode::InvalidCommand,
            "an import cannot carry a coordinator grant",
        ));
    }
    if spec.grant_id.trim().is_empty()
        || spec.classes.is_empty()
        || spec
            .classes
            .iter()
            .any(|c| crate::coordinator::ToolClass::parse(c).is_none())
        || spec.idle_ttl_ms == 0
        || spec.control_skill.version.trim().is_empty()
    {
        return Err(reject(
            RejectCode::InvalidCommand,
            "a coordinator spec needs a grant id, known classes, an idle TTL and a control skill version",
        ));
    }
    Ok(())
}
