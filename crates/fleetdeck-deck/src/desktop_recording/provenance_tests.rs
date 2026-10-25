use fleetdeck_proto::artifacts::{Artifact, ArtifactKind, ArtifactVersion};

use super::*;
use crate::store::desktop_captures::CaptureRow;
use crate::store::desktop_recordings::StopIntent;

const ID: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAV";

fn station() -> StationId {
    StationId::from("st-1".to_owned())
}

fn meta() -> RecordingMeta {
    RecordingMeta {
        requested_seconds: 30,
        duration_ms: 30_000,
        width: 1920,
        height: 1080,
        stop_reason: StopReason::Duration,
    }
}

fn reported(m: Option<RecordingMeta>) -> CaptureProvenance {
    CaptureProvenance {
        capture_id: ID.to_owned(),
        station_id: "station-side-name".to_owned(),
        monitor: "DP-1".to_owned(),
        kind: CaptureKind::Recording,
        caller: CaptureCaller::Owner,
        caller_station: None,
        caller_session: None,
        caller_grant_id: None,
        at: 5,
        recording: m,
    }
}

fn published(capture: Option<CaptureProvenance>) -> ArtifactPublished {
    ArtifactPublished {
        artifact: Artifact {
            artifact_id: "A1".to_owned(),
            station_id: "station-side-name".to_owned(),
            session_id: None,
            title: "Desktop recording".to_owned(),
            kind: ArtifactKind::Video,
            latest_version: 1,
            created_at: 7,
            updated_at: 7,
            author: "owner".to_owned(),
        },
        version: ArtifactVersion {
            artifact_id: "A1".to_owned(),
            version: 1,
            size: 3,
            sha256: "ab".repeat(32),
            files: vec!["capture.mp4".to_owned()],
            created_at: 9,
            capture,
        },
        created: true,
    }
}

fn row() -> RecordingRow {
    RecordingRow {
        capture: CaptureRow {
            station_id: station(),
            capture_id: ID.to_owned(),
            action_hash: "h".to_owned(),
            state: CaptureState::Attempted,
            monitor: "DP-1".to_owned(),
            published: None,
            indexed: false,
            code: None,
            message: None,
            created_at: 1,
            updated_at: 1,
            deleted: false,
            coordinator: None,
        },
        kind: OperationKind::Recording,
        requested_seconds: Some(30),
        owner_login: Some("login".to_owned()),
        link_epoch: Some("epoch".to_owned()),
        stop_intent: StopIntent::None,
        stop_reason: None,
        started_at: None,
        audited_terminal: false,
    }
}

fn accepts(m: RecordingMeta) -> Option<CaptureProvenance> {
    recording_provenance(&station(), &row(), &published(Some(reported(Some(m)))))
}

#[test]
fn the_provenance_is_built_by_the_deck_for_an_owner_recording() {
    let p = accepts(meta()).unwrap();
    assert_eq!(p.capture_id, ID);
    assert_eq!(p.station_id, "st-1");
    assert_eq!(p.monitor, "DP-1");
    assert_eq!(p.kind, CaptureKind::Recording);
    assert_eq!(p.caller, CaptureCaller::Owner);
    assert_eq!(p.caller_station, None);
    assert_eq!(p.caller_session, None);
    assert_eq!(p.caller_grant_id, None);
    // The creation time of the station's version, not the reported one.
    assert_eq!(p.at, 9);
    assert_eq!(p.recording, Some(meta()));
}

#[test]
fn the_save_reasons_and_the_duration_bound_are_accepted() {
    let mut m = meta();
    m.stop_reason = StopReason::OwnerStop;
    assert!(accepts(m).is_some());
    // 30 seconds plus one second of slack, and no more.
    let mut m = meta();
    m.duration_ms = 31_000;
    assert!(accepts(m).is_some());
    let mut m = meta();
    m.duration_ms = 0;
    assert!(accepts(m).is_some());
    let mut m = meta();
    m.duration_ms = 31_001;
    assert!(accepts(m).is_none());
}

#[test]
fn every_rejected_metadata_is_an_indexing_failure() {
    let mut other_seconds = meta();
    other_seconds.requested_seconds = 31;
    assert!(accepts(other_seconds).is_none());
    for reason in [
        StopReason::PolicyOff,
        StopReason::LeaseExpired,
        StopReason::Deadline,
        StopReason::RecordingInterrupted,
    ] {
        let mut m = meta();
        m.stop_reason = reason;
        assert!(accepts(m).is_none(), "{reason:?}");
    }
    for (w, h) in [
        (0, 1080),
        (1920, 0),
        (1921, 1080),
        (1920, 1081),
        (8194, 2),
        (2, 8194),
        (8192, 8192),
    ] {
        let mut m = meta();
        m.width = w;
        m.height = h;
        assert!(accepts(m).is_none(), "{w}x{h}");
    }
}

#[test]
fn a_missing_or_foreign_reported_provenance_is_an_indexing_failure() {
    let r = row();
    // No provenance, and none with a recording.
    assert!(recording_provenance(&station(), &r, &published(None)).is_none());
    assert!(recording_provenance(&station(), &r, &published(Some(reported(None)))).is_none());
    // Another capture, monitor or kind.
    let mut p = reported(Some(meta()));
    p.capture_id = "01ARZ3NDEKTSV4RRFFQ69G5FAW".to_owned();
    assert!(recording_provenance(&station(), &r, &published(Some(p))).is_none());
    let mut p = reported(Some(meta()));
    p.monitor = "DP-2".to_owned();
    assert!(recording_provenance(&station(), &r, &published(Some(p))).is_none());
    let mut p = reported(Some(meta()));
    p.kind = CaptureKind::Screenshot;
    assert!(recording_provenance(&station(), &r, &published(Some(p))).is_none());
}

#[test]
fn a_screenshot_row_a_coordinator_row_and_a_row_without_seconds_never_get_a_provenance() {
    let p = published(Some(reported(Some(meta()))));
    let mut screenshot = row();
    screenshot.kind = OperationKind::Screenshot;
    assert!(recording_provenance(&station(), &screenshot, &p).is_none());
    let mut coordinator = row();
    coordinator.capture.coordinator = Some("{}".to_owned());
    assert!(recording_provenance(&station(), &coordinator, &p).is_none());
    let mut no_seconds = row();
    no_seconds.requested_seconds = None;
    assert!(recording_provenance(&station(), &no_seconds, &p).is_none());
}
