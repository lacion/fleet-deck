// Fold tests of the workflow fields of the summary (B12, B19, design §4.3).
use super::*;
use crate::capability::Capabilities;
use crate::event::{EventBody, ProviderKind, ProviderRefs};
use crate::ids::SessionId;

struct Feed {
    summary: SessionSummary,
    seq: u64,
}

impl Feed {
    fn new() -> Self {
        Self {
            summary: SessionSummary::new(SessionId::from("s1")),
            seq: 0,
        }
    }

    fn push(&mut self, event: Event) -> &SessionSummary {
        self.seq += 1;
        let envelope = EventEnvelope {
            station_seq: self.seq + 100,
            session_id: SessionId::from("s1"),
            session_seq: self.seq,
            at: 1_000 + i64::try_from(self.seq).unwrap(),
            proto_version: 1,
            raw_ref: None,
            event: EventBody::Known(event),
        };
        self.summary.apply(&envelope);
        &self.summary
    }
}

fn started() -> Event {
    Event::SessionStarted {
        provider: ProviderKind::Claude,
        provider_instance: "claude".into(),
        provider_version: None,
        model: None,
        cwd: "/w".to_owned(),
        mode: None,
        capabilities: Box::new(Capabilities::minimal()),
        resumed: None,
        provider_refs: ProviderRefs::new(),
        title: None,
        branch: None,
        repo: None,
    }
}

fn wf(task: &str, status: &str, running: u32, total: u32) -> Event {
    Event::WorkflowUpdated(Box::new(WorkflowUpdate {
        task_id: task.to_owned(),
        node_id: None,
        name: None,
        run_id: None,
        status: status.to_owned(),
        untracked: false,
        phases: Vec::new(),
        agents: Vec::new(),
        running_agents: running,
        agents_total: total,
    }))
}

fn untracked(task: &str, status: &str) -> Event {
    let Event::WorkflowUpdated(mut update) = wf(task, status, 0, 0) else {
        unreachable!()
    };
    update.untracked = true;
    Event::WorkflowUpdated(update)
}

fn task(id: &str, kind: &str) -> BackgroundTask {
    BackgroundTask {
        task_id: id.to_owned(),
        task_kind: kind.to_owned(),
    }
}

fn workflow_task(id: &str) -> BackgroundTask {
    task(id, "local_workflow")
}

fn list(tasks: Vec<BackgroundTask>) -> Event {
    Event::BackgroundUpdated { tasks }
}

fn subagent_tool(id: &str) -> Event {
    Event::ToolStarted {
        node_id: id.into(),
        parent_node_id: None,
        tool_kind: ToolKind::Subagent,
        title: "Agent".to_owned(),
        input_summary: "x".to_owned(),
        files: Vec::new(),
        provider_refs: ProviderRefs::new(),
    }
}

fn ids(s: &SessionSummary) -> Vec<&str> {
    s.running_workflows
        .iter()
        .map(|w| w.task_id.as_str())
        .collect()
}

/// Fills `running_workflows` to the cap with `w0` .. `w255`.
fn fill(f: &mut Feed) {
    for i in 0..MAX_RUNNING_WORKFLOWS {
        f.push(wf(&format!("w{i}"), "running", 1, 1));
    }
    assert_eq!(f.summary.running_workflows.len(), MAX_RUNNING_WORKFLOWS);
}

#[test]
fn a_running_workflow_adds_its_running_agents() {
    let mut f = Feed::new();
    f.push(started());
    f.push(wf("wa", "running", 3, 5));
    f.push(wf("wb", "running", 2, 2));
    f.push(list(vec![
        workflow_task("wa"),
        workflow_task("wb"),
        task("ag", "local_agent"),
    ]));
    let s = f.push(subagent_tool("n1"));
    assert_eq!(s.background_agents, 1);
    assert_eq!(s.background_workflows, 2);
    // 3 + 2 workflow agents + 1 background agent + 1 running subagent tool.
    assert_eq!(s.running_agents, 7);
    assert!(!s.agents_partial);
    // A later event for the same task replaces its counts.
    let s = f.push(wf("wa", "running", 1, 5));
    assert_eq!(s.running_agents, 5);
    assert_eq!(s.running_workflows.len(), 2);
}

#[test]
fn a_final_status_removes_the_workflow() {
    for status in ["completed", "failed", "stopped"] {
        let mut f = Feed::new();
        f.push(started());
        f.push(wf("wa", "running", 2, 2));
        f.push(wf("wb", "running", 1, 1));
        let s = f.push(wf("wa", status, 0, 2));
        assert_eq!(ids(s), ["wb"], "{status}");
        assert_eq!(s.running_agents, 1, "{status}");
    }
}

#[test]
fn background_updated_removes_a_missing_workflow_while_another_runs() {
    let mut f = Feed::new();
    f.push(started());
    f.push(wf("wa", "running", 4, 4));
    f.push(wf("wb", "running", 2, 6));
    let s = f.push(list(vec![workflow_task("wb")]));
    assert_eq!(ids(s), ["wb"]);
    assert_eq!(s.running_agents, 2);
    assert_eq!(s.background_workflows, 1);
}

#[test]
fn a_running_event_after_an_early_list_adds_the_workflow_again() {
    let mut f = Feed::new();
    f.push(started());
    // The list comes before the running event of the task.
    let s = f.push(list(vec![workflow_task("wa")]));
    assert!(s.running_workflows.is_empty());
    assert_eq!(s.background_workflows, 1);
    let s = f.push(wf("wa", "running", 2, 3));
    assert_eq!(ids(s), ["wa"]);
    assert_eq!(s.running_agents, 2);
}

#[test]
fn the_end_of_the_process_clears_the_workflows() {
    let end = |f: &mut Feed| {
        f.push(Event::SessionEnded {
            reason: "exit".to_owned(),
            exit_code: Some(0),
        });
    };
    let lost = |f: &mut Feed| {
        f.push(Event::SessionStateChanged {
            state: SessionState::Lost,
            detail: None,
        });
    };
    for stop in [end, lost] {
        let mut f = Feed::new();
        f.push(started());
        f.push(wf("wa", "running", 2, 2));
        f.push(list(vec![workflow_task("wa")]));
        assert_eq!(f.summary.running_agents, 2);
        stop(&mut f);
        let s = &f.summary;
        assert!(s.running_workflows.is_empty());
        assert_eq!(s.background_workflows, 0);
        assert!(!s.workflows_overflow);
        assert_eq!(s.running_agents, 0);
        assert!(!s.agents_partial);
    }
}

#[test]
fn an_untracked_workflow_sets_agents_partial_until_it_ends() {
    // A final untracked event ends it.
    let mut f = Feed::new();
    f.push(started());
    let s = f.push(untracked("wa", "running"));
    assert!(s.agents_partial);
    assert_eq!(s.running_agents, 0);
    assert!(s.running_workflows[0].untracked);
    let s = f.push(untracked("wa", "completed"));
    assert!(!s.agents_partial);
    assert!(s.running_workflows.is_empty());

    // A list without it ends it.
    f.push(untracked("wb", "running"));
    assert!(f.summary.agents_partial);
    let s = f.push(list(Vec::new()));
    assert!(!s.agents_partial);
    assert!(s.running_workflows.is_empty());
}

#[test]
fn at_most_256_running_workflows_then_partial() {
    let mut f = Feed::new();
    f.push(started());
    fill(&mut f);
    assert!(!f.summary.workflows_overflow);
    assert!(!f.summary.agents_partial);
    let s = f.push(wf("w256", "running", 1, 1));
    assert_eq!(s.running_workflows.len(), MAX_RUNNING_WORKFLOWS);
    assert!(!ids(s).contains(&"w256"));
    assert!(s.workflows_overflow);
    assert!(s.agents_partial);
    // An entry that is already kept still updates.
    let s = f.push(wf("w0", "running", 9, 9));
    assert_eq!(s.running_workflows[0].running_agents, 9);
    // The end of the process clears both.
    f.push(Event::SessionEnded {
        reason: "exit".to_owned(),
        exit_code: None,
    });
    assert!(!f.summary.workflows_overflow);
    assert!(!f.summary.agents_partial);
}

#[test]
fn the_257th_workflow_keeps_partial_after_the_256_end() {
    let mut f = Feed::new();
    f.push(started());
    fill(&mut f);
    f.push(wf("w256", "running", 1, 1));
    for i in 0..MAX_RUNNING_WORKFLOWS {
        f.push(wf(&format!("w{i}"), "completed", 0, 1));
    }
    let s = &f.summary;
    assert!(s.running_workflows.is_empty());
    assert_eq!(s.running_agents, 0);
    assert!(s.workflows_overflow);
    assert!(s.agents_partial);
    // A list that still names the 257th keeps the flag.
    let s = f.push(list(vec![workflow_task("w256")]));
    assert!(s.workflows_overflow);
    assert!(s.agents_partial);
    assert_eq!(s.background_workflows, 1);
    // A list without it proves that every workflow has an entry.
    let s = f.push(list(Vec::new()));
    assert!(!s.workflows_overflow);
    assert!(!s.agents_partial);
}

#[test]
fn a_list_with_a_workflow_without_an_entry_keeps_the_overflow() {
    let mut f = Feed::new();
    f.push(started());
    fill(&mut f);
    f.push(wf("w256", "running", 1, 1));
    assert!(f.summary.workflows_overflow);
    // A listed workflow has no entry, and a shell task is listed too.
    let s = f.push(list(vec![
        workflow_task("w0"),
        workflow_task("w256"),
        task("sh", "local_bash"),
    ]));
    assert!(s.workflows_overflow);
    assert_eq!(ids(s), ["w0"]);
    // The list names only workflows that have an entry.
    let s = f.push(list(vec![workflow_task("w0"), task("sh", "local_bash")]));
    assert!(!s.workflows_overflow);
    assert!(!s.agents_partial);
}

#[test]
fn a_final_status_does_not_clear_the_overflow() {
    let mut f = Feed::new();
    f.push(started());
    fill(&mut f);
    f.push(wf("w256", "running", 1, 1));
    assert!(f.summary.workflows_overflow);
    for status in ["completed", "failed", "stopped"] {
        let s = f.push(wf("w0", status, 0, 1));
        assert!(s.workflows_overflow, "{status}");
        assert!(s.agents_partial, "{status}");
    }
    // The final event of the omitted workflow changes no flag either.
    let s = f.push(wf("w256", "completed", 0, 1));
    assert!(s.workflows_overflow);
}

#[test]
fn the_workflow_fields_are_not_written_when_empty_and_read_when_absent() {
    let mut f = Feed::new();
    f.push(started());
    let v = serde_json::to_value(&f.summary).unwrap();
    let object = v.as_object().unwrap();
    for key in [
        "background_workflows",
        "running_workflows",
        "workflows_overflow",
        "running_agents",
        "agents_partial",
    ] {
        assert!(!object.contains_key(key), "{key}");
    }
    let back: SessionSummary = serde_json::from_value(v).unwrap();
    assert_eq!(back, f.summary);
}
