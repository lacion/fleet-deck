//! `artifacts.delete` on real directories (R13 P5a, A4 station side): the
//! tombstone, the removal of the version tree, the refusal of later
//! publishes and reads, and the recovery of a pending cleanup.

use std::collections::BTreeMap;
use std::sync::Arc;

use fleetdeck_proto::artifacts::{
    ArtifactDeleteParams, ArtifactDeletedState, ArtifactFileQuery, ArtifactGetQuery,
    ArtifactListQuery, ArtifactPublish, code,
};

use super::ArtifactService;
use super::service_tests::{Rig, publish, rig_with, write};
use crate::manager::policy::Policy;
use crate::manager::policy_hub::PolicyHub;
use crate::store::OfflineApprovalPolicy;
use crate::store::artifact_deletions::DeleteBegin;
use crate::workspace::PrivatePaths;

async fn rig() -> Rig {
    rig_with(|_| PrivatePaths::default()).await
}

fn delete_of(id: &str) -> ArtifactDeleteParams {
    ArtifactDeleteParams {
        artifact_id: id.to_owned(),
    }
}

/// Publishes `a.html` twice with different content: two versions.
async fn two_versions(r: &Rig) -> String {
    write(&r.work.join("a.html"), "<p>one</p>");
    let first = r.service.publish(publish("a.html")).await.unwrap();
    let id = first.artifact.artifact_id;
    write(&r.work.join("a.html"), "<p>two</p>");
    let second = r
        .service
        .publish(ArtifactPublish {
            artifact_id: Some(id.clone()),
            ..publish("a.html")
        })
        .await
        .unwrap();
    assert_eq!(second.version.version, 2);
    id
}

/// How many rows the artifact has in `artifacts` and `artifact_versions`.
async fn rows_of(r: &Rig, id: &str) -> (i64, i64) {
    let id = id.to_owned();
    r.store
        .db()
        .call(move |conn| {
            let artifacts = conn.query_row(
                "SELECT COUNT(*) FROM artifacts WHERE artifact_id = ?1",
                [&id],
                |row| row.get(0),
            )?;
            let versions = conn.query_row(
                "SELECT COUNT(*) FROM artifact_versions WHERE artifact_id = ?1",
                [&id],
                |row| row.get(0),
            )?;
            Ok((artifacts, versions))
        })
        .await
        .unwrap()
}

async fn assert_refused(r: &Rig, id: &str) {
    let get = r
        .service
        .get(ArtifactGetQuery {
            artifact_id: id.to_owned(),
            version: None,
        })
        .await
        .unwrap_err();
    assert_eq!(get.code, code::UNKNOWN_ARTIFACT);
    let file = r
        .service
        .file(ArtifactFileQuery {
            artifact_id: id.to_owned(),
            version: 1,
            path: "a.html".to_owned(),
            offset: None,
            length: None,
        })
        .await
        .unwrap_err();
    assert_eq!(file.code, code::UNKNOWN_ARTIFACT);
    let list = r.service.list(ArtifactListQuery::default()).await.unwrap();
    assert!(list.artifacts.iter().all(|a| a.artifact_id != id));
}

#[tokio::test]
async fn a_delete_removes_the_tree_and_the_rows_and_refuses_every_read() {
    let r = rig().await;
    let id = two_versions(&r).await;
    assert!(r.root.join(&id).join("2").is_dir());
    let done = r.service.delete(delete_of(&id)).await.unwrap();
    assert_eq!(done.artifact_id, id);
    assert_eq!(done.state, ArtifactDeletedState::Deleted);
    assert!(!r.root.join(&id).exists());
    assert_refused(&r, &id).await;
    assert_eq!(rows_of(&r, &id).await, (0, 0));
    assert!(
        r.store
            .artifact_deletions_pending()
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        r.store.artifact_delete_begin(id).await.unwrap(),
        DeleteBegin::Complete,
        "the tombstone is complete"
    );
}

#[tokio::test]
async fn a_repeated_delete_answers_deleted_and_changes_nothing() {
    let r = rig().await;
    let id = two_versions(&r).await;
    r.service.delete(delete_of(&id)).await.unwrap();
    // A folder that a stranger puts at the path is not touched: the
    // tombstone is complete, so the cleanup does not run again.
    write(&r.root.join(&id).join("later.txt"), "x");
    let again = r.service.delete(delete_of(&id)).await.unwrap();
    assert_eq!(again.state, ArtifactDeletedState::Deleted);
    assert!(r.root.join(&id).join("later.txt").exists());
    assert_refused(&r, &id).await;
    assert_eq!(rows_of(&r, &id).await, (0, 0));
}

#[tokio::test]
async fn an_unknown_id_answers_deleted_and_records_complete() {
    let r = rig().await;
    let done = r.service.delete(delete_of("01UNKNOWNID")).await.unwrap();
    assert_eq!(done.state, ArtifactDeletedState::Deleted);
    assert_eq!(
        r.store
            .artifact_delete_begin("01UNKNOWNID".to_owned())
            .await
            .unwrap(),
        DeleteBegin::Complete
    );
    assert!(
        r.store
            .artifact_deletions_pending()
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn a_bad_id_is_invalid_params() {
    let r = rig().await;
    let long = "A".repeat(65);
    for bad in ["", "a/b", "..", "a.b", "a b", "../x", "é", long.as_str()] {
        let err = r.service.delete(delete_of(bad)).await.unwrap_err();
        assert_eq!(err.code, code::INVALID_PARAMS, "{bad:?}");
    }
    let edge = "A".repeat(64);
    r.service.delete(delete_of(&edge)).await.unwrap();
}

#[tokio::test]
async fn a_publish_to_a_tombstoned_artifact_is_unknown_and_changes_nothing() {
    let r = rig().await;
    let id = two_versions(&r).await;
    // Pending: the files and the rows are still there.
    assert_eq!(
        r.store.artifact_delete_begin(id.clone()).await.unwrap(),
        DeleteBegin::Pending
    );
    write(&r.work.join("a.html"), "<p>three</p>");
    let err = r
        .service
        .publish(ArtifactPublish {
            artifact_id: Some(id.clone()),
            ..publish("a.html")
        })
        .await
        .unwrap_err();
    assert_eq!(err.code, code::UNKNOWN_ARTIFACT);
    assert_eq!(rows_of(&r, &id).await, (1, 2), "no new row");
    assert!(!r.root.join(&id).join("3").exists());
    assert!(r.root.join(&id).join("1").is_dir());
    assert!(r.root.join(&id).join("2").is_dir());
    let staging = r.root.join(".staging");
    assert!(
        !staging.exists() || std::fs::read_dir(&staging).unwrap().next().is_none(),
        "the caller removed the staging"
    );
    assert_refused(&r, &id).await;
}

#[tokio::test]
async fn a_restart_finishes_a_pending_cleanup() {
    let r = rig().await;
    let id = two_versions(&r).await;
    r.store.artifact_delete_begin(id.clone()).await.unwrap();
    assert_eq!(
        r.store.artifact_deletions_pending().await.unwrap(),
        std::slice::from_ref(&id)
    );
    assert!(r.root.join(&id).is_dir());
    // A new service on the same store and directory, as after a restart.
    let policy = Arc::new(PolicyHub::new(Policy::new(
        vec![r.base.clone()],
        4,
        OfflineApprovalPolicy::Wait,
        BTreeMap::new(),
    )));
    let fresh = ArtifactService::new(
        r.store.clone(),
        policy,
        r.root.clone(),
        Arc::clone(&r.private),
    );
    fresh.finish_pending_deletions().await;
    assert!(!r.root.join(&id).exists());
    assert_eq!(rows_of(&r, &id).await, (0, 0));
    assert!(
        r.store
            .artifact_deletions_pending()
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        r.store.artifact_delete_begin(id).await.unwrap(),
        DeleteBegin::Complete
    );
}

#[tokio::test]
async fn a_link_inside_the_tree_is_removed_and_not_followed() {
    let r = rig().await;
    let id = two_versions(&r).await;
    let outside = tempfile::tempdir().unwrap();
    let file = outside.path().join("keep.txt");
    write(&file, "keep");
    let folder = outside.path().join("dir");
    write(&folder.join("inner.txt"), "keep too");
    let version = r.root.join(&id).join("1");
    std::os::unix::fs::symlink(&file, version.join("file-link")).unwrap();
    std::os::unix::fs::symlink(&folder, version.join("dir-link")).unwrap();
    r.service.delete(delete_of(&id)).await.unwrap();
    assert!(!r.root.join(&id).exists());
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "keep");
    assert_eq!(
        std::fs::read_to_string(folder.join("inner.txt")).unwrap(),
        "keep too"
    );
}

#[tokio::test]
async fn a_cleanup_failure_keeps_the_tombstone_pending_until_a_retry_works() {
    let r = rig().await;
    let id = two_versions(&r).await;
    // The fault: the artifact path is a regular file, not a folder.
    std::fs::remove_dir_all(r.root.join(&id)).unwrap();
    std::fs::write(r.root.join(&id), "not a folder").unwrap();
    let err = r.service.delete(delete_of(&id)).await.unwrap_err();
    assert_eq!(err.code, "failed");
    assert_eq!(
        err.message,
        "The station could not remove the artifact files."
    );
    assert_eq!(
        r.store.artifact_deletions_pending().await.unwrap(),
        std::slice::from_ref(&id)
    );
    assert_eq!(rows_of(&r, &id).await, (1, 2), "the rows stay");
    assert_refused(&r, &id).await;
    // The fault is gone: the second delete completes.
    std::fs::remove_file(r.root.join(&id)).unwrap();
    let done = r.service.delete(delete_of(&id)).await.unwrap();
    assert_eq!(done.state, ArtifactDeletedState::Deleted);
    assert_eq!(rows_of(&r, &id).await, (0, 0));
    assert!(
        r.store
            .artifact_deletions_pending()
            .await
            .unwrap()
            .is_empty()
    );
    assert_refused(&r, &id).await;
}
