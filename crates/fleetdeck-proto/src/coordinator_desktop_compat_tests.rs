//! R13 P6 changes to existing shapes: the new optional fields stay out of the
//! bytes of every non-P6 message, and the new envelope parses through
//! `Mutation::parse`.

use serde_json::json;

use crate::command::{ControlSkill, CoordinatorSpec, GrantScope};
use crate::coordinator_spec::InputScope;
use crate::ids::{CommandId, IncarnationId, ProviderInstanceId, SessionId};
use crate::mutation::{
    CoordinatorBinding, Holder, Mutation, PolicyNow, PolicyNowAnswer, envelope_of,
};
use crate::orchestration::GrantView;

fn binding() -> CoordinatorBinding {
    CoordinatorBinding {
        binding_id: "b1".into(),
        grant_id: "g1".into(),
        generation: 2,
        provider_instance: ProviderInstanceId("claude".into()),
        native_id: "n1".into(),
    }
}

fn desktop_call_params(command_id: &str) -> serde_json::Value {
    json!({
        "command_id": command_id,
        "target_incarnation": "01J0000000000000000000000I",
        "retry_until": 100,
        "caller": "01J0000000000000000000000S",
        "binding": serde_json::to_value(binding()).unwrap(),
        "call": {"tool": "notify", "arguments": {
            "request_id": "r", "title": "t", "body": "b"}}
    })
}

#[test]
fn the_desktop_call_envelope_row_mirrors_coordinator_call() {
    let call = envelope_of("coordinator.call").unwrap();
    let desktop = envelope_of("coordinator.desktop_call").unwrap();
    assert_eq!(desktop.methods, ["coordinator.desktop_call"]);
    assert_eq!(desktop.params, "CoordinatorDesktopCallEnvelope");
    assert_eq!(desktop.payload, call.payload);
    assert_eq!(desktop.holder, Holder::Deck);
    assert!(envelope_of("coordinator.desktop_query").is_none());
}

#[test]
fn a_desktop_call_parses_with_its_own_arm_and_checks_the_command_id() {
    let id = CommandId::from("01J0000000000000000000000C");
    let params = desktop_call_params(id.as_str());
    let parsed = Mutation::parse("coordinator.desktop_call", Some(&id), params.clone()).unwrap();
    let Mutation::DesktopCall(env) = &parsed else {
        panic!("not a desktop call: {parsed:?}");
    };
    assert_eq!(env.caller, SessionId::from("01J0000000000000000000000S"));
    assert_eq!(parsed.method(), "coordinator.desktop_call");
    assert_eq!(parsed.command_id(), &id);
    // The receipt hash covers the call only.
    assert_eq!(
        parsed.canonical_payload(),
        serde_json::to_string(&env.call).unwrap()
    );
    // The frame and the envelope must agree.
    let other = CommandId::from("01J0000000000000000000000D");
    assert!(Mutation::parse("coordinator.desktop_call", Some(&other), params.clone()).is_err());
    assert!(Mutation::parse("coordinator.desktop_call", None, params.clone()).is_err());
    // A `coordinator.call` envelope is not a desktop call, and the reverse.
    assert!(Mutation::parse("coordinator.call", Some(&id), params).is_err());
    // The admission checks mirror `coordinator.call`: the incarnation and the
    // deadline.
    let ok = IncarnationId::from("01J0000000000000000000000I");
    assert!(parsed.admit(1, &ok).is_ok());
    assert!(
        parsed
            .admit(100 + crate::command::CLOCK_SKEW_MS + 1, &ok)
            .is_err()
    );
    assert!(
        parsed
            .admit(1, &IncarnationId::from("01J0000000000000000000000X"))
            .is_err()
    );
}

#[test]
fn a_desktop_call_with_an_unknown_tool_or_field_is_refused() {
    let id = CommandId::from("01J0000000000000000000000C");
    let mut tool = desktop_call_params(id.as_str());
    tool["call"]["tool"] = json!("record_demo");
    assert!(Mutation::parse("coordinator.desktop_call", Some(&id), tool).is_err());
    let mut field = desktop_call_params(id.as_str());
    field["x"] = json!(1);
    assert!(Mutation::parse("coordinator.desktop_call", Some(&id), field).is_err());
}

#[test]
fn policy_now_without_a_binding_keeps_its_old_bytes() {
    let old = r#"{"caller_session":"01J0000000000000000000000K"}"#;
    let ask = PolicyNow {
        caller_session: SessionId::from("01J0000000000000000000000K"),
        binding: None,
    };
    assert_eq!(serde_json::to_string(&ask).unwrap(), old);
    assert_eq!(serde_json::from_str::<PolicyNow>(old).unwrap(), ask);
    let old_answer = r#"{"fleet_control":true,"bound":false}"#;
    let answer = PolicyNowAnswer {
        fleet_control: true,
        bound: false,
        binding_matches: None,
    };
    assert_eq!(serde_json::to_string(&answer).unwrap(), old_answer);
    assert_eq!(
        serde_json::from_str::<PolicyNowAnswer>(old_answer).unwrap(),
        answer
    );
}

#[test]
fn policy_now_carries_the_binding_and_the_match_only_when_asked() {
    let ask = PolicyNow {
        caller_session: SessionId::from("01J0000000000000000000000K"),
        binding: Some(binding()),
    };
    let v = serde_json::to_value(&ask).unwrap();
    assert_eq!(v["binding"]["generation"], 2);
    assert_eq!(serde_json::from_value::<PolicyNow>(v).unwrap(), ask);
    let answer = PolicyNowAnswer {
        fleet_control: true,
        bound: true,
        binding_matches: Some(false),
    };
    let v = serde_json::to_value(&answer).unwrap();
    assert_eq!(v["binding_matches"], false);
    assert_eq!(
        serde_json::from_value::<PolicyNowAnswer>(v).unwrap(),
        answer
    );
}

fn spec() -> CoordinatorSpec {
    CoordinatorSpec {
        grant_id: "grant-1".to_owned(),
        classes: vec!["desktop".to_owned()],
        idle_ttl_ms: 60_000,
        expires_at: 4_102_444_800_000,
        control_skill: ControlSkill {
            version: "1".to_owned(),
            text: "You coordinate the fleet.".to_owned(),
        },
        scope: GrantScope::Lineage,
        answer_cap: None,
        desktop_cap: None,
        capture_cap: None,
        image_byte_cap: None,
        allow_recording: false,
        recording_cap: None,
        recording_second_cap: None,
        allow_input: false,
        input_scope: None,
        input_cap: None,
        input_character_cap: None,
        stations: Vec::new(),
    }
}

#[test]
fn the_spec_caps_are_written_only_when_set_and_parse_back() {
    let none = serde_json::to_value(spec()).unwrap();
    for key in ["desktop_cap", "capture_cap", "image_byte_cap"] {
        assert!(!none.as_object().unwrap().contains_key(key), "{key}");
    }
    let capped = CoordinatorSpec {
        desktop_cap: Some(300),
        capture_cap: Some(20),
        image_byte_cap: Some(67_108_864),
        ..spec()
    };
    let v = serde_json::to_value(&capped).unwrap();
    assert_eq!(v["desktop_cap"], 300);
    assert_eq!(v["capture_cap"], 20);
    assert_eq!(v["image_byte_cap"], 67_108_864);
    assert_eq!(
        serde_json::from_value::<CoordinatorSpec>(v).unwrap(),
        capped
    );
}

#[test]
fn a_grant_view_writes_the_p6_counters_only_when_set() {
    let mut view = GrantView {
        grant_id: "g".into(),
        station_id: "s".into(),
        session_id: "x".into(),
        classes: vec!["read".into()],
        stations: vec!["s".into()],
        spawn_cap: 3,
        spawn_used: 0,
        state: "active".into(),
        revoked_reason: None,
        generation: None,
        expires_at: 1,
        last_used_at: None,
        desktop_cap: None,
        desktop_used: None,
        capture_cap: None,
        capture_used: None,
        image_byte_cap: None,
        image_byte_used: None,
        recording_cap: None,
        recording_used: None,
        recording_second_cap: None,
        recording_second_used: None,
        allow_input: None,
        input_scope: None,
        input_cap: None,
        input_used: None,
        input_character_cap: None,
        input_character_used: None,
    };
    let v = serde_json::to_value(&view).unwrap();
    for key in [
        "desktop_cap",
        "desktop_used",
        "capture_cap",
        "capture_used",
        "image_byte_cap",
        "image_byte_used",
        "recording_cap",
        "recording_used",
        "recording_second_cap",
        "recording_second_used",
    ] {
        assert!(!v.as_object().unwrap().contains_key(key), "{key}");
    }
    view.desktop_cap = Some(300);
    view.desktop_used = Some(2);
    view.image_byte_cap = Some(1_048_576);
    view.image_byte_used = Some(716_800);
    let v = serde_json::to_value(&view).unwrap();
    assert_eq!(v["desktop_used"], 2);
    assert_eq!(v["image_byte_used"], 716_800);
    assert_eq!(serde_json::from_value::<GrantView>(v).unwrap(), view);
}

#[test]
fn the_recording_fields_of_the_spec_are_written_only_when_set_and_parse_back() {
    // A screenshot-only spec has no recording field (P6 bytes unchanged).
    let none = serde_json::to_value(spec()).unwrap();
    for key in ["allow_recording", "recording_cap", "recording_second_cap"] {
        assert!(!none.as_object().unwrap().contains_key(key), "{key}");
    }
    let allowed = CoordinatorSpec {
        allow_recording: true,
        recording_cap: Some(5),
        recording_second_cap: Some(600),
        ..spec()
    };
    let v = serde_json::to_value(&allowed).unwrap();
    assert_eq!(v["allow_recording"], true);
    assert_eq!(v["recording_cap"], 5);
    assert_eq!(v["recording_second_cap"], 600);
    assert_eq!(
        serde_json::from_value::<CoordinatorSpec>(v).unwrap(),
        allowed
    );
    // `allow_recording: false` is the default and is not written either.
    let v = serde_json::to_value(CoordinatorSpec {
        allow_recording: false,
        ..allowed
    })
    .unwrap();
    assert!(!v.as_object().unwrap().contains_key("allow_recording"));
}

#[test]
fn a_grant_view_writes_the_recording_counters_only_when_set() {
    let view = GrantView {
        grant_id: "g".into(),
        station_id: "s".into(),
        session_id: "x".into(),
        classes: vec!["capture".into()],
        stations: vec!["s".into()],
        spawn_cap: 3,
        spawn_used: 0,
        state: "active".into(),
        revoked_reason: None,
        generation: None,
        expires_at: 1,
        last_used_at: None,
        desktop_cap: None,
        desktop_used: None,
        capture_cap: None,
        capture_used: None,
        image_byte_cap: None,
        image_byte_used: None,
        recording_cap: Some(5),
        recording_used: Some(1),
        recording_second_cap: Some(600),
        recording_second_used: Some(30),
        allow_input: None,
        input_scope: None,
        input_cap: None,
        input_used: None,
        input_character_cap: None,
        input_character_used: None,
    };
    let v = serde_json::to_value(&view).unwrap();
    assert_eq!(v["recording_cap"], 5);
    assert_eq!(v["recording_used"], 1);
    assert_eq!(v["recording_second_cap"], 600);
    assert_eq!(v["recording_second_used"], 30);
    assert_eq!(serde_json::from_value::<GrantView>(v).unwrap(), view);
}

#[test]
fn the_input_fields_of_the_spec_are_written_only_when_set_and_parse_back() {
    // A spec without input has none of the four fields (P6 and P5b-2 bytes
    // unchanged, R13 P7 §14 items 1 and 2).
    let none = serde_json::to_value(spec()).unwrap();
    for key in [
        "allow_input",
        "input_scope",
        "input_cap",
        "input_character_cap",
    ] {
        assert!(!none.as_object().unwrap().contains_key(key), "{key}");
    }
    let allowed = CoordinatorSpec {
        allow_input: true,
        input_scope: Some(InputScope::OwnWindows),
        input_cap: Some(100),
        input_character_cap: Some(20_000),
        ..spec()
    };
    let v = serde_json::to_value(&allowed).unwrap();
    assert_eq!(v["allow_input"], true);
    assert_eq!(v["input_scope"], "own_windows");
    assert_eq!(v["input_cap"], 100);
    assert_eq!(v["input_character_cap"], 20_000);
    assert_eq!(
        serde_json::from_value::<CoordinatorSpec>(v).unwrap(),
        allowed
    );
    // `allow_input: false` is the default and is not written either.
    let v = serde_json::to_value(CoordinatorSpec {
        allow_input: false,
        ..allowed.clone()
    })
    .unwrap();
    assert!(!v.as_object().unwrap().contains_key("allow_input"));
    // The scope `any` and every other name is refused (§3.1), and so is an
    // unknown field next to the new ones.
    let mut any = serde_json::to_value(&allowed).unwrap();
    any["input_scope"] = json!("any");
    assert!(serde_json::from_value::<CoordinatorSpec>(any).is_err());
    let mut other = serde_json::to_value(&allowed).unwrap();
    other["input_scope"] = json!("OwnWindows");
    assert!(serde_json::from_value::<CoordinatorSpec>(other).is_err());
    let mut extra = serde_json::to_value(&allowed).unwrap();
    extra["input_cap_used"] = json!(1);
    assert!(serde_json::from_value::<CoordinatorSpec>(extra).is_err());
}

#[test]
fn a_grant_view_writes_the_input_fields_only_when_set() {
    let mut view = GrantView {
        grant_id: "g".into(),
        station_id: "s".into(),
        session_id: "x".into(),
        classes: vec!["desktop".into(), "capture".into(), "input".into()],
        stations: vec!["s".into()],
        spawn_cap: 3,
        spawn_used: 0,
        state: "active".into(),
        revoked_reason: None,
        generation: None,
        expires_at: 1,
        last_used_at: None,
        desktop_cap: None,
        desktop_used: None,
        capture_cap: None,
        capture_used: None,
        image_byte_cap: None,
        image_byte_used: None,
        recording_cap: None,
        recording_used: None,
        recording_second_cap: None,
        recording_second_used: None,
        allow_input: None,
        input_scope: None,
        input_cap: None,
        input_used: None,
        input_character_cap: None,
        input_character_used: None,
    };
    let v = serde_json::to_value(&view).unwrap();
    for key in [
        "allow_input",
        "input_scope",
        "input_cap",
        "input_used",
        "input_character_cap",
        "input_character_used",
    ] {
        assert!(!v.as_object().unwrap().contains_key(key), "{key}");
    }
    view.allow_input = Some(true);
    view.input_scope = Some(InputScope::OwnWindows);
    view.input_cap = Some(100);
    view.input_used = Some(3);
    view.input_character_cap = Some(20_000);
    view.input_character_used = Some(41);
    let v = serde_json::to_value(&view).unwrap();
    assert_eq!(v["allow_input"], true);
    assert_eq!(v["input_scope"], "own_windows");
    assert_eq!(v["input_cap"], 100);
    assert_eq!(v["input_used"], 3);
    assert_eq!(v["input_character_cap"], 20_000);
    assert_eq!(v["input_character_used"], 41);
    assert_eq!(serde_json::from_value::<GrantView>(v).unwrap(), view);
}
