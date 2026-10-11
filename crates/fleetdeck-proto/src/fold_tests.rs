use super::*;
use crate::capability::Capabilities;
use crate::event::{
    ApprovalOption, BackgroundTask, EventBody, ExpiredReason, ProviderKind, ProviderRefs,
    RequestKind, ResolvedBy, ResponseChannel, RunOrigin, TurnStatus,
};
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
        self.push_body(EventBody::Known(event))
    }

    fn push_body(&mut self, event: EventBody) -> &SessionSummary {
        let envelope = EventEnvelope {
            station_seq: self.seq + 100,
            session_id: SessionId::from("s1"),
            session_seq: self.seq,
            at: 1_000 + i64::try_from(self.seq).unwrap(),
            proto_version: 1,
            raw_ref: None,
            event,
        };
        self.summary.apply(&envelope);
        &self.summary
    }
}

fn started() -> Event {
    Event::SessionStarted {
        provider: ProviderKind::Claude,
        provider_instance: "claude".into(),
        provider_version: Some("2.1.0".to_owned()),
        model: Some("haiku".to_owned()),
        cwd: "/w".to_owned(),
        mode: Some("default".to_owned()),
        capabilities: Capabilities::minimal(),
        resumed: None,
        provider_refs: ProviderRefs::new(),
    }
}

fn run(id: &str) -> Event {
    Event::RunStarted {
        run_id: id.into(),
        input_summary: "hi".to_owned(),
        origin: RunOrigin::Board,
    }
}

fn request(id: &str) -> Event {
    Event::ApprovalRequested {
        request_id: id.into(),
        node_id: None,
        request_kind: RequestKind::Command,
        title: "Bash".to_owned(),
        details: "ls".to_owned(),
        input: None,
        options: vec![ApprovalOption {
            option_id: "allow".to_owned(),
            label: "Allow".to_owned(),
        }],
        response: ResponseChannel::RpcReply,
        provider_refs: ProviderRefs::new(),
    }
}

#[test]
fn a_session_moves_through_the_lanes() {
    let mut f = Feed::new();
    let s = f.push(started());
    assert_eq!(
        (s.state, s.activity),
        (SessionState::Running, Activity::Idle)
    );
    assert_eq!(s.model.as_deref(), Some("haiku"));
    assert_eq!(f.push(run("run-1")).activity, Activity::Working);
    f.push(Event::TurnStarted {
        run_id: "run-1".into(),
        turn_id: "turn-1".into(),
        provider_refs: ProviderRefs::new(),
    });
    let s = f.push(request("req-1"));
    assert_eq!(s.activity, Activity::NeedsYou);
    assert_eq!(s.open_requests[0].requested_at, s.updated_at);
    let s = f.push(Event::ApprovalResolved {
        request_id: "req-1".into(),
        option_id: "allow".to_owned(),
        by: ResolvedBy::Board,
    });
    assert_eq!(s.activity, Activity::Working);
    f.push(Event::BackgroundUpdated {
        tasks: vec![BackgroundTask {
            task_id: "b1".to_owned(),
            task_kind: "local_bash".to_owned(),
        }],
    });
    f.push(Event::TurnCompleted {
        turn_id: "turn-1".into(),
        status: TurnStatus::Done,
        usage: None,
    });
    let s = f.push(Event::RunCompleted {
        run_id: "run-1".into(),
        status: TurnStatus::Done,
        background_pending: true,
    });
    // L7: background work after the run keeps the session busy.
    assert_eq!(s.activity, Activity::Background);
    assert_eq!(s.current_turn, None);
    assert_eq!(
        f.push(Event::BackgroundUpdated { tasks: vec![] }).activity,
        Activity::Idle
    );
    let s = f.push(Event::SessionEnded {
        reason: "process_exit".to_owned(),
        exit_code: Some(0),
    });
    assert_eq!(
        (s.state, s.activity),
        (SessionState::Exited, Activity::Ended)
    );
    assert_eq!(s.session_seq, 10);
}

#[test]
fn a_child_completion_does_not_close_the_parent_run() {
    let mut f = Feed::new();
    f.push(started());
    f.push(run("run-1"));
    let s = f.push(Event::RunCompleted {
        run_id: "run-other".into(),
        status: TurnStatus::Done,
        background_pending: false,
    });
    assert_eq!(s.current_run.as_ref().map(|r| r.as_str()), Some("run-1"));
    assert_eq!(s.activity, Activity::Working);
}

#[test]
fn usage_sums_known_parts_and_keeps_unknown_parts_unknown() {
    let mut f = Feed::new();
    f.push(started());
    let usage = |i, c| Event::UsageUpdated {
        usage: Usage {
            input_tokens: i,
            output_tokens: None,
            cost_usd: c,
        },
        rate_limits: vec![],
    };
    f.push(usage(Some(10), None));
    let s = f.push(usage(Some(5), Some(0.25)));
    assert_eq!(s.usage.input_tokens, Some(15));
    assert_eq!(s.usage.output_tokens, None);
    assert_eq!(s.usage.cost_usd, Some(0.25));
    // turn.completed repeats the usage: not counted twice.
    let s = f.push(Event::TurnCompleted {
        turn_id: "t".into(),
        status: TurnStatus::Done,
        usage: Some(Usage {
            input_tokens: Some(99),
            output_tokens: Some(99),
            cost_usd: Some(9.0),
        }),
    });
    assert_eq!(s.usage.input_tokens, Some(15));
}

#[test]
fn expiry_closes_a_request_and_errors_are_kept() {
    let mut f = Feed::new();
    f.push(started());
    f.push(request("req-1"));
    let s = f.push(Event::ApprovalExpired {
        request_id: "req-1".into(),
        reason: ExpiredReason::ProcessLost,
    });
    assert!(s.open_requests.is_empty());
    let s = f.push(Event::Error {
        code: "usage_limit".to_owned(),
        message: "limit".to_owned(),
        retry_at: Some(5),
    });
    assert_eq!(s.last_error.as_ref().map(|e| e.retry_at), Some(Some(5)));
}

#[test]
fn a_replayed_event_and_an_unknown_kind_change_nothing_else() {
    let mut f = Feed::new();
    f.push(started());
    let before = f.summary.clone();
    // The same session_seq again.
    f.summary.apply(&EventEnvelope {
        station_seq: 1,
        session_id: "s1".into(),
        session_seq: 1,
        at: 5,
        proto_version: 1,
        raw_ref: None,
        event: EventBody::Known(Event::ModeChanged {
            mode: "plan".to_owned(),
        }),
    });
    assert_eq!(f.summary, before);
    f.seq += 1;
    let s = f.push_body(EventBody::Unknown {
        kind: "future.kind".to_owned(),
        payload: serde_json::Map::new(),
    });
    assert_eq!(s.session_seq, 2);
    assert_eq!(s.mode.as_deref(), Some("default"));
}

fn tool(id: &str, kind: crate::event::ToolKind, command: &str) -> Event {
    Event::ToolStarted {
        node_id: id.into(),
        parent_node_id: None,
        tool_kind: kind,
        title: "Bash".to_owned(),
        input_summary: command.to_owned(),
        files: Vec::new(),
        provider_refs: ProviderRefs::new(),
    }
}

fn tool_done(id: &str) -> Event {
    Event::ToolCompleted {
        node_id: id.into(),
        status: crate::event::ToolStatus::Ok,
        files: Vec::new(),
        exit_code: Some(0),
    }
}

#[test]
fn running_tools_open_at_start_close_at_completion_and_end_with_their_run() {
    use crate::event::ToolKind;
    let mut f = Feed::new();
    f.push(started());
    f.push(run("run-1"));
    f.push(tool("t1", ToolKind::Shell, "cargo test"));
    let s = f.push(tool("t2", ToolKind::Read, "a.rs"));
    let ids: Vec<&str> = s.running_tools.iter().map(|t| t.node_id.as_str()).collect();
    assert_eq!(ids, ["t1", "t2"]);
    assert_eq!(s.running_tools[0].input_summary, "cargo test");
    let s = f.push(tool_done("t1"));
    assert_eq!(s.running_tools.len(), 1);
    // A child run's completion keeps the tools of the parent run.
    let s = f.push(Event::RunCompleted {
        run_id: "run-other".into(),
        status: TurnStatus::Done,
        background_pending: false,
    });
    assert_eq!(s.running_tools.len(), 1);
    let s = f.push(Event::RunCompleted {
        run_id: "run-1".into(),
        status: TurnStatus::Done,
        background_pending: false,
    });
    assert!(s.running_tools.is_empty(), "no tool outlives its run");

    // At most MAX_RUNNING_TOOLS; the oldest goes first.
    f.push(run("run-2"));
    for i in 0..=MAX_RUNNING_TOOLS {
        f.push(tool(&format!("x{i}"), ToolKind::Shell, "sleep 1"));
    }
    let s = &f.summary;
    assert_eq!(s.running_tools.len(), MAX_RUNNING_TOOLS);
    assert_eq!(s.running_tools[0].node_id.as_str(), "x1");
}

#[test]
fn a_summary_stored_without_running_tools_still_loads() {
    let mut v = serde_json::to_value(SessionSummary::new(SessionId::from("s1"))).unwrap();
    v.as_object_mut().unwrap().remove("running_tools");
    let s: SessionSummary = serde_json::from_value(v).unwrap();
    assert!(s.running_tools.is_empty());
}

#[test]
fn background_subagents_are_counted_apart_from_other_background_work() {
    let mut f = Feed::new();
    f.push(started());
    let task = |id: &str, kind: &str| BackgroundTask {
        task_id: id.to_owned(),
        task_kind: kind.to_owned(),
    };
    let s = f.push(Event::BackgroundUpdated {
        tasks: vec![
            task("b1", "local_bash"),
            task("a1", "local_agent"),
            task("a2", "remote_agent"),
        ],
    });
    assert_eq!((s.background_tasks.len(), s.background_agents), (3, 2));
    let s = f.push(Event::BackgroundUpdated {
        tasks: vec![task("b1", "local_bash")],
    });
    assert_eq!(s.background_agents, 0);
    f.push(Event::BackgroundUpdated {
        tasks: vec![task("a1", "local_agent")],
    });
    let s = f.push(Event::SessionEnded {
        reason: "process_exit".to_owned(),
        exit_code: Some(0),
    });
    assert_eq!((s.background_tasks.len(), s.background_agents), (0, 0));
}
