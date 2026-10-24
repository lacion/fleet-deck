//! The coordinator `record_monitor` after its admission (R13 P5b-2 §13, §6.3,
//! §10, §15; decisions O12-22 and O12-10 items 22 and 32, task Q-K).
//!
//! It follows the screenshot ([`super::capture`]): the capture record of the
//! deck is a recording (`operation_kind = 'recording'`, the `coordinator`
//! column set, the capture ID equal to the operation ID, no owner login), the
//! one `desktop.act` carries a coordinator authority, and the indexing uses a
//! coordinator recording provenance. The deck never sends a second recording:
//! the record is `attempted` before the send, and a result that is not known
//! stays for the recovery, which only asks the station for the status.
//!
//! The mutation slot of the grant is held until the terminal bookkeeping (the
//! outcome audit row) is done. No outer timeout wraps this code: after
//! `desktop_op_attempted` the handler is never dropped, and every wait is cut
//! by what is left of the handler deadline (`seconds` plus 40 s). The deck
//! wait for the station is `seconds` plus 35 s.

use std::time::{Duration, Instant};

use fleetdeck_link::desktop_authority::action_hash_for;
use fleetdeck_proto::StationId;
use fleetdeck_proto::capture::{self, CaptureResult};
use fleetdeck_proto::coordinator_desktop::CaptureState as State;
use fleetdeck_proto::coordinator_desktop_text::{code, deadline_ms, text};
use fleetdeck_proto::desktop::{DesktopActResult, DesktopAction};
use fleetdeck_proto::link::Outcome;
use serde_json::Value;

use super::audit::{self, Facts};
use super::authorize::{Authorized, delivery_pass, recording_pass};
use super::capture::{End, after_error, end, fail_before_send, mark_failed};
use super::capture_status::{
    PENDING_INDEXING_RECORDING, committed_recording, plain, stored_result, with_seconds,
};
use super::{reply_err_capture, reply_ok};
use crate::desktop_capture::audit_indexing_failed;
use crate::desktop_recording::{RecordingError, recording_provenance};
use crate::link::DeckLink;
use crate::link::desktop_authority::{Waits, map_wire, send_coordinator_act};
use crate::store::AuditResult;
use crate::store::coordinator_desktop::{DesktopOp, OpOutcome};
use crate::store::desktop_captures::{Completed, Prepared, stamp_published};
use crate::store::desktop_recordings::NewCoordinatorRecording;

/// What the recording call knows: the raw request ID (only for the reply,
/// never stored), the validated monitor and the validated seconds.
pub(super) struct Rec<'a> {
    pub request_id: &'a str,
    pub monitor: &'a str,
    pub seconds: u32,
}

/// The deck wait of the send (`seconds` plus 35 s, R13 P5b-2 §10), cut by what
/// is left of the handler.
fn waits(deadline: Instant, seconds: u32) -> Waits {
    let left = deadline.saturating_duration_since(Instant::now());
    let limit = u64::from(seconds) * 1_000 + deadline_ms::ACTION_DECK_WAIT;
    let wait = left.min(Duration::from_millis(limit));
    Waits {
        long: wait,
        short: wait,
    }
}

/// The wire answer of the station: a recording or capture code (stored, with
/// its fixed text), `authority_lost`, `authority_unknown` and
/// `desktop_read_timeout` (the coordinator terminal codes of §7.5; stored,
/// passed through), or any other code (mapped as every relay code; the record
/// stays `attempted`, because the station may have run).
async fn wire_failure(
    link: &DeckLink,
    (station, id): (&StationId, &str),
    (wire, message): (&str, &str),
) -> End {
    if let Some((e, stored, fixed)) = RecordingError::from_station(wire, message) {
        mark_failed(link, station, id, (stored, fixed)).await;
        let (_, public, shown) = e.http();
        return end(link, id, OpOutcome::Failed(public), Some(shown)).await;
    }
    let passed = match wire {
        "authority_lost" => Some(code::AUTHORITY_LOST),
        "authority_unknown" => Some(code::AUTHORITY_UNKNOWN),
        "desktop_read_timeout" => Some(code::DESKTOP_READ_TIMEOUT),
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

/// Keeps the station result in the deck record for the recovery.
async fn keep(link: &DeckLink, station: &StationId, id: &str, sent: CaptureResult) {
    let kept = link
        .store
        .capture_keep_published(station.clone(), id.to_owned(), sent.published)
        .await;
    if let Err(e) = kept {
        tracing::warn!(error = %e, "coordinator recording: result not kept");
    }
}

/// The station answered `Ok`: index the artifact with the coordinator
/// recording provenance of the record, and end with the state of the capture.
async fn indexed(
    link: &DeckLink,
    (station, id, coordinator): (&StationId, &str, &str),
    rec: &Rec<'_>,
    result: Option<CaptureResult>,
    deadline: Instant,
) -> End {
    let seconds = Some(rec.seconds);
    let Some(sent) = result.filter(|c| c.capture_id == id) else {
        let failed = (
            capture::code::RECORDING_FAILED,
            capture::text::RECORDING_FAILED,
        );
        mark_failed(link, station, id, failed).await;
        let outcome = OpOutcome::Failed(capture::code::RECORDING_FAILED);
        return end(link, id, outcome, Some(capture::text::RECORDING_FAILED)).await;
    };
    let pending = |message: Option<&str>| {
        stored_result(&with_seconds(
            plain(rec.request_id, station, id, State::Pending, message),
            seconds,
        ))
    };
    let row = match link
        .store
        .recording_get(station.clone(), id.to_owned())
        .await
    {
        Ok(Some(row)) => row,
        // The row is the record that this call made: a missing or unreadable
        // row keeps the result for the recovery and answers `pending`.
        Ok(None) | Err(_) => {
            tracing::warn!("coordinator recording: the record is not readable");
            keep(link, station, id, sent).await;
            return end(link, id, OpOutcome::Done(pending(None)), None).await;
        }
    };
    let Some(provenance) = recording_provenance(station, &row, &sent.published) else {
        // The station reported another provenance or other recording data:
        // never indexed, and never an owner recording. The result is kept
        // for the owner.
        audit_indexing_failed(&link.store, station, id, Some(coordinator)).await;
        keep(link, station, id, sent).await;
        let value = pending(Some(PENDING_INDEXING_RECORDING));
        return end(link, id, OpOutcome::Done(value), None).await;
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
            stored_result(&committed_recording(
                rec.request_id,
                station,
                (id, rec.monitor),
                (&published, seconds),
            ))
        }
        // A tombstone hides the artifact.
        Ok(Ok(Completed::Deleted)) => stored_result(&with_seconds(
            plain(rec.request_id, station, id, State::Deleted, None),
            seconds,
        )),
        Ok(Err(e)) => {
            // After the station commit the recording did not fail: keep the
            // result for the recovery and answer `pending`.
            tracing::warn!(error = %e, "coordinator recording: indexing failed");
            keep(link, station, id, sent).await;
            pending(Some(PENDING_INDEXING_RECORDING))
        }
        // The record stays `attempted`: the recovery asks the station.
        Err(_) => pending(None),
    };
    end(link, id, OpOutcome::Done(value), None).await
}

/// The send of an accepted recording and the end of its operation.
async fn execute(
    link: &DeckLink,
    auth: &Authorized,
    id: &str,
    rec: &Rec<'_>,
    deadline: Instant,
) -> End {
    let ctx = auth.ctx();
    let station = auth.target();
    let action = DesktopAction::ScreenRecord {
        monitor: rec.monitor.to_owned(),
        seconds: rec.seconds,
    };
    let hash = action_hash_for(&action, auth.route().station_node(), Some(ctx));
    let Ok(coordinator) = serde_json::to_string(ctx) else {
        return end(link, id, OpOutcome::Failed(code::AUTHORITY_UNKNOWN), None).await;
    };
    // The deck capture record, before anything is sent.
    let prepared = link
        .store
        .recording_prepare_coordinator(NewCoordinatorRecording {
            station: station.clone(),
            capture_id: id.to_owned(),
            action_hash: hash,
            monitor: rec.monitor.to_owned(),
            seconds: rec.seconds,
            coordinator: coordinator.clone(),
            link_epoch: auth.route().epoch().to_owned(),
        })
        .await;
    match prepared {
        Ok(Prepared::New) => {}
        // A record of this ID exists (not possible for a new operation ID):
        // nothing is sent.
        Ok(_) => return end(link, id, OpOutcome::Failed(code::DESKTOP_FAILED), None).await,
        Err(e) => {
            tracing::warn!(error = %e, "coordinator recording prepare");
            return end(link, id, OpOutcome::Failed(code::AUTHORITY_UNKNOWN), None).await;
        }
    }
    let who = (station, id);
    // The deck-local checks, right before the send.
    if let Err(refusal) = recording_pass(link, ctx, station).await {
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
            tracing::warn!(error = %e, "coordinator recording attempted");
            return End::Stopped(code::AUTHORITY_UNKNOWN);
        }
    }
    // After this commit a crash leaves the outcome unknown: never sent again.
    if let Err(e) = link
        .store
        .capture_attempted(station.clone(), id.to_owned())
        .await
    {
        tracing::warn!(error = %e, "coordinator recording attempted");
        return fail_before_send(link, who, code::AUTHORITY_UNKNOWN).await;
    }
    let sent = send_coordinator_act(
        &link.store,
        auth.route(),
        &action,
        ctx,
        id,
        waits(deadline, rec.seconds),
    )
    .await;
    match sent {
        Ok(DesktopActResult { capture, .. }) => {
            indexed(link, (station, id, &coordinator), rec, capture, deadline).await
        }
        Err(error) => match after_error(error) {
            Ok(outcome) => end(link, id, outcome, None).await,
            Err((wire, message)) => wire_failure(link, who, (&wire, &message)).await,
        },
    }
}

/// A string that is safe to write into an audit row: an artifact ID of
/// letters, digits, `-` and `_`, at most 64 characters.
fn safe_id(id: &str) -> Option<&str> {
    let ok = !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'));
    ok.then_some(id)
}

/// The extra keys of the outcome row: the recording keys, and for a committed
/// result the artifact reference and the fixed stop reason.
fn outcome_extras(op: &DesktopOp, outcome: &OpOutcome) -> Vec<(&'static str, Value)> {
    let mut extra = audit::recording_extras(op);
    if let OpOutcome::Done(value) = outcome
        && value.get("state").and_then(Value::as_str) == Some("committed")
    {
        let artifact = value
            .get("artifact")
            .and_then(|a| a.get("artifact_id"))
            .and_then(Value::as_str)
            .and_then(safe_id);
        if let Some(artifact) = artifact {
            extra.push(("artifact_id", artifact.into()));
        }
        if let Some(reason) = value.get("stop_reason").filter(|r| r.is_string()) {
            extra.push(("stop_reason", reason.clone()));
        }
    }
    extra
}

/// Runs the accepted recording `op` and answers: the state of the capture
/// (`committed`, `pending` or `deleted`) as a success; the code and the
/// capture ID for a failure; `desktop_timeout` for an unknown result. The
/// outcome audit row is written once, by the call that made the first
/// transition, and the mutation slot is held until it is written.
pub(super) async fn run_recording(
    link: &DeckLink,
    auth: &Authorized,
    op: &DesktopOp,
    rec: Rec<'_>,
    deadline: Instant,
) -> Outcome {
    let id = op.operation_id.as_str();
    // The in-flight slots; released on every path (a drop guard), after the
    // terminal bookkeeping.
    let slots = link
        .coordinator_desktop
        .slots()
        .take_call(&auth.ctx().grant_id);
    let ended = if slots.is_some() {
        execute(link, auth, id, &rec, deadline).await
    } else {
        end(link, id, OpOutcome::Failed(code::BUSY), None).await
    };
    let (outcome, first, message) = match ended {
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
            monitor: Some(rec.monitor),
        };
        let extra = outcome_extras(op, &outcome);
        let row = facts.row_with(audit::OUTCOME, result, failure, None, &extra);
        audit::write_or_log(&link.store, row).await;
    }
    // The final authority check before a result is delivered (R13 P5b-2
    // §13.4): the durable result stays, the caller gets no reference.
    if matches!(outcome, OpOutcome::Done(_))
        && let Err(refusal) = delivery_pass(link, auth.ctx(), auth.target()).await
    {
        let lost = if refusal == code::AUTHORITY_UNKNOWN {
            code::AUTHORITY_UNKNOWN
        } else {
            code::AUTHORITY_LOST
        };
        let facts = Facts {
            action: audit::CAPTURE,
            ctx: auth.ctx(),
            target: auth.target(),
            operation_id: Some(id),
            window_id: None,
            monitor: Some(rec.monitor),
        };
        let row = facts.row_with(
            audit::REFUSED,
            AuditResult::Refused,
            Some(lost),
            None,
            &[("stage", "delivery".into())],
        );
        audit::write_or_log(&link.store, row).await;
        drop(slots);
        return reply_err_capture(lost, text(lost).unwrap_or_default(), id);
    }
    drop(slots);
    match (outcome, failure) {
        (OpOutcome::Done(mut value), _) => {
            // The ledger holds no request ID: the reply adds it.
            if let Some(object) = value.as_object_mut() {
                object.insert("request_id".to_owned(), rec.request_id.into());
            }
            reply_ok(value)
        }
        (_, Some(c)) => reply_err_capture(c, message.or_else(|| text(c)).unwrap_or_default(), id),
        (_, None) => reply_err_capture(code::DESKTOP_FAILED, "", id),
    }
}
