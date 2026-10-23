//! Workflow runs from `task_progress` frames: `workflow.updated` with only the
//! changes, the run id, the limits and the checkpoint (B12).

use std::collections::BTreeSet;
use std::path::Path;

use fleetdeck_proto::event::{EventEnvelope, WORKFLOW_EVENT_BUDGET, WorkflowAgent, WorkflowUpdate};
use fleetdeck_proto::link::{ControlFrame, MAX_CONTROL_FRAME_BYTES};
use fleetdeck_proto::{Event, SessionId};
use serde_json::{Value, json};

use crate::adapter::ProviderError;
use crate::claude::ClaudeCore;
use crate::claude::core::TaskEntry;
use crate::claude::frames::parse_value;
use crate::claude::tests::{config, emits, feed, kinds, result, running_core, tool_use};

/// The `out` frames of a recorded workflow fixture.
fn frames(name: &str) -> Vec<Value> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../spikes/s1-claude/fixtures/workflow")
        .join(name);
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str::<Value>(l).unwrap())
        .filter(|r| r.get("dir").and_then(Value::as_str) == Some("out"))
        .map(|r| r["frame"].clone())
        .collect()
}

/// A fresh core and the events of its first `count` frames of the fixture
/// (`None`: every frame).
fn replay(name: &str, count: Option<usize>) -> (ClaudeCore, Vec<Event>) {
    let mut core = ClaudeCore::new(config(false));
    let mut events = Vec::new();
    for frame in frames(name).into_iter().take(count.unwrap_or(usize::MAX)) {
        events.extend(emits(&core.on_frame(parse_value(frame).unwrap())));
    }
    (core, events)
}

fn workflow_updates(events: &[Event]) -> Vec<WorkflowUpdate> {
    events
        .iter()
        .filter_map(|e| match e {
            Event::WorkflowUpdated(u) => Some((**u).clone()),
            _ => None,
        })
        .collect()
}

fn system(core: &mut ClaudeCore, frame: Value) -> Vec<Event> {
    emits(&feed(core, frame))
}

fn started(core: &mut ClaudeCore, task: &str, tool_use_id: &str, name: &str) -> Vec<Event> {
    system(
        core,
        json!({"type":"system","subtype":"task_started","task_id":task,"task_type":"local_workflow",
               "tool_use_id":tool_use_id,"workflow_name":name,"description":"d"}),
    )
}

fn progress(core: &mut ClaudeCore, task: &str, list: Value) -> Vec<Event> {
    system(
        core,
        json!({"type":"system","subtype":"task_progress","task_id":task,"description":"x",
               "workflow_progress":list}),
    )
}

fn finish(core: &mut ClaudeCore, task: &str, status: &str) -> Vec<Event> {
    system(
        core,
        json!({"type":"system","subtype":"task_updated","task_id":task,"patch":{"status":status}}),
    )
}

fn agent(index: u32, state: &str, extra: &Value) -> Value {
    let mut entry = json!({"type":"workflow_agent","index":index,"label":format!("a{index}"),
                           "state":state,"startedAt":1000});
    for (k, v) in extra.as_object().unwrap() {
        entry[k] = v.clone();
    }
    entry
}

fn phase(index: u32, title: &str) -> Value {
    json!({"type":"workflow_phase","index":index,"title":title})
}

/// A user frame with the result of the tool call `id`.
fn tool_result(
    core: &mut ClaudeCore,
    id: &str,
    content: &str,
    structured: Option<Value>,
) -> Vec<Event> {
    let mut frame = json!({"type":"user","message":{"role":"user","content":[
        {"type":"tool_result","tool_use_id":id,"content":content}]}});
    if let Some(s) = structured {
        frame["tool_use_result"] = s;
    }
    system(core, frame)
}

/// A core in a turn that called `Workflow` (`tool_use_id` `t1`).
fn workflow_core() -> ClaudeCore {
    let mut core = running_core();
    tool_use(&mut core, "t1", "Workflow", json!({"script":"x"}));
    core
}

fn tool_completed_refs(events: &[Event]) -> Vec<serde_json::Map<String, Value>> {
    events
        .iter()
        .filter_map(|e| match e {
            Event::ToolCompleted { provider_refs, .. } => Some(provider_refs.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn two_phases_give_phases_agents_and_the_run_id() {
    let (core, events) = replay("workflow_two_phases.jsonl", None);
    let updates = workflow_updates(&events);
    let first = &updates[0];
    assert_eq!(first.status, "running");
    assert_eq!(first.name.as_deref(), Some("bl-tiny"));
    assert!(first.phases.is_empty() && first.agents.is_empty());
    assert_eq!((first.running_agents, first.agents_total), (0, 0));
    assert!(first.node_id.is_some() && first.run_id.is_none());
    // The run id is in `tool.completed` and in one event with no agents.
    let refs = tool_completed_refs(&events);
    assert!(
        refs.iter()
            .any(|r| r.get("workflow_run_id") == Some(&json!("wf_0918c8f1-e62")))
    );
    let run_events: Vec<usize> = updates
        .iter()
        .enumerate()
        .filter(|(_, u)| u.run_id.is_some() && u.agents.is_empty() && u.phases.is_empty())
        .filter(|(_, u)| u.status == "running")
        .map(|(i, _)| i)
        .collect();
    assert_eq!(run_events, vec![1], "{updates:#?}");
    assert!(
        updates[1..]
            .iter()
            .all(|u| u.run_id.as_deref() == Some("wf_0918c8f1-e62"))
    );
    // Phases.
    let phases: Vec<(u32, String)> = updates
        .iter()
        .flat_map(|u| u.phases.iter().map(|p| (p.index, p.title.clone())))
        .collect();
    assert_eq!(
        phases,
        vec![(1, "Alpha".to_owned()), (2, "Beta".to_owned())]
    );
    // Agent 1: running, then done with its result.
    let one: Vec<&WorkflowAgent> = updates
        .iter()
        .flat_map(|u| u.agents.iter())
        .filter(|a| a.index == 1)
        .collect();
    assert_eq!(one.first().map(|a| a.state.as_str()), Some("running"));
    assert_eq!(one.last().map(|a| a.state.as_str()), Some("done"));
    assert_eq!(
        one.last().and_then(|a| a.result_preview.as_deref()),
        Some("alpha")
    );
    assert_eq!(one[0].agent_id.as_deref(), Some("a2dcb1da6566191c3"));
    // The end: completed, no agents, nothing running.
    let last = updates.last().unwrap();
    assert_eq!(last.status, "completed");
    assert!(last.agents.is_empty() && last.phases.is_empty());
    assert_eq!((last.running_agents, last.agents_total), (0, 2));
    assert_eq!(core.workflows["wey0ztgn3"].status, "completed");
}

#[test]
fn fanout_reports_parallel_agents_and_only_changes() {
    let all = frames("workflow_fanout.jsonl");
    let mut core = ClaudeCore::new(config(false));
    let mut peak = 0;
    let mut counts = Vec::new();
    for (i, frame) in all.iter().enumerate() {
        let has_list = frame.get("workflow_progress").is_some_and(Value::is_array);
        let events = emits(&core.on_frame(parse_value(frame.clone()).unwrap()));
        let n = workflow_updates(&events);
        peak = peak.max(n.iter().map(|u| u.running_agents).max().unwrap_or(0));
        if frame["subtype"] == "task_progress" {
            counts.push((i, has_list, n.len()));
        }
    }
    assert_eq!(peak, 2);
    // A frame without a list emits nothing.
    assert!(
        counts.iter().filter(|c| !c.1).all(|c| c.2 == 0),
        "{counts:?}"
    );
    let with_list: Vec<&(usize, bool, usize)> = counts.iter().filter(|c| c.1).collect();
    assert_eq!(with_list[0].2, 1);
    // A list that repeats the state emits nothing.
    let (mut core, _) = replay(
        "workflow_fanout.jsonl",
        Some(frames_until_first_list("workflow_fanout.jsonl") + 1),
    );
    let list = all
        .iter()
        .find(|f| f.get("workflow_progress").is_some())
        .unwrap()
        .clone();
    assert!(workflow_updates(&system(&mut core, list.clone())).is_empty());
    // A change of only the counters emits nothing.
    let mut again = list.clone();
    for item in again["workflow_progress"].as_array_mut().unwrap() {
        if item["type"] == "workflow_agent" {
            item["tokens"] = json!(12345);
            item["toolCalls"] = json!(7);
            item["lastToolName"] = json!("Bash");
        }
    }
    assert!(workflow_updates(&system(&mut core, again)).is_empty());
}

/// The index of the first frame with a `workflow_progress` list.
fn frames_until_first_list(name: &str) -> usize {
    frames(name)
        .iter()
        .position(|f| f.get("workflow_progress").is_some())
        .unwrap()
}

#[test]
fn an_interrupt_stops_the_workflow_without_agent_entries() {
    let (mut core, events) = replay("workflow_interrupt.jsonl", None);
    let updates = workflow_updates(&events);
    let stopped: Vec<&WorkflowUpdate> = updates.iter().filter(|u| u.status == "stopped").collect();
    assert_eq!(stopped.len(), 1, "{updates:#?}");
    assert!(stopped[0].agents.is_empty());
    assert_eq!(stopped[0].running_agents, 0);
    assert_eq!(stopped[0].agents_total, 2);
    // A later progress frame of the task emits nothing.
    let task = stopped[0].task_id.clone();
    let after = progress(&mut core, &task, json!([agent(3, "start", &json!({}))]));
    assert!(workflow_updates(&after).is_empty());
    // A second final status emits nothing either.
    assert!(workflow_updates(&finish(&mut core, &task, "killed")).is_empty());
}

#[test]
fn an_agent_permission_names_its_agent() {
    let (_, events) = replay("workflow_agent_approval.jsonl", None);
    let refs: Vec<&serde_json::Map<String, Value>> = events
        .iter()
        .filter_map(|e| match e {
            Event::ApprovalRequested {
                title,
                provider_refs,
                ..
            } if title == "Write" => Some(provider_refs),
            _ => None,
        })
        .collect();
    assert_eq!(refs.len(), 1);
    assert_eq!(refs[0].get("agent_id"), Some(&json!("a3f92b5c63d2c47ee")));
    // The main agent's request has none.
    assert!(events.iter().any(|e| matches!(e,
        Event::ApprovalRequested { title, provider_refs, .. }
            if title == "Workflow" && !provider_refs.contains_key("agent_id"))));
}

#[test]
fn a_resume_notification_of_an_unknown_task_emits_no_workflow_event() {
    let (_, events) = replay("workflow_resume_orphan.jsonl", None);
    assert!(workflow_updates(&events).is_empty());
    assert!(
        kinds(&events).contains(&"task.updated".to_owned()),
        "{:?}",
        kinds(&events)
    );
}

#[test]
fn a_queued_agent_is_not_running() {
    let mut core = workflow_core();
    started(&mut core, "w1", "t1", "n");
    let queued = progress(
        &mut core,
        "w1",
        json!([phase(1, "P"), {
        "type":"workflow_agent","index":1,"label":"q","state":"start"}]),
    );
    let u = &workflow_updates(&queued)[0];
    assert_eq!(u.agents[0].state, "queued");
    assert_eq!((u.running_agents, u.agents_total), (0, 1));
    let run = progress(
        &mut core,
        "w1",
        json!([agent(1, "start", &json!({"label":"q"}))]),
    );
    let u = &workflow_updates(&run)[0];
    assert_eq!(u.agents[0].state, "running");
    assert_eq!((u.running_agents, u.agents_total), (1, 1));
    // The other state words.
    let more = progress(
        &mut core,
        "w1",
        json!([
            agent(2, "error", &json!({"error":"boom"})),
            agent(3, "progress", &json!({})),
            agent(4, "weird", &json!({})),
        ]),
    );
    let more = workflow_updates(&more);
    let states: Vec<&str> = more[0].agents.iter().map(|a| a.state.as_str()).collect();
    assert_eq!(states, vec!["failed", "running", "weird"]);
}

#[test]
fn bad_entries_are_skipped() {
    let mut core = workflow_core();
    started(&mut core, "w1", "t1", "n");
    let events = progress(
        &mut core,
        "w1",
        json!([
            {"type":"workflow_agent","label":"no index","state":"start"},
            {"type":"workflow_agent","index":1,"label":7,"state":"start"},
            {"type":"workflow_phase","title":"no index"},
            {"type":"workflow_phase","index":1,"title":5},
            {"type":"workflow_log","index":9},
            agent(2, "start", &json!({"agentId":"a.b"})),
            agent(3, "start", &json!({"agentId":"abc123"})),
        ]),
    );
    let u = &workflow_updates(&events)[0];
    assert!(u.phases.is_empty());
    let got: Vec<(u32, Option<&str>)> = u
        .agents
        .iter()
        .map(|a| (a.index, a.agent_id.as_deref()))
        .collect();
    assert_eq!(got, vec![(2, None), (3, Some("abc123"))]);
    // A list that is not an array is not a workflow frame.
    let before = core.unknown_frames();
    let none = system(
        &mut core,
        json!({"type":"system","subtype":"task_progress",
        "task_id":"w1","workflow_progress":"x"}),
    );
    assert!(none.is_empty());
    assert_eq!(core.unknown_frames(), before);
    // A task id with `/` gets no entry and no workflow event; the task event stays.
    let bad = started(&mut core, "a/b", "t1", "n");
    assert!(workflow_updates(&bad).is_empty());
    assert_eq!(kinds(&bad), vec!["task.updated"]);
    assert!(!core.workflows.contains_key("a/b"));
    let p = progress(&mut core, "a/b", json!([agent(1, "start", &json!({}))]));
    assert!(p.is_empty());
}

#[test]
fn the_run_id_is_read_only_from_the_structured_result() {
    let run = |content: &str, structured: Option<Value>| {
        let mut core = workflow_core();
        started(&mut core, "w1", "t1", "n");
        let events = tool_result(&mut core, "t1", content, structured);
        (core, events)
    };
    let (core, events) = run("Run ID: wf_x\nWorkflow launched", None);
    assert!(!tool_completed_refs(&events)[0].contains_key("workflow_run_id"));
    assert!(workflow_updates(&events).is_empty());
    assert!(core.workflows["w1"].run_id.is_none());
    for bad in ["../x", "wf_", "wf_A", "wf_a/b", "x_wf_1"] {
        let (core, events) = run("ok", Some(json!({"runId":bad})));
        assert!(
            !tool_completed_refs(&events)[0].contains_key("workflow_run_id"),
            "{bad}"
        );
        assert!(core.workflows["w1"].run_id.is_none(), "{bad}");
    }
    let (core, events) = run("ok", Some(json!({"runId":"wf_0918c8f1-e62"})));
    assert_eq!(
        tool_completed_refs(&events)[0].get("workflow_run_id"),
        Some(&json!("wf_0918c8f1-e62"))
    );
    let updates = workflow_updates(&events);
    assert_eq!(updates.len(), 1);
    assert_eq!(updates[0].run_id.as_deref(), Some("wf_0918c8f1-e62"));
    assert!(updates[0].agents.is_empty() && updates[0].phases.is_empty());
    assert_eq!(
        core.workflows["w1"].run_id.as_deref(),
        Some("wf_0918c8f1-e62")
    );
}

#[test]
fn the_checkpoint_keeps_the_workflows() {
    let mut core = workflow_core();
    started(&mut core, "w1", "t1", "n");
    progress(
        &mut core,
        "w1",
        json!([phase(1, "P"), agent(1, "start", &json!({}))]),
    );
    // One more workflow that the core does not follow.
    core.untracked_workflows.push_back("old".to_owned());
    core.tasks.insert(
        "old".to_owned(),
        TaskEntry {
            task_kind: "local_workflow".to_owned(),
            running: true,
            ..TaskEntry::default()
        },
    );
    let checkpoint = core.checkpoint().unwrap();
    let mut restored = ClaudeCore::restore(config(false), &checkpoint).unwrap();
    assert_eq!(restored.next_workflow, core.next_workflow);
    assert_eq!(restored.untracked_workflows, core.untracked_workflows);
    // The same frame is no change; a changed agent is.
    let same = progress(
        &mut restored,
        "w1",
        json!([phase(1, "P"), agent(1, "start", &json!({}))]),
    );
    assert!(workflow_updates(&same).is_empty());
    let done = json!([agent(1, "done", &json!({"resultPreview":"r"}))]);
    let from_restored = workflow_updates(&progress(&mut restored, "w1", done.clone()));
    let from_live = workflow_updates(&progress(&mut core, "w1", done));
    assert_eq!(from_restored, from_live);
    assert_eq!(from_restored[0].agents[0].state, "done");
    // The untracked id ends with one untracked event.
    let end = workflow_updates(&finish(&mut restored, "old", "completed"));
    assert_eq!(end.len(), 1);
    assert!(end[0].untracked);
}

/// The text of `n` control characters: each is a 6-byte JSON escape.
fn ctl(n: usize) -> String {
    "\u{1}".repeat(n)
}

#[test]
fn a_maximum_frame_splits_under_the_frame_limit() {
    let mut core = workflow_core();
    started(&mut core, "w1", "t1", &ctl(500));
    let mut list = Vec::new();
    for i in 1..=100 {
        list.push(phase(i, &ctl(500)));
    }
    for i in 1..=1000u32 {
        // Half of the agents run; the others have a long state text.
        let state = if i % 2 == 0 {
            "start".to_owned()
        } else {
            ctl(100)
        };
        list.push(
            json!({"type":"workflow_agent","index":i,"label":ctl(500),"state":state,
            "startedAt":1792771552664_i64,"phaseIndex":i,"agentId":"a".repeat(64),
            "model":ctl(500),"attempt":4000000000_u32,"toolCalls":4000000000_u32,
            "tokens":u64::MAX,"lastToolName":ctl(500),"promptPreview":ctl(500),
            "resultPreview":ctl(500),"error":ctl(500)}),
        );
    }
    let events = progress(&mut core, "w1", Value::Array(list));
    let updates = workflow_updates(&events);
    assert!(updates.len() > 1);
    let mut seen = BTreeSet::new();
    for (i, u) in updates.iter().enumerate() {
        assert!(serde_json::to_vec(u).unwrap().len() <= WORKFLOW_EVENT_BUDGET);
        let frame = ControlFrame::Event {
            incarnation: "i".repeat(64).into(),
            envelope: EventEnvelope {
                station_seq: u64::MAX,
                session_id: SessionId::from("s".repeat(128)),
                session_seq: u64::MAX,
                at: i64::MAX,
                proto_version: 1,
                raw_ref: None,
                event: Event::WorkflowUpdated(Box::new(u.clone())).into(),
            },
        };
        assert!(serde_json::to_string(&frame).unwrap().len() <= MAX_CONTROL_FRAME_BYTES);
        // The same absolute fields in every event; phases only in the first.
        assert_eq!(u.status, "running");
        assert_eq!((u.running_agents, u.agents_total), (500, 1000));
        assert_eq!(u.phases.len(), if i == 0 { 100 } else { 0 });
        for a in &u.agents {
            assert!(seen.insert(a.index), "agent {} twice", a.index);
        }
    }
    assert_eq!(seen, (1..=1000).collect::<BTreeSet<u32>>());
}

#[test]
fn limits_cut_texts_and_cap_agents() {
    let mut core = workflow_core();
    started(&mut core, "w1", "t1", &"n".repeat(300));
    let long = "é".repeat(300);
    let list: Vec<Value> = (1..=1001u32)
        .map(|i| agent(i, "start", &json!({"label": long, "model": long})))
        .chain([phase(1, &long)])
        .collect();
    let updates = workflow_updates(&progress(&mut core, "w1", Value::Array(list)));
    let agents: Vec<&WorkflowAgent> = updates.iter().flat_map(|u| u.agents.iter()).collect();
    assert_eq!(agents.len(), 1000);
    assert!(agents.iter().all(|a| a.index <= 1000));
    assert_eq!(agents[0].label.chars().count(), 121);
    assert!(agents[0].label.ends_with('…'));
    assert_eq!(agents[0].model.as_deref().unwrap().chars().count(), 65);
    assert_eq!(updates[0].phases[0].title.chars().count(), 121);
    assert_eq!(updates[0].name.as_deref().unwrap().chars().count(), 81);
    // The 33rd ended workflow drops the oldest ended one; a running one stays.
    let mut core = workflow_core();
    started(&mut core, "keep", "t1", "keep");
    for i in 0..33 {
        let id = format!("w{i}");
        started(&mut core, &id, "t1", &id);
        finish(&mut core, &id, "completed");
    }
    assert!(core.workflows.contains_key("keep"));
    assert!(!core.workflows.contains_key("w0"));
    assert!(core.workflows.contains_key("w1") && core.workflows.contains_key("w32"));
    assert_eq!(core.workflows.len(), 33);
}

#[test]
fn the_65th_running_workflow_is_untracked() {
    let mut core = workflow_core();
    for i in 0..64 {
        let tool = format!("tool{i}");
        tool_use(&mut core, &tool, "Workflow", json!({"script":"x"}));
        started(&mut core, &format!("w{i}"), &tool, "n");
        tool_result(
            &mut core,
            &tool,
            "ok",
            Some(json!({"runId": format!("wf_{i}")})),
        );
    }
    let open = started(&mut core, "w64", "t1", "n");
    let updates = workflow_updates(&open);
    assert_eq!(updates.len(), 1);
    assert!(updates[0].untracked && updates[0].status == "running");
    assert!(updates[0].phases.is_empty() && updates[0].agents.is_empty());
    assert_eq!((updates[0].running_agents, updates[0].agents_total), (0, 0));
    let none = progress(&mut core, "w64", json!([agent(1, "start", &json!({}))]));
    assert!(none.is_empty());
    let end = workflow_updates(&finish(&mut core, "w64", "completed"));
    assert_eq!(end.len(), 1);
    assert!(end[0].untracked && end[0].status == "completed");
    assert!(core.untracked_workflows.is_empty());
    for i in 0..64 {
        assert_eq!(
            core.workflows[&format!("w{i}")].run_id.as_deref(),
            Some(format!("wf_{i}").as_str())
        );
    }
    // It stays untracked when a slot becomes free.
    started(&mut core, "w65", "t1", "n");
    finish(&mut core, "w0", "completed");
    assert!(core.untracked_workflows.iter().any(|id| id == "w65"));
    assert!(progress(&mut core, "w65", json!([agent(1, "start", &json!({}))])).is_empty());
}

#[test]
fn an_interrupt_without_a_turn_is_refused_while_a_workflow_runs() {
    let mut core = workflow_core();
    started(&mut core, "w1", "t1", "n");
    result(&mut core, "success", "completed");
    assert_eq!(core.workflows["w1"].status, "running");
    assert!(matches!(core.interrupt(None), Err(ProviderError::NoTurn)));
}
