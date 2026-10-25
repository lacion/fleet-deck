//! The reconcile rules and the recovery task of the desktop capture (R13 P5a
//! task E). Moved from `desktop_capture.rs` for the module size rule (R13 P6
//! F0).

use std::sync::Arc;
use std::time::Duration;

use fleetdeck_proto::capture::{CaptureStatus, code, text};
use tokio::sync::watch;
use tokio::task::JoinHandle;

use super::errors::KNOWN;
use super::{RECOVERY_TICK, STATUS_WAIT, ask_status, audit_indexing_failed, provenance_of};
use crate::desktop_relay::RelayCtx;
use crate::link::DeckLink;
use crate::store::desktop_captures::{CaptureRow, CaptureState, UNKNOWN_AFTER_MS};
use crate::store::desktop_recordings::OperationKind;
use crate::store::{DeckError, DeckStore};

/// Applies a station status to a row (§6.3, decision 8). A committed result
/// indexes the artifact; a failure stores only a capture code (any other code
/// is `capture_failed`); `unknown` ends the row 24 h after its creation.
///
/// # Errors
/// A database error.
pub async fn reconcile(
    store: &DeckStore,
    row: &CaptureRow,
    status: CaptureStatus,
) -> Result<(), DeckError> {
    let station = row.station_id.clone();
    // A recording has its own provenance, failure codes and audit rows.
    if let Some(recording) = store
        .recording_get(station.clone(), row.capture_id.clone())
        .await?
        && recording.kind == OperationKind::Recording
    {
        return crate::desktop_recording::reconcile(store, &recording, status).await;
    }
    match status {
        CaptureStatus::Committed { published, .. } => {
            // The provenance comes from the row: a row with a `coordinator`
            // value is never indexed as an owner capture (R13 P6 §4.7).
            let Ok(provenance) = provenance_of(
                row.coordinator.as_deref(),
                &station,
                &row.capture_id,
                &row.monitor,
                &published,
            ) else {
                // The artifact is never indexed. The row stays `committed`
                // and unindexed for the owner (the same row as any indexing
                // failure); the audit row is written once, when the row
                // first moves there.
                if row.state != CaptureState::Committed {
                    audit_indexing_failed(
                        store,
                        &station,
                        &row.capture_id,
                        row.coordinator.as_deref(),
                    )
                    .await;
                }
                store
                    .capture_keep_published(station, row.capture_id.clone(), published)
                    .await?;
                return Ok(());
            };
            store
                .complete_capture(station, row.capture_id.clone(), published, provenance)
                .await?;
        }
        CaptureStatus::Failed { error, .. } => {
            let (stored, fixed) = KNOWN
                .iter()
                .find(|k| k.code == error)
                .map_or((code::CAPTURE_FAILED, text::CAPTURE_FAILED), |k| {
                    (k.code, k.text)
                });
            store
                .capture_failed(
                    station,
                    row.capture_id.clone(),
                    stored.to_owned(),
                    fixed.to_owned(),
                )
                .await?;
        }
        CaptureStatus::Unknown { .. } => {
            if store.now_ms().saturating_sub(row.created_at) >= UNKNOWN_AFTER_MS {
                store
                    .capture_unknown(station, row.capture_id.clone())
                    .await?;
            }
        }
        // Nothing to do: `pending` waits, and a tombstone answers `deleted`.
        CaptureStatus::Pending { .. } | CaptureStatus::Deleted { .. } => {}
    }
    Ok(())
}

/// Asks the station about an unresolved row and reconciles it.
pub(super) async fn refresh(ctx: &RelayCtx, row: &CaptureRow, status_wait: Duration) {
    let Some(status) = ask_status(&ctx.link, &row.station_id, &row.capture_id, status_wait).await
    else {
        return;
    };
    if let Err(e) = reconcile(&ctx.store, row, status).await {
        tracing::warn!(error = %e, "desktop capture: not reconciled");
    }
}

/// Starts the recovery task. It stops when `shutdown` turns true.
pub fn spawn(
    store: DeckStore,
    link: Arc<DeckLink>,
    shutdown: watch::Receiver<bool>,
) -> JoinHandle<()> {
    spawn_with(store, link, shutdown, RECOVERY_TICK, STATUS_WAIT)
}

/// [`spawn`] with another pass interval and status wait (a test makes them
/// short).
pub fn spawn_with(
    store: DeckStore,
    link: Arc<DeckLink>,
    shutdown: watch::Receiver<bool>,
    tick: Duration,
    status_wait: Duration,
) -> JoinHandle<()> {
    tokio::spawn(run_recovery(store, link, shutdown, tick, status_wait))
}

async fn run_recovery(
    store: DeckStore,
    link: Arc<DeckLink>,
    mut shutdown: watch::Receiver<bool>,
    tick: Duration,
    status_wait: Duration,
) {
    let mut board = store.watch_board();
    let mut timer = tokio::time::interval(tick);
    timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        if *shutdown.borrow() {
            break;
        }
        tokio::select! {
            () = pass(&store, &link, status_wait) => {}
            result = shutdown.changed() => {
                if result.is_err() || *shutdown.borrow() {
                    break;
                }
                continue;
            }
        }
        tokio::select! {
            result = board.changed() => {
                if result.is_err() {
                    break;
                }
            }
            _ = timer.tick() => {}
            result = shutdown.changed() => {
                if result.is_err() || *shutdown.borrow() {
                    break;
                }
            }
        }
    }
}

/// One pass: each unresolved row whose station link is live and has
/// `desktop_capture.v1` is asked once and reconciled. It sends no action.
async fn pass(store: &DeckStore, link: &DeckLink, status_wait: Duration) {
    let rows = match store.captures_unresolved().await {
        Ok(rows) => rows,
        Err(e) => {
            tracing::warn!(error = %e, "desktop capture recovery: rows not read");
            return;
        }
    };
    for row in rows {
        let Some(status) = ask_status(link, &row.station_id, &row.capture_id, status_wait).await
        else {
            continue;
        };
        if let Err(e) = reconcile(store, &row, status).await {
            tracing::warn!(error = %e, "desktop capture recovery: not reconciled");
        }
    }
}
