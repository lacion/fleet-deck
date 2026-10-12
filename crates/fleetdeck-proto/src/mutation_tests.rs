use serde_json::{Value, json};

use super::*;
use crate::command::SessionCommand;

const ID: &str = "01J0000000000000000000000C";
const INC: &str = "01J0000000000000000000000I";
const NOW: i64 = 1_767_225_600_000;

fn header() -> serde_json::Map<String, Value> {
    let mut m = serde_json::Map::new();
    m.insert("command_id".into(), json!(ID));
    m.insert("target_incarnation".into(), json!(INC));
    m.insert("retry_until".into(), json!(NOW + 60_000));
    m
}

fn with(field: &str, payload: Value) -> Value {
    let mut m = header();
    m.insert(field.into(), payload);
    Value::Object(m)
}

/// One valid `params` sample per mutating method.
pub(crate) fn sample(method: &str) -> Value {
    let s = "01J0000000000000000000000S";
    match method {
        "station.configure" => with(
            "change",
            json!({"op": "set_max_sessions", "max_sessions": 2}),
        ),
        "link.rotate_key" => with(
            "rotation",
            json!({"old_key": "a".repeat(64), "new_key": "b".repeat(64), "signature": "c".repeat(128)}),
        ),
        "shell.open" => with(
            "change",
            json!({"op": "open", "shell_id": "main", "cwd": "/w", "cols": 80, "rows": 24}),
        ),
        "shell.resize" => with(
            "change",
            json!({"op": "resize", "shell_id": "main", "cols": 100, "rows": 30}),
        ),
        "shell.close" => with("change", json!({"op": "close", "shell_id": "main"})),
        "coordinator.call" => with(
            "call",
            json!({"caller": s, "binding": {"binding_id": "b", "grant_id": "g",
                   "generation": 1, "provider_instance": "claude", "native_id": "n"},
                   "op": {"op": "send", "to": {"station": "alpha", "session_id": s},
                          "text": "hi", "delivery": "follow_up"}}),
        ),
        _ => {
            let op = method.rsplit('.').next().unwrap_or_default();
            let command = match op {
                "start" => json!({"op": "start", "spec": {"provider_instance": "claude",
                    "cwd": "/w", "mode": null, "model": null, "title": null}, "prompt": null}),
                "import" => json!({"op": "import", "spec": {"provider_instance": "claude",
                    "cwd": "/w", "mode": null, "model": null, "title": null},
                    "native_session_id": "cli-1", "prompt": null}),
                "prompt" | "steer" | "follow_up" => {
                    json!({"op": op, "session_id": s, "input": {"text": "hi"}})
                }
                "answer" => json!({"op": "answer", "session_id": s, "request_id": "r",
                    "decision": {"option_id": "allow", "updated_input": null, "text": null}}),
                "set_mode" => json!({"op": op, "session_id": s, "mode": "plan"}),
                "set_model" => json!({"op": op, "session_id": s, "model": "haiku"}),
                "stop" => json!({"op": op, "session_id": s, "how": "graceful"}),
                "revert" => json!({"op": op, "session_id": s, "turn_id": "turn-1"}),
                "rename" => json!({"op": op, "session_id": s, "title": "docs"}),
                "set_auto_continue" => json!({"op": op, "session_id": s, "enabled": true}),
                "coordinator_renew" => {
                    json!({"op": op, "session_id": s, "grant_id": "g-1", "generation": 2})
                }
                "coordinator_revoke" => json!({"op": op, "session_id": s, "grant_id": "g-1"}),
                "resolve_revert" => json!({
                    "op": op,
                    "session_id": s,
                    "revert_id": "rv-1",
                    "resolution": "reconciled"
                }),
                _ => json!({"op": op, "session_id": s}),
            };
            let mut m = header();
            m.insert("origin".into(), json!("board"));
            m.insert("command".into(), command);
            Value::Object(m)
        }
    }
}

fn parse(method: &str, params: Value) -> Result<Mutation, Reject> {
    Mutation::parse(method, Some(&CommandId::from(ID)), params)
}

#[test]
fn every_mutating_method_parses_its_envelope_and_names_its_method() {
    let mut count = 0;
    for kind in ENVELOPES {
        for method in kind.methods {
            let parsed = parse(method, sample(method)).unwrap();
            assert_eq!(parsed.method(), *method);
            assert_eq!(parsed.command_id().as_str(), ID);
            assert!(sample(method).get(kind.payload).is_some(), "{method}");
            parsed.admit(NOW, &IncarnationId::from(INC)).unwrap();
            assert!(!parsed.canonical_payload().is_empty());
            count += 1;
        }
    }
    assert_eq!(count, 24);
}

#[test]
fn session_methods_are_exactly_the_session_command_methods() {
    for method in SESSION_METHODS {
        let Mutation::Session(env) = parse(method, sample(method)).unwrap() else {
            panic!("{method} is not a session envelope");
        };
        let command: &SessionCommand = &env.command;
        assert_eq!(command.method(), *method);
    }
}

#[test]
fn a_command_id_or_method_that_disagrees_is_an_invalid_command() {
    let other = CommandId::from("01J0000000000000000000000X");
    for kind in ENVELOPES {
        let method = kind.methods[0];
        let err = Mutation::parse(method, Some(&other), sample(method)).unwrap_err();
        assert_eq!(err.code, RejectCode::InvalidCommand, "{method}");
        let err = Mutation::parse(method, None, sample(method)).unwrap_err();
        assert_eq!(err.code, RejectCode::InvalidCommand, "{method}");
    }
    let err = parse("session.stop", sample("session.prompt")).unwrap_err();
    assert_eq!(err.code, RejectCode::InvalidCommand);
    let err = parse("station.info", json!({})).unwrap_err();
    assert_eq!(err.code, RejectCode::InvalidCommand);
    // A payload of another envelope does not parse.
    let err = parse("station.configure", sample("coordinator.call")).unwrap_err();
    assert_eq!(err.code, RejectCode::InvalidCommand);
}

#[test]
fn unknown_operations_and_fields_fail_closed() {
    let bad_op = with(
        "call",
        json!({"caller": "s", "binding": {"binding_id": "b", "grant_id": "g",
               "generation": 1, "provider_instance": "claude", "native_id": "n"},
        "op": {"op": "delete_everything"}}),
    );
    assert!(parse("coordinator.call", bad_op).is_err());
    let bad_change = with("change", json!({"op": "set_allowed_roots", "roots": ["/"]}));
    assert!(parse("station.configure", bad_change).is_err());
    let extra = with(
        "change",
        json!({"op": "set_max_sessions", "max_sessions": 2, "allow_shell": true}),
    );
    assert!(parse("station.configure", extra).is_err());
}

#[test]
fn every_envelope_is_refused_without_a_receipt_for_another_incarnation_or_a_past_deadline() {
    let other = IncarnationId::from("01J0000000000000000000000Z");
    for kind in ENVELOPES {
        let method = kind.methods[0];
        let parsed = parse(method, sample(method)).unwrap();
        let err = parsed.admit(NOW, &other).unwrap_err();
        assert_eq!(err.code, RejectCode::IncarnationMismatch, "{method}");
        assert!(!err.code.stores_receipt());
        let late = NOW + 60_000 + crate::command::CLOCK_SKEW_MS + 1;
        let err = parsed.admit(late, &IncarnationId::from(INC)).unwrap_err();
        assert_eq!(err.code, RejectCode::Expired, "{method}");
    }
    // Only a session command may omit the incarnation (the local socket).
    let mut local = sample("session.prompt");
    local["target_incarnation"] = Value::Null;
    assert!(parse("session.prompt", local).is_ok());
    let mut remote = sample("station.configure");
    remote["target_incarnation"] = Value::Null;
    assert!(parse("station.configure", remote).is_err());
}

#[test]
fn the_payload_hash_text_changes_with_the_payload_only() {
    let a = parse("coordinator.call", sample("coordinator.call")).unwrap();
    let mut changed = sample("coordinator.call");
    changed["call"]["op"]["text"] = json!("other");
    let b = parse("coordinator.call", changed).unwrap();
    assert_ne!(a.canonical_payload(), b.canonical_payload());
    let mut later = sample("coordinator.call");
    later["retry_until"] = json!(NOW + 120_000);
    let c = parse("coordinator.call", later).unwrap();
    assert_eq!(a.canonical_payload(), c.canonical_payload());
}

#[test]
fn the_rotation_signature_binds_the_receiver_and_the_command() {
    let r = KeyRotation {
        old_key: "aa".into(),
        new_key: "bb".into(),
        signature: String::new(),
    };
    let id = CommandId::from("c1");
    assert_eq!(
        r.signed_message("rr", &id),
        b"fleetdeck-rotate-key-v1\0aa\0bb\0rr\0c1\0".to_vec()
    );
    assert_ne!(r.signed_message("other", &id), r.signed_message("rr", &id));
    assert_ne!(
        r.signed_message("rr", &CommandId::from("c2")),
        r.signed_message("rr", &id)
    );
}

#[test]
fn every_params_type_name_is_the_rust_type() {
    let names = [
        std::any::type_name::<CommandEnvelope>(),
        std::any::type_name::<StationConfigureEnvelope>(),
        std::any::type_name::<KeyRotationEnvelope>(),
        std::any::type_name::<crate::shell::ShellEnvelope>(),
        std::any::type_name::<CoordinatorCallEnvelope>(),
    ];
    for (kind, name) in ENVELOPES.iter().zip(names) {
        assert!(name.ends_with(&format!("::{}", kind.params)), "{name}");
    }
}
