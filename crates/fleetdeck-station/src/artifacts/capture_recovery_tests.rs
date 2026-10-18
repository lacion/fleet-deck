//! The startup recovery of the captures with recordings (R13 P5b task S2,
//! design §5.3, §5.4): the interrupted recording, the durable removal with a
//! failing sync and its retry, the staging of a committed recording, the
//! orphan scan with its garbage accounting, and the recovery gate of the
//! artifact reads.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs::File;
use std::io::Write;
use std::os::fd::OwnedFd;
use std::os::unix::fs::symlink;
use std::sync::Arc;

use fleetdeck_proto::artifacts::{ArtifactFileQuery, ArtifactGetQuery, ArtifactListQuery};
use fleetdeck_proto::capture::{CaptureCaller, CaptureKind, CaptureProvenance, code, text};
use fleetdeck_proto::coordinator_desktop_wire::CoordinatorContext;
use fleetdeck_proto::recording::{PEAK_RESERVATION, RecordingMeta, RecordingPhase, StopReason};
use fleetdeck_proto::{SessionId, StationId};

use super::{CaptureFileKind, DurableFs, SyncFs, import_capture};
use crate::artifacts::capture_import::tests::{FP, Recorder, Rig, rig};
use crate::desktop::Fail;
use crate::desktop::capture::{CAPTURE_QUOTA, Staging};
use crate::store::capture_ops::{
    Begin, CaptureState, GarbageArea, NewCaptureArtifact, RecordingBegin,
};

const A: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAA";
const B: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAB";
const C: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAC";
const D: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAD";

#[allow(clippy::unnecessary_wraps, reason = "the signature of the check")]
fn open_room(_: &OwnedFd, _: u64) -> Result<(), Fail> {
    Ok(())
}

fn source(bytes: &[u8]) -> File {
    let mut file = tempfile::tempfile().unwrap();
    file.write_all(bytes).unwrap();
    file
}

/// A recording row for `id` whose artifact folder is `art-<id>`.
async fn begin(r: &Rig, id: &str) -> String {
    let artifact = format!("art-{id}");
    let begin = r
        .store
        .recording_begin(RecordingBegin {
            deck_fp: FP,
            capture_id: id,
            action_hash: "h",
            monitor: "DP-1",
            artifact_id: &artifact,
            seconds: 5,
            link_epoch: "epoch",
            coordinator: None,
            now: 5,
        })
        .await
        .unwrap();
    assert_eq!(begin, Begin::Started);
    artifact
}

/// The staging folder of a capture with one file of `bytes`.
fn put_staging(r: &Rig, id: &str, bytes: &[u8]) {
    let mut staging = Staging::create(&r.captures, id, "capture.mp4").unwrap();
    staging.file().write_all(bytes).unwrap();
}

/// The destination folder `<artifacts>/<artifact>/1/capture.mp4`.
fn put_destination(r: &Rig, artifact: &str, bytes: &[u8]) {
    import_capture(
        &r.root,
        artifact,
        CaptureFileKind::Mp4,
        &mut source(bytes),
        bytes.len() as u64,
        &SyncFs,
        &open_room,
    )
    .unwrap();
}

fn new_artifact(id: &str, artifact: &str, size: u64) -> NewCaptureArtifact {
    NewCaptureArtifact {
        artifact_id: artifact.to_owned(),
        title: "Desktop recording".to_owned(),
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
            recording: Some(RecordingMeta {
                requested_seconds: 5,
                duration_ms: 4_000,
                width: 1920,
                height: 1080,
                stop_reason: StopReason::Duration,
            }),
        },
    }
}

async fn committed_bytes(r: &Rig, size: u64) {
    r.store
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

fn sync_fs() -> Arc<dyn DurableFs> {
    Arc::new(SyncFs)
}

#[tokio::test]
async fn a_running_recording_becomes_recording_interrupted_and_its_trees_go() {
    let r = rig().await;
    let artifact = begin(&r, A).await;
    r.store
        .recording_phase(FP, A, RecordingPhase::Recording, Some(9))
        .await
        .unwrap();
    put_staging(&r, A, b"half a clip");
    put_destination(&r, &artifact, b"half a clip");
    r.service.ensure_recovered().await.unwrap();
    let row = r.store.recording_get(FP, A).await.unwrap().unwrap();
    assert_eq!(row.op.state, CaptureState::Failed);
    assert_eq!(row.op.code.as_deref(), Some(code::RECORDING_INTERRUPTED));
    assert_eq!(row.op.message.as_deref(), Some(text::RECORDING_INTERRUPTED));
    assert_eq!(row.stop_reason, Some(StopReason::RecordingInterrupted));
    assert_eq!(row.op.reserved_bytes, 0);
    assert!(!r.captures.join(A).exists());
    assert!(!r.root.join(&artifact).exists());
    assert!(r.store.garbage_list().await.unwrap().is_empty());
    // The restart changes nothing more.
    r.service.ensure_recovered().await.unwrap();
}

/// A running recording row of a coordinator (its context is stored).
async fn begin_coordinated(r: &Rig, id: &str) -> String {
    let context = CoordinatorContext {
        grant_id: "grant-1".to_owned(),
        binding_id: "binding-1".to_owned(),
        generation: 3,
        caller_station: StationId::new("01HZZZZZZZZZZZZZZZZZZZZZZA"),
        caller_session: SessionId::new("01HZZZZZZZZZZZZZZZZZZZZZZB"),
        caller_label: "bold-fox".to_owned(),
    };
    let artifact = format!("art-{id}");
    let begin = r
        .store
        .recording_begin(RecordingBegin {
            deck_fp: FP,
            capture_id: id,
            action_hash: "h",
            monitor: "DP-1",
            artifact_id: &artifact,
            seconds: 5,
            link_epoch: "epoch",
            coordinator: Some(&context),
            now: 5,
        })
        .await
        .unwrap();
    assert_eq!(begin, Begin::Started);
    artifact
}

#[tokio::test]
async fn a_running_coordinator_recording_becomes_authority_unknown_and_its_trees_go() {
    let r = rig().await;
    let artifact = begin_coordinated(&r, A).await;
    // An owner recording beside it keeps `recording_interrupted`.
    let owner_artifact = begin(&r, B).await;
    put_staging(&r, A, b"half a clip");
    put_destination(&r, &artifact, b"half a clip");
    r.service.ensure_recovered().await.unwrap();
    let row = r.store.recording_get(FP, A).await.unwrap().unwrap();
    assert_eq!(row.op.state, CaptureState::Failed);
    assert_eq!(row.op.code.as_deref(), Some("authority_unknown"));
    assert_eq!(
        row.op.message.as_deref(),
        fleetdeck_proto::coordinator_desktop_text::text("authority_unknown")
    );
    assert_eq!(row.stop_reason, Some(StopReason::RecordingInterrupted));
    assert_eq!(row.op.reserved_bytes, 0);
    assert!(!r.captures.join(A).exists());
    assert!(!r.root.join(&artifact).exists());
    let owner = r.store.recording_get(FP, B).await.unwrap().unwrap();
    assert_eq!(owner.op.code.as_deref(), Some(code::RECORDING_INTERRUPTED));
    assert_eq!(owner.stop_reason, Some(StopReason::RecordingInterrupted));
    assert!(!r.root.join(&owner_artifact).exists());
}

#[tokio::test]
async fn a_failed_recording_keeps_its_reservation_until_the_removal_is_durable() {
    // A full removal syncs: the staging folder, `captures/`, `1/`, the
    // artifact folder and `artifacts/`.
    let probe = rig().await;
    let artifact = begin(&probe, A).await;
    put_staging(&probe, A, b"x");
    put_destination(&probe, &artifact, b"x");
    probe
        .store
        .recording_fail(FP, A, "recording_failed", "x", StopReason::RecorderExited)
        .await
        .unwrap();
    let rec = Arc::new(Recorder::default());
    probe
        .service
        .recover_captures_with(Arc::clone(&rec) as Arc<dyn DurableFs>)
        .await
        .unwrap();
    assert_eq!(rec.count(), 5);

    // Each of the five syncs fails once: the operation stays in its cleanup
    // state with the whole reservation, and the next start finishes it.
    for at in 0..5 {
        let r = rig().await;
        let artifact = begin(&r, A).await;
        put_staging(&r, A, b"x");
        put_destination(&r, &artifact, b"x");
        r.store
            .recording_fail(FP, A, "recording_failed", "x", StopReason::RecorderExited)
            .await
            .unwrap();
        let failing = Arc::new(Recorder::failing_at(at));
        r.service.recover_captures_with(failing).await.unwrap();
        let held = r.store.recording_get(FP, A).await.unwrap().unwrap();
        assert_eq!(held.op.state, CaptureState::Failed, "sync {at}");
        assert_eq!(held.op.reserved_bytes, PEAK_RESERVATION, "sync {at}");
        assert_eq!(r.store.captures_to_recover().await.unwrap().len(), 1);
        // The failure stays the first one.
        assert_eq!(held.op.code.as_deref(), Some("recording_failed"));

        r.service.recover_captures_with(sync_fs()).await.unwrap();
        let freed = r.store.recording_get(FP, A).await.unwrap().unwrap();
        assert_eq!(freed.op.reserved_bytes, 0, "sync {at}");
        assert!(!r.captures.join(A).exists());
        assert!(!r.root.join(&artifact).exists());
        assert!(r.store.captures_to_recover().await.unwrap().is_empty());
    }
}

#[tokio::test]
async fn the_staging_of_a_committed_recording_is_removed_durably_before_its_reservation_drops() {
    let r = rig().await;
    let artifact = begin(&r, A).await;
    put_staging(&r, A, b"the clip");
    put_destination(&r, &artifact, b"the clip");
    r.store
        .recording_commit(FP, A, new_artifact(A, &artifact, 8), 8)
        .await
        .unwrap();
    // A failing sync keeps the reservation and the committed artifact.
    r.service
        .recover_captures_with(Arc::new(Recorder::failing_at(1)))
        .await
        .unwrap();
    let held = r.store.recording_get(FP, A).await.unwrap().unwrap();
    assert_eq!(held.op.state, CaptureState::Committed);
    assert_eq!(held.op.reserved_bytes, 8);
    assert!(r.root.join(&artifact).join("1/capture.mp4").exists());
    // The next start finishes it; the artifact stays.
    r.service.recover_captures_with(sync_fs()).await.unwrap();
    let freed = r.store.recording_get(FP, A).await.unwrap().unwrap();
    assert_eq!(freed.op.state, CaptureState::Committed);
    assert_eq!(freed.op.reserved_bytes, 0);
    assert!(!r.captures.join(A).exists());
    assert_eq!(
        std::fs::read(r.root.join(&artifact).join("1/capture.mp4")).unwrap(),
        b"the clip"
    );
}

#[tokio::test]
async fn the_scan_removes_unowned_staging_entries_and_follows_no_link() {
    let r = rig().await;
    let elsewhere = r.data.join("elsewhere");
    std::fs::create_dir(&elsewhere).unwrap();
    std::fs::write(elsewhere.join("keep"), b"x").unwrap();
    std::fs::create_dir_all(r.captures.join("orphan/deep")).unwrap();
    std::fs::write(r.captures.join("orphan/a"), b"12345").unwrap();
    std::fs::write(r.captures.join("orphan/deep/b"), b"1234567").unwrap();
    std::fs::write(r.captures.join("stray-file"), b"x").unwrap();
    symlink(&elsewhere, r.captures.join("link-to-dir")).unwrap();
    r.service.ensure_recovered().await.unwrap();
    assert_eq!(std::fs::read_dir(&r.captures).unwrap().count(), 0);
    assert!(elsewhere.join("keep").exists());
    assert!(r.store.garbage_list().await.unwrap().is_empty());
}

#[tokio::test]
async fn garbage_counts_in_the_quota_until_its_durable_removal_finishes() {
    let r = rig().await;
    std::fs::create_dir_all(r.captures.join("orphan/deep")).unwrap();
    std::fs::write(r.captures.join("orphan/a"), b"12345").unwrap();
    std::fs::write(r.captures.join("orphan/deep/b"), b"1234567").unwrap();
    // The first sync fails: the entry stays and its 12 bytes are counted.
    r.service
        .recover_captures_with(Arc::new(Recorder::failing_at(0)))
        .await
        .unwrap();
    assert_eq!(
        r.store.garbage_list().await.unwrap(),
        vec![(GarbageArea::Staging, "orphan".to_owned(), 12)]
    );
    assert!(r.captures.join("orphan").exists());
    // Without the garbage the peak reservation fits; with it, it does not.
    committed_bytes(&r, CAPTURE_QUOTA - PEAK_RESERVATION - 11).await;
    let refused = r
        .store
        .recording_begin(RecordingBegin {
            deck_fp: FP,
            capture_id: A,
            action_hash: "h",
            monitor: "DP-1",
            artifact_id: "art-a",
            seconds: 5,
            link_epoch: "epoch",
            coordinator: None,
            now: 5,
        })
        .await
        .unwrap();
    assert_eq!(refused, Begin::Quota);
    // The next start removes it durably and deletes its row.
    r.service.recover_captures_with(sync_fs()).await.unwrap();
    assert!(r.store.garbage_list().await.unwrap().is_empty());
    assert!(!r.captures.join("orphan").exists());
    assert_eq!(begin(&r, A).await, "art-01ARZ3NDEKTSV4RRFFQ69G5FAA");
}

#[tokio::test]
async fn the_scan_removes_the_destination_of_a_failed_operation_with_no_version() {
    let r = rig().await;
    // A failed operation whose reservation is zero already; a power loss
    // restored its folders after an unsynced unlink.
    let artifact = begin(&r, C).await;
    r.store
        .recording_fail(FP, C, "recording_failed", "x", StopReason::RecorderExited)
        .await
        .unwrap();
    r.store.capture_cleaned(FP, C).await.unwrap();
    put_destination(&r, &artifact, b"restored");
    put_staging(&r, C, b"restored");
    assert!(r.store.captures_to_recover().await.unwrap().is_empty());
    r.service.ensure_recovered().await.unwrap();
    assert!(!r.root.join(&artifact).exists());
    assert!(!r.captures.join(C).exists());
    let row = r.store.recording_get(FP, C).await.unwrap().unwrap();
    assert_eq!(row.op.state, CaptureState::Failed);
    assert_eq!(row.op.code.as_deref(), Some("recording_failed"));
}

#[tokio::test]
async fn the_scan_never_touches_an_ordinary_or_a_committed_artifact_folder() {
    let r = rig().await;
    std::fs::create_dir_all(r.root.join("ORDINARY/1")).unwrap();
    std::fs::write(r.root.join("ORDINARY/1/a.html"), b"x").unwrap();
    std::fs::create_dir_all(r.root.join(".staging/tmp")).unwrap();
    // A committed recording keeps its folder; its staging goes.
    let artifact = begin(&r, D).await;
    put_staging(&r, D, b"clip");
    put_destination(&r, &artifact, b"clip");
    r.store
        .recording_commit(FP, D, new_artifact(D, &artifact, 4), 0)
        .await
        .unwrap();
    r.service.ensure_recovered().await.unwrap();
    assert!(r.root.join("ORDINARY/1/a.html").exists());
    assert!(r.root.join(".staging/tmp").exists());
    assert_eq!(
        std::fs::read(r.root.join(&artifact).join("1/capture.mp4")).unwrap(),
        b"clip"
    );
    assert!(!r.captures.join(D).exists(), "its staging copy is garbage");
}

#[tokio::test]
async fn the_scan_keeps_the_staging_of_a_running_operation_and_drops_a_stale_garbage_row() {
    let r = rig().await;
    begin(&r, B).await;
    put_staging(&r, B, b"recording");
    r.store
        .garbage_record(GarbageArea::Staging, "long-gone", 99)
        .await
        .unwrap();
    // The scan alone, as a running operation exists.
    r.service.scan_orphans(&sync_fs()).await.unwrap();
    assert!(r.captures.join(B).join("capture.mp4").exists());
    assert!(r.store.garbage_list().await.unwrap().is_empty());
}

#[tokio::test]
async fn an_artifact_read_waits_for_the_recovery() {
    let r = rig().await;
    let artifact = begin(&r, A).await;
    put_destination(&r, &artifact, b"half");
    r.store
        .recording_fail(FP, A, "recording_failed", "x", StopReason::RecorderExited)
        .await
        .unwrap();
    // `list` runs the recovery first: the reservation is free when it answers.
    let list = r.service.list(ArtifactListQuery::default()).await.unwrap();
    assert!(list.artifacts.is_empty());
    let row = r.store.recording_get(FP, A).await.unwrap().unwrap();
    assert_eq!(row.op.reserved_bytes, 0);
    assert!(!r.root.join(&artifact).exists());

    // `get` and `file` wait too; two reads and a start share one recovery.
    let r = rig().await;
    let artifact = begin(&r, B).await;
    put_destination(&r, &artifact, b"half");
    r.store
        .recording_fail(FP, B, "recording_failed", "x", StopReason::RecorderExited)
        .await
        .unwrap();
    let get = r.service.get(ArtifactGetQuery {
        artifact_id: "nope".to_owned(),
        version: None,
    });
    let file = r.service.file(ArtifactFileQuery {
        artifact_id: "nope".to_owned(),
        version: 1,
        path: "capture.mp4".to_owned(),
        offset: None,
        length: None,
    });
    let (get, file, start) = tokio::join!(get, file, r.service.ensure_recovered());
    assert_eq!(get.unwrap_err().code, "unknown_artifact");
    assert_eq!(file.unwrap_err().code, "unknown_artifact");
    start.unwrap();
    let row = r.store.recording_get(FP, B).await.unwrap().unwrap();
    assert_eq!(row.op.reserved_bytes, 0);
    assert!(!r.root.join(&artifact).exists());
}
