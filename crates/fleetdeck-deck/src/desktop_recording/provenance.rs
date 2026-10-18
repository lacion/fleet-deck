//! The provenance of an owner recording and its indexing (R13 P5b-1 task K1,
//! design `omarchy-p5b.md` §4.2, §5.2, §11.1; decision O12-10 item 22).
//!
//! The deck builds the provenance itself. The metadata that the station
//! reports is accepted only when it holds the checks of
//! [`recording_provenance`]; else it is an indexing failure. A recording is
//! never indexed with the provenance of an owner screenshot.

use fleetdeck_proto::StationId;
use fleetdeck_proto::artifacts::ArtifactPublished;
use fleetdeck_proto::capture::{CaptureCaller, CaptureKind, CaptureProvenance, CaptureStatus};
use fleetdeck_proto::recording::{RecordingMeta, StopReason, valid_dimensions};

use super::{CaptureAudit, audit_capture, audit_terminal};
use crate::store::desktop_captures::{CaptureState, Completed};
use crate::store::desktop_recordings::{OperationKind, RecordingRow};
use crate::store::{AuditResult, DeckError, DeckStore};

/// The most that the station may add to the requested duration: one second.
const DURATION_SLACK_MS: u64 = 1_000;

/// True for the metadata that the deck accepts for `row`.
fn meta_ok(row: &RecordingRow, meta: &RecordingMeta) -> bool {
    let Some(seconds) = row.requested_seconds else {
        return false;
    };
    meta.requested_seconds == seconds
        && matches!(
            meta.stop_reason,
            StopReason::Duration | StopReason::OwnerStop
        )
        && meta.duration_ms <= u64::from(seconds) * 1_000 + DURATION_SLACK_MS
        && valid_dimensions(meta.width, meta.height)
}

/// The immutable provenance of an owner recording, built by the deck: `at` is
/// the creation time of the station's version, the caller is the owner, and
/// the metadata is the station's `RecordingMeta` when it holds the checks.
///
/// `None` is an indexing failure: a row that is not an owner recording (a
/// screenshot, or a row with a `coordinator` value), a missing or invalid
/// `RecordingMeta` in the reported provenance, or a reported provenance of
/// another capture, monitor or kind.
#[must_use]
pub fn recording_provenance(
    station: &StationId,
    row: &RecordingRow,
    published: &ArtifactPublished,
) -> Option<CaptureProvenance> {
    if row.kind != OperationKind::Recording || row.capture.coordinator.is_some() {
        return None;
    }
    let reported = published.version.capture.as_ref()?;
    if reported.kind != CaptureKind::Recording
        || reported.capture_id != row.capture.capture_id
        || reported.monitor != row.capture.monitor
    {
        return None;
    }
    let meta = reported.recording.clone()?;
    if !meta_ok(row, &meta) {
        return None;
    }
    Some(CaptureProvenance {
        capture_id: row.capture.capture_id.clone(),
        station_id: station.as_str().to_owned(),
        monitor: row.capture.monitor.clone(),
        kind: CaptureKind::Recording,
        caller: CaptureCaller::Owner,
        caller_station: None,
        caller_session: None,
        caller_grant_id: None,
        at: published.version.created_at,
        recording: Some(meta),
    })
}

/// What [`index`] did.
pub(super) enum Indexed {
    /// Indexed, with the provenance that was stored.
    Done(Box<ArtifactPublished>),
    /// The artifact has a tombstone: nothing is indexed.
    Deleted,
    /// Not indexed; the station result stays in the row for the recovery.
    Pending,
}

/// Indexes a station result of a recording: the provenance check, then the
/// one transaction of `complete_capture`. On a refused provenance or a store
/// error the station result is kept (`capture_keep_published`) and the audit
/// row `indexing_failed` is written once, when the row first moves there.
pub(super) async fn index(
    store: &DeckStore,
    row: &RecordingRow,
    published: ArtifactPublished,
) -> Indexed {
    let station = row.capture.station_id.clone();
    let id = row.capture.capture_id.clone();
    let Some(provenance) = recording_provenance(&station, row, &published) else {
        if row.capture.state != CaptureState::Committed {
            audit_capture(
                store,
                &station,
                AuditResult::Failed,
                CaptureAudit {
                    capture_id: &id,
                    monitor: &row.capture.monitor,
                    seconds: row.requested_seconds,
                    stage: "indexing_failed",
                    code: "indexing_failed",
                    stop_reason: None,
                    artifact: None,
                },
            )
            .await;
        }
        keep(store, &station, &id, published).await;
        return Indexed::Pending;
    };
    let done = store
        .complete_capture(
            station.clone(),
            id.clone(),
            published.clone(),
            provenance.clone(),
        )
        .await;
    match done {
        Ok(Completed::Indexed) => {
            let stamped =
                crate::store::desktop_captures::stamp_published(&station, published, &provenance);
            let reason = provenance.recording.as_ref().map(|m| m.stop_reason);
            audit_terminal(
                store,
                row,
                "ok",
                reason,
                Some(&stamped.artifact.artifact_id),
            )
            .await;
            Indexed::Done(Box::new(stamped))
        }
        Ok(Completed::Deleted) => Indexed::Deleted,
        Err(e) => {
            tracing::warn!(error = %e, "desktop recording: indexing failed");
            keep(store, &station, &id, published).await;
            Indexed::Pending
        }
    }
}

async fn keep(store: &DeckStore, station: &StationId, id: &str, published: ArtifactPublished) {
    let kept = store
        .capture_keep_published(station.clone(), id.to_owned(), published)
        .await;
    if let Err(e) = kept {
        tracing::warn!(error = %e, "desktop recording: result not kept");
    }
}

/// Applies a station status to a recording row (the rules of
/// `desktop_capture::reconcile` with the recording tables). A committed
/// result indexes the artifact with the recording provenance; a failure
/// stores a recording code (any other code is `recording_failed`); `unknown`
/// ends the row 24 h after its creation.
///
/// # Errors
/// A database error.
pub(crate) async fn reconcile(
    store: &DeckStore,
    row: &RecordingRow,
    status: CaptureStatus,
) -> Result<(), DeckError> {
    let station = row.capture.station_id.clone();
    let id = row.capture.capture_id.clone();
    match status {
        CaptureStatus::Committed { published, .. } => {
            index(store, row, published).await;
        }
        CaptureStatus::Failed { error, message, .. } => {
            let (stored, fixed) = super::RecordingError::fixed(&error, &message);
            store
                .capture_failed(station, id, stored.to_owned(), fixed.to_owned())
                .await?;
            // The row as it is now: a row that the station committed does
            // not change, and then no failure is audited.
            if let Some(now) = store
                .recording_get(
                    row.capture.station_id.clone(),
                    row.capture.capture_id.clone(),
                )
                .await?
                && now.capture.state == CaptureState::Failed
            {
                audit_terminal(store, &now, stored, None, None).await;
            }
        }
        CaptureStatus::Unknown { .. } => {
            if store.now_ms().saturating_sub(row.capture.created_at)
                >= crate::store::desktop_captures::UNKNOWN_AFTER_MS
            {
                store.capture_unknown(station, id).await?;
            }
        }
        // Nothing to do: `pending` waits, and a tombstone answers `deleted`.
        CaptureStatus::Pending { .. } | CaptureStatus::Deleted { .. } => {}
    }
    Ok(())
}

#[cfg(test)]
#[path = "provenance_tests.rs"]
mod tests;
