//! The owner stop (R13 P5b-1 task K1, design `omarchy-p5b.md` §4.4, §7.4,
//! §10): the intent is committed before the send, so a stop that cannot be
//! delivered is carried by the next lease answer, and a stop never needs
//! `desktop_control` or the capture switch of the target.

use axum::http::StatusCode;
use fleetdeck_proto::StationId;
use fleetdeck_proto::capture::valid_capture_id;
use fleetdeck_proto::recording::{CaptureStopParams, RecordingCaptureStatus, StopReason};
use fleetdeck_proto::version::feature;

use super::active::{compose, status_id, terminal};
use super::provenance::reconcile;
use super::{RecordOptions, RecordingError, audit_stop, request};
use crate::desktop_relay::{RelayCtx, RelayError};
use crate::store::desktop_captures::CaptureState;
use crate::store::desktop_recordings::{OperationKind, StopIntent, reason_str};
use crate::store::{AuditResult, DeckError};

fn store_error(e: &DeckError) -> RecordingError {
    tracing::warn!(error = %e, "desktop recording store");
    RecordingError::Relay(RelayError::Failed)
}

fn intent_name(i: StopIntent) -> &'static str {
    match i {
        StopIntent::None => "none",
        StopIntent::Save => "save",
        StopIntent::Discard => "discard",
    }
}

/// Stops the recording `capture_id` of `station` as the owner (§4.4): commits
/// the stop intent `save` with the reason `owner_stop` (the latch rules: a
/// stored `discard` is never replaced), sends `desktop.capture_stop` once with
/// a 5 s RPC, and answers the current status: `200` when it is terminal,
/// `202` otherwise. A repeated stop is idempotent.
///
/// # Errors
/// [`RecordingError`]: 404 `unknown_capture`, 409 `not_recording`, and
/// `desktop_timeout` when the whole handler (10 s) passes.
pub async fn stop(
    ctx: &RelayCtx,
    station: &StationId,
    capture_id: &str,
    opts: RecordOptions,
) -> Result<(StatusCode, RecordingCaptureStatus), RecordingError> {
    let run = run(ctx, station, capture_id, opts);
    match tokio::time::timeout(opts.stop_handler, run).await {
        Ok(result) => result,
        Err(_) => Err(RelayError::Timeout.into()),
    }
}

async fn run(
    ctx: &RelayCtx,
    station: &StationId,
    capture_id: &str,
    opts: RecordOptions,
) -> Result<(StatusCode, RecordingCaptureStatus), RecordingError> {
    let row = ctx
        .store
        .recording_get(station.clone(), capture_id.to_owned())
        .await
        .map_err(|e| store_error(&e))?
        .ok_or_else(RecordingError::unknown_capture)?;
    if row.kind != OperationKind::Recording {
        let e = RecordingError::not_recording();
        audit_stop(
            &ctx.store,
            station,
            AuditResult::Refused,
            capture_id,
            "stage=refused code=not_recording",
        )
        .await;
        return Err(e);
    }
    // The intent is committed before the send (§4.4).
    let latched = ctx
        .store
        .recording_stop_intent(
            station.clone(),
            capture_id.to_owned(),
            true,
            StopReason::OwnerStop,
        )
        .await
        .map_err(|e| store_error(&e))?
        .ok_or_else(RecordingError::unknown_capture)?;
    audit_stop(
        &ctx.store,
        station,
        AuditResult::Ok,
        capture_id,
        &format!(
            "stage=intent intent={} reason={}",
            intent_name(latched.stop_intent),
            latched.stop_reason.map(reason_str).unwrap_or_default()
        ),
    )
    .await;
    // A row that is over (committed and indexed, failed, deleted) is not sent:
    // the answer is its durable status.
    let over = latched.capture.deleted
        || !(matches!(
            latched.capture.state,
            CaptureState::Prepared | CaptureState::Attempted
        ) || latched.capture.unresolved());
    let answer = if over {
        None
    } else {
        send(ctx, station, capture_id, opts).await
    };
    if let Some(a) = &answer
        && let Err(e) = reconcile(&ctx.store, &latched, a.status.clone()).await
    {
        tracing::warn!(error = %e, "desktop recording: stop not reconciled");
    }
    let fresh = ctx
        .store
        .recording_get(station.clone(), capture_id.to_owned())
        .await
        .map_err(|e| store_error(&e))?
        .ok_or_else(RecordingError::unknown_capture)?;
    let status = compose(&fresh, answer.as_ref());
    let code = if terminal(&status.status) {
        StatusCode::OK
    } else {
        StatusCode::ACCEPTED
    };
    audit_stop(
        &ctx.store,
        station,
        AuditResult::Ok,
        capture_id,
        &format!(
            "stage=outcome state={} delivered={}",
            state_name(&status),
            answer.is_some()
        ),
    )
    .await;
    Ok((code, status))
}

fn state_name(s: &RecordingCaptureStatus) -> &'static str {
    use fleetdeck_proto::capture::CaptureStatus as S;
    match s.status {
        S::Pending { .. } => "pending",
        S::Unknown { .. } => "unknown",
        S::Committed { .. } => "committed",
        S::Failed { .. } => "failed",
        S::Deleted { .. } => "deleted",
    }
}

/// `desktop.capture_stop` on the live link: the status that the station
/// answers, or `None` when the stop was not delivered (no link, no feature, a
/// timeout, an error answer, or an answer for another capture). A stop that
/// was not delivered is carried by the next lease answer.
async fn send(
    ctx: &RelayCtx,
    station: &StationId,
    capture_id: &str,
    opts: RecordOptions,
) -> Option<RecordingCaptureStatus> {
    let control = ctx.link.control_for(station)?;
    if !control.params().negotiated.has(feature::DESKTOP_RECORDING) {
        return None;
    }
    let params = serde_json::json!(CaptureStopParams {
        capture_id: capture_id.to_owned(),
        reason: StopReason::OwnerStop,
    });
    let answer: RecordingCaptureStatus =
        request(&control, "desktop.capture_stop", params, opts.stop_wait)
            .await
            .ok()?;
    (valid_capture_id(capture_id) && status_id(&answer.status) == capture_id).then_some(answer)
}
