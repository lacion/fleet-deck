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
        capabilities: Box::new(Capabilities::minimal()),
        resumed: None,
        provider_refs: ProviderRefs::new(),
        title: None,
        branch: None,
        repo: None,
    }
}

fn run(id: &str) -> Event {
    Event::RunStarted {
        author: None,
        input_id: None,
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
            kind: None,
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
        answered_by: None,
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
            cost_inferred: false,
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
            cost_inferred: false,
        }),
    });
    assert_eq!(s.usage.input_tokens, Some(15));
}

#[test]
fn rate_limit_windows_merge_by_scope_and_a_sparse_update_keeps_the_others() {
    let mut f = Feed::new();
    f.push(started());
    let limits = |l: &[(&str, Option<f64>)]| Event::UsageUpdated {
        usage: Usage::default(),
        rate_limits: l
            .iter()
            .map(|(scope, used)| RateLimit {
                scope: (*scope).to_owned(),
                used_percent: *used,
                resets_at: Some(1_000),
            })
            .collect(),
    };
    assert!(f.summary.rate_limits.is_empty(), "no window: unknown");
    f.push(limits(&[("weekly", Some(10.0)), ("five_hour", Some(40.0))]));
    let s = f.push(limits(&[("five_hour", None)]));
    let got: Vec<_> = s
        .rate_limits
        .iter()
        .map(|r| (r.scope.as_str(), r.used_percent))
        .collect();
    assert_eq!(got, [("five_hour", None), ("weekly", Some(10.0))]);
    // A rate-limit update leaves the token usage unknown, not zero.
    assert_eq!(s.usage, Usage::default());
    let many: Vec<_> = (0..40).map(|i| (format!("s{i:02}"), Some(1.0))).collect();
    let many: Vec<_> = many.iter().map(|(s, u)| (s.as_str(), *u)).collect();
    let s = f.push(limits(&many));
    assert_eq!(s.rate_limits.len(), MAX_RATE_LIMITS);
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
fn a_start_or_a_new_run_clears_the_error_of_an_earlier_attempt() {
    let error = || Event::Error {
        code: "not_dispatched".to_owned(),
        message: "resume was not sent: an old reason".to_owned(),
        retry_at: None,
    };
    let mut f = Feed::new();
    f.push(started());
    assert!(f.push(error()).last_error.is_some());
    // A resume that worked starts the session again.
    assert!(f.push(started()).last_error.is_none());
    assert!(f.push(error()).last_error.is_some());
    // A new run means the session takes input again.
    assert!(f.push(run("run-9")).last_error.is_none());
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

fn agent_task(id: &str) -> BackgroundTask {
    BackgroundTask {
        task_id: id.to_owned(),
        task_kind: "local_agent".to_owned(),
    }
}

#[test]
fn a_lost_session_runs_nothing_but_keeps_its_requests_and_error() {
    for state in [SessionState::Lost, SessionState::Exited] {
        let mut f = Feed::new();
        f.push(started());
        f.push(run("r1"));
        f.push(Event::TurnStarted {
            run_id: "r1".into(),
            turn_id: "t1".into(),
            provider_refs: ProviderRefs::new(),
        });
        f.push(tool("n1", crate::event::ToolKind::Subagent, "child"));
        f.push(Event::BackgroundUpdated {
            tasks: vec![agent_task("a1")],
        });
        f.push(request("q1"));
        f.push(Event::Error {
            code: "boom".to_owned(),
            message: "the provider failed".to_owned(),
            retry_at: None,
        });
        // The station recovery order: run and turn fail, then the loss.
        f.push(Event::TurnCompleted {
            turn_id: "t1".into(),
            status: TurnStatus::Failed,
            usage: None,
        });
        f.push(Event::RunCompleted {
            run_id: "r1".into(),
            status: TurnStatus::Failed,
            background_pending: false,
        });
        let s = f.push(Event::SessionStateChanged {
            state,
            detail: Some("station restart: the provider process is gone".to_owned()),
        });
        assert_eq!(s.state, state);
        assert_eq!(
            (s.current_run.as_ref(), s.current_turn.as_ref()),
            (None, None)
        );
        assert!(s.running_tools.is_empty());
        assert!(s.background_tasks.is_empty());
        assert_eq!(s.background_agents, 0, "a lost card counts no subagents");
        assert_eq!(s.activity, Activity::Ended);
        assert_eq!(
            s.open_requests.len(),
            1,
            "requests close by their own events"
        );
        assert_eq!(s.last_error.as_ref().map(|e| e.code.as_str()), Some("boom"));
        // The empty list that follows the loss changes nothing.
        let s = f.push(Event::BackgroundUpdated { tasks: Vec::new() });
        assert_eq!((s.background_agents, s.state), (0, state));
    }
}

#[test]
fn a_loss_with_only_background_work_clears_it() {
    let mut f = Feed::new();
    f.push(started());
    let s = f.push(Event::BackgroundUpdated {
        tasks: vec![agent_task("a1"), agent_task("a2")],
    });
    assert_eq!((s.background_agents, s.activity), (2, Activity::Background));
    let s = f.push(Event::SessionStateChanged {
        state: SessionState::Lost,
        detail: None,
    });
    assert_eq!((s.background_tasks.len(), s.background_agents), (0, 0));
}

#[test]
fn a_normal_run_completion_keeps_background_work() {
    let mut f = Feed::new();
    f.push(started());
    f.push(run("r1"));
    f.push(tool("n1", crate::event::ToolKind::Subagent, "child"));
    f.push(Event::BackgroundUpdated {
        tasks: vec![agent_task("a1")],
    });
    f.push(tool_done("n1"));
    let s = f.push(Event::RunCompleted {
        run_id: "r1".into(),
        status: TurnStatus::Done,
        background_pending: true,
    });
    assert_eq!(s.current_run, None);
    assert_eq!((s.background_tasks.len(), s.background_agents), (1, 1));
    assert_eq!(s.activity, Activity::Background);
    // A running state change keeps it too; a later run still counts it.
    f.push(Event::SessionStateChanged {
        state: SessionState::Running,
        detail: None,
    });
    let s = f.push(run("r2"));
    assert_eq!(s.background_agents, 1);
    let s = f.push(Event::RunCompleted {
        run_id: "r2".into(),
        status: TurnStatus::Done,
        background_pending: true,
    });
    assert_eq!(s.background_agents, 1);
}

/// L13: `turn.finalized` comes after `turn.completed` and never brings the
/// turn back or keeps the card working.
#[test]
fn a_late_turn_finalized_changes_neither_the_turn_nor_the_activity() {
    use crate::event::{CheckpointSide, TurnCheckpoint};
    let mut f = Feed::new();
    f.push(started());
    f.push(run("run-1"));
    f.push(Event::TurnStarted {
        run_id: "run-1".into(),
        turn_id: "turn-1".into(),
        provider_refs: ProviderRefs::new(),
    });
    f.push(Event::TurnCompleted {
        turn_id: "turn-1".into(),
        status: TurnStatus::Done,
        usage: None,
    });
    let before = f
        .push(Event::RunCompleted {
            run_id: "run-1".into(),
            status: TurnStatus::Done,
            background_pending: false,
        })
        .clone();
    let after = f
        .push(Event::TurnFinalized {
            turn_id: "turn-1".into(),
            checkpoint: Some(TurnCheckpoint {
                worktree_id: None,
                scope: "tracked".into(),
                start: CheckpointSide::Unavailable {
                    reason: "not_a_git_worktree".into(),
                },
                end: CheckpointSide::Unavailable {
                    reason: "not_a_git_worktree".into(),
                },
            }),
            diff_summary: None,
        })
        .clone();
    assert_eq!(after.activity, Activity::Idle);
    assert_eq!(after.current_turn, None);
    assert_eq!(after.current_run, None);
    assert_eq!(after.usage, before.usage);
    assert_eq!(after.open_requests, before.open_requests);
}

#[test]
fn an_issue_event_sets_the_issue_link() {
    let mut f = Feed::new();
    f.push(started());
    let issue = crate::integrations::IssueRef {
        system: "linear".into(),
        key: "ENG-42".into(),
        url: "https://linear.app/acme/issue/ENG-42".into(),
        title: "Slow start".into(),
    };
    let s = f.push(Event::SessionIssue {
        issue: issue.clone(),
    });
    assert_eq!(s.issue, Some(issue));
    assert_eq!(s.activity, Activity::Idle);
}

#[test]
fn a_rename_sets_and_clears_the_title_and_an_old_reader_skips_it() {
    let mut f = Feed::new();
    f.push(started());
    let s = f.push(Event::SessionRenamed {
        title: Some("docs".into()),
    });
    assert_eq!(s.title.as_deref(), Some("docs"));
    assert_eq!(s.activity, Activity::Idle);
    assert_eq!(f.push(Event::SessionRenamed { title: None }).title, None);
    // A reader of an older build gets the kind as unknown (N-1): it keeps
    // the summary as it was.
    let mut payload = serde_json::Map::new();
    payload.insert("title".into(), serde_json::json!("x"));
    let unknown = EventBody::Unknown {
        kind: "session.renamed".into(),
        payload,
    };
    let before = f.summary.clone();
    let after = f.push_body(unknown);
    assert_eq!(after.title, before.title);
}

/// v0 parity (PLAN §16 "Model badge follows `/model`"): the summary that the
/// card badge shows takes the model of the latest model change.
#[test]
fn a_model_change_moves_the_model_badge_to_the_new_model() {
    let mut f = Feed::new();
    assert_eq!(f.push(started()).model.as_deref(), Some("haiku"));
    let s = f.push(Event::ModelChanged {
        model: "opus".to_owned(),
    });
    assert_eq!(s.model.as_deref(), Some("opus"));
    assert_eq!(f.push(run("run-1")).model.as_deref(), Some("opus"));
}

#[test]
fn a_session_notice_does_not_change_the_summary() {
    let mut f = Feed::new();
    f.push(started());
    let mut expected = f.summary.clone();
    let s = f.push(Event::SessionNotice {
        code: "v1_plugin".to_owned(),
        text: "The Fleet Deck v1 plugin is active in this session.".to_owned(),
    });
    // Only the sequence and the time move; all other fields stay.
    expected.session_seq = s.session_seq;
    expected.updated_at = s.updated_at;
    assert_eq!(*s, expected);
}

#[test]
fn session_models_set_the_models_of_the_summary_and_the_last_list_wins() {
    let mut f = Feed::new();
    f.push(started());
    assert_eq!(f.summary.models, None);
    let choice = |value: &str| crate::event::ModelChoice {
        value: value.to_owned(),
        label: value.to_uppercase(),
        description: None,
    };
    f.push(Event::SessionModels {
        models: vec![choice("opus")],
    });
    let s = f.push(Event::SessionModels {
        models: vec![choice("sonnet"), choice("haiku")],
    });
    assert_eq!(s.models, Some(vec![choice("sonnet"), choice("haiku")]));
}

#[test]
fn slash_commands_set_the_commands_and_the_skills_of_the_summary_and_the_last_list_wins() {
    let mut f = Feed::new();
    f.push(started());
    assert_eq!(f.summary.slash_commands, None);
    assert_eq!(f.summary.skills, None);
    let names = |a: &[&str]| a.iter().map(|n| (*n).to_owned()).collect::<Vec<_>>();
    f.push(Event::SlashCommands {
        commands: names(&["compact"]),
        skills: Vec::new(),
    });
    let s = f.push(Event::SlashCommands {
        commands: names(&["compact", "review", "deploy"]),
        skills: names(&["deploy"]),
    });
    assert_eq!(
        s.slash_commands,
        Some(names(&["compact", "review", "deploy"]))
    );
    assert_eq!(s.skills, Some(names(&["deploy"])));
}

#[path = "fold_tests_more.rs"]
mod more;
