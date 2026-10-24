//! The capture operation rows (R13 P5a task D2): begin, repeat, conflict,
//! the quota with the full reservation, the atomic commit, the tombstone
//! guard, failure and the lookup.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use fleetdeck_proto::capture::{CaptureCaller, CaptureKind, CaptureProvenance};
use fleetdeck_testkit::Det;

use super::{Begin, CaptureState, Lookup, NewCaptureArtifact};
use crate::desktop::capture::{CAPTURE_QUOTA, MAX_PNG_BYTES};
use crate::store::artifacts::CommitError;
use crate::store::{Store, StoreConfig};

const FP: &str = "ab00000000000000000000000000000000000000000000000000000000000001";
const A: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAA";
const B: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAB";
const C: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAC";
const ART_A: &str = "art-01ARZ3NDEKTSV4RRFFQ69G5FAA";

async fn store() -> Store {
    let det = Det::new();
    let store = Store::open_in_memory(det.clock(), det.ids(), StoreConfig::default())
        .await
        .unwrap();
    store.recover(Vec::new()).await.unwrap();
    store
}

async fn begin(store: &Store, id: &str, hash: &str) -> Begin {
    store
        .capture_begin(FP, id, hash, "DP-1", &format!("art-{id}"), 10)
        .await
        .unwrap()
}

fn new_artifact(id: &str, size: u64) -> NewCaptureArtifact {
    NewCaptureArtifact {
        artifact_id: format!("art-{id}"),
        title: "Desktop capture \u{b7} srv/DP-1".to_owned(),
        size,
        sha256: "ab".repeat(32),
        provenance: CaptureProvenance {
            capture_id: id.to_owned(),
            station_id: "srv".to_owned(),
            monitor: "DP-1".to_owned(),
            kind: CaptureKind::Screenshot,
            caller: CaptureCaller::Owner,
            caller_station: None,
            caller_session: None,
            caller_grant_id: None,
            at: 7,
            recording: None,
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

#[tokio::test]
async fn a_new_request_starts_a_repeat_returns_the_row_and_another_hash_conflicts() {
    let s = store().await;
    assert_eq!(begin(&s, A, "h1").await, Begin::Started);
    let row = s.capture_get(FP, A).await.unwrap().unwrap();
    assert_eq!(row.state, CaptureState::Running);
    assert_eq!(row.reserved_bytes, MAX_PNG_BYTES);
    assert_eq!(
        (row.monitor.as_str(), row.artifact_id.as_str()),
        ("DP-1", ART_A)
    );
    assert_eq!(begin(&s, A, "h1").await, Begin::Existing(row));
    assert_eq!(begin(&s, A, "h2").await, Begin::Conflict);
    // The same ID of another deck is another operation.
    let other = s
        .capture_begin("cd", A, "h1", "DP-1", "art-x", 10)
        .await
        .unwrap();
    assert_eq!(other, Begin::Started);
    assert_eq!(counts(&s).await, (0, 0));
}

#[tokio::test]
async fn only_the_full_reservation_can_cross_the_quota() {
    let s = store().await;
    // Committed bytes at quota - 32 MiB: one more reservation fits exactly.
    assert_eq!(begin(&s, A, "h").await, Begin::Started);
    s.capture_commit(FP, A, new_artifact(A, CAPTURE_QUOTA - MAX_PNG_BYTES))
        .await
        .unwrap();
    assert_eq!(begin(&s, B, "h").await, Begin::Started);
    // The reservation of B crosses it for C, though no byte of B is written.
    assert_eq!(begin(&s, C, "h").await, Begin::Quota);
    assert!(s.capture_get(FP, C).await.unwrap().is_none());
    // B fails: its files may still exist, so the reservation stays.
    s.capture_fail(FP, B, "capture_failed", "x").await.unwrap();
    assert_eq!(begin(&s, C, "h").await, Begin::Quota);
    // The reservation is free again after the cleanup.
    s.capture_cleaned(FP, B).await.unwrap();
    assert_eq!(begin(&s, C, "h").await, Begin::Started);
}

#[tokio::test]
async fn a_lowered_quota_refuses_a_capture_that_the_default_allows() {
    let s = store().await;
    let begin_with = |quota: u64| {
        let s = s.clone();
        async move {
            s.capture_begin_with(FP, A, "h", "DP-1", "art-a", None, quota, 10)
                .await
                .unwrap()
        }
    };
    // One reservation of MAX_PNG_BYTES needs a quota of at least that.
    assert_eq!(begin_with(MAX_PNG_BYTES - 1).await, Begin::Quota);
    assert!(s.capture_get(FP, A).await.unwrap().is_none());
    assert_eq!(begin_with(MAX_PNG_BYTES).await, Begin::Started);
}

#[tokio::test]
async fn one_byte_over_the_quota_is_refused() {
    let s = store().await;
    assert_eq!(begin(&s, A, "h").await, Begin::Started);
    s.capture_commit(FP, A, new_artifact(A, CAPTURE_QUOTA - MAX_PNG_BYTES + 1))
        .await
        .unwrap();
    assert_eq!(begin(&s, B, "h").await, Begin::Quota);
}

#[tokio::test]
async fn a_commit_makes_the_artifact_the_version_the_provenance_and_the_state_at_once() {
    let s = store().await;
    let head = s.head();
    assert_eq!(begin(&s, A, "h").await, Begin::Started);
    let done = s.capture_commit(FP, A, new_artifact(A, 123)).await.unwrap();
    assert!(done.created);
    assert_eq!(done.artifact.session_id, None);
    assert_eq!(done.artifact.author, "owner");
    assert_eq!(done.version.files, ["capture.png"]);
    let (artifact, versions) = s.artifact_detail(ART_A.to_owned()).await.unwrap().unwrap();
    assert_eq!(
        artifact.kind,
        fleetdeck_proto::artifacts::ArtifactKind::Image
    );
    assert_eq!(artifact.latest_version, 1);
    assert_eq!(versions.len(), 1);
    assert_eq!(versions[0].capture, done.version.capture);
    assert_eq!(versions[0].capture.as_ref().unwrap().capture_id, A);
    let row = s.capture_get(FP, A).await.unwrap().unwrap();
    assert_eq!(row.state, CaptureState::Committed);
    assert_eq!(row.reserved_bytes, 0);
    assert_eq!(row.sha256.as_deref(), Some("ab".repeat(32).as_str()));
    assert_eq!(
        s.head(),
        head,
        "a capture has no session and writes no event"
    );
}

#[tokio::test]
async fn a_tombstoned_artifact_id_is_not_committed_and_nothing_changes() {
    let s = store().await;
    assert_eq!(begin(&s, A, "h").await, Begin::Started);
    s.artifact_delete_begin(ART_A.to_owned()).await.unwrap();
    let err = s
        .capture_commit(FP, A, new_artifact(A, 5))
        .await
        .unwrap_err();
    assert!(matches!(err, CommitError::UnknownArtifact(_)), "{err:?}");
    assert_eq!(counts(&s).await, (0, 0));
    let row = s.capture_get(FP, A).await.unwrap().unwrap();
    assert_eq!(row.state, CaptureState::Running);
}

#[tokio::test]
async fn a_commit_without_a_running_row_rolls_back() {
    let s = store().await;
    assert!(s.capture_commit(FP, A, new_artifact(A, 5)).await.is_err());
    assert_eq!(counts(&s).await, (0, 0));
    assert_eq!(begin(&s, B, "h").await, Begin::Started);
    s.capture_fail(FP, B, "capture_failed", "x").await.unwrap();
    assert!(s.capture_commit(FP, B, new_artifact(B, 5)).await.is_err());
    assert_eq!(counts(&s).await, (0, 0));
}

#[tokio::test]
async fn a_failure_keeps_its_code_and_a_late_failure_does_not_undo_a_commit() {
    let s = store().await;
    assert_eq!(begin(&s, A, "h").await, Begin::Started);
    s.capture_fail(FP, A, "screen_locked", "locked")
        .await
        .unwrap();
    let row = s.capture_get(FP, A).await.unwrap().unwrap();
    assert_eq!(row.state, CaptureState::Failed);
    assert_eq!(
        row.reserved_bytes, MAX_PNG_BYTES,
        "the files may still exist"
    );
    assert_eq!(row.code.as_deref(), Some("screen_locked"));
    assert_eq!(row.message.as_deref(), Some("locked"));
    assert!(matches!(
        s.capture_lookup(FP, A).await.unwrap(),
        Lookup::Failed(_)
    ));
    // A repeat of a failed request is `Existing`; it never starts again.
    assert!(matches!(begin(&s, A, "h").await, Begin::Existing(_)));

    assert_eq!(begin(&s, B, "h").await, Begin::Started);
    s.capture_commit(FP, B, new_artifact(B, 9)).await.unwrap();
    s.capture_fail(FP, B, "capture_failed", "late")
        .await
        .unwrap();
    let row = s.capture_get(FP, B).await.unwrap().unwrap();
    assert_eq!(row.state, CaptureState::Committed);
}

#[tokio::test]
async fn the_lookup_reads_each_state_and_a_deleted_artifact_wins() {
    let s = store().await;
    assert_eq!(s.capture_lookup(FP, A).await.unwrap(), Lookup::Unknown);
    assert_eq!(begin(&s, A, "h").await, Begin::Started);
    assert!(matches!(
        s.capture_lookup(FP, A).await.unwrap(),
        Lookup::Running(_)
    ));
    assert_eq!(s.captures_to_recover().await.unwrap().len(), 1);
    s.capture_commit(FP, A, new_artifact(A, 9)).await.unwrap();
    assert!(s.captures_to_recover().await.unwrap().is_empty());
    match s.capture_lookup(FP, A).await.unwrap() {
        Lookup::Committed {
            artifact,
            version,
            op,
        } => {
            assert_eq!(artifact.artifact_id, ART_A);
            assert_eq!(version.size, 9);
            assert_eq!(op.state, CaptureState::Committed);
        }
        other => panic!("{other:?}"),
    }
    // Task B's deletion: the tombstone, then the rows.
    s.artifact_delete_begin(ART_A.to_owned()).await.unwrap();
    assert!(matches!(
        s.capture_lookup(FP, A).await.unwrap(),
        Lookup::Deleted(_)
    ));
    s.artifact_delete_finish(ART_A.to_owned()).await.unwrap();
    assert!(matches!(
        s.capture_lookup(FP, A).await.unwrap(),
        Lookup::Deleted(_)
    ));
    assert_eq!(counts(&s).await, (0, 0));
    // The freed bytes leave the quota.
    assert_eq!(begin(&s, B, "h").await, Begin::Started);
}

#[tokio::test]
async fn the_has_version_check_sees_only_a_version_row() {
    let s = store().await;
    assert_eq!(begin(&s, A, "h").await, Begin::Started);
    assert!(!s.artifact_has_version(ART_A).await.unwrap());
    s.capture_commit(FP, A, new_artifact(A, 9)).await.unwrap();
    assert!(s.artifact_has_version(ART_A).await.unwrap());
}

#[tokio::test]
async fn a_cleaned_row_frees_its_reservation_and_only_a_failed_row_changes() {
    let s = store().await;
    assert_eq!(begin(&s, A, "h").await, Begin::Started);
    // A `running` row does not change.
    s.capture_cleaned(FP, A).await.unwrap();
    let row = s.capture_get(FP, A).await.unwrap().unwrap();
    assert_eq!(row.state, CaptureState::Running);
    assert_eq!(row.reserved_bytes, MAX_PNG_BYTES);
    s.capture_fail(FP, A, "capture_failed", "x").await.unwrap();
    s.capture_cleaned(FP, A).await.unwrap();
    let row = s.capture_get(FP, A).await.unwrap().unwrap();
    assert_eq!(row.state, CaptureState::Failed);
    assert_eq!(row.reserved_bytes, 0);
    assert_eq!(row.code.as_deref(), Some("capture_failed"));
}

#[tokio::test]
async fn the_recovery_list_has_the_running_rows_and_the_failed_rows_with_a_reservation() {
    let s = store().await;
    let ids = |rows: Vec<super::CaptureOp>| -> Vec<String> {
        rows.into_iter().map(|r| r.capture_id).collect()
    };
    assert_eq!(begin(&s, A, "h").await, Begin::Started);
    assert_eq!(begin(&s, B, "h").await, Begin::Started);
    assert_eq!(begin(&s, C, "h").await, Begin::Started);
    // A stays running, B fails with its reservation, C commits.
    s.capture_fail(FP, B, "capture_failed", "x").await.unwrap();
    s.capture_commit(FP, C, new_artifact(C, 9)).await.unwrap();
    assert_eq!(
        ids(s.captures_to_recover().await.unwrap()),
        [A.to_owned(), B.to_owned()]
    );
    // B is cleaned: only A is left.
    s.capture_cleaned(FP, B).await.unwrap();
    assert_eq!(ids(s.captures_to_recover().await.unwrap()), [A.to_owned()]);
}
