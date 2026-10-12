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

use fleetdeck_proto::command::{CommandOrigin, CommandResult, SessionCommand};
use fleetdeck_proto::{Event, RejectCode, SessionId};

use super::decide::{reject, result};
use super::policy::Policy;
use crate::coordinator::BindingState;
use crate::store::{Caller, DecideError, Decision};

fn forbidden() -> DecideError {
    reject(
        RejectCode::Forbidden,
        "only the owner's paired deck can renew or revoke a coordinator binding (ADR-006 §7)",
    )
}

/// The caller check: the station-local socket never renews or revokes.
///
/// # Errors
/// The `forbidden` rejection.
pub fn guard(command: &SessionCommand, caller: &Caller) -> Result<(), DecideError> {
    let lifecycle = matches!(
        command,
        SessionCommand::CoordinatorRenew { .. } | SessionCommand::CoordinatorRevoke { .. }
    );
    if lifecycle && !caller.is_peer() {
        return Err(forbidden());
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
    policy: &Policy,
    origin: CommandOrigin,
    session_id: &SessionId,
    grant_id: &str,
) -> Result<CommandResult, DecideError> {
    owner(origin)?;
    d.summary(session_id)?
        .ok_or_else(|| reject(RejectCode::UnknownSession, "no such session"))?;
    let bindings = policy.coordinator_bindings();
    let binding = bindings.find(session_id, grant_id).ok_or_else(|| {
        reject(
            RejectCode::InvalidCommand,
            "the session has no coordinator binding under this grant",
        )
    })?;
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

/// # Errors
/// A stored rejection, or a database error.
pub fn renew(
    d: &mut Decision<'_>,
    policy: &Policy,
    origin: CommandOrigin,
    session_id: &SessionId,
    grant_id: &str,
    generation: u32,
) -> Result<CommandResult, DecideError> {
    owner(origin)?;
    d.summary(session_id)?
        .ok_or_else(|| reject(RejectCode::UnknownSession, "no such session"))?;
    let bindings = policy.coordinator_bindings();
    let binding = bindings.find(session_id, grant_id).ok_or_else(|| {
        reject(
            RejectCode::InvalidCommand,
            "the session has no coordinator binding under this grant",
        )
    })?;
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
