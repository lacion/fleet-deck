//! The active recording discovery and the recording status (R13 P5b-1 task
//! K1, design `omarchy-p5b.md` §4.2, §4.3).
//!
//! The progress of a recording comes only from a station answer that this
//! request got: a stale observation of the deck is never presented as proof
//! that a recording is still running.

use fleetdeck_link::control::Control;
use fleetdeck_proto::StationId;
use fleetdeck_proto::capture::{CaptureStatus, CaptureStatusQuery, valid_capture_id};
use fleetdeck_proto::link::Outcome;
use fleetdeck_proto::recording::{ActiveRecording, RecordingCaptureStatus};
use fleetdeck_proto::version::feature;

use super::provenance::reconcile;
use super::{RecordOptions, RecordingError, request};
use crate::desktop_capture::status_of;
use crate::desktop_relay::{RelayCtx, RelayError};
use crate::link::desktop_authority::CallError;
use crate::store::desktop_recordings::{OperationKind, RecordingRow};
use crate::store::{DeckError, LinkFeature};

/// The capture id of a status.
pub(super) fn status_id(s: &CaptureStatus) -> &str {
    match s {
        CaptureStatus::Pending { capture_id }
        | CaptureStatus::Unknown { capture_id }
        | CaptureStatus::Committed { capture_id, .. }
        | CaptureStatus::Failed { capture_id, .. }
        | CaptureStatus::Deleted { capture_id } => capture_id,
    }
}

/// True for a state that ends the operation for the board: `committed`,
/// `failed` and `deleted` (`200`); `pending` and `unknown` are `202`.
pub(super) fn terminal(s: &CaptureStatus) -> bool {
    matches!(
        s,
        CaptureStatus::Committed { .. }
            | CaptureStatus::Failed { .. }
            | CaptureStatus::Deleted { .. }
    )
}

/// The status of `row` for a recording-aware answer. `answer` is the station
/// answer of this request, if any: its progress is shown only while the row
/// is pending, and its stop reason fills a missing one.
pub(super) fn compose(
    row: &RecordingRow,
    answer: Option<&RecordingCaptureStatus>,
) -> RecordingCaptureStatus {
    let status = status_of(&row.capture);
    let recording = match (&status, answer) {
        (CaptureStatus::Pending { .. }, Some(a)) => a.recording.clone(),
        _ => None,
    };
    let committed = match &status {
        CaptureStatus::Committed { published, .. } => published
            .version
            .capture
            .as_ref()
            .and_then(|c| c.recording.as_ref())
            .map(|m| m.stop_reason),
        _ => None,
    };
    let stop_reason = committed
        .or_else(|| answer.and_then(|a| a.stop_reason))
        .or(row.stop_reason);
    RecordingCaptureStatus {
        status,
        recording,
        stop_reason,
    }
}

/// One station read of a recording-aware answer, on the live link: `None`
/// without a link, without the features, on a timeout, an error answer or an
/// answer for another capture.
async fn ask(
    control: &Control,
    capture_id: &str,
    wait: std::time::Duration,
) -> Option<RecordingCaptureStatus> {
    let negotiated = &control.params().negotiated;
    if !(negotiated.has(feature::DESKTOP_CAPTURE) && negotiated.has(feature::DESKTOP_RECORDING)) {
        return None;
    }
    let params = serde_json::json!(CaptureStatusQuery {
        capture_id: capture_id.to_owned(),
    });
    let outcome = tokio::time::timeout(
        wait,
        control.request("desktop.capture_status", None, params),
    )
    .await
    .ok()?
    .ok()?;
    let Outcome::Ok(value) = outcome else {
        return None;
    };
    // A parse error is not logged: the answer could hold desktop data.
    let answer: RecordingCaptureStatus = serde_json::from_value(value).ok()?;
    (status_id(&answer.status) == capture_id).then_some(answer)
}

/// The status of a recording (§4.2): the row, after one station call and the
/// reconcile while it is unresolved.
///
/// # Errors
/// A database error.
pub async fn status(
    ctx: &RelayCtx,
    station: &StationId,
    row: RecordingRow,
    opts: RecordOptions,
) -> Result<RecordingCaptureStatus, DeckError> {
    let mut row = row;
    let mut answer = None;
    if row.capture.unresolved() {
        // The whole handler stops at its deadline; the recovery goes on.
        let asked = tokio::time::timeout(opts.lookup, async {
            let control = ctx.link.control_for(station)?;
            let answer = ask(&control, &row.capture.capture_id, opts.status_wait).await?;
            if let Err(e) = reconcile(&ctx.store, &row, answer.status.clone()).await {
                tracing::warn!(error = %e, "desktop recording: not reconciled");
            }
            Some(answer)
        })
        .await;
        answer = asked.ok().flatten();
        if let Some(fresh) = ctx
            .store
            .recording_get(station.clone(), row.capture.capture_id.clone())
            .await?
        {
            row = fresh;
        }
    }
    Ok(compose(&row, answer.as_ref()))
}

/// The active recording of `station` (§4.3): `desktop.capture_active` to the
/// target. An offline target is [`RelayError::Unreachable`], never `idle`.
///
/// # Errors
/// [`RecordingError`]: `station_unreachable`, `desktop_unsupported` (501),
/// `desktop_timeout`, `desktop_failed`.
pub async fn active(
    ctx: &RelayCtx,
    station: &StationId,
    opts: RecordOptions,
) -> Result<ActiveRecording, RecordingError> {
    match ctx.store.link_feature(station, feature::DESKTOP_RECORDING) {
        LinkFeature::Down => return Err(RelayError::Unreachable.into()),
        LinkFeature::Without => return Err(RecordingError::unsupported()),
        LinkFeature::Negotiated => {}
    }
    let control = ctx
        .link
        .control_for(station)
        .ok_or(RelayError::Unreachable)?;
    if !control.params().negotiated.has(feature::DESKTOP_RECORDING) {
        return Err(RecordingError::unsupported());
    }
    let answer: ActiveRecording = request(
        &control,
        "desktop.capture_active",
        serde_json::json!({}),
        opts.status_wait,
    )
    .await
    .map_err(|e| match e {
        CallError::Relay(e) => RecordingError::Relay(e),
        CallError::Wire { code, message } => {
            RecordingError::Relay(crate::link::desktop_authority::map_wire(&code, &message))
        }
    })?;
    let ActiveRecording::Active { capture_id, status } = answer else {
        return Ok(ActiveRecording::Idle);
    };
    if !(valid_capture_id(&capture_id) && status_id(&status.status) == capture_id) {
        return Err(RelayError::Failed.into());
    }
    // The status is the deck's own row with the progress of this answer: a
    // station text or an artifact of the station never reaches the board
    // through this call.
    let row = ctx
        .store
        .recording_get(station.clone(), capture_id.clone())
        .await
        .map_err(|e| {
            tracing::warn!(error = %e, "desktop recording store");
            RecordingError::Relay(RelayError::Failed)
        })?
        .filter(|r| r.kind == OperationKind::Recording);
    let status = match row {
        Some(row) => compose(&row, Some(&status)),
        None => RecordingCaptureStatus {
            status: CaptureStatus::Pending {
                capture_id: capture_id.clone(),
            },
            recording: status.recording,
            stop_reason: status.stop_reason,
        },
    };
    Ok(ActiveRecording::Active { capture_id, status })
}
