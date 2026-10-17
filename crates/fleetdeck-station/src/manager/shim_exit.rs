//! The exit events of a shim that ended while the station was down
//! (ADR-005 §3.2 step 3, §4.5; review sa-9c1141b8 MAJOR 3; DL-122).
//!
//! Recovery read a valid exit file. The session's adapter is restored from
//! its committed checkpoint and transitions (no provider runs any more, so
//! nothing is written) and reports the exit, so open inputs, tools and
//! requests end the same way as after a live exit. Without a readable
//! checkpoint the session still gets `session.ended` with the reported
//! code; it is never called lost when the exit is known.

use fleetdeck_proto::Event;
use fleetdeck_proto::event::ProviderKind;
use fleetdeck_providers::checkpoint::AdapterCheckpoint;
use fleetdeck_providers::claude::ClaudeCore;
use fleetdeck_providers::codex::CodexCore;
use fleetdeck_providers::host::shim::wire::ExitState;

use super::durable::DurableCore;
use super::shim_station::Configs;
use crate::store::{EffectState, NewEvent, ShimRow, ShimTransition, Store, StoreError};

/// The provider's exit code from a `code:<n>` termination.
#[must_use]
pub fn exit_code(exit: &ExitState) -> Option<i32> {
    exit.termination
        .strip_prefix("code:")
        .and_then(|n| n.parse().ok())
}

fn core_exit<C: DurableCore>(
    config: Option<C::Config>,
    checkpoint: Option<&AdapterCheckpoint>,
    transitions: &[ShimTransition],
    code: Option<i32>,
) -> Option<Vec<NewEvent>> {
    let mut core = C::load(config?, checkpoint?).ok()?;
    for t in transitions {
        let unsent = t.frames > 0 && t.claimed == 0;
        if unsent || t.effect.state == EffectState::RetiredNotDispatched {
            continue;
        }
        drop(core.command(&t.effect));
    }
    Some(core.exit(code).events)
}

/// The events that end the session of the `exited` shim `row`.
///
/// # Errors
/// A store error.
pub async fn exit_events(
    store: &Store,
    row: &ShimRow,
    configs: &Configs,
    exit: &ExitState,
) -> Result<Vec<NewEvent>, StoreError> {
    let code = exit_code(exit);
    let summary = store.summary(row.session_id.clone()).await?;
    let checkpoint = store
        .shim_checkpoint(row.shim_id.clone())
        .await?
        .and_then(|text| AdapterCheckpoint::from_json(&text).ok());
    let transitions = store.shim_transitions(row.shim_id.clone()).await?;
    let events = summary.and_then(|summary| match summary.provider {
        Some(ProviderKind::Claude) => core_exit::<ClaudeCore>(
            (configs.claude)(&summary),
            checkpoint.as_ref(),
            &transitions,
            code,
        ),
        Some(ProviderKind::Codex) => core_exit::<CodexCore>(
            (configs.codex)(&summary),
            checkpoint.as_ref(),
            &transitions,
            code,
        ),
        _ => None,
    });
    let mut events = events.unwrap_or_default();
    let ended = events.iter().any(|e| {
        matches!(
            &e.event,
            fleetdeck_proto::EventBody::Known(Event::SessionEnded { .. })
        )
    });
    if !ended {
        events.push(
            Event::SessionEnded {
                reason: format!("exited ({})", exit.cause),
                exit_code: code,
            }
            .into(),
        );
    }
    Ok(events)
}
