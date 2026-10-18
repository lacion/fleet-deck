//! The live recordings of the station: the slot holder, the active
//! recording of a deck, the stop of a recording without a live handle, the
//! shutdown, and the removal when the recording ends.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use fleetdeck_proto::recording::StopReason;

use super::latch::Latch;
use super::registry::{PendingStop, StopOutcome};
use super::rig_tests::{ID, ID2, T};
use crate::artifacts::capture_import::tests::FP;
use crate::store::capture_ops::{Begin, RecordingBegin, StopIntent};

const OTHER_DECK: &str = "cd00000000000000000000000000000000000000000000000000000000000002";

#[tokio::test]
async fn the_recording_of_a_deck_holds_the_slot_for_that_deck_only() {
    let t = T::new().await;
    assert!(t.desktop().recording_of(FP).is_none());
    let run = t.start(ID, "DP-1", 60);
    t.until_recording(ID).await;
    assert_eq!(t.desktop().recording_of(FP).unwrap().capture_id, ID);
    assert!(t.desktop().recording_of(OTHER_DECK).is_none());
    t.desktop()
        .stop_recording(t.store(), FP, ID, StopReason::OwnerStop)
        .await
        .unwrap();
    run.await.unwrap().unwrap();
    assert!(t.desktop().recording_of(FP).is_none());
}

#[tokio::test]
async fn a_screenshot_that_holds_the_slot_is_not_a_recording() {
    let t = T::new().await;
    let guard = t.desktop().hold_screenshot();
    assert!(t.desktop().recording_of(FP).is_none());
    drop(guard);
}

#[tokio::test]
async fn a_stop_without_a_live_handle_writes_the_latch() {
    let t = T::new().await;
    let begin = t
        .store()
        .recording_begin(RecordingBegin {
            deck_fp: FP,
            capture_id: ID2,
            action_hash: "h",
            monitor: "DP-1",
            artifact_id: "ART",
            seconds: 5,
            link_epoch: "e",
            now: 1,
        })
        .await
        .unwrap();
    assert!(matches!(begin, Begin::Started));
    let outcome = t
        .desktop()
        .stop_recording(t.store(), FP, ID2, StopReason::OwnerStop)
        .await
        .unwrap();
    assert_eq!(outcome, StopOutcome::Latched);
    let row = t.row(ID2).await.unwrap();
    assert_eq!(row.stop_intent, StopIntent::Save);
    assert_eq!(row.stop_reason, Some(StopReason::OwnerStop));
    // A discard after the save replaces it; another deck's stop touches
    // nothing.
    t.desktop()
        .stop_recording(t.store(), FP, ID2, StopReason::DeckPolicy)
        .await
        .unwrap();
    let outcome = t
        .desktop()
        .stop_recording(t.store(), OTHER_DECK, ID2, StopReason::DeckPolicy)
        .await
        .unwrap();
    assert_eq!(outcome, StopOutcome::NotRunning);
    assert_eq!(
        t.row(ID2).await.unwrap().stop_reason,
        Some(StopReason::DeckPolicy)
    );
}

#[tokio::test]
async fn a_stop_of_an_unknown_or_finished_recording_does_nothing() {
    let t = T::new().await;
    let outcome = t
        .desktop()
        .stop_recording(t.store(), FP, ID, StopReason::OwnerStop)
        .await
        .unwrap();
    assert_eq!(outcome, StopOutcome::NotRunning);
    t.run(ID, "DP-1", 1).await.unwrap();
    let outcome = t
        .desktop()
        .stop_recording(t.store(), FP, ID, StopReason::DeckPolicy)
        .await
        .unwrap();
    assert_eq!(outcome, StopOutcome::NotRunning);
    assert_eq!(
        t.row(ID).await.unwrap().stop_reason,
        Some(StopReason::Duration)
    );
}

#[tokio::test]
async fn the_shutdown_with_nothing_running_does_nothing() {
    let t = T::new().await;
    t.desktop().shutdown_recordings();
    assert!(t.run(ID, "DP-1", 1).await.is_ok());
}

#[tokio::test]
async fn a_pending_stop_follows_the_latch_rules_and_is_bounded() {
    let t = T::new().await;
    let reg = &t.desktop().recordings;
    let save = PendingStop {
        save: true,
        reason: StopReason::OwnerStop,
    };
    let discard = PendingStop {
        save: false,
        reason: StopReason::DeckPolicy,
    };
    // A discard replaces a save, never the reverse; the first discard wins.
    reg.stop_pending_or_live(FP, ID, save);
    assert_eq!(reg.pending_stop(FP, ID), Some(save));
    reg.stop_pending_or_live(FP, ID, discard);
    assert_eq!(reg.pending_stop(FP, ID), Some(discard));
    reg.stop_pending_or_live(FP, ID, save);
    assert_eq!(reg.pending_stop(FP, ID), Some(discard));
    reg.stop_pending_or_live(
        FP,
        ID,
        PendingStop {
            save: false,
            reason: StopReason::PolicyOff,
        },
    );
    assert_eq!(reg.pending_stop(FP, ID), Some(discard));
    // At most 64 stay; the oldest goes.
    for n in 0..70 {
        reg.stop_pending_or_live(FP, &format!("cap-{n}"), save);
    }
    assert_eq!(reg.pending_count(), 64);
    assert_eq!(reg.pending_stop(FP, ID), None);
    assert_eq!(reg.pending_stop(FP, "cap-5"), None);
    assert_eq!(reg.pending_stop(FP, "cap-6"), Some(save));
    assert_eq!(reg.pending_stop(FP, "cap-69"), Some(save));
}

#[tokio::test]
async fn a_recording_that_registers_takes_its_pending_stop() {
    let t = T::new().await;
    let reg = &t.desktop().recordings;
    reg.stop_pending_or_live(
        FP,
        ID,
        PendingStop {
            save: false,
            reason: StopReason::DeckPolicy,
        },
    );
    let latch = Latch::new(t.store().clone(), FP, ID);
    assert_eq!(latch.first_stop(), None);
    let live = reg.insert(&latch);
    assert_eq!(latch.first_discard(), Some(StopReason::DeckPolicy));
    assert_eq!(reg.pending_count(), 0);
    assert!(reg.get(FP, ID).is_some());
    drop(live);
    assert!(reg.get(FP, ID).is_none());
}
