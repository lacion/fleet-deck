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
        "shell.handover" => with(
            "change",
            json!({"op": "handover", "shell_id": "tui-1", "session_id": "s1", "cols": 80, "rows": 24}),
        ),
        "shell.release" => with("change", json!({"op": "release", "shell_id": "tui-1"})),
        "coordinator.call" => with(
            "call",
            json!({"caller": s, "binding": {"binding_id": "b", "grant_id": "g",
                   "generation": 1, "provider_instance": "claude", "native_id": "n"},
                   "op": {"op": "send", "to": {"station": "alpha", "session_id": s},
                          "text": "hi", "delivery": "follow_up"}}),
        ),
        "coordinator.desktop_call" => {
            let mut v = with(
                "call",
                json!({"tool": "notify", "arguments": {"request_id": "r", "title": "t",
                       "body": "b"}}),
            );
            v["caller"] = json!(s);
            v["binding"] = json!({"binding_id": "b", "grant_id": "g", "generation": 1,
                "provider_instance": "claude", "native_id": "n"});
            v
        }
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
                "set_effort" => json!({"op": op, "session_id": s, "effort": "high"}),
                "stop" => json!({"op": op, "session_id": s, "how": "graceful"}),
                "revert" => json!({"op": op, "session_id": s, "turn_id": "turn-1"}),
                "rename" => json!({"op": op, "session_id": s, "title": "docs"}),
                "set_auto_continue" => json!({"op": op, "session_id": s, "enabled": true}),
                "coordinator_renew" => {
                    json!({"op": op, "session_id": s, "grant_id": "g-1", "generation": 2})
                }
                "coordinator_revoke" => json!({"op": op, "session_id": s, "grant_id": "g-1"}),
                "coordinator_set" => json!({"op": op, "session_id": s, "seq": 3,
                    "grant_id": "g-1", "spec": {"grant_id": "g-1", "classes": ["observe"],
                    "idle_ttl_ms": 60_000, "expires_at": 4_102_444_800_000_i64,
                    "control_skill": {"version": "1", "text": "You coordinate the fleet."}}}),
                "reconcile_execution" => {
                    json!({"op": op, "session_id": s, "shim_id": "01J0000000000000000000000H"})
                }
                "stop_execution" => json!({"op": op, "session_id": s,
                    "shim_id": "01J0000000000000000000000H", "plan_id": "plan-1"}),
                "shell_command" => json!({"op": op, "session_id": s, "command": "ls"}),
                "release_shell" => {
                    json!({"op": op, "session_id": s, "effect_id": "01J0000000000000000000000E"})
                }
                "land" => json!({"op": op, "session_id": s, "land_id": "land-1", "plan": {
                    "project_id": "p1", "settings_revision": 1, "home_generation": 1,
                    "mode": "merge", "fetch_remote": "origin",
                    "remote": {"repo": "github.com/o/r", "dest": "https://github.com:443/o/r"},
                    "branch_ref": "fd/o-r/a", "publish_ref": "fd/o-r/a",
                    "branch_target": {"target": "origin", "dest": "https://github.com:443/o/r"},
                    "check_env": {}, "pr_title": "t", "pr_body": "b"}}),
                "move_out" => {
                    json!({"op": op, "session_id": s, "move_id": "mv-1", "to_station": "bravo"})
                }
                "move_finish" => json!({"op": op, "session_id": s, "move_id": "mv-1",
                    "to_station": "bravo", "to_session": "01J0000000000000000000000T"}),
                "move_undo" => json!({"op": op, "session_id": s, "move_id": "mv-1"}),
                "move_in" => json!({"op": op, "move_id": "mv-1", "kind": "native",
                    "provider_instance": "claude",
                    "from": {"station_id": "alpha", "session_id": s, "publication": {
                        "local_branch": "fd/alpha/ada", "dest": "https://github.com:443/o/r",
                        "ref_name": "fd/alpha/ada", "oid": "a".repeat(40)}},
                    "fleet_git": {"kind": "worktree", "project_id": "p1", "project_name": "app",
                        "settings": {}, "settings_revision": 1, "home_generation": 1,
                        "home_path": "/w/app",
                        "remote": {"repo": "github.com/o/r", "dest": "https://github.com:443/o/r"},
                        "slug": "bravo", "clone_url": "https://github.com/o/r.git"}}),
                "worktree_purge" => {
                    json!({"op": op, "session_id": s, "confirm_name": "app--fd-x--01j000000000"})
                }
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
    assert_eq!(count, 43);
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

#[test]
fn the_shell_commands_round_trip_with_their_method_and_session() {
    let s = "01J0000000000000000000000S";
    for (value, method) in [
        (
            json!({"op": "shell_command", "session_id": s, "command": "ls -la"}),
            "session.shell_command",
        ),
        (
            json!({"op": "release_shell", "session_id": s, "effect_id": "01J0000000000000000000000E"}),
            "session.release_shell",
        ),
    ] {
        let command: SessionCommand = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(command.method(), method);
        assert_eq!(command.session_id().map(|id| id.as_str()), Some(s));
        assert_eq!(serde_json::to_value(&command).unwrap(), value);
        assert!(SESSION_METHODS.contains(&method));
    }
    assert!(crate::version::feature::ALL.contains(&crate::version::feature::SHELL_COMMAND));
    assert_eq!(crate::version::feature::SHELL_COMMAND, "shell_command.v1");
}

/// `stations_mut` returns every station that an op names, in order, and a
/// write through it changes the op.
#[test]
fn stations_mut_returns_every_station_field_of_a_variant() {
    let s = "01J0000000000000000000000S";
    let spec = json!({"provider_instance": "claude", "cwd": "/w", "mode": null,
                      "model": null, "title": null});
    // Each op, with the JSON pointers of its station fields.
    let with_station: Vec<(Value, Vec<&str>)> = vec![
        (
            json!({"op": "spawn", "station": "n1", "spec": spec, "prompt": null}),
            vec!["/station"],
        ),
        (
            json!({"op": "send", "to": {"station": "n1", "session_id": s}, "text": "hi",
                   "delivery": "follow_up"}),
            vec!["/to/station"],
        ),
        (
            json!({"op": "interrupt", "station": "n1", "session_id": s}),
            vec!["/station"],
        ),
        (
            json!({"op": "stop", "station": "n1", "session_id": s}),
            vec!["/station"],
        ),
        (
            json!({"op": "assign", "task": "t", "to": {"station": "n1", "session_id": s}}),
            vec!["/to/station"],
        ),
        (
            json!({"op": "resume", "station": "n1", "session_id": s}),
            vec!["/station"],
        ),
        (
            json!({"op": "rename", "station": "n1", "session_id": s, "title": "x"}),
            vec!["/station"],
        ),
        (
            json!({"op": "set_mode", "station": "n1", "session_id": s, "mode": "plan"}),
            vec!["/station"],
        ),
        (
            json!({"op": "project_assign", "station": "n1", "session_id": s,
                   "target": {"kind": "none"}}),
            vec!["/station"],
        ),
        (
            json!({"op": "answer", "station": "n1", "session_id": s, "request_id": "r1"}),
            vec!["/station"],
        ),
        (
            json!({"op": "land", "station": "n1", "session_id": s}),
            vec!["/station"],
        ),
        (
            json!({"op": "move", "station": "n1", "session_id": s, "to_station": "n2"}),
            vec!["/station", "/to_station"],
        ),
    ];
    for (value, pointers) in with_station {
        let name = value["op"].as_str().unwrap().to_owned();
        let mut op: CoordinatorOp = serde_json::from_value(value.clone()).unwrap();
        let seen: Vec<String> = op
            .stations_mut()
            .into_iter()
            .map(|st| st.as_str().to_owned())
            .collect();
        let expected: Vec<String> = pointers
            .iter()
            .map(|p| value.pointer(p).unwrap().as_str().unwrap().to_owned())
            .collect();
        assert_eq!(seen, expected, "{name}");
        for st in op.stations_mut() {
            *st = StationId::from("changed");
        }
        let after = serde_json::to_value(&op).unwrap();
        for p in &pointers {
            assert_eq!(after.pointer(p).unwrap(), "changed", "{name} {p}");
        }
    }
}

/// An op without a station returns an empty list.
#[test]
fn stations_mut_is_empty_for_an_op_without_a_station() {
    let without = [
        json!({"op": "assign_auto", "task": "t", "requirements": {}}),
        json!({"op": "complete_task", "task_id": "t", "result": null}),
        json!({"op": "ask_coordinator", "question": "q?"}),
        json!({"op": "send_group", "group": "@idle", "text": "hi", "delivery": "steer"}),
        json!({"op": "project_start", "project_id": "p", "prompt": "go", "worktree": true}),
    ];
    for value in without {
        let name = value["op"].as_str().unwrap().to_owned();
        let mut op: CoordinatorOp = serde_json::from_value(value).unwrap();
        assert!(op.stations_mut().is_empty(), "{name}");
    }
}
