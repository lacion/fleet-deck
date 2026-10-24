//! Adapter evidence for PLAN §6.2 / §6.4 from the recorded Claude fixtures.
//! Each test name is cited in `spikes/s1-claude/EVIDENCE.md`, ADR-001, and the
//! capability matrix. No network and no `claude` binary are needed.

#![allow(clippy::unwrap_used)]

use std::path::Path;

use s1_claude::replay::replay;
use serde_json::Value;
use spike_canon::fixture::{Fixture, Record};
use spike_canon::{CanonEvent, InputState};

fn load(name: &str) -> Fixture {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/rust");
    Fixture::load(&dir.join(format!("{name}.jsonl"))).unwrap()
}

fn input_states(ev: &[CanonEvent], input: &str) -> Vec<InputState> {
    ev.iter()
        .filter_map(|e| match e {
            CanonEvent::InputUpdated {
                input_id, state, ..
            } if input_id == input => Some(*state),
            _ => None,
        })
        .collect()
}

fn prompt_ids(fx: &Fixture) -> Vec<String> {
    fx.records
        .iter()
        .filter_map(|r| match r {
            Record::Host { cmd } if cmd["op"] == "prompt" => {
                cmd["input_id"].as_str().map(str::to_owned)
            }
            _ => None,
        })
        .collect()
}

/// "Prompt confirmed" (§6.4 `confirmed`) for Claude = the `command_lifecycle`
/// frame with state `started` for the input's client `uuid`. In every recording,
/// each input that reached `started` belongs to a turn that was already announced,
/// and `started` never arrives for an unknown uuid.
#[test]
fn prompt_confirmed_is_command_lifecycle_started() {
    for name in [
        "simple",
        "multi_turn",
        "queued_turn",
        "turn_interrupt",
        "message_steering",
        "provider_thread_resume",
        "tool_call_approve",
        "tool_call_deny",
    ] {
        let fx = load(name);
        let ev = replay(&fx).unwrap().events;
        let mut announced = Vec::new();
        for e in &ev {
            match e {
                CanonEvent::TurnStarted { turn_id, .. } => announced.push(turn_id.clone()),
                CanonEvent::InputUpdated {
                    state: InputState::Started,
                    turn_id,
                    ..
                } => {
                    let t = turn_id.as_ref().unwrap();
                    assert!(
                        announced.contains(t),
                        "{name}: input started in unknown turn"
                    );
                }
                _ => {}
            }
        }
        for id in prompt_ids(&fx) {
            let states = input_states(&ev, &id);
            assert_eq!(states.first(), Some(&InputState::Received), "{name} {id}");
            assert!(
                states.contains(&InputState::Started),
                "{name} {id}: {states:?}"
            );
        }
    }
}

/// A queued second prompt is confirmed only after the first turn ended
/// (`started` of input 2 follows `turn.completed` of turn 1).
#[test]
fn a_queued_prompt_is_confirmed_when_its_turn_starts() {
    let fx = load("queued_turn");
    let ev = replay(&fx).unwrap().events;
    let second = &prompt_ids(&fx)[1];
    let started = ev
        .iter()
        .position(|e| matches!(e, CanonEvent::InputUpdated { input_id, state: InputState::Started, .. } if input_id == second))
        .unwrap();
    let first_done = ev
        .iter()
        .position(|e| matches!(e, CanonEvent::TurnCompleted { .. }))
        .unwrap();
    assert!(first_done < started);
}

/// A steer prompt written while a tool runs joins the running turn: the CLI
/// drains it after the tool result, before the next model request.
#[test]
fn a_steer_prompt_starts_inside_the_running_turn_after_a_tool_result() {
    let fx = load("message_steering");
    let ev = replay(&fx).unwrap().events;
    let steer = &prompt_ids(&fx)[1];
    let started = ev
        .iter()
        .position(|e| matches!(e, CanonEvent::InputUpdated { input_id, state: InputState::Started, turn_id: Some(t), .. } if input_id == steer && t == "turn-1"))
        .unwrap();
    let tool_done = ev
        .iter()
        .position(|e| matches!(e, CanonEvent::ToolCompleted { .. }))
        .unwrap();
    let turn_done = ev
        .iter()
        .position(|e| matches!(e, CanonEvent::TurnCompleted { .. }))
        .unwrap();
    assert!(tool_done < started && started < turn_done);
    assert_eq!(
        input_states(&ev, steer).last(),
        Some(&InputState::Completed)
    );
}

/// An interrupted input ends `cancelled`, not `completed` (§6.4: the effect is
/// known, the turn ended early).
#[test]
fn an_interrupted_input_ends_cancelled() {
    for name in ["turn_interrupt", "interrupt_during_tool"] {
        let fx = load(name);
        let ev = replay(&fx).unwrap().events;
        let first = &prompt_ids(&fx)[0];
        assert_eq!(
            input_states(&ev, first).last(),
            Some(&InputState::Cancelled),
            "{name}"
        );
    }
}

/// Approvals: the CLI's native answer is a `behavior` (`allow` | `deny`) plus
/// optional `updatedInput` / `updatedPermissions`; it has no option ids. The
/// adapter offers `allow`, `allow_always` (only with `permission_suggestions`),
/// `deny`, and the native request id is a UUID chosen by the CLI.
#[test]
fn approval_options_map_to_native_behaviors() {
    let fx = load("tool_call_approve");
    let ev = replay(&fx).unwrap().events;
    let options: Vec<_> = ev
        .iter()
        .find_map(|e| match e {
            CanonEvent::ApprovalRequested { options, .. } => Some(options.clone()),
            _ => None,
        })
        .unwrap()
        .into_iter()
        .map(|o| o.option_id)
        .collect();
    assert_eq!(options, ["allow", "allow_always", "deny"]);
    let mut native_ids = Vec::new();
    let mut behaviors = Vec::new();
    for r in &fx.records {
        match r {
            Record::Out { frame } if frame["request"]["subtype"] == "can_use_tool" => {
                native_ids.push(frame["request_id"].as_str().unwrap().to_owned());
            }
            Record::In { frame } if frame["type"] == "control_response" => {
                behaviors.push(frame["response"]["response"]["behavior"].clone());
            }
            _ => {}
        }
    }
    assert_eq!(native_ids.len(), 1);
    assert_eq!(native_ids[0].len(), 36, "a UUID: {}", native_ids[0]);
    assert_eq!(behaviors, [Value::from("allow")]);
}

/// The CLI's `permission_suggestions` include `destination: localSettings`:
/// echoing them for `allow_always` would write `.claude/settings.local.json` in
/// the workspace (input for ADR-001: the station must pick the destination).
#[test]
fn permission_suggestions_name_their_destination() {
    let fx = load("tool_call_approve");
    let dests: Vec<String> = fx
        .records
        .iter()
        .find_map(|r| match r {
            Record::Out { frame } if frame["request"]["subtype"] == "can_use_tool" => {
                frame["request"]["permission_suggestions"]
                    .as_array()
                    .cloned()
            }
            _ => None,
        })
        .unwrap()
        .iter()
        .filter_map(|s| s["destination"].as_str().map(str::to_owned))
        .collect();
    assert!(dests.contains(&"localSettings".to_owned()), "{dests:?}");
    assert!(dests.contains(&"session".to_owned()), "{dests:?}");
}
