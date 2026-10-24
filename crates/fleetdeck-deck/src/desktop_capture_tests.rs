//! The capture relay's pure parts (R13 P5a task E): the code table, the
//! feature stamp, the status of a row and the reconcile rules. The link
//! behavior is in `tests/board_desktop_capture.rs`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::Duration;

use fleetdeck_proto::artifacts::{Artifact, ArtifactKind, ArtifactPublished, ArtifactVersion};
use fleetdeck_proto::capture::{CaptureStatus, code, text};
use fleetdeck_proto::desktop::DesktopInfo;
use fleetdeck_proto::version::feature;
use fleetdeck_testkit::Det;

use super::*;

const ID: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAV";

async fn store() -> (DeckStore, Det) {
    let det = Det::new();
    let s = DeckStore::open_in_memory(det.clock(), det.ids())
        .await
        .unwrap();
    (s, det)
}

fn published() -> ArtifactPublished {
    ArtifactPublished {
        artifact: Artifact {
            artifact_id: "A1".to_owned(),
            station_id: "name".to_owned(),
            session_id: None,
            title: "t".to_owned(),
            kind: ArtifactKind::Image,
            latest_version: 1,
            created_at: 9,
            updated_at: 9,
            author: "owner".to_owned(),
        },
        version: ArtifactVersion {
            artifact_id: "A1".to_owned(),
            version: 1,
            size: 3,
            sha256: "ab".repeat(32),
            files: vec!["capture.png".to_owned()],
            created_at: 9,
            capture: None,
        },
        created: true,
    }
}

fn info() -> DesktopInfo {
    serde_json::from_value(serde_json::json!({
        "kind": "omarchy", "omarchy_version": null, "monitors": [], "locked": false,
        "dnd": null, "idle_inhibited": null, "walls": [],
        "allowed": {"desktop": true, "capture": true, "input": false},
        "tools": {"wtype": false, "pointer": false, "recorder": false, "grim": true,
                  "chromium": true, "notify_send": true}
    }))
    .unwrap()
}

#[test]
fn every_station_capture_code_has_its_status_and_fixed_text() {
    let table = [
        (code::BUSY, 409, text::BUSY),
        (code::DISK_FULL, 507, text::DISK_FULL),
        (code::CAPTURE_QUOTA, 409, text::CAPTURE_QUOTA),
        (code::CAPTURE_TOO_LARGE, 413, text::CAPTURE_TOO_LARGE),
        (code::CAPTURE_DIMENSIONS, 422, text::CAPTURE_DIMENSIONS),
        (code::CAPTURE_FAILED, 502, text::CAPTURE_FAILED),
        (
            code::CAPTURE_PUBLISH_FAILED,
            502,
            text::CAPTURE_PUBLISH_FAILED,
        ),
        (code::SCREEN_LOCKED, 409, text::SCREEN_LOCKED),
        (code::CAPTURE_ID_CONFLICT, 409, text::CAPTURE_ID_CONFLICT),
        (code::CAPTURE_INTERRUPTED, 502, text::CAPTURE_INTERRUPTED),
    ];
    for (c, status, fixed) in table {
        // The station text never counts.
        let (e, stored, stored_text) = CaptureError::from_station(c, "station text").unwrap();
        assert_eq!(
            e.http(),
            (StatusCode::from_u16(status).unwrap(), c, fixed),
            "{c}"
        );
        assert_eq!((stored, stored_text), (c, fixed));
    }
}

#[test]
fn the_missing_grim_is_a_desktop_unsupported_with_its_own_text() {
    let (e, stored, fixed) = CaptureError::from_station("unsupported", text::NO_GRIM).unwrap();
    assert_eq!(
        e.http(),
        (StatusCode::CONFLICT, "desktop_unsupported", text::NO_GRIM)
    );
    assert_eq!((stored, fixed), ("unsupported", text::NO_GRIM));
    // `unsupported` with another text, and every other code, map by `map_wire`.
    assert!(CaptureError::from_station("unsupported", "x").is_none());
    for c in [
        "forbidden",
        "not_found",
        "failed",
        "conflict",
        "invalid_params",
        "x",
    ] {
        assert!(
            CaptureError::from_station(c, text::NO_GRIM).is_none(),
            "{c}"
        );
    }
}

#[tokio::test]
async fn the_features_are_the_negotiated_p5a_features_only() {
    let (s, _det) = store().await;
    let st = StationId::from("st-1".to_owned());
    let stamp = |s: &DeckStore| {
        let mut i = info();
        i.features = vec!["stale".to_owned()];
        stamp_features(s, &st, &mut i);
        i.features
    };
    // No live link.
    assert!(stamp(&s).is_empty());
    s.set_link_features(&st, 1, vec![feature::DESKTOP_CAPTURE.to_owned()]);
    assert_eq!(stamp(&s), [feature::DESKTOP_CAPTURE]);
    s.set_link_features(
        &st,
        1,
        vec![
            feature::ARTIFACTS_DELETE.to_owned(),
            feature::DESKTOP_CAPTURE.to_owned(),
            feature::DESKTOP.to_owned(),
        ],
    );
    assert_eq!(
        stamp(&s),
        [feature::DESKTOP_CAPTURE, feature::ARTIFACTS_DELETE]
    );
    // Another live link without them: fail closed.
    s.set_link_features(&st, 2, vec![feature::DESKTOP.to_owned()]);
    assert!(stamp(&s).is_empty());
    s.clear_link_features(&st, 2);
    s.clear_link_features(&st, 1);
    assert!(stamp(&s).is_empty());
}

#[tokio::test]
async fn a_stored_desktop_info_is_stamped_in_both_store_paths() {
    let (s, _det) = store().await;
    let st = StationId::from("st-1".to_owned());
    s.set_link_features(&st, 1, vec![feature::DESKTOP_CAPTURE.to_owned()]);
    let mut station: fleetdeck_proto::link::StationInfo =
        serde_json::from_value(serde_json::json!({
            "name": "n", "incarnation": "i", "head": 1, "live_sessions": 0
        }))
        .unwrap();
    station.desktop = Some(info());
    s.set_station_info(&st, station);
    let kept = s.station_info(&st).unwrap().0.desktop.unwrap();
    assert_eq!(kept.features, [feature::DESKTOP_CAPTURE]);
    s.set_link_features(
        &st,
        1,
        vec![
            feature::DESKTOP_CAPTURE.to_owned(),
            feature::ARTIFACTS_DELETE.to_owned(),
        ],
    );
    s.update_station_desktop(&st, info());
    let kept = s.station_info(&st).unwrap().0.desktop.unwrap();
    assert_eq!(
        kept.features,
        [feature::DESKTOP_CAPTURE, feature::ARTIFACTS_DELETE]
    );
}

fn row(state: CaptureState, indexed: bool, deleted: bool) -> CaptureRow {
    CaptureRow {
        station_id: StationId::from("st-1".to_owned()),
        capture_id: ID.to_owned(),
        action_hash: "h".to_owned(),
        state,
        monitor: "DP-1".to_owned(),
        published: Some(published()),
        indexed,
        code: None,
        message: None,
        created_at: 1,
        updated_at: 1,
        deleted,
        coordinator: None,
    }
}

#[test]
fn the_status_of_a_row_follows_the_design_and_a_tombstone_wins() {
    let id = ID.to_owned();
    assert_eq!(
        status_of(&row(CaptureState::Committed, true, false)),
        CaptureStatus::Committed {
            capture_id: id.clone(),
            published: published()
        }
    );
    for deleted_state in [
        CaptureState::Committed,
        CaptureState::Attempted,
        CaptureState::Failed,
    ] {
        assert_eq!(
            status_of(&row(deleted_state, true, true)),
            CaptureStatus::Deleted {
                capture_id: id.clone()
            }
        );
    }
    for pending_state in [
        CaptureState::Prepared,
        CaptureState::Attempted,
        CaptureState::Committed,
    ] {
        assert_eq!(
            status_of(&row(pending_state, false, false)),
            CaptureStatus::Pending {
                capture_id: id.clone()
            }
        );
    }
    assert_eq!(
        status_of(&row(CaptureState::Unknown, false, false)),
        CaptureStatus::Unknown { capture_id: id }
    );
    let mut failed = row(CaptureState::Failed, false, false);
    failed.code = Some("busy".to_owned());
    failed.message = Some(text::BUSY.to_owned());
    assert_eq!(
        status_of(&failed),
        CaptureStatus::Failed {
            capture_id: ID.to_owned(),
            error: "busy".to_owned(),
            message: text::BUSY.to_owned()
        }
    );
}

#[test]
fn a_stored_failure_maps_back_to_its_http_error() {
    let mut failed = row(CaptureState::Failed, false, false);
    failed.code = Some(code::DISK_FULL.to_owned());
    failed.message = Some(text::DISK_FULL.to_owned());
    assert_eq!(
        CaptureError::from_row(&failed).http(),
        (
            StatusCode::INSUFFICIENT_STORAGE,
            "disk_full",
            text::DISK_FULL
        )
    );
    failed.code = Some("unsupported".to_owned());
    failed.message = Some(text::NO_GRIM.to_owned());
    assert_eq!(
        CaptureError::from_row(&failed).http(),
        (StatusCode::CONFLICT, "desktop_unsupported", text::NO_GRIM)
    );
    failed.code = Some("garbage".to_owned());
    assert_eq!(CaptureError::from_row(&failed).http().1, "capture_failed");
}

/// An `attempted` row in the store and its copy.
async fn attempted(s: &DeckStore) -> CaptureRow {
    let st = StationId::from("st-1".to_owned());
    s.capture_prepare(st.clone(), ID.to_owned(), "h".into(), "DP-1".into())
        .await
        .unwrap();
    s.capture_attempted(st.clone(), ID.to_owned())
        .await
        .unwrap();
    s.capture_get(st, ID.to_owned()).await.unwrap().unwrap()
}

#[tokio::test]
async fn an_unknown_status_ends_the_row_only_after_24_hours() {
    let (s, det) = store().await;
    let row = attempted(&s).await;
    let st = row.station_id.clone();
    let unknown = || CaptureStatus::Unknown {
        capture_id: ID.to_owned(),
    };
    reconcile(&s, &row, unknown()).await.unwrap();
    let state = |s: DeckStore, st: StationId| async move {
        s.capture_get(st, ID.to_owned())
            .await
            .unwrap()
            .unwrap()
            .state
    };
    assert_eq!(state(s.clone(), st.clone()).await, CaptureState::Attempted);
    det.advance(Duration::from_secs(24 * 3600 - 1));
    reconcile(&s, &row, unknown()).await.unwrap();
    assert_eq!(state(s.clone(), st.clone()).await, CaptureState::Attempted);
    det.advance(Duration::from_secs(1));
    reconcile(&s, &row, unknown()).await.unwrap();
    assert_eq!(state(s.clone(), st.clone()).await, CaptureState::Unknown);
    // Terminal: it is not asked for again.
    assert!(s.captures_unresolved().await.unwrap().is_empty());
}

#[tokio::test]
async fn a_station_failure_stores_only_a_capture_code() {
    let (s, _det) = store().await;
    let row = attempted(&s).await;
    let st = row.station_id.clone();
    let failed = |error: &str| CaptureStatus::Failed {
        capture_id: ID.to_owned(),
        error: error.to_owned(),
        message: "station text".to_owned(),
    };
    reconcile(
        &s,
        &row,
        CaptureStatus::Pending {
            capture_id: ID.to_owned(),
        },
    )
    .await
    .unwrap();
    reconcile(
        &s,
        &row,
        CaptureStatus::Deleted {
            capture_id: ID.to_owned(),
        },
    )
    .await
    .unwrap();
    let got = s
        .capture_get(st.clone(), ID.to_owned())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(got.state, CaptureState::Attempted);
    reconcile(&s, &row, failed("screen_locked")).await.unwrap();
    let got = s.capture_get(st, ID.to_owned()).await.unwrap().unwrap();
    assert_eq!(got.state, CaptureState::Failed);
    assert_eq!(got.code.as_deref(), Some("screen_locked"));
    assert_eq!(got.message.as_deref(), Some(text::SCREEN_LOCKED));

    // Another code is `capture_failed` with the fixed text.
    let (s, _det) = store().await;
    let row = attempted(&s).await;
    reconcile(&s, &row, failed("forbidden")).await.unwrap();
    let got = s
        .capture_get(row.station_id.clone(), ID.to_owned())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(got.code.as_deref(), Some(code::CAPTURE_FAILED));
    assert_eq!(got.message.as_deref(), Some(text::CAPTURE_FAILED));
}

#[tokio::test]
async fn a_committed_status_indexes_the_artifact_with_the_provenance_of_the_row() {
    let (s, _det) = store().await;
    let row = attempted(&s).await;
    reconcile(
        &s,
        &row,
        CaptureStatus::Committed {
            capture_id: ID.to_owned(),
            published: published(),
        },
    )
    .await
    .unwrap();
    let got = s
        .capture_get(row.station_id.clone(), ID.to_owned())
        .await
        .unwrap()
        .unwrap();
    assert!(got.indexed);
    let capture = got.published.unwrap().version.capture.unwrap();
    assert_eq!(capture.station_id, "st-1");
    assert_eq!(capture.monitor, "DP-1");
    assert_eq!(capture.capture_id, ID);
    // `at` is the creation time of the station's version.
    assert_eq!(capture.at, 9);
    assert_eq!(capture.caller_station, None);
}
