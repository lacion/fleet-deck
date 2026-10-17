//! `task.updated` from the `system/task_*` frames, the late text of a
//! background subagent, and the tools of a running background agent (B6).

use std::path::Path;

use fleetdeck_proto::event::ToolStatus;
use fleetdeck_proto::{Event, NodeId, TurnId};
use serde_json::{Value, json};

use crate::claude::frames::parse_value;
use crate::claude::tests::{config, emits, feed, kinds, result, running_core, tool_use};
use crate::claude::{ClaudeCore, HostCmd};

/// The events of a recorded fixture (`spikes/s1-claude/fixtures/rust`), from
/// the production core. Written frames are not compared here: the replay
/// test of the fixtures does that.
fn replay(name: &str) -> Vec<Event> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../spikes/s1-claude/fixtures/rust")
        .join(name);
    let text = std::fs::read_to_string(path).unwrap();
    let mut core: Option<ClaudeCore> = None;
    let mut events = Vec::new();
    for line in text.lines() {
        let rec: Value = serde_json::from_str(line).unwrap();
        let out = match rec.get("dir").and_then(Value::as_str) {
            Some("spawn") => {
                core = Some(ClaudeCore::new(config(false)));
                continue;
            }
            Some("host") => {
                let cmd: HostCmd = serde_json::from_value(rec["cmd"].clone()).unwrap();
                core.as_mut().unwrap().apply(&cmd).unwrap()
            }
            Some("out") => core
                .as_mut()
                .unwrap()
                .on_frame(parse_value(rec["frame"].clone()).unwrap()),
            Some("exit") => core.as_mut().unwrap().on_exit(None),
            _ => continue,
        };
        events.extend(emits(&out));
    }
    events
}

/// The `(status, summary)` pairs of the `task.updated` events of `kind`.
fn task_states(events: &[Event], kind: &str) -> Vec<(String, Option<String>)> {
    events
        .iter()
        .filter_map(|e| match e {
            Event::TaskUpdated {
                task_kind,
                status,
                summary,
                ..
            } if task_kind == kind => Some((status.clone(), summary.clone())),
            _ => None,
        })
        .collect()
}

/// The node of the tool call with this title.
fn node_of(events: &[Event], tool: &str) -> NodeId {
    events
        .iter()
        .find_map(|e| match e {
            Event::ToolStarted { node_id, title, .. } if title == tool => Some(node_id.clone()),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no {tool} call in {}", kinds(events).join(",")))
}

fn system(core: &mut ClaudeCore, frame: Value) -> Vec<Event> {
    emits(&feed(core, frame))
}

fn task_started(core: &mut ClaudeCore, task: &str, tool_use_id: &str) -> Vec<Event> {
    system(
        core,
        json!({"type":"system","subtype":"task_started","task_id":task,"task_type":"local_agent",
               "tool_use_id":tool_use_id,"description":"Test subagent"}),
    )
}

fn child(core: &mut ClaudeCore, parent: &str, blocks: Value) -> Vec<Event> {
    emits(&feed(
        core,
        json!({"type":"assistant","parent_tool_use_id":parent,
               "message":{"role":"assistant","content":blocks}}),
    ))
}

fn tool_result(core: &mut ClaudeCore, id: &str) -> Vec<Event> {
    emits(&feed(
        core,
        json!({"type":"user","message":{"role":"user","content":[
            {"type":"tool_result","tool_use_id":id,"content":"ok"}]}}),
    ))
}

#[test]
fn the_subagent_fixture_reports_the_task_from_running_to_completed() {
    let events = replay("subagent.jsonl");
    let states = task_states(&events, "local_agent");
    assert_eq!(
        states.first(),
        Some(&("running".to_owned(), None)),
        "{states:?}"
    );
    assert_eq!(
        states.last(),
        Some(&("completed".to_owned(), Some("sub-ok".to_owned())))
    );
    // An event is a change: no two neighbours are equal.
    assert!(states.windows(2).all(|w| w[0] != w[1]), "{states:?}");
    let agent = node_of(&events, "Agent");
    let tasks: Vec<&Event> = events
        .iter()
        .filter(|e| matches!(e, Event::TaskUpdated { .. }))
        .collect();
    assert!(tasks.iter().all(|e| matches!(
        e,
        Event::TaskUpdated { task_id, node_id: Some(n), .. }
            if task_id == "aec7cbebd3902b8ce" && n == agent.as_str()
    )));
    assert!(matches!(
        tasks[0],
        Event::TaskUpdated { description: Some(d), .. } if d == "Test subagent"
    ));
}

#[test]
fn the_text_of_a_background_subagent_after_the_parent_result_keeps_its_launch_turn() {
    let events = replay("subagent.jsonl");
    let agent = node_of(&events, "Agent");
    let mut launch: Option<TurnId> = None;
    let mut turn_ended = false;
    let mut late = Vec::new();
    for e in &events {
        match e {
            Event::TurnStarted { turn_id, .. } if launch.is_none() => {
                launch = Some(turn_id.clone())
            }
            Event::TurnCompleted { turn_id, .. } if Some(turn_id) == launch.as_ref() => {
                turn_ended = true;
            }
            Event::MessageCompleted {
                turn_id,
                parent_node_id: Some(p),
                text,
            } if *p == agent => late.push((turn_ended, turn_id.clone(), text.clone())),
            _ => {}
        }
    }
    assert_eq!(late.len(), 1, "{late:?}");
    assert!(late[0].0, "the text came after the parent turn ended");
    assert_eq!(Some(&late[0].1), launch.as_ref());
    assert_eq!(late[0].2, "sub-ok");
}

#[test]
fn a_background_command_task_is_bound_to_its_bash_card() {
    let events = replay("background_task.jsonl");
    let states = task_states(&events, "local_bash");
    assert_eq!(states.first().map(|s| s.0.as_str()), Some("running"));
    assert_eq!(states.last().map(|s| s.0.as_str()), Some("completed"));
    assert!(
        states
            .last()
            .and_then(|s| s.1.as_deref())
            .is_some_and(|s| s.contains("completed (exit code 0)")),
        "{states:?}"
    );
    let bash = node_of(&events, "Bash");
    assert!(events.iter().all(|e| match e {
        Event::TaskUpdated { node_id, .. } => node_id.as_deref() == Some(bash.as_str()),
        _ => true,
    }));
}

#[test]
fn a_child_tool_of_a_running_background_agent_is_not_closed_at_the_parent_result() {
    let mut core = running_core();
    tool_use(&mut core, "toolu_task", "Task", json!({"prompt":"look"}));
    task_started(&mut core, "t1", "toolu_task");
    // The launch call completes at once; the agent keeps working.
    tool_result(&mut core, "toolu_task");
    child(
        &mut core,
        "toolu_task",
        json!([{"type":"tool_use","id":"toolu_child","name":"Read","input":{"file_path":"/w/a"}}]),
    );
    let ev = result(&mut core, "success", "completed");
    assert!(!kinds(&ev).contains(&"tool.completed".to_owned()), "{ev:?}");
    // Its own result completes it, after the parent turn ended.
    let ev = tool_result(&mut core, "toolu_child");
    assert!(matches!(
        ev.as_slice(),
        [Event::ToolCompleted {
            status: ToolStatus::Ok,
            ..
        }]
    ));
}

#[test]
fn the_end_of_a_task_closes_the_tools_that_are_still_open_under_it() {
    let mut core = running_core();
    tool_use(&mut core, "toolu_task", "Task", json!({"prompt":"look"}));
    task_started(&mut core, "t1", "toolu_task");
    tool_result(&mut core, "toolu_task");
    child(
        &mut core,
        "toolu_task",
        json!([{"type":"tool_use","id":"toolu_child","name":"Read","input":{"file_path":"/w/a"}}]),
    );
    result(&mut core, "success", "completed");
    let ev = system(
        &mut core,
        json!({"type":"system","subtype":"task_notification","task_id":"t1","status":"failed",
               "summary":"stopped by the owner","tool_use_id":"toolu_task"}),
    );
    // The task event first, then the close of the child; the Task node was done.
    assert_eq!(kinds(&ev), ["task.updated", "tool.completed"]);
    assert!(matches!(
        &ev[1],
        Event::ToolCompleted {
            status: ToolStatus::Failed,
            ..
        }
    ));
    // The same notification again changes nothing.
    let ev = system(
        &mut core,
        json!({"type":"system","subtype":"task_notification","task_id":"t1","status":"failed",
               "summary":"stopped by the owner","tool_use_id":"toolu_task"}),
    );
    assert!(ev.is_empty(), "{ev:?}");
}

#[test]
fn only_a_final_status_ends_a_task() {
    let mut core = running_core();
    tool_use(&mut core, "toolu_task", "Task", json!({"prompt":"look"}));
    task_started(&mut core, "t1", "toolu_task");
    tool_result(&mut core, "toolu_task");
    child(
        &mut core,
        "toolu_task",
        json!([{"type":"tool_use","id":"toolu_child","name":"Read","input":{"file_path":"/w/a"}}]),
    );
    let ev = system(
        &mut core,
        json!({"type":"system","subtype":"task_updated","task_id":"t1","patch":{"status":"paused"}}),
    );
    assert!(matches!(
        ev.as_slice(),
        [Event::TaskUpdated { status, .. }] if status == "paused"
    ));
    let ev = result(&mut core, "success", "completed");
    assert!(!kinds(&ev).contains(&"tool.completed".to_owned()), "{ev:?}");
    for status in ["killed", "stopped", "cancelled", "completed"] {
        let mut core = running_core();
        tool_use(&mut core, "toolu_task", "Task", json!({"prompt":"x"}));
        task_started(&mut core, "t1", "toolu_task");
        tool_result(&mut core, "toolu_task");
        child(
            &mut core,
            "toolu_task",
            json!([{"type":"tool_use","id":"toolu_child","name":"Read","input":{}}]),
        );
        let ev = system(
            &mut core,
            json!({"type":"system","subtype":"task_updated","task_id":"t1","patch":{"status":status}}),
        );
        assert_eq!(kinds(&ev), ["task.updated", "tool.completed"], "{status}");
    }
}

#[test]
fn a_task_frame_without_a_status_or_a_task_id_emits_nothing() {
    let mut core = running_core();
    let ev = system(
        &mut core,
        json!({"type":"system","subtype":"task_updated","task_id":"t1","patch":{"end_time":1}}),
    );
    assert!(ev.is_empty(), "{ev:?}");
    let ev = system(
        &mut core,
        json!({"type":"system","subtype":"task_notification","task_id":"t1"}),
    );
    assert!(ev.is_empty(), "{ev:?}");
    let before = core.unknown_frames();
    let ev = system(
        &mut core,
        json!({"type":"system","subtype":"task_started","tool_use_id":"x"}),
    );
    assert!(ev.is_empty(), "{ev:?}");
    assert_eq!(core.unknown_frames(), before + 1);
}

#[test]
fn a_task_summary_is_cut_to_200_characters_and_an_unknown_task_has_no_node() {
    let mut core = running_core();
    let long = "x".repeat(500);
    let ev = system(
        &mut core,
        json!({"type":"system","subtype":"task_notification","task_id":"t9","status":"completed",
               "summary":long}),
    );
    let [
        Event::TaskUpdated {
            node_id: None,
            task_kind,
            summary: Some(summary),
            ..
        },
    ] = ev.as_slice()
    else {
        panic!("{ev:?}")
    };
    assert_eq!(task_kind, "unknown");
    assert_eq!(
        summary.chars().count(),
        201,
        "200 characters and an ellipsis"
    );
}

#[test]
fn a_nested_agent_started_with_no_turn_running_keeps_the_launch_turn_of_its_parent() {
    let mut core = running_core();
    tool_use(&mut core, "toolu_a", "Task", json!({"prompt":"a"}));
    tool_result(&mut core, "toolu_a");
    result(&mut core, "success", "completed");
    assert!(core.current_turn().is_none());
    child(
        &mut core,
        "toolu_a",
        json!([{"type":"tool_use","id":"toolu_b","name":"Agent","input":{"prompt":"b"}}]),
    );
    let ev = child(
        &mut core,
        "toolu_b",
        json!([{"type":"text","text":"deep text"}]),
    );
    assert!(matches!(
        ev.as_slice(),
        [Event::MessageCompleted { turn_id, text, .. }]
            if turn_id.as_str() == "turn-1" && text == "deep text"
    ));
}

#[test]
fn a_child_message_of_an_unknown_parent_still_needs_a_running_turn() {
    let mut core = running_core();
    result(&mut core, "success", "completed");
    let ev = child(
        &mut core,
        "toolu_unknown",
        json!([{"type":"text","text":"stray"}]),
    );
    assert!(ev.is_empty(), "{ev:?}");
}
