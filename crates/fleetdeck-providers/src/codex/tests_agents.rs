//! `task.updated` from Codex agent states and child turns (B6).

use std::path::Path;

use fleetdeck_proto::Event;
use fleetdeck_proto::event::ToolKind;
use serde_json::{Value, json};

use crate::codex::rpc::parse_value;
use crate::codex::tests::{T, config, emits, notify, ready_core, start_turn};
use crate::codex::{CodexCore, HostCmd};

/// The events of a recorded fixture (`spikes/s2-codex/fixtures`), from the
/// production core. Written frames are not compared here: the replay test of
/// the fixtures does that.
fn replay(name: &str) -> Vec<Event> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../spikes/s2-codex/fixtures")
        .join(name);
    let text = std::fs::read_to_string(path).unwrap();
    let mut core: Option<CodexCore> = None;
    let mut events = Vec::new();
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let rec: Value = serde_json::from_str(line).unwrap();
        let out = match rec.get("dir").and_then(Value::as_str) {
            Some("spawn") => {
                core = Some(CodexCore::new(config("<workdir>")));
                continue;
            }
            Some("host") => {
                let cmd: HostCmd = serde_json::from_value(rec["cmd"].clone()).unwrap();
                core.as_mut().unwrap().apply(&cmd).unwrap()
            }
            Some("out") => core
                .as_mut()
                .unwrap()
                .on_frame(parse_value(rec["frame"].clone())),
            Some("exit") => core.as_mut().unwrap().on_exit(None),
            _ => continue,
        };
        events.extend(emits(&out));
    }
    events
}

/// The `(task id, node id, status, summary)` of every `task.updated` event.
fn tasks(events: &[Event]) -> Vec<(String, Option<String>, String, Option<String>)> {
    events
        .iter()
        .filter_map(|e| match e {
            Event::TaskUpdated {
                task_id,
                node_id,
                status,
                summary,
                task_kind,
                ..
            } => {
                assert_eq!(task_kind, "codex_agent");
                Some((
                    task_id.clone(),
                    node_id.clone(),
                    status.clone(),
                    summary.clone(),
                ))
            }
            _ => None,
        })
        .collect()
}

fn collab(id: &str, tool: &str, status: &str, states: Value) -> Value {
    json!({"type": "collabAgentToolCall", "id": id, "tool": tool, "status": status,
           "receiverThreadIds": ["child-1"], "agentsStates": states})
}

fn item(core: &mut CodexCore, method: &str, item: Value) -> Vec<Event> {
    emits(&notify(core, method, json!({"threadId": T, "item": item})))
}

#[test]
fn the_subagent_fixture_reports_the_agent_from_running_to_completed_once() {
    let events = replay("subagent.jsonl");
    let node = events
        .iter()
        .find_map(|e| match e {
            Event::ToolStarted {
                node_id,
                tool_kind: ToolKind::Subagent,
                ..
            } => Some(node_id.as_str().to_owned()),
            _ => None,
        })
        .unwrap();
    let child = "01a128fa-7e09-7a73-9780-a51dd6f22a27".to_owned();
    assert_eq!(
        tasks(&events),
        [
            (
                child.clone(),
                Some(node.clone()),
                "running".to_owned(),
                None
            ),
            (
                child,
                Some(node),
                "completed".to_owned(),
                Some("sub-ok".to_owned())
            ),
        ]
    );
}

#[test]
fn the_native_agent_statuses_map_to_the_task_statuses() {
    let table = [
        ("pendingInit", "running"),
        ("running", "running"),
        ("inProgress", "running"),
        ("completed", "completed"),
        ("errored", "failed"),
        ("failed", "failed"),
        ("error", "failed"),
        ("shutdown", "stopped"),
        ("interrupted", "stopped"),
        ("notFound", "notFound"),
    ];
    for (native, status) in table {
        let mut core = ready_core();
        start_turn(&mut core, "in-1", "n1");
        let ev = item(
            &mut core,
            "item/completed",
            collab(
                "c1",
                "wait",
                "completed",
                json!({"child-1": {"status": native, "message": null}}),
            ),
        );
        assert_eq!(
            tasks(&ev),
            [(
                "child-1".to_owned(),
                Some("node-2".to_owned()),
                status.to_owned(),
                None
            )],
            "{native}"
        );
    }
}

#[test]
fn an_item_that_starts_reports_its_agent_states_and_a_repeat_is_dropped() {
    let mut core = ready_core();
    start_turn(&mut core, "in-1", "n1");
    let states = json!({"child-1": {"status": "running", "message": "working"}});
    let ev = item(
        &mut core,
        "item/started",
        collab("c1", "wait", "inProgress", states.clone()),
    );
    assert_eq!(
        tasks(&ev),
        [(
            "child-1".to_owned(),
            Some("node-2".to_owned()),
            "running".to_owned(),
            Some("working".to_owned())
        )]
    );
    let ev = item(
        &mut core,
        "item/completed",
        collab("c1", "wait", "completed", states),
    );
    assert!(tasks(&ev).is_empty(), "{ev:?}");
}

#[test]
fn a_summary_is_cut_and_a_later_call_reports_under_the_first_node() {
    let mut core = ready_core();
    start_turn(&mut core, "in-1", "n1");
    item(
        &mut core,
        "item/completed",
        collab("spawn1", "spawnAgent", "completed", json!({})),
    );
    let long = "y".repeat(500);
    let ev = item(
        &mut core,
        "item/completed",
        collab(
            "wait1",
            "wait",
            "completed",
            json!({"child-1": {"status": "completed", "message": long}}),
        ),
    );
    let states = tasks(&ev);
    let [(_, Some(node), _, Some(summary))] = states.as_slice() else {
        panic!("{ev:?}")
    };
    assert_eq!(node, "node-2", "the spawn call named the thread first");
    assert_eq!(summary.chars().count(), 200);
}

#[test]
fn child_turns_report_running_and_the_end_of_the_agent() {
    let mut core = ready_core();
    start_turn(&mut core, "in-1", "n1");
    item(
        &mut core,
        "item/completed",
        collab("spawn1", "spawnAgent", "completed", json!({})),
    );
    let ev = emits(&notify(
        &mut core,
        "turn/started",
        json!({"threadId": "child-1", "turn": {"id": "c1"}}),
    ));
    assert_eq!(tasks(&ev).len(), 1);
    let ev = emits(&notify(
        &mut core,
        "turn/completed",
        json!({"threadId": "child-1", "turn": {"id": "c1", "status": "failed", "items": [
            {"type": "agentMessage", "id": "m1", "text": "first"},
            {"type": "agentMessage", "id": "m2", "text": "it broke"}]}}),
    ));
    assert_eq!(
        tasks(&ev),
        [(
            "child-1".to_owned(),
            Some("node-2".to_owned()),
            "failed".to_owned(),
            Some("it broke".to_owned())
        )]
    );
}

#[test]
fn a_child_turn_status_maps_like_an_agent_status() {
    for (native, status) in [
        ("completed", "completed"),
        ("failed", "failed"),
        ("interrupted", "stopped"),
        ("somethingNew", "somethingNew"),
    ] {
        let mut core = ready_core();
        start_turn(&mut core, "in-1", "n1");
        item(
            &mut core,
            "item/completed",
            collab("spawn1", "spawnAgent", "completed", json!({})),
        );
        let ev = emits(&notify(
            &mut core,
            "turn/completed",
            json!({"threadId": "child-1", "turn": {"id": "c1", "status": native}}),
        ));
        let got: Vec<String> = tasks(&ev).into_iter().map(|t| t.2).collect();
        assert_eq!(got, [status], "{native}");
    }
}

#[test]
fn a_child_turn_of_a_thread_that_no_call_named_reports_nothing() {
    let mut core = ready_core();
    start_turn(&mut core, "in-1", "n1");
    for method in ["turn/started", "turn/completed"] {
        let ev = emits(&notify(
            &mut core,
            method,
            json!({"threadId": "stranger", "turn": {"id": "c1", "status": "completed"}}),
        ));
        assert!(ev.is_empty(), "{ev:?}");
    }
}
