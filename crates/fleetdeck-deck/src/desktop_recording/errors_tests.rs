use super::*;

#[test]
fn every_recording_code_has_its_status_and_exact_text() {
    let cases = [
        ("busy", 409, "A capture is already running."),
        (
            "disk_full",
            507,
            "The station has too little free space for a capture.",
        ),
        (
            "capture_quota",
            409,
            "This capture would exceed the station's capture storage limit. Delete some captures first, or raise Capture storage in the station settings.",
        ),
        ("recording_too_large", 413, "The recording exceeds 256 MiB."),
        (
            "recording_dimensions",
            422,
            "The monitor exceeds the recording size limit or has unsupported dimensions.",
        ),
        ("recording_failed", 502, "The recording tool failed."),
        (
            "recording_invalid",
            502,
            "The recording file could not be verified.",
        ),
        (
            "recording_publish_failed",
            502,
            "The recording could not be saved as an artifact.",
        ),
        (
            "recording_cancelled",
            409,
            "The recording was stopped before it started.",
        ),
        (
            "recording_interrupted",
            409,
            "The recording was interrupted before it was saved.",
        ),
        (
            "screen_locked",
            409,
            "The screen is locked or its lock state is unknown.",
        ),
        (
            "capture_id_conflict",
            409,
            "This capture ID was used for another request.",
        ),
        (
            "desktop_not_found",
            404,
            "The target of the desktop action was not found.",
        ),
        (
            "desktop_timeout",
            504,
            "The station did not answer in time; the action may have run.",
        ),
        (
            "recording_too_long",
            409,
            "This recording is longer than this station allows. Shorten it, or raise Longest recording in the station settings.",
        ),
    ];
    assert_eq!(cases.len(), KNOWN.len());
    for (code, status, text) in cases {
        let (e, stored, fixed) = RecordingError::from_station(code, "any station text").unwrap();
        assert_eq!(
            e.http(),
            (StatusCode::from_u16(status).unwrap(), code, text),
            "{code}"
        );
        assert_eq!((stored, fixed), (code, text));
    }
}

#[test]
fn unsupported_has_the_missing_tool_text_only_for_the_exact_station_text() {
    let (e, stored, fixed) =
        RecordingError::from_station("unsupported", text::NO_RECORDER).unwrap();
    assert_eq!(
        e.http(),
        (
            StatusCode::NOT_IMPLEMENTED,
            "desktop_unsupported",
            "The recording tool wf-recorder is not installed."
        )
    );
    assert_eq!((stored, fixed), ("unsupported", text::NO_RECORDER));
    for other in ["", "x", "The recording tool wf-recorder is not installed"] {
        let (e, _, fixed) = RecordingError::from_station("unsupported", other).unwrap();
        assert_eq!(
            e.http(),
            (
                StatusCode::NOT_IMPLEMENTED,
                "desktop_unsupported",
                "This station does not support monitor recordings."
            )
        );
        assert_eq!(fixed, text::RECORDING_UNSUPPORTED);
    }
}

#[test]
fn a_relay_unsupported_is_a_501_with_the_recording_text_and_other_relay_errors_keep_theirs() {
    assert_eq!(
        RecordingError::Relay(RelayError::Unsupported).http(),
        (
            StatusCode::NOT_IMPLEMENTED,
            "desktop_unsupported",
            "This station does not support monitor recordings."
        )
    );
    assert_eq!(
        RecordingError::Relay(RelayError::Unreachable).http(),
        (
            StatusCode::CONFLICT,
            "station_unreachable",
            "The station is not connected."
        )
    );
    assert_eq!(
        RecordingError::Relay(RelayError::ControlOff).http(),
        (
            StatusCode::CONFLICT,
            "desktop_control_off",
            "Desktop control is off on this deck."
        )
    );
}

#[test]
fn an_unknown_station_code_is_not_relayed_and_a_failed_status_falls_back_to_recording_failed() {
    for unknown in ["forbidden", "capture_failed", "something_new", "failed"] {
        assert!(
            RecordingError::from_station(unknown, "x").is_none(),
            "{unknown}"
        );
        assert_eq!(
            RecordingError::fixed(unknown, "x"),
            ("recording_failed", "The recording tool failed.")
        );
    }
}

#[test]
fn the_audit_result_of_a_refusal_is_refused_and_of_a_failure_failed() {
    assert_eq!(
        RecordingError::id_conflict().audit_result(),
        AuditResult::Refused
    );
    assert_eq!(
        RecordingError::invalid_body().audit_result(),
        AuditResult::Refused
    );
    assert_eq!(
        RecordingError::unsupported().audit_result(),
        AuditResult::Refused
    );
    assert_eq!(
        RecordingError::Relay(RelayError::Timeout).audit_result(),
        AuditResult::Failed
    );
    let (e, _, _) = RecordingError::from_station("recording_failed", "").unwrap();
    assert_eq!(e.audit_result(), AuditResult::Failed);
}
