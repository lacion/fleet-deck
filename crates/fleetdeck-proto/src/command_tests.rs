use super::*;
use serde_json::json;

fn envelope(retry_until: i64, target: Option<&str>) -> CommandEnvelope {
    CommandEnvelope {
        authority: None,
        command_id: "01J0000000000000000000000A".into(),
        target_incarnation: target.map(IncarnationId::from),
        retry_until,
        origin: CommandOrigin::Board,
        command: SessionCommand::Interrupt {
            session_id: "s1".into(),
        },
    }
}

#[test]
fn commands_serialize_with_an_op_tag_and_map_to_methods() {
    let cmd = SessionCommand::Answer {
        session_id: "s1".into(),
        request_id: "req-1".into(),
        decision: ApprovalDecision {
            answered_by: None,
            option_id: "allow".into(),
            updated_input: None,
            text: None,
        },
    };
    let v = serde_json::to_value(&cmd).unwrap();
    assert_eq!(v["op"], "answer");
    assert_eq!(cmd.method(), "approval.answer");
    assert_eq!(cmd.session_id().map(SessionId::as_str), Some("s1"));
}

#[test]
fn admission_checks_the_incarnation_and_the_deadline() {
    let inc = IncarnationId::from("inc-1");
    let now = 1_000_000_000;
    assert!(envelope(now, Some("inc-1")).admit(now, &inc).is_ok());
    assert!(envelope(now, None).admit(now, &inc).is_ok());
    let err = envelope(now, Some("inc-0")).admit(now, &inc).unwrap_err();
    assert_eq!(err.code, RejectCode::IncarnationMismatch);
    assert!(!err.code.stores_receipt());
    // Inside the skew allowance, then expired.
    assert!(envelope(now - CLOCK_SKEW_MS, None).admit(now, &inc).is_ok());
    let err = envelope(now - CLOCK_SKEW_MS - 1, None)
        .admit(now, &inc)
        .unwrap_err();
    assert_eq!(err.code, RejectCode::Expired);
    assert!(!err.code.stores_receipt());
    let far = now + RETRY_WINDOW_MS + CLOCK_SKEW_MS + 1;
    let err = envelope(far, None).admit(now, &inc).unwrap_err();
    assert_eq!(err.code, RejectCode::InvalidCommand);
    assert!(err.code.stores_receipt());
}

// Receipts outlive the retry window (G3).
const _: () = assert!(RECEIPT_RETENTION_MS > RETRY_WINDOW_MS + 2 * CLOCK_SKEW_MS);

#[test]
fn an_unknown_reject_code_degrades() {
    let r: Reject = serde_json::from_value(json!({
        "code": "quota_exceeded", "message": "m", "unsupported": null
    }))
    .unwrap();
    assert_eq!(r.code, RejectCode::Unknown);
}

#[test]
fn equal_commands_have_equal_canonical_payloads() {
    let a = envelope(1, None);
    let mut b = envelope(2, Some("x"));
    assert_eq!(a.canonical_payload(), b.canonical_payload());
    b.command = SessionCommand::Fork {
        session_id: "s1".into(),
    };
    assert_ne!(a.canonical_payload(), b.canonical_payload());
}

#[test]
fn an_import_carries_a_native_id_that_can_never_read_as_a_flag() {
    let cmd: SessionCommand = serde_json::from_value(json!({
        "op": "import",
        "spec": {"provider_instance": "claude", "cwd": "/w", "mode": null, "model": null, "title": null},
        "native_session_id": "0b6c1d2e-aaaa-4bbb-8ccc-123456789abc",
        "prompt": null,
    }))
    .unwrap();
    assert_eq!(cmd.method(), "session.import");
    assert_eq!(cmd.session_id(), None);
    assert!(check_native_session_id("0b6c1d2e-aaaa-4bbb-8ccc-123456789abc").is_ok());
    assert!(check_native_session_id("thr_01.x").is_ok());
    for bad in [
        "",
        "-r",
        "--resume",
        "a b",
        "a/../b",
        "a;rm",
        &"a".repeat(129),
    ] {
        assert!(check_native_session_id(bad).is_err(), "{bad}");
    }
}

fn launched_spec() -> SessionSpec {
    SessionSpec {
        provider_instance: "claude".into(),
        cwd: "/w".into(),
        mode: None,
        model: None,
        title: None,
        coordinator: None,
        launch: Some(Box::new(crate::launch::LaunchIdentity {
            root: "/".into(),
            rel: String::new(),
            dev: 1,
            ino: 2,
        })),
    }
}

#[test]
fn a_command_never_keeps_a_launch_identity() {
    let mut start = SessionCommand::Start {
        spec: launched_spec(),
        prompt: None,
        workspace: None,
        setup_cmd: None,
        batch: None,
        issue: None,
        origin_text: false,
    };
    start.clear_station_local();
    let mut import = SessionCommand::Import {
        spec: launched_spec(),
        native_session_id: "thr_01".into(),
        prompt: None,
        transcript: None,
    };
    import.clear_station_local();
    for cmd in [&start, &import] {
        match cmd {
            SessionCommand::Start { spec, .. } | SessionCommand::Import { spec, .. } => {
                assert!(spec.launch.is_none());
            }
            _ => unreachable!(),
        }
    }
    // A JSON command from the deck deserializes the field, then loses it.
    let mut wire: SessionCommand = serde_json::from_value(json!({
        "op": "start",
        "spec": {
            "provider_instance": "claude", "cwd": "/w", "mode": null, "model": null,
            "title": null,
            "launch": {"root": "/", "rel": "", "dev": 1, "ino": 2},
        },
        "prompt": null,
    }))
    .unwrap();
    match &wire {
        SessionCommand::Start { spec, .. } => assert!(spec.launch.is_some()),
        _ => unreachable!(),
    }
    wire.clear_station_local();
    match &wire {
        SessionCommand::Start { spec, .. } => assert!(spec.launch.is_none()),
        _ => unreachable!(),
    }
}
