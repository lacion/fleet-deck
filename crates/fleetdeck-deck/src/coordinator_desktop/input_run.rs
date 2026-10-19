//! The run of an accepted input operation (R13 P7 §7.6, §8.3, §10, §11, §12;
//! the pattern of [`super::record`]): the in-flight slot, the deck-local
//! recheck, the `attempted` step with the board sign-out, the one
//! `desktop.act`, the end of the operation, the audit rows and the final
//! delivery pass.
//!
//! The order before the send is fixed. The operation becomes `attempted` in
//! one transaction that also writes the exposure snapshot (the machine
//! addresses of the target) and deletes the browser sessions of those
//! addresses ([`DeckStore::desktop_op_attempted_input`]). After the commit the
//! deck clears the exposure cache, writes the audit rows of the deleted
//! sessions and revokes their sockets; when a relay does not end in time the
//! operation fails with `input_interrupted` and nothing is sent. The deck
//! never sends a second time: a result that is not known stays `unknown`.
//!
//! The mutation slot of the grant is held until the terminal bookkeeping is
//! done. No outer timeout wraps this code: after the `attempted` step the
//! handler is never dropped, and the one wait of the send is cut by what is
//! left of the handler deadline (40 s; the deck wait is 35 s).

use std::time::Instant;

use fleetdeck_proto::StationId;
use fleetdeck_proto::coordinator_desktop_text::code;
use fleetdeck_proto::desktop::DesktopAction;
use fleetdeck_proto::link::Outcome;

use super::audit::{self, Facts};
use super::authorize::{Authorized, Held, Tool, input_delivery_pass, local_pass};
use super::relay::{Ran, finish, waits};
use super::{input, reply_err, reply_err_op_text, reply_ok};
use crate::board_fence::{audit_signed_out, machine_addresses, revoke_before_dispatch};
use crate::link::DeckLink;
use crate::link::desktop_authority::send_coordinator_act;
use crate::store::AuditResult;
use crate::store::coordinator_desktop::{DesktopOp, OpOutcome};

/// The machine addresses of `target` for the exposure snapshot (§7.6): its
/// current link peer, the `host_addresses` of its desktop information, and
/// for the local station the loopback and interface addresses of the deck.
fn exposure_addresses(link: &DeckLink, target: &StationId) -> Vec<String> {
    let store = &link.store;
    let hosts = store
        .station_info(target)
        .and_then(|(info, _)| info.desktop)
        .map(|desktop| desktop.host_addresses)
        .unwrap_or_default();
    let local = store.local_station().as_ref() == Some(target);
    machine_addresses(store, store.link_peer(target), &hosts, local)
}

/// The window handle of an input action (R13 P7b §9.1: the five new
/// variants too).
fn window_of(action: &DesktopAction) -> Option<&str> {
    match action {
        DesktopAction::InputKeyPress { window_id, .. }
        | DesktopAction::InputTypeText { window_id, .. }
        | DesktopAction::InputPointerMove { window_id, .. }
        | DesktopAction::InputPointerClick { window_id, .. }
        | DesktopAction::InputAnyKeyPress { window_id, .. }
        | DesktopAction::InputAnyTypeText { window_id, .. }
        | DesktopAction::InputAnyPointerMove { window_id, .. }
        | DesktopAction::InputAnyPointerClick { window_id, .. } => Some(window_id),
        _ => None,
    }
}

/// The send of an accepted input operation and the end of its operation.
async fn execute(
    link: &DeckLink,
    auth: &Authorized,
    (id, tool): (&str, Tool),
    action: &DesktopAction,
    deadline: Instant,
) -> Ran {
    let ctx = auth.ctx();
    let target = auth.target();
    // The deck-local checks, right before the send.
    let held = Held {
        grant_id: &ctx.grant_id,
        binding_id: &ctx.binding_id,
        generation: ctx.generation,
        caller: &ctx.caller_station,
        target,
        class: auth.class(),
        tool: Some(tool),
    };
    if let Err(refusal) = local_pass(link, &held).await {
        return finish(link, id, OpOutcome::Failed(refusal)).await;
    }
    let control = auth.route().control();
    if control.ended().is_some() || !control.is_current() {
        return finish(link, id, OpOutcome::Failed(code::STATION_UNREACHABLE)).await;
    }
    // `attempted`, the exposure snapshot and the sign-out: one transaction.
    let addrs = exposure_addresses(link, target);
    let signed_out = match link
        .store
        .desktop_op_attempted_input(id, &addrs, link.store.now_ms())
        .await
    {
        Ok(Some(signed_out)) => signed_out,
        Ok(None) => return Ran::Stopped(code::AUTHORITY_UNKNOWN),
        Err(e) => {
            tracing::warn!(error = %e, "coordinator input attempted");
            return Ran::Stopped(code::AUTHORITY_UNKNOWN);
        }
    };
    link.store.invalidate_exposure();
    audit_signed_out(&link.store, &signed_out, id).await;
    if !revoke_before_dispatch(&link.store, &signed_out, &addrs, id).await {
        return finish(link, id, OpOutcome::Failed(code::INPUT_INTERRUPTED)).await;
    }
    let sent =
        send_coordinator_act(&link.store, auth.route(), action, ctx, id, waits(deadline)).await;
    finish(link, id, input::outcome_of(sent)).await
}

/// Runs the accepted input operation `op` and answers: an empty result for
/// `done`; the code and the operation ID for a failure; `desktop_timeout`
/// with the fixed explanatory text for an unknown result (the texts of the
/// five P7b variants are the broader ones, R13 P7b §13). The outcome audit
/// row is written once, by the call that made the first transition, and the
/// mutation slot is held until it is written.
pub(super) async fn run_input(
    link: &DeckLink,
    auth: &Authorized,
    op: &DesktopOp,
    action: &DesktopAction,
    deadline: Instant,
) -> Outcome {
    let id = op.operation_id.as_str();
    let any = input::is_any(op.tool, auth.grant().input_scope);
    // The in-flight slots; released on every path (a drop guard), after the
    // terminal bookkeeping.
    let slots = link
        .coordinator_desktop
        .slots()
        .take_call(&auth.ctx().grant_id);
    let ran = if slots.is_some() {
        execute(link, auth, (id, Tool::of_op(op.tool)), action, deadline).await
    } else {
        finish(link, id, OpOutcome::Failed(code::BUSY)).await
    };
    let (outcome, first) = match ran {
        Ran::Stopped(stop) => return reply_err(stop, Some(id)),
        Ran::Ended { outcome, first } => (outcome, first),
    };
    let (result, failure) = match &outcome {
        OpOutcome::Done(_) => (AuditResult::Ok, None),
        OpOutcome::Failed(c) => (AuditResult::Failed, Some(*c)),
        OpOutcome::Unknown => (AuditResult::Failed, Some(code::DESKTOP_TIMEOUT)),
    };
    let audit_action = input::audit_action(op.tool);
    if first {
        // The handle is in the outcome row of a `done` operation only.
        let facts = Facts {
            action: audit_action,
            ctx: auth.ctx(),
            target: auth.target(),
            operation_id: Some(id),
            window_id: window_of(action).filter(|_| matches!(outcome, OpOutcome::Done(_))),
            monitor: None,
        };
        let row = facts.row_with(
            audit::OUTCOME,
            result,
            failure,
            None,
            &input::outcome_extras(op),
        );
        audit::write_or_log(&link.store, row).await;
    }
    // The final authority check before a result is delivered (DL-242): the
    // durable result stays, the caller gets `authority_lost` or
    // `authority_unknown`.
    if matches!(outcome, OpOutcome::Done(_))
        && let Err(refusal) =
            input_delivery_pass(link, auth.ctx(), auth.target(), Tool::of_op(op.tool)).await
    {
        let lost = if refusal == code::AUTHORITY_UNKNOWN {
            code::AUTHORITY_UNKNOWN
        } else {
            code::AUTHORITY_LOST
        };
        let facts = Facts {
            action: audit_action,
            ctx: auth.ctx(),
            target: auth.target(),
            operation_id: Some(id),
            window_id: None,
            monitor: None,
        };
        let mut extra = input::outcome_extras(op);
        extra.push(("stage", "delivery".into()));
        let row = facts.row_with(
            audit::REFUSED,
            AuditResult::Refused,
            Some(lost),
            None,
            &extra,
        );
        audit::write_or_log(&link.store, row).await;
        drop(slots);
        return reply_err(lost, Some(id));
    }
    drop(slots);
    match outcome {
        OpOutcome::Done(value) => reply_ok(value),
        OpOutcome::Unknown => {
            reply_err_op_text(code::DESKTOP_TIMEOUT, input::unknown_text(any), id)
        }
        OpOutcome::Failed(c) => reply_err_op_text(c, input::message(c, any), id),
    }
}
