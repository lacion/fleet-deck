//! The recording rows (R13 P5b task S2): begin, repeat, conflict, the quota
//! with garbage, the stop latch, the atomic commit with its tombstone and
//! latch guards, failure with its reservation, and the garbage accounting.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use fleetdeck_proto::artifacts::ArtifactKind;
use fleetdeck_proto::capture::{CaptureCaller, CaptureKind, CaptureProvenance};
use fleetdeck_proto::recording::{PEAK_RESERVATION, RecordingMeta, RecordingPhase, StopReason};
use fleetdeck_testkit::Det;

use super::{GarbageArea, RecordingBegin, StopIntent};
use crate::desktop::capture::{CAPTURE_QUOTA, MAX_PNG_BYTES};
use crate::store::artifacts::CommitError;
use crate::store::capture_ops::{Begin, CaptureState, Lookup, NewCaptureArtifact};
use crate::store::{Store, StoreConfig};

const FP: &str = "ab00000000000000000000000000000000000000000000000000000000000001";
const A: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAA";
const B: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAB";

async fn store() -> Store {
    let det = Det::new();
    let store = Store::open_in_memory(det.clock(), det.ids(), StoreConfig::default())
        .await
        .unwrap();
    store.recover(Vec::new()).await.unwrap();
    store
}

fn request<'a>(id: &'a str, hash: &'a str, artifact: &'a str) -> RecordingBegin<'a> {
    RecordingBegin {
        deck_fp: FP,
        capture_id: id,
        action_hash: hash,
        monitor: "DP-1",
        artifact_id: artifact,
        seconds: 5,
        link_epoch: "epoch-1",
        coordinator: None,
        now: 10,
    }
}

async fn begin(store: &Store, id: &str, hash: &str) -> Begin {
    let artifact = format!("art-{id}");
    store
        .recording_begin(request(id, hash, &artifact))
        .await
        .unwrap()
}

fn meta(reason: StopReason) -> RecordingMeta {
    RecordingMeta {
        requested_seconds: 5,
        duration_ms: 4_900,
        width: 1920,
        height: 1080,
        stop_reason: reason,
    }
}

fn new_artifact(id: &str, size: u64, reason: StopReason) -> NewCaptureArtifact {
    NewCaptureArtifact {
        artifact_id: format!("art-{id}"),
        title: "Desktop recording \u{b7} srv/DP-1".to_owned(),
        size,
        sha256: "cd".repeat(32),
        provenance: CaptureProvenance {
            capture_id: id.to_owned(),
            station_id: "srv".to_owned(),
            monitor: "DP-1".to_owned(),
            kind: CaptureKind::Recording,
            caller: CaptureCaller::Owner,
            caller_station: None,
            caller_session: None,
            caller_grant_id: None,
            at: 7,
            recording: Some(meta(reason)),
        },
    }
}

async fn counts(store: &Store) -> (i64, i64) {
    store
        .db()
        .call(|conn| {
            let a = conn.query_row("SELECT COUNT(*) FROM artifacts", [], |r| r.get(0))?;
            let v = conn.query_row("SELECT COUNT(*) FROM artifact_versions", [], |r| r.get(0))?;
            Ok((a, v))
        })
        .await
        .unwrap()
}

/// A committed capture version of `size` bytes, as the quota counts it.
async fn committed_bytes(store: &Store, size: u64) {
    store
        .db()
        .call(move |conn| {
            conn.execute_batch(&format!(
                "INSERT INTO artifacts VALUES ('ARTC', NULL, 't', 'image', 1, 'owner', 1, 1);
                 INSERT INTO artifact_versions VALUES ('ARTC', 1, {size}, 's', 'm', '[]', 1, '{{}}');"
            ))?;
            Ok(())
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn a_new_recording_starts_with_the_peak_reservation_and_its_columns() {
    let s = store().await;
    assert_eq!(begin(&s, A, "h1").await, Begin::Started);
    let row = s.recording_get(FP, A).await.unwrap().unwrap();
    assert_eq!(row.op.kind, CaptureKind::Recording);
    assert_eq!(row.op.state, CaptureState::Running);
    assert_eq!(row.op.reserved_bytes, PEAK_RESERVATION);
    assert_eq!(row.op.artifact_id, format!("art-{A}"));
    assert_eq!(row.requested_seconds, 5);
    assert_eq!(row.link_epoch.as_deref(), Some("epoch-1"));
    assert_eq!(row.phase, Some(RecordingPhase::Preparing));
    assert_eq!(row.stop_intent, StopIntent::None);
    assert_eq!(row.stop_reason, None);
    assert_eq!(row.recorder_started_at, None);
    assert_eq!(counts(&s).await, (0, 0));
}

#[tokio::test]
async fn a_repeat_returns_the_row_and_any_other_request_conflicts() {
    let s = store().await;
    assert_eq!(begin(&s, A, "h1").await, Begin::Started);
    let Begin::Existing(row) = begin(&s, A, "h1").await else {
        panic!("a repeat returns the row");
    };
    assert_eq!(row.capture_id, A);
    assert_eq!(row.kind, CaptureKind::Recording);
    // Another hash, another monitor and other seconds conflict.
    assert_eq!(begin(&s, A, "h2").await, Begin::Conflict);
    let mut other = request(A, "h1", "art-x");
    other.monitor = "HDMI-A-1";
    assert_eq!(s.recording_begin(other).await.unwrap(), Begin::Conflict);
    let mut other = request(A, "h1", "art-x");
    other.seconds = 6;
    assert_eq!(s.recording_begin(other).await.unwrap(), Begin::Conflict);
    // A screenshot with the same capture ID and hash is another kind.
    assert_eq!(
        s.capture_begin(FP, A, "h1", "DP-1", "art-y", 11)
            .await
            .unwrap(),
        Begin::Conflict
    );
    // And a recording on the ID of a screenshot conflicts too.
    assert_eq!(
        s.capture_begin(FP, B, "h1", "DP-1", "art-z", 11)
            .await
            .unwrap(),
        Begin::Started
    );
    assert_eq!(begin(&s, B, "h1").await, Begin::Conflict);
    // The repeat did not change the row.
    let row = s.recording_get(FP, A).await.unwrap().unwrap();
    assert_eq!(row.op.artifact_id, format!("art-{A}"));
    assert_eq!(row.op.reserved_bytes, PEAK_RESERVATION);
    // A screenshot row is no recording.
    assert!(s.recording_get(FP, B).await.unwrap().is_none());
    assert!(s.recording_get(FP, "none").await.unwrap().is_none());
}

#[tokio::test]
async fn the_recording_quota_counts_commits_reservations_and_garbage() {
    // Committed bytes alone leave exactly room for the peak reservation.
    let s = store().await;
    committed_bytes(&s, CAPTURE_QUOTA - PEAK_RESERVATION).await;
    assert_eq!(begin(&s, A, "h1").await, Begin::Started);
    // The reservation of the first recording now counts.
    assert_eq!(begin(&s, B, "h2").await, Begin::Quota);

    // One byte of garbage crosses the quota; its removal frees it.
    let s = store().await;
    committed_bytes(&s, CAPTURE_QUOTA - PEAK_RESERVATION).await;
    s.garbage_record(GarbageArea::Artifacts, "ORPHAN", 1)
        .await
        .unwrap();
    assert_eq!(begin(&s, A, "h1").await, Begin::Quota);
    assert!(s.recording_get(FP, A).await.unwrap().is_none());
    s.garbage_delete(GarbageArea::Artifacts, "ORPHAN")
        .await
        .unwrap();
    assert_eq!(begin(&s, A, "h1").await, Begin::Started);
}

#[tokio::test]
async fn the_screenshot_quota_counts_garbage_too() {
    let s = store().await;
    committed_bytes(&s, CAPTURE_QUOTA - MAX_PNG_BYTES).await;
    s.garbage_record(GarbageArea::Staging, "STAGE", 1)
        .await
        .unwrap();
    assert_eq!(
        s.capture_begin(FP, A, "h1", "DP-1", "art-a", 1)
            .await
            .unwrap(),
        Begin::Quota
    );
    s.garbage_delete(GarbageArea::Staging, "STAGE")
        .await
        .unwrap();
    assert_eq!(
        s.capture_begin(FP, A, "h1", "DP-1", "art-a", 1)
            .await
            .unwrap(),
        Begin::Started
    );
}

#[tokio::test]
async fn the_phase_moves_and_the_recorder_start_is_kept() {
    let s = store().await;
    begin(&s, A, "h1").await;
    s.recording_phase(FP, A, RecordingPhase::Recording, Some(1_234))
        .await
        .unwrap();
    let row = s.recording_get(FP, A).await.unwrap().unwrap();
    assert_eq!(row.phase, Some(RecordingPhase::Recording));
    assert_eq!(row.recorder_started_at, Some(1_234));
    s.recording_phase(FP, A, RecordingPhase::Stopping, None)
        .await
        .unwrap();
    let row = s.recording_get(FP, A).await.unwrap().unwrap();
    assert_eq!(row.phase, Some(RecordingPhase::Stopping));
    assert_eq!(
        row.recorder_started_at,
        Some(1_234),
        "an earlier start stays"
    );
}

#[tokio::test]
async fn the_latch_goes_none_save_discard_and_never_back() {
    let s = store().await;
    // none -> save -> discard
    begin(&s, A, "h1").await;
    assert_eq!(
        s.recording_latch(FP, A, true, StopReason::OwnerStop)
            .await
            .unwrap(),
        Some((StopIntent::Save, StopReason::OwnerStop))
    );
    // A second save keeps the first save reason.
    assert_eq!(
        s.recording_latch(FP, A, true, StopReason::Duration)
            .await
            .unwrap(),
        Some((StopIntent::Save, StopReason::OwnerStop))
    );
    assert_eq!(
        s.recording_latch(FP, A, false, StopReason::PolicyOff)
            .await
            .unwrap(),
        Some((StopIntent::Discard, StopReason::PolicyOff))
    );
    // The first discard reason wins; a save after a discard is refused.
    assert_eq!(
        s.recording_latch(FP, A, false, StopReason::ScreenLocked)
            .await
            .unwrap(),
        Some((StopIntent::Discard, StopReason::PolicyOff))
    );
    assert_eq!(
        s.recording_latch(FP, A, true, StopReason::Duration)
            .await
            .unwrap(),
        Some((StopIntent::Discard, StopReason::PolicyOff))
    );
    let row = s.recording_get(FP, A).await.unwrap().unwrap();
    assert_eq!(row.stop_intent, StopIntent::Discard);
    assert_eq!(row.stop_reason, Some(StopReason::PolicyOff));

    // none -> discard
    begin(&s, B, "h2").await;
    assert_eq!(
        s.recording_latch(FP, B, false, StopReason::LeaseExpired)
            .await
            .unwrap(),
        Some((StopIntent::Discard, StopReason::LeaseExpired))
    );
}

#[tokio::test]
async fn the_latch_refuses_a_reason_that_does_not_fit_and_a_row_that_does_not_run() {
    let s = store().await;
    begin(&s, A, "h1").await;
    assert!(
        s.recording_latch(FP, A, true, StopReason::PolicyOff)
            .await
            .is_err()
    );
    assert!(
        s.recording_latch(FP, A, false, StopReason::Duration)
            .await
            .is_err()
    );
    assert_eq!(
        s.recording_get(FP, A).await.unwrap().unwrap().stop_intent,
        StopIntent::None
    );
    // An unknown ID, a screenshot and a failed recording have no latch.
    assert_eq!(
        s.recording_latch(FP, "none", false, StopReason::PolicyOff)
            .await
            .unwrap(),
        None
    );
    s.capture_begin(FP, B, "h2", "DP-1", "art-b", 1)
        .await
        .unwrap();
    assert_eq!(
        s.recording_latch(FP, B, false, StopReason::PolicyOff)
            .await
            .unwrap(),
        None
    );
    s.recording_fail(FP, A, "recording_failed", "x", StopReason::RecorderExited)
        .await
        .unwrap();
    assert_eq!(
        s.recording_latch(FP, A, false, StopReason::PolicyOff)
            .await
            .unwrap(),
        None
    );
}

#[tokio::test]
async fn a_commit_makes_the_video_artifact_in_one_transaction_and_keeps_the_staging_bytes() {
    let s = store().await;
    begin(&s, A, "h1").await;
    let committed = s
        .recording_commit(FP, A, new_artifact(A, 1_000, StopReason::OwnerStop), 4_096)
        .await
        .unwrap();
    assert_eq!(committed.artifact.kind, ArtifactKind::Video);
    assert_eq!(committed.artifact.author, "owner");
    assert_eq!(committed.version.files, ["capture.mp4"]);
    assert_eq!(committed.version.size, 1_000);
    assert_eq!(counts(&s).await, (1, 1));
    let row = s.recording_get(FP, A).await.unwrap().unwrap();
    assert_eq!(row.op.state, CaptureState::Committed);
    assert_eq!(
        row.op.reserved_bytes, 4_096,
        "the staging copy stays reserved"
    );
    assert_eq!(row.op.sha256, Some("cd".repeat(32)));
    assert_eq!(row.stop_reason, Some(StopReason::OwnerStop));
    assert_eq!(row.duration_ms, Some(4_900));
    assert_eq!((row.width, row.height), (Some(1920), Some(1080)));
    assert!(row.ended_at.is_some());
    // The lookup reads back the video kind and the recording provenance.
    let Lookup::Committed {
        artifact, version, ..
    } = s.capture_lookup(FP, A).await.unwrap()
    else {
        panic!("a committed recording");
    };
    assert_eq!(artifact.kind, ArtifactKind::Video);
    assert_eq!(
        version.capture.unwrap().recording,
        Some(meta(StopReason::OwnerStop))
    );
    // The reservation drops only when the staging copy is removed.
    s.capture_staging_cleaned(FP, A).await.unwrap();
    assert_eq!(
        s.recording_get(FP, A)
            .await
            .unwrap()
            .unwrap()
            .op
            .reserved_bytes,
        0
    );
    // The commit is the only way: a second one finds no running row.
    assert!(
        s.recording_commit(FP, A, new_artifact(A, 1_000, StopReason::OwnerStop), 0)
            .await
            .is_err()
    );
    assert_eq!(counts(&s).await, (1, 1));
}

#[tokio::test]
async fn the_accounting_of_a_committed_recording_is_its_size_plus_its_staging_bytes() {
    let s = store().await;
    begin(&s, A, "h1").await;
    s.recording_commit(FP, A, new_artifact(A, 1_000, StopReason::Duration), 3_000)
        .await
        .unwrap();
    // 1_000 committed + 3_000 staging + the rest: one byte over the room that
    // the peak reservation of a second recording needs.
    committed_bytes(&s, CAPTURE_QUOTA - PEAK_RESERVATION - 3_999).await;
    assert_eq!(begin(&s, B, "h2").await, Begin::Quota);
    // The reservation drops with the durable removal of the staging copy.
    s.capture_staging_cleaned(FP, A).await.unwrap();
    assert_eq!(begin(&s, B, "h2").await, Begin::Started);
}

#[tokio::test]
async fn a_tombstone_refuses_the_commit_and_changes_nothing() {
    let s = store().await;
    begin(&s, A, "h1").await;
    s.artifact_delete_begin(format!("art-{A}")).await.unwrap();
    let err = s
        .recording_commit(FP, A, new_artifact(A, 10, StopReason::Duration), 10)
        .await
        .unwrap_err();
    assert!(matches!(err, CommitError::UnknownArtifact(_)), "{err:?}");
    assert_eq!(counts(&s).await, (0, 0));
    let row = s.recording_get(FP, A).await.unwrap().unwrap();
    assert_eq!(row.op.state, CaptureState::Running);
    assert_eq!(row.op.reserved_bytes, PEAK_RESERVATION);
}

#[tokio::test]
async fn a_discard_latch_a_wrong_provenance_or_another_kind_refuses_the_commit() {
    let s = store().await;
    // The discard latch (design §7.6): nothing is committed.
    begin(&s, A, "h1").await;
    s.recording_latch(FP, A, false, StopReason::DeckPolicy)
        .await
        .unwrap();
    assert!(
        s.recording_commit(FP, A, new_artifact(A, 10, StopReason::Duration), 10)
            .await
            .is_err()
    );
    assert_eq!(counts(&s).await, (0, 0));
    assert_eq!(
        s.recording_get(FP, A).await.unwrap().unwrap().op.state,
        CaptureState::Running
    );

    // A provenance that is not a recording, or has a discard reason.
    begin(&s, B, "h2").await;
    let mut screenshot = new_artifact(B, 10, StopReason::Duration);
    screenshot.provenance.kind = CaptureKind::Screenshot;
    assert!(s.recording_commit(FP, B, screenshot, 10).await.is_err());
    let mut none = new_artifact(B, 10, StopReason::Duration);
    none.provenance.recording = None;
    assert!(s.recording_commit(FP, B, none, 10).await.is_err());
    let bad_reason = new_artifact(B, 10, StopReason::PolicyOff);
    assert!(s.recording_commit(FP, B, bad_reason, 10).await.is_err());
    // The screenshot commit does not take a recording row.
    assert!(
        s.capture_commit(FP, B, new_artifact(B, 10, StopReason::Duration))
            .await
            .is_err()
    );
    assert_eq!(counts(&s).await, (0, 0));
    assert_eq!(
        s.recording_get(FP, B).await.unwrap().unwrap().op.state,
        CaptureState::Running
    );
}

#[tokio::test]
async fn a_failed_recording_keeps_its_reservation_until_it_is_cleaned() {
    let s = store().await;
    begin(&s, A, "h1").await;
    s.recording_phase(FP, A, RecordingPhase::Recording, Some(50))
        .await
        .unwrap();
    s.recording_fail(
        FP,
        A,
        "recording_interrupted",
        "The recording was interrupted before it was saved.",
        StopReason::LinkClosed,
    )
    .await
    .unwrap();
    let row = s.recording_get(FP, A).await.unwrap().unwrap();
    assert_eq!(row.op.state, CaptureState::Failed);
    assert_eq!(row.op.code.as_deref(), Some("recording_interrupted"));
    assert_eq!(row.op.reserved_bytes, PEAK_RESERVATION);
    assert_eq!(row.stop_reason, Some(StopReason::LinkClosed));
    assert!(row.ended_at.is_some());
    assert_eq!(s.captures_to_recover().await.unwrap().len(), 1);
    // A repeat answers from the row; a second failure changes nothing.
    s.recording_fail(FP, A, "recording_failed", "x", StopReason::RecorderExited)
        .await
        .unwrap();
    let again = s.recording_get(FP, A).await.unwrap().unwrap();
    assert_eq!(again.op.code.as_deref(), Some("recording_interrupted"));
    assert_eq!(again.stop_reason, Some(StopReason::LinkClosed));
    s.capture_cleaned(FP, A).await.unwrap();
    assert_eq!(
        s.recording_get(FP, A)
            .await
            .unwrap()
            .unwrap()
            .op
            .reserved_bytes,
        0
    );
    assert!(s.captures_to_recover().await.unwrap().is_empty());
}

#[tokio::test]
async fn a_committed_row_with_a_reservation_is_recovered_until_its_staging_is_cleaned() {
    let s = store().await;
    begin(&s, A, "h1").await;
    s.recording_commit(FP, A, new_artifact(A, 10, StopReason::Duration), 10)
        .await
        .unwrap();
    let todo = s.captures_to_recover().await.unwrap();
    assert_eq!(todo.len(), 1);
    assert_eq!(todo[0].state, CaptureState::Committed);
    s.capture_staging_cleaned(FP, A).await.unwrap();
    assert!(s.captures_to_recover().await.unwrap().is_empty());
}

#[tokio::test]
async fn garbage_rows_are_counted_replaced_listed_and_deleted() {
    let s = store().await;
    s.garbage_record(GarbageArea::Staging, "S1", 5)
        .await
        .unwrap();
    s.garbage_record(GarbageArea::Artifacts, "A1", 7)
        .await
        .unwrap();
    s.garbage_record(GarbageArea::Staging, "S1", 9)
        .await
        .unwrap();
    assert_eq!(
        s.garbage_list().await.unwrap(),
        vec![
            (GarbageArea::Artifacts, "A1".to_owned(), 7),
            (GarbageArea::Staging, "S1".to_owned(), 9),
        ]
    );
    s.garbage_delete(GarbageArea::Staging, "S1").await.unwrap();
    s.garbage_delete(GarbageArea::Staging, "S1").await.unwrap();
    assert_eq!(s.garbage_list().await.unwrap().len(), 1);
}

#[tokio::test]
async fn the_scan_sets_keep_running_and_reserved_rows_and_name_the_orphans() {
    let s = store().await;
    // A running recording and a failed one that still holds its reservation.
    begin(&s, A, "h1").await;
    begin(&s, B, "h2").await;
    s.recording_fail(FP, B, "recording_failed", "x", StopReason::RecorderExited)
        .await
        .unwrap();
    // A failed one whose cleanup finished, and a committed one with a version.
    let c = "01ARZ3NDEKTSV4RRFFQ69G5FAC";
    let d = "01ARZ3NDEKTSV4RRFFQ69G5FAD";
    begin(&s, c, "h3").await;
    s.recording_fail(FP, c, "recording_failed", "x", StopReason::RecorderExited)
        .await
        .unwrap();
    s.capture_cleaned(FP, c).await.unwrap();
    begin(&s, d, "h4").await;
    s.recording_commit(FP, d, new_artifact(d, 10, StopReason::Duration), 0)
        .await
        .unwrap();
    let sets = s.capture_scan_sets().await.unwrap();
    assert_eq!(
        sets.staging_keep,
        [A.to_owned(), B.to_owned()].into_iter().collect()
    );
    // Only the failed one with a zero reservation and no version is garbage.
    assert_eq!(
        sets.artifact_orphans,
        [format!("art-{c}")].into_iter().collect()
    );
}
