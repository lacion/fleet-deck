//! The stop latch of one recording: the first discard reason wins in memory
//! and in the row, a save after a discard does not save, and the phase never
//! goes back.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use fleetdeck_proto::recording::{RecordingPhase, StopReason};

use super::latch::Latch;
use crate::artifacts::capture_import::tests::{FP, rig};
use crate::store::capture_ops::{Begin, RecordingBegin, StopIntent};

const ID: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAV";

async fn started() -> (
    crate::artifacts::capture_import::tests::Rig,
    std::sync::Arc<Latch>,
) {
    let s = rig().await;
    let begin = s
        .store
        .recording_begin(RecordingBegin {
            deck_fp: FP,
            capture_id: ID,
            action_hash: "h",
            monitor: "DP-1",
            artifact_id: "ART",
            seconds: 5,
            link_epoch: "e",
            coordinator: None,
            now: 1,
        })
        .await
        .unwrap();
    assert!(matches!(begin, Begin::Started));
    let latch = Latch::new(s.store.clone(), FP, ID);
    (s, latch)
}

#[tokio::test]
async fn the_first_discard_reason_wins_in_memory_and_in_the_row() {
    let (s, latch) = started().await;
    assert_eq!(latch.first_discard(), None);
    assert_eq!(
        latch.discard_now(StopReason::PolicyOff),
        StopReason::PolicyOff
    );
    assert_eq!(
        latch.discard_now(StopReason::LinkClosed),
        StopReason::PolicyOff
    );
    latch.stop(false, StopReason::PolicyOff).await;
    let held = latch.stop(false, StopReason::LinkClosed).await;
    assert_eq!(held, Some((StopIntent::Discard, StopReason::PolicyOff)));
    assert_eq!(latch.first_discard(), Some(StopReason::PolicyOff));
    let row = s.store.recording_get(FP, ID).await.unwrap().unwrap();
    assert_eq!(row.stop_intent, StopIntent::Discard);
    assert_eq!(row.stop_reason, Some(StopReason::PolicyOff));
}

#[tokio::test]
async fn a_save_after_a_discard_does_not_save() {
    let (s, latch) = started().await;
    latch.stop(false, StopReason::DeckPolicy).await;
    let held = latch.stop(true, StopReason::OwnerStop).await;
    assert_eq!(held, Some((StopIntent::Discard, StopReason::DeckPolicy)));
    let row = s.store.recording_get(FP, ID).await.unwrap().unwrap();
    assert_eq!(row.stop_reason, Some(StopReason::DeckPolicy));
}

#[tokio::test]
async fn a_discard_after_a_save_replaces_it() {
    let (s, latch) = started().await;
    let held = latch.stop(true, StopReason::OwnerStop).await;
    assert_eq!(held, Some((StopIntent::Save, StopReason::OwnerStop)));
    assert_eq!(latch.first_discard(), None);
    let held = latch.stop(false, StopReason::DiskReserve).await;
    assert_eq!(held, Some((StopIntent::Discard, StopReason::DiskReserve)));
    let row = s.store.recording_get(FP, ID).await.unwrap().unwrap();
    assert_eq!(row.stop_reason, Some(StopReason::DiskReserve));
}

#[tokio::test]
async fn the_ceiling_is_remembered() {
    let (_s, latch) = started().await;
    assert!(!latch.ceiling());
    latch.mark_ceiling();
    assert!(latch.ceiling());
}

#[tokio::test]
async fn the_phase_never_goes_back() {
    let (s, latch) = started().await;
    latch.mark_stopping().await;
    let row = s.store.recording_get(FP, ID).await.unwrap().unwrap();
    assert_eq!(row.phase, Some(RecordingPhase::Stopping));
    latch.mark_finalizing().await;
    latch.mark_stopping().await;
    latch.mark_recording(42).await;
    let row = s.store.recording_get(FP, ID).await.unwrap().unwrap();
    // `recording` after `finalizing` keeps `finalizing` and records the start.
    assert_eq!(row.phase, Some(RecordingPhase::Finalizing));
    assert_eq!(row.recorder_started_at, Some(42));
}

#[tokio::test]
async fn a_row_that_is_not_running_answers_none() {
    let (s, latch) = started().await;
    s.store
        .recording_fail(FP, ID, "recording_failed", "m", StopReason::RecorderExited)
        .await
        .unwrap();
    assert_eq!(latch.stop(false, StopReason::PolicyOff).await, None);
    // The in-memory discard went out anyway.
    assert_eq!(latch.first_discard(), Some(StopReason::PolicyOff));
}
