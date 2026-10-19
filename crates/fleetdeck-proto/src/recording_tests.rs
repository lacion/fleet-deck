use serde_json::json;

use super::*;
use crate::artifacts::{Artifact, ArtifactVersion};
use crate::capture::{CaptureCaller, CaptureProvenance};

const ALL_REASONS: [StopReason; 18] = [
    StopReason::Duration,
    StopReason::OwnerStop,
    StopReason::PolicyOff,
    StopReason::ScreenLocked,
    StopReason::MonitorChanged,
    StopReason::LinkClosed,
    StopReason::LeaseExpired,
    StopReason::LeaseDenied,
    StopReason::DeckPolicy,
    StopReason::OwnerLoginEnded,
    StopReason::CoordinatorAuthorityLost,
    StopReason::OutputLimit,
    StopReason::DiskReserve,
    StopReason::RecorderExited,
    StopReason::StationShutdown,
    StopReason::RecordingInterrupted,
    StopReason::ArtifactDeleted,
    StopReason::Deadline,
];

/// The names of `ALL_REASONS`, in the same order.
const NAMES: [&str; 18] = [
    "duration",
    "owner_stop",
    "policy_off",
    "screen_locked",
    "monitor_changed",
    "link_closed",
    "lease_expired",
    "lease_denied",
    "deck_policy",
    "owner_login_ended",
    "coordinator_authority_lost",
    "output_limit",
    "disk_reserve",
    "recorder_exited",
    "station_shutdown",
    "recording_interrupted",
    "artifact_deleted",
    "deadline",
];

fn meta() -> RecordingMeta {
    RecordingMeta {
        requested_seconds: 30,
        duration_ms: 30_040,
        width: 1920,
        height: 1080,
        stop_reason: StopReason::Duration,
    }
}

fn provenance(kind: CaptureKind, recording: Option<RecordingMeta>) -> CaptureProvenance {
    CaptureProvenance {
        capture_id: "01ARZ3NDEKTSV4RRFFQ69G5FAV".into(),
        station_id: "st".into(),
        monitor: "DP-1".into(),
        kind,
        caller: CaptureCaller::Owner,
        caller_station: None,
        caller_session: None,
        caller_grant_id: None,
        at: 7,
        recording,
    }
}

fn published(kind: ArtifactKind, capture: Option<CaptureProvenance>) -> ArtifactPublished {
    ArtifactPublished {
        artifact: Artifact {
            artifact_id: "A1".into(),
            station_id: "s".into(),
            session_id: None,
            title: "t".into(),
            kind,
            latest_version: 1,
            created_at: 1,
            updated_at: 1,
            author: "owner".into(),
        },
        version: ArtifactVersion {
            artifact_id: "A1".into(),
            version: 1,
            size: 3,
            sha256: "ab".into(),
            files: vec![RECORDING_FILE.into()],
            created_at: 1,
            capture,
        },
        created: true,
    }
}

#[test]
fn the_limits_and_the_file_name_are_fixed() {
    assert_eq!(MAX_RECORDING_BYTES, 256 * 1024 * 1024);
    assert_eq!(PEAK_RESERVATION, 512 * 1024 * 1024);
    assert_eq!(RECORDING_FILE, "capture.mp4");
}

#[test]
fn valid_seconds_accepts_one_to_thirty_six_hundred() {
    assert_eq!(MAX_RECORDING_SECONDS, 3600);
    assert!(!valid_seconds(0));
    assert!(valid_seconds(1));
    assert!(valid_seconds(120));
    assert!(valid_seconds(121));
    assert!(valid_seconds(3600));
    assert!(!valid_seconds(3601));
    assert!(!valid_seconds(u64::MAX));
}

#[test]
fn valid_dimensions_checks_range_parity_and_pixels() {
    assert!(valid_dimensions(2, 2));
    assert!(valid_dimensions(1920, 1080));
    assert!(valid_dimensions(8192, 2048));
    assert!(valid_dimensions(4096, 4096));
    assert!(!valid_dimensions(0, 2));
    assert!(!valid_dimensions(2, 0));
    assert!(!valid_dimensions(1, 2));
    assert!(!valid_dimensions(1920, 1081));
    assert!(!valid_dimensions(8194, 2));
    assert!(!valid_dimensions(2, 8194));
    assert!(valid_dimensions(8192, 2));
    // 8192 x 2050 = 16,793,600 pixels: over the product limit.
    assert!(!valid_dimensions(8192, 2050));
    assert!(!valid_dimensions(u32::MAX, u32::MAX));
}

#[test]
fn a_stop_reason_serializes_to_its_snake_case_name() {
    assert_eq!(ALL_REASONS.len(), NAMES.len());
    for (reason, name) in ALL_REASONS.into_iter().zip(NAMES) {
        assert_eq!(serde_json::to_value(reason).unwrap(), json!(name));
        assert_eq!(
            serde_json::from_value::<StopReason>(json!(name)).unwrap(),
            reason
        );
    }
    assert!(serde_json::from_value::<StopReason>(json!("nope")).is_err());
}

#[test]
fn only_duration_and_owner_stop_save() {
    for reason in ALL_REASONS {
        let expect = matches!(reason, StopReason::Duration | StopReason::OwnerStop);
        assert_eq!(reason.is_save(), expect, "{reason:?}");
    }
}

#[test]
fn a_lease_carries_exactly_the_reasons_of_the_table() {
    let discard = [
        StopReason::DeckPolicy,
        StopReason::OwnerLoginEnded,
        StopReason::CoordinatorAuthorityLost,
        StopReason::ArtifactDeleted,
        StopReason::Deadline,
    ];
    for reason in ALL_REASONS {
        assert_eq!(
            reason.lease_allowed(LeaseDecision::Save),
            reason == StopReason::OwnerStop,
            "save {reason:?}"
        );
        assert_eq!(
            reason.lease_allowed(LeaseDecision::Discard),
            discard.contains(&reason),
            "discard {reason:?}"
        );
        assert!(
            !reason.lease_allowed(LeaseDecision::Continue),
            "continue {reason:?}"
        );
    }
}

#[test]
fn the_owner_code_follows_the_table() {
    let ri = "recording_interrupted";
    let table = [
        (StopReason::PolicyOff, ri),
        (StopReason::ScreenLocked, "screen_locked"),
        (StopReason::MonitorChanged, "desktop_not_found"),
        (StopReason::LinkClosed, ri),
        (StopReason::LeaseExpired, ri),
        (StopReason::LeaseDenied, ri),
        (StopReason::DeckPolicy, ri),
        (StopReason::OwnerLoginEnded, ri),
        (StopReason::DiskReserve, "disk_full"),
        (StopReason::RecorderExited, "recording_failed"),
        (StopReason::StationShutdown, ri),
        (StopReason::RecordingInterrupted, ri),
        (StopReason::ArtifactDeleted, ri),
        (StopReason::Deadline, "desktop_timeout"),
        // The table has no owner code here; the answer is the generic one.
        (StopReason::CoordinatorAuthorityLost, ri),
        (StopReason::Duration, ri),
        (StopReason::OwnerStop, ri),
    ];
    for (reason, want) in table {
        for ceiling in [false, true] {
            assert_eq!(reason.owner_code(ceiling), want, "{reason:?} {ceiling}");
        }
    }
    assert_eq!(
        StopReason::OutputLimit.owner_code(true),
        "recording_too_large"
    );
    assert_eq!(
        StopReason::OutputLimit.owner_code(false),
        "recording_failed"
    );
}

#[test]
fn the_coordinator_code_follows_the_table() {
    let lost = Some("authority_lost");
    let unknown = Some("authority_unknown");
    let table = [
        (StopReason::PolicyOff, lost),
        (StopReason::ScreenLocked, Some("screen_locked")),
        (StopReason::MonitorChanged, Some("desktop_not_found")),
        (StopReason::LinkClosed, unknown),
        (StopReason::LeaseExpired, unknown),
        (StopReason::LeaseDenied, unknown),
        (StopReason::DeckPolicy, lost),
        (StopReason::OwnerLoginEnded, None),
        (StopReason::CoordinatorAuthorityLost, lost),
        (StopReason::DiskReserve, Some("disk_full")),
        (StopReason::RecorderExited, Some("recording_failed")),
        (StopReason::StationShutdown, unknown),
        (StopReason::RecordingInterrupted, unknown),
        (StopReason::ArtifactDeleted, lost),
        (StopReason::Deadline, Some("desktop_read_timeout")),
        (StopReason::Duration, None),
        (StopReason::OwnerStop, None),
    ];
    for (reason, want) in table {
        for ceiling in [false, true] {
            assert_eq!(reason.coordinator_code(ceiling), want, "{reason:?}");
        }
    }
    assert_eq!(
        StopReason::OutputLimit.coordinator_code(true),
        Some("recording_too_large")
    );
    assert_eq!(
        StopReason::OutputLimit.coordinator_code(false),
        Some("recording_failed")
    );
}

#[test]
fn the_recording_codes_and_texts_are_exact() {
    let rows = [
        (code::RECORDING_DURATION, "recording_duration"),
        (code::RECORDING_TOO_LONG, "recording_too_long"),
        (code::RECORDING_DIMENSIONS, "recording_dimensions"),
        (code::RECORDING_TOO_LARGE, "recording_too_large"),
        (code::RECORDING_FAILED, "recording_failed"),
        (code::RECORDING_INVALID, "recording_invalid"),
        (code::RECORDING_PUBLISH_FAILED, "recording_publish_failed"),
        (code::RECORDING_CANCELLED, "recording_cancelled"),
        (code::RECORDING_INTERRUPTED, "recording_interrupted"),
        (code::NOT_RECORDING, "not_recording"),
        (code::CAPTURE_ID_REQUIRED, "capture_id_required"),
    ];
    for (got, want) in rows {
        assert_eq!(got, want);
    }
    use crate::capture::text;
    assert_eq!(
        text::RECORDING_DURATION,
        "The recording length must be from 1 to 3600 seconds."
    );
    assert_eq!(
        text::RECORDING_TOO_LONG,
        "This recording is longer than this station allows. Shorten it, or raise Longest recording in the station settings."
    );
    assert_eq!(
        text::RECORDING_DIMENSIONS,
        "The monitor exceeds the recording size limit or has unsupported dimensions."
    );
    assert_eq!(text::RECORDING_TOO_LARGE, "The recording exceeds 256 MiB.");
    assert_eq!(text::RECORDING_FAILED, "The recording tool failed.");
    assert_eq!(
        text::RECORDING_INVALID,
        "The recording file could not be verified."
    );
    assert_eq!(
        text::RECORDING_PUBLISH_FAILED,
        "The recording could not be saved as an artifact."
    );
    assert_eq!(
        text::RECORDING_CANCELLED,
        "The recording was stopped before it started."
    );
    assert_eq!(
        text::RECORDING_INTERRUPTED,
        "The recording was interrupted before it was saved."
    );
    assert_eq!(text::NOT_RECORDING, "This capture is not a recording.");
    assert_eq!(text::CAPTURE_ID_REQUIRED, "A recording needs a capture ID.");
    assert_eq!(
        text::NO_RECORDER,
        "The recording tool wf-recorder is not installed."
    );
    assert_eq!(
        text::RECORDING_UNSUPPORTED,
        "This station does not support monitor recordings."
    );
}

#[test]
fn recording_meta_round_trips_and_refuses_unknown_fields() {
    let m = meta();
    let v = serde_json::to_value(&m).unwrap();
    assert_eq!(
        v,
        json!({"requested_seconds":30,"duration_ms":30040,"width":1920,
               "height":1080,"stop_reason":"duration"})
    );
    assert_eq!(
        serde_json::from_value::<RecordingMeta>(v.clone()).unwrap(),
        m
    );
    let mut extra = v;
    extra["x"] = json!(1);
    assert!(serde_json::from_value::<RecordingMeta>(extra).is_err());
}

#[test]
fn a_recording_provenance_carries_the_metadata_and_a_screenshot_does_not() {
    let rec = provenance(CaptureKind::Recording, Some(meta()));
    let v = serde_json::to_value(&rec).unwrap();
    assert_eq!(v["kind"], "recording");
    assert_eq!(v["recording"]["stop_reason"], "duration");
    assert_eq!(serde_json::from_value::<CaptureProvenance>(v).unwrap(), rec);

    let shot = provenance(CaptureKind::Screenshot, None);
    let v = serde_json::to_value(&shot).unwrap();
    assert!(!v.as_object().unwrap().contains_key("recording"));
}

#[test]
fn a_recording_status_flattens_the_capture_status() {
    let id = "01ARZ3NDEKTSV4RRFFQ69G5FAV".to_owned();
    let plain = RecordingCaptureStatus {
        status: CaptureStatus::Pending {
            capture_id: id.clone(),
        },
        recording: None,
        stop_reason: None,
    };
    assert_eq!(
        serde_json::to_value(&plain).unwrap(),
        json!({"state":"pending","capture_id":id})
    );
    let full = RecordingCaptureStatus {
        status: CaptureStatus::Pending {
            capture_id: id.clone(),
        },
        recording: Some(RecordingProgress {
            phase: RecordingPhase::Recording,
            requested_seconds: 30,
            started_at: Some(5),
            elapsed_ms: None,
        }),
        stop_reason: Some(StopReason::OwnerStop),
    };
    let v = serde_json::to_value(&full).unwrap();
    assert_eq!(
        v,
        json!({
            "state":"pending","capture_id":id,
            "recording":{"phase":"recording","requested_seconds":30,
                         "started_at":5,"elapsed_ms":null},
            "stop_reason":"owner_stop"
        })
    );
    assert_eq!(
        serde_json::from_value::<RecordingCaptureStatus>(v).unwrap(),
        full
    );
    // An old P5a status parses with no recording fields.
    let old = json!({"state":"unknown","capture_id":id});
    let parsed: RecordingCaptureStatus = serde_json::from_value(old).unwrap();
    assert_eq!(parsed.recording, None);
    assert_eq!(parsed.stop_reason, None);
}

#[test]
fn the_active_recording_has_the_tagged_shape() {
    let idle = serde_json::to_value(ActiveRecording::Idle).unwrap();
    assert_eq!(idle, json!({"state":"idle"}));
    assert_eq!(
        serde_json::from_value::<ActiveRecording>(idle).unwrap(),
        ActiveRecording::Idle
    );
    let active = ActiveRecording::Active {
        capture_id: "01ARZ3NDEKTSV4RRFFQ69G5FAV".into(),
        status: RecordingCaptureStatus {
            status: CaptureStatus::Pending {
                capture_id: "01ARZ3NDEKTSV4RRFFQ69G5FAV".into(),
            },
            recording: None,
            stop_reason: None,
        },
    };
    let v = serde_json::to_value(&active).unwrap();
    assert_eq!(v["state"], "active");
    assert_eq!(v["status"]["state"], "pending");
    assert_eq!(
        serde_json::from_value::<ActiveRecording>(v).unwrap(),
        active
    );
}

#[test]
fn the_control_params_and_answers_round_trip() {
    let stop = CaptureStopParams {
        capture_id: "01ARZ3NDEKTSV4RRFFQ69G5FAV".into(),
        reason: StopReason::OwnerStop,
    };
    let v = serde_json::to_value(&stop).unwrap();
    assert_eq!(
        v,
        json!({"capture_id":"01ARZ3NDEKTSV4RRFFQ69G5FAV","reason":"owner_stop"})
    );
    assert_eq!(
        serde_json::from_value::<CaptureStopParams>(v.clone()).unwrap(),
        stop
    );
    let mut extra = v;
    extra["pid"] = json!(1);
    assert!(serde_json::from_value::<CaptureStopParams>(extra).is_err());

    let now = CapturePolicyNowParams {
        capture_id: "01ARZ3NDEKTSV4RRFFQ69G5FAV".into(),
        action_hash: "ab".into(),
    };
    let v = serde_json::to_value(&now).unwrap();
    assert_eq!(
        serde_json::from_value::<CapturePolicyNowParams>(v).unwrap(),
        now
    );

    let answer = CapturePolicyAnswer {
        valid: true,
        desktop_epoch: "e1".into(),
        allow_desktop: true,
        allow_desktop_capture: true,
        monitor_allowed: true,
        monitor_present: true,
        unlocked: true,
    };
    let v = serde_json::to_value(&answer).unwrap();
    assert_eq!(
        serde_json::from_value::<CapturePolicyAnswer>(v).unwrap(),
        answer
    );

    let req = CaptureLeaseRequest {
        capture_id: "01ARZ3NDEKTSV4RRFFQ69G5FAV".into(),
        action_hash: "ab".into(),
        sequence: 1,
        phase: LeasePhase::Import,
    };
    let v = serde_json::to_value(&req).unwrap();
    assert_eq!(v["phase"], "import");
    assert_eq!(
        serde_json::from_value::<CaptureLeaseRequest>(v).unwrap(),
        req
    );

    for (phase, name) in [
        (LeasePhase::Start, "start"),
        (LeasePhase::Record, "record"),
        (LeasePhase::Import, "import"),
    ] {
        assert_eq!(serde_json::to_value(phase).unwrap(), json!(name));
    }
    for (decision, name) in [
        (LeaseDecision::Continue, "continue"),
        (LeaseDecision::Save, "save"),
        (LeaseDecision::Discard, "discard"),
    ] {
        assert_eq!(serde_json::to_value(decision).unwrap(), json!(name));
    }
    for (phase, name) in [
        (RecordingPhase::Preparing, "preparing"),
        (RecordingPhase::Recording, "recording"),
        (RecordingPhase::Stopping, "stopping"),
        (RecordingPhase::Finalizing, "finalizing"),
    ] {
        assert_eq!(serde_json::to_value(phase).unwrap(), json!(name));
    }

    let cont = CaptureLeaseAnswer {
        sequence: 2,
        decision: LeaseDecision::Continue,
        reason: None,
    };
    let v = serde_json::to_value(&cont).unwrap();
    assert_eq!(v, json!({"sequence":2,"decision":"continue"}));
    assert_eq!(
        serde_json::from_value::<CaptureLeaseAnswer>(v).unwrap(),
        cont
    );
    let disc = CaptureLeaseAnswer {
        sequence: 3,
        decision: LeaseDecision::Discard,
        reason: Some(StopReason::DeckPolicy),
    };
    let v = serde_json::to_value(&disc).unwrap();
    assert_eq!(v["reason"], "deck_policy");
    assert_eq!(
        serde_json::from_value::<CaptureLeaseAnswer>(v).unwrap(),
        disc
    );
}

#[test]
fn the_legacy_projection_hides_video_and_recording_provenance() {
    let mut p = published(
        ArtifactKind::Video,
        Some(provenance(CaptureKind::Recording, Some(meta()))),
    );
    let stored = p.clone();
    project_legacy(&mut p);
    assert_eq!(p.artifact.kind, ArtifactKind::Other);
    assert_eq!(p.version.capture, None);
    assert_eq!(p.version.sha256, stored.version.sha256);
    assert_eq!(p.version.files, stored.version.files);
}

#[test]
fn the_legacy_projection_keeps_a_screenshot_and_an_other_artifact() {
    let shot = published(
        ArtifactKind::Image,
        Some(provenance(CaptureKind::Screenshot, None)),
    );
    let mut p = shot.clone();
    project_legacy(&mut p);
    assert_eq!(p, shot);

    let plain = published(ArtifactKind::Other, None);
    let mut p = plain.clone();
    project_legacy(&mut p);
    assert_eq!(p, plain);
}
