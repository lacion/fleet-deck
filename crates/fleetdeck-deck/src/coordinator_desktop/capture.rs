//! The coordinator `screenshot` after its admission (R13 P6 §4.6, §4.7, §6.2
//! steps 9 and 10, §7.2, §7.4, decisions O12-9 items 22 and 33, task T3).
//!
//! It reuses the P5a capture record (`desktop_captures`) with the capture ID
//! equal to the operation ID and the `coordinator` column set, the one
//! `desktop.act` with a coordinator authority, and the P5a indexing with a
//! coordinator provenance. The deck never sends a second screenshot: the
//! record is `attempted` before the send, and a result that is not known stays
//! for the recovery, which only asks the station for the status.
//!
//! No outer timeout wraps this code: after `desktop_op_attempted` the handler
//! is never dropped, and every wait is cut by what is left of the handler
//! deadline. The operation reaches a final state before the reply.

use std::time::{Duration, Instant};

use fleetdeck_link::desktop_authority::action_hash_for;
use fleetdeck_proto::StationId;
use fleetdeck_proto::capture::{self, CaptureResult};
use fleetdeck_proto::coordinator_desktop::CaptureState as State;
use fleetdeck_proto::coordinator_desktop_text::{code, deadline_ms, text};
use fleetdeck_proto::desktop::{DesktopActResult, DesktopAction};
use fleetdeck_proto::link::Outcome;

use super::audit::{self, Facts};
use super::authorize::{Authorized, Held, local_pass};
use super::capture_status::{PENDING_INDEXING, committed, plain, stored_result};
use super::{reply_err_capture, reply_ok};
use crate::desktop_capture::{CaptureError, audit_indexing_failed, provenance_of};
use crate::desktop_relay::RelayError;
use crate::link::DeckLink;
use crate::link::desktop_authority::{CallError, Waits, map_wire, send_coordinator_act};
use crate::store::AuditResult;
use crate::store::coordinator_desktop::{DesktopOp, OpOutcome};
use crate::store::desktop_captures::{Completed, Prepared, stamp_published};

/// What the screenshot call knows: the raw request ID (only for the reply,
/// never stored) and the validated monitor.
pub(super) struct Shot<'a> {
    pub request_id: &'a str,
    pub monitor: &'a str,
}

/// How the send ended: the final state with the text to answer when it is
/// not the fixed text of the code, or a stop before any change.
pub(super) enum End {
    Final {
        outcome: OpOutcome,
        /// True when this call made the first transition of the operation.
        first: bool,
        /// A capture text (§11.3) that replaces the text of the code.
        message: Option<&'static str>,
    },
    /// Nothing was sent and the row did not change (a store error).
    Stopped(&'static str),
}

/// The deck wait of the send (35 s, the P5a capture wait), cut by what is
/// left of the handler.
fn waits(deadline: Instant) -> Waits {
    let left = deadline.saturating_duration_since(Instant::now());
    let wait = left.min(Duration::from_millis(deadline_ms::ACTION_DECK_WAIT));
    Waits {
        long: wait,
        short: wait,
    }
}

/// Ends the operation and logs a failed write (the real outcome is answered
/// anyway; start recovery fixes a row that stays open).
pub(super) async fn end(
    link: &DeckLink,
    operation_id: &str,
    outcome: OpOutcome,
    message: Option<&'static str>,
) -> End {
    let first = match link
        .store
        .desktop_op_finish(operation_id, outcome.clone(), link.store.now_ms())
        .await
    {
        Ok(first) => first,
        Err(e) => {
            tracing::warn!(error = %e, "coordinator capture finish");
            false
        }
    };
    End::Final {
        outcome,
        first,
        message,
    }
}

/// The capture record of a failure that happened before the send (or that the
/// station reported for sure): `failed` with `code` and its fixed text.
pub(super) async fn mark_failed(
    link: &DeckLink,
    station: &StationId,
    id: &str,
    stored: (&str, &str),
) {
    let marked = link
        .store
        .capture_failed(
            station.clone(),
            id.to_owned(),
            stored.0.to_owned(),
            stored.1.to_owned(),
        )
        .await;
    if let Err(e) = marked {
        tracing::warn!(error = %e, "coordinator capture: failure not stored");
    }
}

/// A failure before the send: the record and the operation fail together.
pub(super) async fn fail_before_send(
    link: &DeckLink,
    (station, id): (&StationId, &str),
    refusal: &'static str,
) -> End {
    mark_failed(
        link,
        station,
        id,
        (refusal, text(refusal).unwrap_or_default()),
    )
    .await;
    end(link, id, OpOutcome::Failed(refusal), None).await
}

/// The wire answer of the station: a capture code (stored, with its text),
/// `authority_lost` and `authority_unknown` (stored, passed through), or any
/// other code (mapped as every relay code; the record stays `attempted`,
/// because the station may have run, as the owner path does).
async fn wire_failure(
    link: &DeckLink,
    (station, id): (&StationId, &str),
    (wire, message): (&str, &str),
) -> End {
    if let Some((e, stored, fixed)) = CaptureError::from_station(wire, message) {
        mark_failed(link, station, id, (stored, fixed)).await;
        let (_, public, shown) = e.http();
        return end(link, id, OpOutcome::Failed(public), Some(shown)).await;
    }
    let passed = match wire {
        "authority_lost" => Some(code::AUTHORITY_LOST),
        "authority_unknown" => Some(code::AUTHORITY_UNKNOWN),
        _ => None,
    };
    if let Some(passed) = passed {
        mark_failed(
            link,
            station,
            id,
            (passed, text(passed).unwrap_or_default()),
        )
        .await;
        return end(link, id, OpOutcome::Failed(passed), None).await;
    }
    end(
        link,
        id,
        OpOutcome::Failed(map_wire(wire, message).http().1),
        None,
    )
    .await
}

/// The station answered `Ok`: index the artifact with the coordinator
/// provenance of the record, and end with the state of the capture.
async fn indexed(
    link: &DeckLink,
    (station, id, coordinator): (&StationId, &str, &str),
    shot: &Shot<'_>,
    result: Option<CaptureResult>,
    deadline: Instant,
) -> End {
    let Some(sent) = result.filter(|c| c.capture_id == id) else {
        mark_failed(
            link,
            station,
            id,
            (capture::code::CAPTURE_FAILED, capture::text::CAPTURE_FAILED),
        )
        .await;
        let failed = OpOutcome::Failed(capture::code::CAPTURE_FAILED);
        return end(link, id, failed, Some(capture::text::CAPTURE_FAILED)).await;
    };
    let pending = |message: Option<&str>| {
        stored_result(&plain(
            shot.request_id,
            station,
            id,
            State::Pending,
            message,
        ))
    };
    let provenance = match provenance_of(
        Some(coordinator),
        station,
        id,
        shot.monitor,
        &sent.published,
    ) {
        Ok(provenance) => provenance,
        Err(_) => {
            // The station reported another provenance: never indexed, and
            // never an owner capture (§4.7). The result is kept for the owner.
            audit_indexing_failed(&link.store, station, id, Some(coordinator)).await;
            let kept = link
                .store
                .capture_keep_published(station.clone(), id.to_owned(), sent.published)
                .await;
            if let Err(e) = kept {
                tracing::warn!(error = %e, "coordinator capture: result not kept");
            }
            let value = pending(Some(PENDING_INDEXING));
            return end(link, id, OpOutcome::Done(value), None).await;
        }
    };
    let left = deadline.saturating_duration_since(Instant::now());
    let done = tokio::time::timeout(
        left,
        link.store.complete_capture(
            station.clone(),
            id.to_owned(),
            sent.published.clone(),
            provenance.clone(),
        ),
    )
    .await;
    let value = match done {
        Ok(Ok(Completed::Indexed)) => {
            let published = stamp_published(station, sent.published, &provenance);
            stored_result(&committed(
                shot.request_id,
                station,
                (id, shot.monitor),
                &published,
            ))
        }
        // A tombstone hides the artifact.
        Ok(Ok(Completed::Deleted)) => {
            stored_result(&plain(shot.request_id, station, id, State::Deleted, None))
        }
        Ok(Err(e)) => {
            // After the station commit the screenshot did not fail: keep the
            // result for the recovery and answer `pending` (§7.4).
            tracing::warn!(error = %e, "coordinator capture: indexing failed");
            let kept = link
                .store
                .capture_keep_published(station.clone(), id.to_owned(), sent.published)
                .await;
            if let Err(e) = kept {
                tracing::warn!(error = %e, "coordinator capture: result not kept");
            }
            pending(Some(PENDING_INDEXING))
        }
        // The record stays `attempted`: the recovery asks the station.
        Err(_) => pending(None),
    };
    end(link, id, OpOutcome::Done(value), None).await
}

/// The final state of a send that the station did not answer with a result.
pub(super) fn after_error(sent: CallError) -> Result<OpOutcome, (String, String)> {
    match sent {
        CallError::Relay(RelayError::Timeout | RelayError::Unreachable) => Ok(OpOutcome::Unknown),
        CallError::Relay(e) => Ok(OpOutcome::Failed(e.http().1)),
        CallError::Wire { code, message } => Err((code, message)),
    }
}

/// The send of an accepted screenshot and the end of its operation.
async fn execute(
    link: &DeckLink,
    auth: &Authorized,
    id: &str,
    shot: &Shot<'_>,
    deadline: Instant,
) -> End {
    let ctx = auth.ctx();
    let station = auth.target();
    // The in-flight slots; they are released on every path (a drop guard).
    let Some(_slots) = link.coordinator_desktop.slots().take_call(&ctx.grant_id) else {
        return end(link, id, OpOutcome::Failed(code::BUSY), None).await;
    };
    let action = DesktopAction::ScreenCapture {
        monitor: shot.monitor.to_owned(),
    };
    let hash = action_hash_for(&action, auth.route().station_node(), Some(ctx));
    let Ok(coordinator) = serde_json::to_string(ctx) else {
        return end(link, id, OpOutcome::Failed(code::AUTHORITY_UNKNOWN), None).await;
    };
    // The deck capture record, before anything is sent.
    let prepared = link
        .store
        .capture_prepare_coordinator(
            station.clone(),
            id.to_owned(),
            hash,
            shot.monitor.to_owned(),
            coordinator.clone(),
        )
        .await;
    match prepared {
        Ok(Prepared::New) => {}
        // A record of this ID exists (not possible for a new operation ID):
        // nothing is sent.
        Ok(_) => return end(link, id, OpOutcome::Failed(code::DESKTOP_FAILED), None).await,
        Err(e) => {
            tracing::warn!(error = %e, "coordinator capture prepare");
            return end(link, id, OpOutcome::Failed(code::AUTHORITY_UNKNOWN), None).await;
        }
    }
    let who = (station, id);
    // The deck-local checks, right before the send.
    let held = Held {
        grant_id: &ctx.grant_id,
        binding_id: &ctx.binding_id,
        generation: ctx.generation,
        caller: &ctx.caller_station,
        target: station,
        class: auth.class(),
    };
    if let Err(refusal) = local_pass(link, &held).await {
        return fail_before_send(link, who, refusal).await;
    }
    let control = auth.route().control();
    if control.ended().is_some() || !control.is_current() {
        return fail_before_send(link, who, code::STATION_UNREACHABLE).await;
    }
    match link
        .store
        .desktop_op_attempted(id, link.store.now_ms())
        .await
    {
        Ok(true) => {}
        Ok(false) => return End::Stopped(code::AUTHORITY_UNKNOWN),
        Err(e) => {
            tracing::warn!(error = %e, "coordinator capture attempted");
            return End::Stopped(code::AUTHORITY_UNKNOWN);
        }
    }
    // After this commit a crash leaves the outcome unknown: never sent again.
    if let Err(e) = link
        .store
        .capture_attempted(station.clone(), id.to_owned())
        .await
    {
        tracing::warn!(error = %e, "coordinator capture attempted");
        return fail_before_send(link, who, code::AUTHORITY_UNKNOWN).await;
    }
    let sent =
        send_coordinator_act(&link.store, auth.route(), &action, ctx, id, waits(deadline)).await;
    match sent {
        Ok(DesktopActResult { capture, .. }) => {
            indexed(link, (station, id, &coordinator), shot, capture, deadline).await
        }
        Err(error) => match after_error(error) {
            Ok(outcome) => end(link, id, outcome, None).await,
            Err((wire, message)) => wire_failure(link, who, (&wire, &message)).await,
        },
    }
}

/// Runs the accepted screenshot `op` and answers: the state of the capture
/// (`committed`, `pending` or `deleted`) as a success; the code and the
/// capture ID for a failure; `desktop_timeout` for an unknown result. The
/// outcome audit row is written once, by the call that made the first
/// transition.
pub(super) async fn run_screenshot(
    link: &DeckLink,
    auth: &Authorized,
    op: &DesktopOp,
    shot: Shot<'_>,
    deadline: Instant,
) -> Outcome {
    let id = op.operation_id.as_str();
    let (outcome, first, message) = match execute(link, auth, id, &shot, deadline).await {
        End::Stopped(stop) => {
            return reply_err_capture(stop, text(stop).unwrap_or_default(), id);
        }
        End::Final {
            outcome,
            first,
            message,
        } => (outcome, first, message),
    };
    let (result, failure) = match &outcome {
        OpOutcome::Done(_) => (AuditResult::Ok, None),
        OpOutcome::Failed(c) => (AuditResult::Failed, Some(*c)),
        OpOutcome::Unknown => (AuditResult::Failed, Some(code::DESKTOP_TIMEOUT)),
    };
    if first {
        let facts = Facts {
            action: audit::CAPTURE,
            ctx: auth.ctx(),
            target: auth.target(),
            operation_id: Some(id),
            window_id: None,
            monitor: Some(shot.monitor),
        };
        let row = facts.row(audit::OUTCOME, result, failure, None);
        audit::write_or_log(&link.store, row).await;
    }
    match (outcome, failure) {
        (OpOutcome::Done(mut value), _) => {
            // The ledger holds no request ID: the reply adds it.
            if let Some(object) = value.as_object_mut() {
                object.insert("request_id".to_owned(), shot.request_id.into());
            }
            reply_ok(value)
        }
        (_, Some(c)) => reply_err_capture(c, message.or_else(|| text(c)).unwrap_or_default(), id),
        (_, None) => reply_err_capture(code::DESKTOP_FAILED, "", id),
    }
}

#[cfg(test)]
#[path = "capture_tests.rs"]
mod tests;
