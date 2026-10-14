//! A superseded link or a stale snapshot never replaces newer read-model
//! state (NV-12).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use fleetdeck_proto::{SessionId, SessionSummary};
use serde_json::json;

use super::fleet::NewTask;
use super::outbox::NewOutboxItem;
use super::review::Viewer;
use super::review_tests::{W, setup};
use super::{DeckError, DeckStore};

fn summaries(ids: &[&str]) -> Vec<SessionSummary> {
    ids.iter()
        .map(|id| SessionSummary::new((*id).into()))
        .collect()
}

/// The station of `setup` at cursor 10 with the sessions s1 to s3, one
/// review comment on s2 and an open task of s3.
async fn newer_state() -> W {
    let w = setup().await;
    w.snapshot(&["s1", "s2", "s3"], 10).await;
    w.add_as("s2", &Viewer::Owner, "a.rs", 1).await;
    w.s.create_task(NewTask {
        task_id: "t3".to_owned(),
        title: "write the docs".to_owned(),
        creator: "owner".to_owned(),
        session_id: SessionId::from("s3".to_owned()),
        item: NewOutboxItem {
            station_id: w.station.clone(),
            origin: "mail".to_owned(),
            payload: json!({ "kind": "session" }),
            expires_at: i64::MAX,
        },
    })
    .await
    .unwrap();
    w
}

async fn cursor(s: &DeckStore, w: &W) -> u64 {
    s.station(w.station.clone()).await.unwrap().unwrap().cursor
}

async fn assert_newer_state_is_unchanged(w: &W) {
    assert_eq!(w.rows("SELECT COUNT(*) FROM sessions").await, 3);
    assert_eq!(w.rows("SELECT COUNT(*) FROM review_comments").await, 1);
    assert_eq!(
        w.rows("SELECT COUNT(*) FROM tasks WHERE task_id = 't3' AND state = 'open'")
            .await,
        1
    );
    assert_eq!(cursor(&w.s, w).await, 10);
}

#[tokio::test]
async fn a_stale_snapshot_of_the_same_incarnation_changes_nothing() {
    let w = newer_state().await;
    let err =
        w.s.apply_snapshot(w.station.clone(), "i1".into(), 5, summaries(&["s1"]))
            .await
            .unwrap_err();
    assert!(matches!(err, DeckError::StaleSnapshot), "{err}");
    assert_newer_state_is_unchanged(&w).await;
}

#[tokio::test]
async fn a_snapshot_at_or_above_the_cursor_applies() {
    let w = newer_state().await;
    for watermark in [10, 12] {
        w.s.apply_snapshot(
            w.station.clone(),
            "i1".into(),
            watermark,
            summaries(&["s1", "s2", "s3"]),
        )
        .await
        .unwrap();
        assert_eq!(cursor(&w.s, &w).await, watermark);
    }
}

#[tokio::test]
async fn a_new_incarnation_applies_with_a_lower_watermark() {
    let w = newer_state().await;
    w.s.apply_snapshot(w.station.clone(), "i2".into(), 3, summaries(&["s1"]))
        .await
        .unwrap();
    assert_eq!(cursor(&w.s, &w).await, 3);
    assert_eq!(w.rows("SELECT COUNT(*) FROM sessions").await, 1);
}

#[tokio::test]
async fn a_snapshot_of_a_superseded_link_changes_nothing() {
    let w = newer_state().await;
    let err =
        w.s.apply_snapshot_with(
            w.station.clone(),
            "i1".into(),
            12,
            summaries(&["s1"]),
            None,
            Some(Arc::new(|| false)),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, DeckError::Superseded), "{err}");
    assert_newer_state_is_unchanged(&w).await;
    // The current link applies the same batch.
    w.s.apply_snapshot_with(
        w.station.clone(),
        "i1".into(),
        12,
        summaries(&["s1"]),
        None,
        Some(Arc::new(|| true)),
    )
    .await
    .unwrap();
    assert_eq!(cursor(&w.s, &w).await, 12);
}
