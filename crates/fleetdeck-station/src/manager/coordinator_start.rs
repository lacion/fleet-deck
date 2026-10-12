//! The coordinator part of a start effect (ADR-006 §5.4, DL-216).
//!
//! The start effect of a session whose spec carries a grant mints the
//! credential, adds a `pending` binding (generation 0) under its digest
//! and gives the adapter the arguments and the credential. The station
//! checks the handoff again here: a provider without a verified handoff,
//! a missing ingress, a resume and a shim-hosted start fail the start
//! instead of running a coordinator session without its credential.

use fleetdeck_proto::command::{CoordinatorSpec, SessionSpec};
use fleetdeck_proto::event::ProviderKind;
use fleetdeck_proto::{Event, SessionId};
use fleetdeck_providers::coordinator::CoordinatorLaunch;

use crate::coordinator::{Binding, BindingState, Bindings, Handoff, ToolClass};
use crate::store::NewEvent;

/// The reason of `coordinator.ended` at the end of the process (§5.5).
pub(super) const REASON_PROCESS_EXIT: &str = "process_exit";

/// The binding facts of a stored event batch (DL-217): the native
/// identity of `session.started` binds each pending binding of the
/// session (`coordinator.bound`, generation 1), or revokes an active
/// binding whose root identity changed (`coordinator.ended`).
pub(super) fn bound_events(
    bindings: &Bindings,
    session_id: &SessionId,
    observed: &[Event],
) -> Vec<NewEvent> {
    let mut out = Vec::new();
    for event in observed {
        let Event::SessionStarted { provider_refs, .. } = event else {
            continue;
        };
        let Some(native) = provider_refs
            .get("session_id")
            .or_else(|| provider_refs.get("thread_id"))
            .and_then(serde_json::Value::as_str)
        else {
            continue;
        };
        for b in bindings.session_native(session_id, native) {
            out.push(NewEvent::from(match b.state {
                BindingState::Active => Event::CoordinatorBound {
                    binding_id: b.binding_id,
                    grant_id: b.grant_id,
                    generation: b.generation,
                    provider_instance: b.provider_instance,
                    native_id: native.to_owned(),
                },
                _ => Event::CoordinatorEnded {
                    binding_id: b.binding_id,
                    generation: b.generation,
                    reason: "revoked".to_owned(),
                },
            }));
        }
    }
    out
}

/// Revokes the bindings of a session whose process ended and returns
/// their `coordinator.ended` events (DL-217).
pub(super) fn ended_events(bindings: &Bindings, session_id: &SessionId) -> Vec<NewEvent> {
    bindings
        .end_session(session_id)
        .into_iter()
        .map(|b| {
            NewEvent::from(Event::CoordinatorEnded {
                binding_id: b.binding_id,
                generation: b.generation,
                reason: REASON_PROCESS_EXIT.to_owned(),
            })
        })
        .collect()
}

/// Prepares the handoff of one start and adds its pending binding.
///
/// # Errors
/// The reason that the start fails with.
pub(super) fn prepare(
    bindings: &Bindings,
    provider: ProviderKind,
    session_id: &SessionId,
    spec: &SessionSpec,
    coordinator: &CoordinatorSpec,
    resumed_or_durable: bool,
) -> Result<CoordinatorLaunch, String> {
    if resumed_or_durable {
        return Err("a coordinator session starts only as a new direct child".to_owned());
    }
    bindings.handoff(provider, None).map_err(str::to_owned)?;
    let url = bindings
        .endpoint()
        .ok_or_else(|| "the coordinator endpoint does not listen".to_owned())?;
    let classes = coordinator
        .classes
        .iter()
        .map(|c| ToolClass::parse(c).ok_or_else(|| format!("unknown grant class `{c}`")))
        .collect::<Result<_, _>>()?;
    let handoff = Handoff::prepare(provider, &url).map_err(|e| e.to_string())?;
    bindings.insert(
        handoff.digest.clone(),
        Binding {
            binding_id: format!("{session_id}:{}", coordinator.grant_id),
            grant_id: coordinator.grant_id.clone(),
            session_id: session_id.clone(),
            provider_instance: spec.provider_instance.to_string(),
            native_id: None,
            generation: 0,
            classes,
            state: BindingState::Pending,
            control_skill: coordinator.control_skill.text.clone(),
        },
    );
    Ok(handoff.into_launch())
}
