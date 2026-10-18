//! The state of a capture as the caller sees it (R13 P6 §4.8, §11.3).

use fleetdeck_proto::artifacts::{Artifact, ArtifactKind, ArtifactVersion};
use serde_json::json;

use super::*;
use crate::store::coordinator_desktop::{Charges, OpClass};

const ID: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAV";

fn op(state: OpState) -> DesktopOp {
    DesktopOp {
        operation_id: ID.into(),
        grant_id: "g1".into(),
        request_key: "key".into(),
        payload_hash: "hash".into(),
        tool: OpTool::Screenshot,
        class: OpClass::Capture,
        binding_id: "b1".into(),
        generation: 1,
        caller_station: StationId::from("caller"),
        caller_session: "coord-1".into(),
        target_station: StationId::from("target"),
        action_hash: None,
        resource: json!({}),
        state,
        result: None,
        code: None,
        charged: Charges {
            desktop: 1,
            capture: 1,
        },
        created_at: 1,
        updated_at: 2,
        seconds: None,
    }
}

fn published() -> ArtifactPublished {
    ArtifactPublished {
        artifact: Artifact {
            artifact_id: "ART1".into(),
            station_id: "target".into(),
            session_id: None,
            title: "t".into(),
            kind: ArtifactKind::Image,
            latest_version: 1,
            created_at: 5,
            updated_at: 5,
            author: "coordinator:caller/coord-1".into(),
        },
        version: ArtifactVersion {
            artifact_id: "ART1".into(),
            version: 1,
            size: 42,
            sha256: "ab".repeat(32),
            files: vec!["capture.png".into()],
            created_at: 7,
            capture: None,
        },
        created: true,
    }
}

fn row(state: CaptureState, indexed: bool, deleted: bool) -> CaptureRow {
    CaptureRow {
        station_id: StationId::from("target"),
        capture_id: ID.into(),
        action_hash: "h".into(),
        state,
        monitor: "DP-1".into(),
        published: (state == CaptureState::Committed).then(published),
        indexed,
        code: None,
        message: None,
        created_at: 1,
        updated_at: 2,
        deleted,
        coordinator: Some("{}".into()),
    }
}

fn value(row: Option<&CaptureRow>, state: OpState) -> Value {
    serde_json::to_value(state_of("req-1", &op(state), row)).expect("json")
}

#[test]
fn a_committed_and_indexed_capture_names_its_artifact_and_metadata() {
    let r = row(CaptureState::Committed, true, false);
    assert_eq!(
        value(Some(&r), OpState::Done),
        json!({"request_id": "req-1", "state": "committed", "capture_id": ID,
            "station": "target",
            "artifact": {"station_id": "target", "artifact_id": "ART1", "version": 1},
            "monitor": "DP-1", "size": 42, "sha256": "ab".repeat(32), "at": 7})
    );
}

#[test]
fn a_saved_capture_that_is_not_indexed_is_pending_with_its_text_never_failed() {
    let r = row(CaptureState::Committed, false, false);
    let v = value(Some(&r), OpState::Done);
    assert_eq!(v["state"], "pending");
    assert_eq!(v["message"], PENDING_INDEXING);
    assert!(v.get("artifact").is_none(), "{v}");
}

#[test]
fn a_prepared_or_attempted_capture_is_pending_without_a_text() {
    for state in [CaptureState::Prepared, CaptureState::Attempted] {
        let v = value(Some(&row(state, false, false)), OpState::Attempted);
        assert_eq!(
            v,
            json!({"request_id": "req-1", "state": "pending", "capture_id": ID,
                "station": "target"})
        );
    }
}

#[test]
fn a_tombstone_wins_and_an_aged_unknown_has_its_text() {
    let r = row(CaptureState::Committed, true, true);
    assert_eq!(value(Some(&r), OpState::Done)["state"], "deleted");
    let v = value(
        Some(&row(CaptureState::Unknown, false, false)),
        OpState::Unknown,
    );
    assert_eq!(v["state"], "unknown");
    assert_eq!(
        v["message"],
        "The screenshot result is unknown. Check the artifact list before taking another screenshot."
    );
}

#[test]
fn a_failed_capture_has_its_code_and_fixed_text() {
    let mut r = row(CaptureState::Failed, false, false);
    r.code = Some("disk_full".into());
    r.message = Some("stored text is ignored".into());
    let v = value(Some(&r), OpState::Failed);
    assert_eq!(v["state"], "failed");
    assert_eq!(v["error"], "disk_full");
    assert_eq!(
        v["message"],
        "The station has too little free space for a capture."
    );
}

#[test]
fn the_failure_texts_of_each_family_are_the_contract_texts() {
    // A capture code keeps its capture text, including `busy`.
    assert_eq!(
        failure_of(Some("busy"), Some("x")),
        ("busy".into(), "A capture is already running.".into())
    );
    // The missing grim is `desktop_unsupported` with its own text.
    assert_eq!(
        failure_of(Some("unsupported"), Some(capture::text::NO_GRIM)),
        (
            "desktop_unsupported".into(),
            "The screenshot tool grim is not installed.".into()
        )
    );
    // A code of §11 keeps the text of §11.
    assert_eq!(
        failure_of(Some("authority_lost"), None),
        (
            "authority_lost".into(),
            "Desktop authority was withdrawn before this operation could continue.".into()
        )
    );
    // Anything else, and no code, is `capture_failed`.
    for stored in [Some("nonsense"), None] {
        assert_eq!(
            failure_of(stored, None),
            (
                "capture_failed".into(),
                "The screenshot tool failed.".into()
            )
        );
    }
}

#[test]
fn an_operation_without_a_record_is_read_from_the_operation() {
    assert_eq!(value(None, OpState::Attempted)["state"], "pending");
    assert_eq!(value(None, OpState::Unknown)["state"], "unknown");
    let mut failed = op(OpState::Failed);
    failed.code = Some("busy".into());
    let v = serde_json::to_value(state_of("req-1", &failed, None)).expect("json");
    assert_eq!(v["state"], "failed");
    assert_eq!(v["error"], "busy");
    // No capture row: the capacity text of §11.1.
    assert_eq!(
        v["message"],
        "The desktop operation cannot start because its capacity limit is in use."
    );
}

#[test]
fn the_unknown_request_carries_only_its_request_id() {
    assert_eq!(
        serde_json::to_value(unknown("req-1")).expect("json"),
        json!({"request_id": "req-1", "state": "unknown"})
    );
}

#[test]
fn the_result_that_the_ledger_keeps_has_no_request_id() {
    let r = row(CaptureState::Committed, true, false);
    let stored = stored_result(&state_of("SENTINEL-REQUEST", &op(OpState::Done), Some(&r)));
    assert!(stored.get("request_id").is_none(), "{stored}");
    assert!(!stored.to_string().contains("SENTINEL"), "{stored}");
    assert_eq!(stored["state"], "committed");
}
