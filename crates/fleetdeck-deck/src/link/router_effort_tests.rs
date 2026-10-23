//! The predicate of the effort gate (B18 Phase 2, design §9.2 item 1).

use super::*;

fn envelope(command: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "command_id": "c1",
        "target_incarnation": null,
        "retry_until": 1,
        "origin": "board",
        "command": command,
    })
}

fn spec(effort: Option<&str>) -> serde_json::Value {
    serde_json::json!({
        "provider_instance": "claude", "cwd": "/w", "mode": null,
        "model": null, "title": null, "effort": effort,
    })
}

#[test]
fn a_start_or_an_import_with_an_effort_is_an_effort_command() {
    let start =
        |effort| envelope(serde_json::json!({"op": "start", "spec": spec(effort), "prompt": null}));
    assert!(effort_command(&start(Some("high"))));
    assert!(!effort_command(&start(None)));
    let import = |effort| {
        envelope(serde_json::json!({
            "op": "import", "spec": spec(effort), "prompt": null,
            "native_session_id": "0b6c1d2e-aaaa-4bbb-8ccc-123456789abc",
        }))
    };
    assert!(effort_command(&import(Some("low"))));
    assert!(!effort_command(&import(None)));
}

#[test]
fn a_set_effort_is_an_effort_command_and_other_commands_are_not() {
    let set =
        envelope(serde_json::json!({"op": "set_effort", "session_id": "s1", "effort": "max"}));
    assert!(effort_command(&set));
    let model =
        envelope(serde_json::json!({"op": "set_model", "session_id": "s1", "model": "haiku"}));
    assert!(!effort_command(&model));
    let interrupt = envelope(serde_json::json!({"op": "interrupt", "session_id": "s1"}));
    assert!(!effort_command(&interrupt));
    assert!(!effort_command(&serde_json::json!({"nonsense": true})));
}
