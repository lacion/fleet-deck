//! The provenance of a coordinator recording (R13 P5b-2 §13.4, task Q-K):
//! the deck builds it from the stored context, and the station must report
//! `caller: coordinator` with the same caller station, session and grant, the
//! same capture and monitor, `kind: recording` and a valid `RecordingMeta`.
//! Anything else is no provenance (never the owner's).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use fleetdeck_proto::StationId;
use fleetdeck_proto::artifacts::{Artifact, ArtifactKind, ArtifactPublished, ArtifactVersion};
use fleetdeck_proto::capture::{CaptureCaller, CaptureKind, CaptureProvenance};
use fleetdeck_proto::coordinator_desktop_wire::CoordinatorContext;
use fleetdeck_proto::recording::{RecordingMeta, StopReason};

use super::recording_provenance;
use crate::store::desktop_captures::{CaptureRow, CaptureState};
use crate::store::desktop_recordings::{OperationKind, RecordingRow, StopIntent};

const ID: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAV";

fn station() -> StationId {
    StationId::from("st-1".to_owned())
}

fn context() -> CoordinatorContext {
    CoordinatorContext {
        grant_id: "g-1".to_owned(),
        binding_id: "b-1".to_owned(),
        generation: 1,
        caller_station: StationId::from("caller".to_owned()),
        caller_session: "coord-1".into(),
        caller_label: "swift-fox".to_owned(),
    }
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

/// What a station reports for the coordinator recording of the row.
fn reported() -> CaptureProvenance {
    CaptureProvenance {
        capture_id: ID.to_owned(),
        station_id: "station-side-name".to_owned(),
        monitor: "DP-1".to_owned(),
        kind: CaptureKind::Recording,
        caller: CaptureCaller::Coordinator,
        caller_station: Some("caller".to_owned()),
        caller_session: Some("coord-1".to_owned()),
        caller_grant_id: Some("g-1".to_owned()),
        at: 5,
        recording: Some(meta()),
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
            author: "coordinator".to_owned(),
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
            coordinator: Some(serde_json::to_string(&context()).unwrap()),
        },
        kind: OperationKind::Recording,
        requested_seconds: Some(30),
        owner_login: None,
        link_epoch: Some("epoch".to_owned()),
        stop_intent: StopIntent::None,
        stop_reason: None,
        started_at: None,
        audited_terminal: false,
    }
}

/// A change of the reported provenance.
type Change = fn(&mut CaptureProvenance);

fn with(change: impl FnOnce(&mut CaptureProvenance)) -> Option<CaptureProvenance> {
    let mut p = reported();
    change(&mut p);
    recording_provenance(&station(), &row(), &published(Some(p)))
}

#[test]
fn the_provenance_is_built_by_the_deck_from_the_stored_context() {
    let p = with(|_| {}).expect("a provenance");
    assert_eq!(p.capture_id, ID);
    assert_eq!(p.station_id, "st-1");
    assert_eq!(p.monitor, "DP-1");
    assert_eq!(p.kind, CaptureKind::Recording);
    assert_eq!(p.caller, CaptureCaller::Coordinator);
    assert_eq!(p.caller_station.as_deref(), Some("caller"));
    assert_eq!(p.caller_session.as_deref(), Some("coord-1"));
    assert_eq!(p.caller_grant_id.as_deref(), Some("g-1"));
    // The creation time of the station's version, not the reported one.
    assert_eq!(p.at, 9);
    assert_eq!(p.recording, Some(meta()));
}

#[test]
fn the_save_reasons_and_the_duration_bound_are_accepted() {
    assert!(with(|p| p.recording.as_mut().unwrap().stop_reason = StopReason::OwnerStop).is_some());
    assert!(with(|p| p.recording.as_mut().unwrap().duration_ms = 31_000).is_some());
}

#[test]
fn every_other_report_is_no_provenance_and_never_the_owners() {
    let cases: [(&str, Change); 12] = [
        ("an owner caller", |p| p.caller = CaptureCaller::Owner),
        ("no caller station", |p| p.caller_station = None),
        ("another caller station", |p| {
            p.caller_station = Some("other".to_owned());
        }),
        ("another session", |p| {
            p.caller_session = Some("other".to_owned());
        }),
        ("another grant", |p| {
            p.caller_grant_id = Some("g-2".to_owned());
        }),
        ("no grant", |p| p.caller_grant_id = None),
        ("another capture", |p| p.capture_id = "OTHER".to_owned()),
        ("another monitor", |p| p.monitor = "DP-2".to_owned()),
        ("a screenshot kind", |p| p.kind = CaptureKind::Screenshot),
        ("no meta", |p| p.recording = None),
        ("other seconds", |p| {
            p.recording.as_mut().unwrap().requested_seconds = 31;
        }),
        ("a discard reason", |p| {
            p.recording.as_mut().unwrap().stop_reason = StopReason::LeaseExpired;
        }),
    ];
    for (what, change) in cases {
        assert!(with(change).is_none(), "{what}");
    }
    // Too long a duration, and dimensions that cannot be recorded.
    assert!(with(|p| p.recording.as_mut().unwrap().duration_ms = 31_001).is_none());
    assert!(with(|p| p.recording.as_mut().unwrap().width = 1921).is_none());
    // No provenance at all.
    assert!(recording_provenance(&station(), &row(), &published(None)).is_none());
}

#[test]
fn a_context_that_does_not_parse_and_a_screenshot_row_get_no_provenance() {
    let p = published(Some(reported()));
    let mut broken = row();
    broken.capture.coordinator = Some("not json".to_owned());
    assert!(recording_provenance(&station(), &broken, &p).is_none());
    let mut screenshot = row();
    screenshot.kind = OperationKind::Screenshot;
    assert!(recording_provenance(&station(), &screenshot, &p).is_none());
}
