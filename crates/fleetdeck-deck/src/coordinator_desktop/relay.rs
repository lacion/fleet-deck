//! The coordinator send of a desktop mutation (R13 P6 §6.2 steps 9 and 10,
//! §7.2, decision O12-9 item 8 and Q6, Q11): the in-flight slots, the
//! deck-local checks right before the send, `attempted`, the one
//! `desktop.act` with the coordinator authority, and the mapping of the
//! outcome to a final operation state.
//!
//! No outer timeout wraps this code: after `desktop_op_attempted` the handler
//! is never dropped, and the one wait of the send is cut by what is left of
//! the handler deadline. The operation always reaches a final state before the
//! reply. A `desktop.act` is never retried.

use std::time::{Duration, Instant};

use fleetdeck_proto::coordinator_desktop_text::{code, deadline_ms};
use fleetdeck_proto::desktop::{DesktopActResult, DesktopAction};
use serde_json::json;

use super::authorize::{Authorized, Held, local_pass};
use crate::desktop_relay::RelayError;
use crate::link::DeckLink;
use crate::link::desktop_authority::{CallError, Waits, map_wire, send_coordinator_act};
use crate::store::coordinator_desktop::OpOutcome;

/// What the send did.
#[derive(Debug, PartialEq)]
pub(super) enum Ran {
    /// The operation reached a final state (or its write failed, `first` is
    /// false then). `first` is true when this call made the first transition,
    /// so the outcome row is written once.
    Ended { outcome: OpOutcome, first: bool },
    /// Nothing was sent and the row was not changed (a store error before the
    /// send): the answer is this code.
    Stopped(&'static str),
}

/// Ends the operation and logs a failed write (the real outcome is answered
/// anyway; start recovery fixes a row that stays open).
pub(super) async fn finish(link: &DeckLink, operation_id: &str, outcome: OpOutcome) -> Ran {
    let first = match link
        .store
        .desktop_op_finish(operation_id, outcome.clone(), link.store.now_ms())
        .await
    {
        Ok(first) => first,
        Err(e) => {
            tracing::warn!(error = %e, "coordinator desktop finish");
            false
        }
    };
    Ran::Ended { outcome, first }
}

/// The wait of the send for `action`: the deck wait of §9 (95 s for a preview,
/// 35 s for the others), cut by what is left of the handler deadline.
pub(super) fn waits(deadline: Instant) -> Waits {
    let left = deadline.saturating_duration_since(Instant::now());
    Waits {
        long: left.min(Duration::from_millis(deadline_ms::PREVIEW_DECK_WAIT)),
        short: left.min(Duration::from_millis(deadline_ms::ACTION_DECK_WAIT)),
    }
}

/// The final state of a send (§11.2 codes, Q11): a deck wait that ran out or
/// a link that ended after the send is `unknown`; a station answer is `done`
/// or `failed`.
fn outcome_of(action: &DesktopAction, sent: Result<DesktopActResult, CallError>) -> OpOutcome {
    match sent {
        Ok(result) if matches!(action, DesktopAction::BrowserOpen { .. }) => {
            match result.window_id {
                Some(window_id) => OpOutcome::Done(json!({ "window_id": window_id })),
                None => OpOutcome::Failed(code::DESKTOP_FAILED),
            }
        }
        Ok(_) => OpOutcome::Done(json!({})),
        Err(CallError::Relay(RelayError::Timeout | RelayError::Unreachable)) => OpOutcome::Unknown,
        Err(CallError::Relay(e)) => OpOutcome::Failed(e.http().1),
        Err(CallError::Wire {
            code: wire,
            message,
        }) => OpOutcome::Failed(match wire.as_str() {
            "authority_lost" => code::AUTHORITY_LOST,
            "authority_unknown" => code::AUTHORITY_UNKNOWN,
            "desktop_window_not_found" => code::DESKTOP_WINDOW_NOT_FOUND,
            _ => map_wire(&wire, &message).http().1,
        }),
    }
}

/// Sends the accepted operation `operation_id` once and ends it.
pub(super) async fn execute(
    link: &DeckLink,
    auth: &Authorized,
    operation_id: &str,
    action: &DesktopAction,
    deadline: Instant,
) -> Ran {
    let ctx = auth.ctx();
    // The in-flight slots; they are released on every path (a drop guard).
    let Some(_slots) = link.coordinator_desktop.slots().take_call(&ctx.grant_id) else {
        return finish(link, operation_id, OpOutcome::Failed(code::BUSY)).await;
    };
    // The deck-local checks, right before the send.
    let held = Held {
        grant_id: &ctx.grant_id,
        binding_id: &ctx.binding_id,
        generation: ctx.generation,
        caller: &ctx.caller_station,
        target: auth.target(),
        class: auth.class(),
        tool: None,
    };
    if let Err(refusal) = local_pass(link, &held).await {
        return finish(link, operation_id, OpOutcome::Failed(refusal)).await;
    }
    let control = auth.route().control();
    if control.ended().is_some() || !control.is_current() {
        let gone = OpOutcome::Failed(code::STATION_UNREACHABLE);
        return finish(link, operation_id, gone).await;
    }
    match link
        .store
        .desktop_op_attempted(operation_id, link.store.now_ms())
        .await
    {
        Ok(true) => {}
        Ok(false) => return Ran::Stopped(code::AUTHORITY_UNKNOWN),
        Err(e) => {
            tracing::warn!(error = %e, "coordinator desktop attempted");
            return Ran::Stopped(code::AUTHORITY_UNKNOWN);
        }
    }
    let sent = send_coordinator_act(
        &link.store,
        auth.route(),
        action,
        ctx,
        operation_id,
        waits(deadline),
    )
    .await;
    finish(link, operation_id, outcome_of(action, sent)).await
}

#[cfg(test)]
#[path = "relay_tests.rs"]
mod tests;
