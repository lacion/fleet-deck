use serde_json::{Value, json};

use super::*;
use crate::ids::ProviderInstanceId;

fn binding() -> CoordinatorBinding {
    CoordinatorBinding {
        binding_id: "b1".into(),
        grant_id: "g1".into(),
        generation: 2,
        provider_instance: ProviderInstanceId("claude".into()),
        native_id: "n1".into(),
    }
}

fn ctx() -> CoordinatorContext {
    CoordinatorContext {
        grant_id: "g1".into(),
        binding_id: "b1".into(),
        generation: 2,
        caller_station: "st-a".into(),
        caller_session: "01J0000000000000000000000S".into(),
        caller_label: "swift-otter".into(),
    }
}

#[test]
fn the_context_has_six_plain_string_fields_and_refuses_more() {
    let v = serde_json::to_value(ctx()).unwrap();
    assert_eq!(
        v,
        json!({
            "grant_id": "g1", "binding_id": "b1", "generation": 2,
            "caller_station": "st-a", "caller_session": "01J0000000000000000000000S",
            "caller_label": "swift-otter"
        })
    );
    assert_eq!(
        serde_json::from_value::<CoordinatorContext>(v.clone()).unwrap(),
        ctx()
    );
    let mut extra = v;
    extra["x"] = json!(1);
    assert!(serde_json::from_value::<CoordinatorContext>(extra).is_err());
}

#[test]
fn the_call_envelope_round_trips_and_is_closed() {
    let env = CoordinatorDesktopCallEnvelope {
        command_id: CommandId::from("01J0000000000000000000000C"),
        target_incarnation: IncarnationId::from("01J0000000000000000000000I"),
        retry_until: 99,
        caller: SessionId::from("01J0000000000000000000000S"),
        binding: binding(),
        call: serde_json::from_value(json!({"tool": "screenshot", "arguments": {
            "request_id": "r", "monitor": "DP-1"}}))
        .unwrap(),
    };
    let v = serde_json::to_value(&env).unwrap();
    assert_eq!(v["call"]["tool"], "screenshot");
    assert_eq!(v["retry_until"], 99);
    assert_eq!(
        serde_json::from_value::<CoordinatorDesktopCallEnvelope>(v.clone()).unwrap(),
        env
    );
    let mut extra = v.clone();
    extra["x"] = json!(1);
    assert!(serde_json::from_value::<CoordinatorDesktopCallEnvelope>(extra).is_err());
    let mut other_tool = v;
    other_tool["call"]["tool"] = json!("record_demo");
    assert!(serde_json::from_value::<CoordinatorDesktopCallEnvelope>(other_tool).is_err());
}

#[test]
fn the_query_envelope_round_trips_and_is_closed() {
    let env = CoordinatorDesktopQueryEnvelope {
        query_id: "q1".into(),
        target_incarnation: IncarnationId::from("01J0000000000000000000000I"),
        deadline: 5,
        caller: SessionId::from("01J0000000000000000000000S"),
        binding: binding(),
        query: serde_json::from_value(json!({"tool": "desktop_info", "arguments": {}})).unwrap(),
    };
    let v = serde_json::to_value(&env).unwrap();
    assert_eq!(v["query"], json!({"tool": "desktop_info", "arguments": {}}));
    assert_eq!(
        serde_json::from_value::<CoordinatorDesktopQueryEnvelope>(v.clone()).unwrap(),
        env
    );
    let mut extra = v;
    extra["retry_until"] = json!(1);
    assert!(serde_json::from_value::<CoordinatorDesktopQueryEnvelope>(extra).is_err());
}

#[test]
fn a_reply_is_tagged_by_status_and_skips_absent_ids() {
    let ok = DesktopReply::Ok {
        result: json!({"window_id": "w"}),
    };
    assert_eq!(
        serde_json::to_value(&ok).unwrap(),
        json!({"status": "ok", "result": {"window_id": "w"}})
    );
    let err = DesktopReply::Err {
        error: "busy".into(),
        message: "m".into(),
        operation_id: None,
        capture_id: Some("c".into()),
    };
    let v = serde_json::to_value(&err).unwrap();
    assert_eq!(
        v,
        json!({"status": "err", "error": "busy", "message": "m", "capture_id": "c"})
    );
    assert_eq!(serde_json::from_value::<DesktopReply>(v).unwrap(), err);
    assert!(
        serde_json::from_value::<DesktopReply>(json!({"status": "ok", "result": 1, "x": 1}))
            .is_err()
    );
}

#[test]
fn a_protected_read_is_tagged_by_kind() {
    let cases = [
        (ProtectedRead::Info, json!({"kind": "info"})),
        (
            ProtectedRead::ActionStatus {
                operation_id: "op".into(),
            },
            json!({"kind": "action_status", "operation_id": "op"}),
        ),
        (
            ProtectedRead::CaptureStatus {
                capture_id: "c".into(),
            },
            json!({"kind": "capture_status", "capture_id": "c"}),
        ),
        (
            ProtectedRead::CaptureFile {
                artifact_id: "a".into(),
                version: 1,
                offset: 0,
                length: 716_800,
            },
            json!({"kind": "capture_file", "artifact_id": "a", "version": 1,
                "offset": 0, "length": 716_800}),
        ),
    ];
    for (read, json) in cases {
        assert_eq!(serde_json::to_value(&read).unwrap(), json);
        assert_eq!(serde_json::from_value::<ProtectedRead>(json).unwrap(), read);
    }
    // A non-zero offset parses: the target refuses it, not serde.
    let far: ProtectedRead = serde_json::from_value(json!({"kind": "capture_file",
        "artifact_id": "a", "version": 1, "offset": 5, "length": 1}))
    .unwrap();
    assert!(matches!(far, ProtectedRead::CaptureFile { offset: 5, .. }));
    // A field of a struct variant that does not exist is refused. (serde
    // ignores the extra fields of the unit variant `info`; the target runs
    // the typed value and the authority check compares its re-serialization.)
    assert!(
        serde_json::from_value::<ProtectedRead>(
            json!({"kind": "capture_status", "capture_id": "c", "x": 1})
        )
        .is_err()
    );
    assert!(serde_json::from_value::<ProtectedRead>(json!({"kind": "file"})).is_err());
}

#[test]
fn a_read_request_and_an_authority_check_have_their_shapes() {
    let read = CoordinatorRead {
        read_id: "r".repeat(32),
        read: ProtectedRead::Info,
        read_hash: "ab".repeat(32),
    };
    let v = serde_json::to_value(&read).unwrap();
    assert_eq!(v["read"], json!({"kind": "info"}));
    assert_eq!(serde_json::from_value::<CoordinatorRead>(v).unwrap(), read);

    let execute = DesktopAuthorityCheck {
        operation_id: "op".into(),
        action_hash: "ab".into(),
        phase: AuthorityPhase::Execute,
        read: None,
    };
    assert_eq!(
        serde_json::to_value(&execute).unwrap(),
        json!({"operation_id": "op", "action_hash": "ab", "phase": "execute"})
    );
    let phase_read = DesktopAuthorityCheck {
        phase: AuthorityPhase::Read,
        read: Some(ProtectedRead::Info),
        ..execute
    };
    let v = serde_json::to_value(&phase_read).unwrap();
    assert_eq!(v["phase"], "read");
    assert_eq!(v["read"], json!({"kind": "info"}));
    assert_eq!(
        serde_json::from_value::<DesktopAuthorityCheck>(v).unwrap(),
        phase_read
    );
    for phase in ["execute", "import", "read"] {
        let p: AuthorityPhase = serde_json::from_value(json!(phase)).unwrap();
        assert_eq!(serde_json::to_value(p).unwrap(), json!(phase));
    }
    assert!(serde_json::from_value::<AuthorityPhase>(json!("finish")).is_err());
}

#[test]
fn the_authority_answer_always_writes_the_reason() {
    let valid = DesktopAuthorityAnswer {
        valid: true,
        reason: None,
    };
    assert_eq!(
        serde_json::to_value(valid).unwrap(),
        json!({"valid": true, "reason": null})
    );
    for (reason, text) in [
        (AuthorityReason::AuthorityLost, "authority_lost"),
        (AuthorityReason::AuthorityUnknown, "authority_unknown"),
    ] {
        let a = DesktopAuthorityAnswer {
            valid: false,
            reason: Some(reason),
        };
        let v = serde_json::to_value(&a).unwrap();
        assert_eq!(v["reason"], text);
        assert_eq!(
            serde_json::from_value::<DesktopAuthorityAnswer>(v).unwrap(),
            a
        );
    }
    assert!(
        serde_json::from_value::<DesktopAuthorityAnswer>(
            json!({"valid": false, "reason": "no_v2"})
        )
        .is_err()
    );
}

#[test]
fn the_policy_request_and_answer_have_their_shapes() {
    let ask = DesktopPolicyNow {
        purpose: PolicyPurpose::Desktop,
        monitor: None,
    };
    assert_eq!(
        serde_json::to_value(&ask).unwrap(),
        json!({"purpose": "desktop", "monitor": null})
    );
    let capture = DesktopPolicyNow {
        purpose: PolicyPurpose::Capture,
        monitor: Some("DP-1".into()),
    };
    let v = serde_json::to_value(&capture).unwrap();
    assert_eq!(v, json!({"purpose": "capture", "monitor": "DP-1"}));
    assert_eq!(
        serde_json::from_value::<DesktopPolicyNow>(v).unwrap(),
        capture
    );
    // An unknown purpose is refused. The purpose `input` exists since R13 P7
    // §6.4 (feature `desktop_input_mcp.v1`).
    assert!(serde_json::from_value::<DesktopPolicyNow>(json!({"purpose": "click"})).is_err());
    assert!(serde_json::from_value::<DesktopPolicyNow>(json!({"purpose": "Input"})).is_err());
    let answer = DesktopPolicyAnswer {
        allow_fleet_control: true,
        allow_desktop: false,
        allow_desktop_capture: true,
        monitor_allowed: false,
    };
    let v: Value = serde_json::to_value(&answer).unwrap();
    assert_eq!(
        v,
        json!({"allow_fleet_control": true, "allow_desktop": false,
            "allow_desktop_capture": true, "monitor_allowed": false})
    );
    assert_eq!(
        serde_json::from_value::<DesktopPolicyAnswer>(v).unwrap(),
        answer
    );
}

#[test]
fn the_input_policy_request_and_answer_have_their_shapes() {
    // §6.4: `{purpose: "input", monitor: string | null}`; `monitor` is always
    // written, as for the P6 purposes.
    let ask = DesktopPolicyNow {
        purpose: PolicyPurpose::Input,
        monitor: None,
    };
    let v = serde_json::to_value(&ask).unwrap();
    assert_eq!(v, json!({"purpose": "input", "monitor": null}));
    assert_eq!(serde_json::from_value::<DesktopPolicyNow>(v).unwrap(), ask);
    let named = DesktopPolicyNow {
        purpose: PolicyPurpose::Input,
        monitor: Some("DP-1".into()),
    };
    assert_eq!(
        serde_json::to_value(&named).unwrap(),
        json!({"purpose": "input", "monitor": "DP-1"})
    );
    // The closed answer of this purpose has the five booleans of §6.4.
    let answer = DesktopInputPolicyAnswer {
        allow_fleet_control: true,
        allow_desktop: true,
        allow_desktop_capture: true,
        allow_desktop_input: false,
        monitor_allowed: true,
    };
    let v: Value = serde_json::to_value(&answer).unwrap();
    assert_eq!(
        v,
        json!({"allow_fleet_control": true, "allow_desktop": true,
            "allow_desktop_capture": true, "allow_desktop_input": false,
            "monitor_allowed": true})
    );
    assert_eq!(
        serde_json::from_value::<DesktopInputPolicyAnswer>(v.clone()).unwrap(),
        answer
    );
    // It is closed: a missing or an extra field is refused, and the P6 answer
    // is not an input answer (and the reverse).
    let mut extra = v.clone();
    extra["x"] = json!(1);
    assert!(serde_json::from_value::<DesktopInputPolicyAnswer>(extra).is_err());
    let mut missing = v.clone();
    missing
        .as_object_mut()
        .unwrap()
        .remove("allow_desktop_input");
    assert!(serde_json::from_value::<DesktopInputPolicyAnswer>(missing).is_err());
    assert!(serde_json::from_value::<DesktopPolicyAnswer>(v).is_err());
    // The P6 request and answer bytes are unchanged.
    for (purpose, text) in [
        (PolicyPurpose::Desktop, "desktop"),
        (PolicyPurpose::Capture, "capture"),
    ] {
        let ask = DesktopPolicyNow {
            purpose,
            monitor: None,
        };
        assert_eq!(
            serde_json::to_string(&ask).unwrap(),
            format!(r#"{{"purpose":"{text}","monitor":null}}"#)
        );
    }
    let p6 = DesktopPolicyAnswer {
        allow_fleet_control: true,
        allow_desktop: true,
        allow_desktop_capture: false,
        monitor_allowed: true,
    };
    assert_eq!(
        serde_json::to_string(&p6).unwrap(),
        r#"{"allow_fleet_control":true,"allow_desktop":true,"allow_desktop_capture":false,"monitor_allowed":true}"#
    );
}

#[test]
fn the_p7b_policy_purposes_have_their_wire_names_and_answers_fit_them() {
    // R13 P7b §9.2: the three new purposes; the bytes of `input` do not move.
    for (purpose, text) in [
        (PolicyPurpose::InputClick, "input_click"),
        (PolicyPurpose::InputAny, "input_any"),
        (PolicyPurpose::InputAnyClick, "input_any_click"),
    ] {
        let ask = DesktopPolicyNow {
            purpose,
            monitor: None,
        };
        let v = serde_json::to_value(&ask).unwrap();
        assert_eq!(v, json!({"purpose": text, "monitor": null}));
        assert_eq!(serde_json::from_value::<DesktopPolicyNow>(v).unwrap(), ask);
    }
    assert_eq!(
        serde_json::to_string(&DesktopPolicyNow {
            purpose: PolicyPurpose::Input,
            monitor: None
        })
        .unwrap(),
        r#"{"purpose":"input","monitor":null}"#
    );
    for bad in ["inputclick", "Input_Click", "input_any_clicks", "any"] {
        assert!(
            serde_json::from_value::<DesktopPolicyNow>(json!({"purpose": bad})).is_err(),
            "{bad}"
        );
    }
}

fn gates(click: Option<bool>, any: Option<bool>) -> DesktopInputGatesAnswer {
    DesktopInputGatesAnswer {
        allow_fleet_control: true,
        allow_desktop: true,
        allow_desktop_capture: true,
        allow_desktop_input: true,
        monitor_allowed: true,
        allow_desktop_click: click,
        allow_desktop_input_any: any,
    }
}

#[test]
fn the_input_gates_answer_writes_only_the_gates_that_are_set_and_is_closed() {
    let both = gates(Some(true), Some(false));
    let v: Value = serde_json::to_value(&both).unwrap();
    assert_eq!(
        v,
        json!({"allow_fleet_control": true, "allow_desktop": true,
            "allow_desktop_capture": true, "allow_desktop_input": true,
            "monitor_allowed": true, "allow_desktop_click": true,
            "allow_desktop_input_any": false})
    );
    assert_eq!(
        serde_json::from_value::<DesktopInputGatesAnswer>(v.clone()).unwrap(),
        both
    );
    let click = serde_json::to_value(gates(Some(false), None)).unwrap();
    assert_eq!(click["allow_desktop_click"], false);
    assert!(
        !click
            .as_object()
            .unwrap()
            .contains_key("allow_desktop_input_any")
    );
    let any = serde_json::to_value(gates(None, Some(true))).unwrap();
    assert!(!any.as_object().unwrap().contains_key("allow_desktop_click"));
    assert_eq!(
        serde_json::from_value::<DesktopInputGatesAnswer>(any).unwrap(),
        gates(None, Some(true))
    );
    // It is closed: an unknown field and a missing base field are refused,
    // and a null gate reads as an absent one.
    let mut extra = v.clone();
    extra["x"] = json!(1);
    assert!(serde_json::from_value::<DesktopInputGatesAnswer>(extra).is_err());
    let mut missing = v.clone();
    missing.as_object_mut().unwrap().remove("monitor_allowed");
    assert!(serde_json::from_value::<DesktopInputGatesAnswer>(missing).is_err());
    let mut null = v;
    null["allow_desktop_click"] = Value::Null;
    assert_eq!(
        serde_json::from_value::<DesktopInputGatesAnswer>(null).unwrap(),
        gates(None, Some(false))
    );
    // The answer of the `input` purpose keeps its bytes and its type.
    let p7 = serde_json::to_value(DesktopInputPolicyAnswer {
        allow_fleet_control: true,
        allow_desktop: true,
        allow_desktop_capture: true,
        allow_desktop_input: true,
        monitor_allowed: true,
    })
    .unwrap();
    assert!(
        serde_json::from_value::<DesktopInputPolicyAnswer>(serde_json::to_value(both).unwrap())
            .is_err()
    );
    assert_eq!(
        serde_json::from_value::<DesktopInputGatesAnswer>(p7).unwrap(),
        gates(None, None)
    );
}

#[test]
fn an_input_gates_answer_fits_exactly_the_gates_of_its_purpose() {
    use PolicyPurpose::{Capture, Desktop, Input, InputAny, InputAnyClick, InputClick};
    let cases = [
        // (click, any, the purposes that fit)
        (None, None, vec![]),
        (Some(true), None, vec![InputClick]),
        (Some(false), None, vec![InputClick]),
        (None, Some(true), vec![InputAny]),
        (None, Some(false), vec![InputAny]),
        (Some(true), Some(true), vec![InputAnyClick]),
        (Some(false), Some(false), vec![InputAnyClick]),
        (Some(true), Some(false), vec![InputAnyClick]),
    ];
    for (click, any, fits) in cases {
        let answer = gates(click, any);
        for purpose in [Desktop, Capture, Input, InputClick, InputAny, InputAnyClick] {
            assert_eq!(
                answer.fits(purpose),
                fits.contains(&purpose),
                "{click:?} {any:?} {purpose:?}"
            );
        }
    }
}

#[test]
fn the_input_windows_read_has_its_kind() {
    let v = serde_json::to_value(ProtectedRead::InputWindows).unwrap();
    assert_eq!(v, json!({"kind": "input_windows"}));
    assert_eq!(
        serde_json::from_value::<ProtectedRead>(v).unwrap(),
        ProtectedRead::InputWindows
    );
    // The other kinds keep their bytes.
    assert_eq!(
        serde_json::to_value(ProtectedRead::Info).unwrap(),
        json!({"kind": "info"})
    );
    let read = CoordinatorRead {
        read_id: "r1".into(),
        read: ProtectedRead::InputWindows,
        read_hash: "h".into(),
    };
    let v = serde_json::to_value(&read).unwrap();
    assert_eq!(v["read"], json!({"kind": "input_windows"}));
    assert_eq!(serde_json::from_value::<CoordinatorRead>(v).unwrap(), read);
}
