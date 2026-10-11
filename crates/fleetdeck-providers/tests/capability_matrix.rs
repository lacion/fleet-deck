//! The capability matrix of PLAN §6.2 for both providers (M2 P5-3).
//!
//! Each test is one matrix row. It checks the value that each provider's
//! capabilities report, and then the behaviour that the value promises in
//! the recorded fixtures of that provider (replayed through the production
//! sans-IO cores). A capability that a session reports and a behaviour that
//! the recordings show can therefore not drift apart.

#![allow(
    clippy::unwrap_used,
    reason = "a test harness: a panic is the failure report"
)]

#[path = "capability_matrix/replay.rs"]
mod replay;

use fleetdeck_proto::Event;
use fleetdeck_proto::capability::{
    BackgroundReporting, Capabilities, Degradation, Operation, TerminalStatusQuality,
    UsageReporting,
};
use fleetdeck_proto::event::{ExpiredReason, RequestKind, ResponseChannel, ToolKind, TurnStatus};
use fleetdeck_providers::{ProviderError, claude, codex};
use replay::{
    P, PROVIDERS, caps, codex_core, count, fixtures, messages, native_session, position, records,
    replay,
};
// ----- one test per row of PLAN §6.2 ------------------------------------------

#[test]
fn row_resume_reopens_the_same_native_session_with_its_memory() {
    for p in PROVIDERS {
        assert!(caps(p).resume, "{p:?}");
        let r = replay(p, "provider_thread_resume");
        assert_eq!(r.processes.len(), 2, "{p:?}");
        assert_eq!(
            native_session(&r.processes[0]),
            native_session(&r.processes[1]),
            "{p:?}"
        );
        // The second process remembers a word that only the first one heard.
        let first = messages(&r.processes[0]);
        let second = messages(&r.processes[1]);
        assert_eq!(first.len(), 1, "{p:?}");
        assert_eq!(second.len(), 1, "{p:?}");
        assert!(
            ["walnut", "maple"].contains(&second[0].as_str()),
            "{p:?}: {second:?}"
        );
    }
    // A resume that the host asked for is visible on the new session (L5).
    let claude = replay(P::Claude, "provider_thread_resume");
    assert!(matches!(
        claude.processes[1][0],
        Event::SessionStarted {
            resumed: Some(true),
            ..
        }
    ));
}

#[test]
fn row_fork_opens_a_new_native_session_with_the_parent_history() {
    for (p, scenario) in [(P::Claude, "fork_session"), (P::Codex, "fork_thread")] {
        assert!(caps(p).fork, "{p:?}");
        assert!(caps(p).supports(Operation::Fork), "{p:?}");
        let r = replay(p, scenario);
        assert_eq!(r.processes.len(), 2, "{p:?}");
        assert_ne!(
            native_session(&r.processes[0]),
            native_session(&r.processes[1]),
            "{p:?}"
        );
        assert_eq!(messages(&r.processes[1]), vec!["maple".to_owned()], "{p:?}");
    }
}

#[test]
fn row_open_requests_expire_on_restart_and_are_asked_again() {
    for p in PROVIDERS {
        let c = caps(p);
        assert!(!c.requests_survive_restart, "{p:?}");
        let r = replay(p, "approval_restart");
        assert_eq!(r.processes.len(), 2, "{p:?}");
        let killed: Vec<&Event> = r.processes[0].iter().collect();
        let expired = position(&killed, |e| {
            matches!(
                e,
                Event::ApprovalExpired {
                    reason: ExpiredReason::ProcessLost,
                    ..
                }
            )
        });
        assert!(
            expired.is_some(),
            "{p:?}: the open request expires with the process"
        );
        assert_eq!(count(&killed, "approval.resolved"), 0, "{p:?}");
        // The new process asks again with a new request that the board answers.
        let next: Vec<&Event> = r.processes[1].iter().collect();
        assert_eq!(count(&next, "approval.requested"), 1, "{p:?}");
        assert_eq!(count(&next, "approval.resolved"), 1, "{p:?}");
    }
}

#[test]
fn row_interrupt_ends_the_turn_as_interrupted_and_the_session_continues() {
    for p in PROVIDERS {
        assert!(caps(p).interrupt, "{p:?}");
        let r = replay(p, "turn_interrupt");
        let all = r.all();
        let accepted = position(&all, |e| {
            matches!(e, Event::InterruptResult { accepted: true, .. })
        });
        let interrupted = position(&all, |e| {
            matches!(
                e,
                Event::TurnCompleted {
                    status: TurnStatus::Interrupted,
                    ..
                }
            )
        });
        assert!(accepted.unwrap() < interrupted.unwrap(), "{p:?}");
        let done = all
            .iter()
            .filter(|e| {
                matches!(
                    e,
                    Event::TurnCompleted {
                        status: TurnStatus::Done,
                        ..
                    }
                )
            })
            .count();
        assert_eq!(done, 1, "{p:?}: the next prompt runs after the interrupt");
    }
    assert_eq!(Operation::Interrupt.degradation(), Degradation::KillProcess);
}

/// The input that starts in the turn of an earlier input.
fn inputs_started(events: &[&Event]) -> Vec<(String, Option<String>)> {
    events
        .iter()
        .filter_map(|e| match e {
            Event::InputUpdated {
                input_id,
                turn_id,
                state: fleetdeck_proto::event::InputState::Started,
            } => Some((
                input_id.to_string(),
                turn_id.as_ref().map(ToString::to_string),
            )),
            _ => None,
        })
        .collect()
}

#[test]
fn row_steer_joins_the_running_turn() {
    for p in PROVIDERS {
        assert!(caps(p).steer, "{p:?}");
        let r = replay(p, "message_steering");
        let all = r.all();
        assert_eq!(
            count(&all, "turn.started"),
            1,
            "{p:?}: a steer starts no new turn"
        );
        let started = inputs_started(&all);
        assert_eq!(started.len(), 2, "{p:?}: {started:?}");
        assert_eq!(
            started[0].1, started[1].1,
            "{p:?}: both inputs are in one turn"
        );
    }
    assert_eq!(
        Operation::Steer.degradation(),
        Degradation::InterruptAndRestart
    );
}

#[test]
fn row_follow_up_queue_is_native_for_claude_and_a_station_queue_for_codex() {
    assert!(caps(P::Claude).native_follow_up);
    assert!(!caps(P::Codex).native_follow_up);
    assert_eq!(Operation::FollowUp.degradation(), Degradation::StationQueue);
    for p in PROVIDERS {
        // Both providers accept a follow-up: Codex through the host queue.
        assert!(caps(p).supports(Operation::FollowUp), "{p:?}");
        let r = replay(p, "queued_turn");
        let all = r.all();
        assert_eq!(
            count(&all, "run.started"),
            2,
            "{p:?}: one run per queued input"
        );
        let first_done = position(&all, |e| matches!(e, Event::TurnCompleted { .. })).unwrap();
        // The provider hears of the second input before the first turn ends
        // only with a native queue.
        let second_received = all
            .iter()
            .enumerate()
            .filter(|(_, e)| {
                matches!(
                    e,
                    Event::InputUpdated {
                        state: fleetdeck_proto::event::InputState::Received,
                        ..
                    }
                )
            })
            .map(|(i, _)| i)
            .nth(1)
            .unwrap();
        assert_eq!(
            second_received < first_done,
            caps(p).native_follow_up,
            "{p:?}"
        );
        let done = all
            .iter()
            .filter(|e| {
                matches!(
                    e,
                    Event::TurnCompleted {
                        status: TurnStatus::Done,
                        ..
                    }
                )
            })
            .count();
        assert_eq!(done, 2, "{p:?}");
    }
}

#[test]
fn row_terminal_status_is_strong_every_turn_and_run_ends_once() {
    for p in PROVIDERS {
        assert_eq!(
            caps(p).terminal_status,
            TerminalStatusQuality::Strong,
            "{p:?}"
        );
        for scenario in fixtures(p) {
            for (i, events) in replay(p, scenario).processes.iter().enumerate() {
                let all: Vec<&Event> = events.iter().collect();
                let at = format!("{p:?} {scenario} process {i}");
                assert_eq!(
                    count(&all, "turn.started"),
                    count(&all, "turn.completed"),
                    "{at}"
                );
                assert_eq!(
                    count(&all, "run.started"),
                    count(&all, "run.completed"),
                    "{at}"
                );
                assert_eq!(count(&all, "session.ended"), 1, "{at}");
                assert!(all.last().unwrap().is_terminal_for_session(), "{at}");
            }
        }
    }
}

#[test]
fn row_live_deltas_stream_before_the_completed_message() {
    for p in PROVIDERS {
        assert!(caps(p).live_deltas, "{p:?}");
        let r = replay(p, "simple");
        let all = r.all();
        let delta = position(&all, |e| matches!(e, Event::MessageDelta { .. })).unwrap();
        let done = position(&all, |e| matches!(e, Event::MessageCompleted { .. })).unwrap();
        assert!(delta < done, "{p:?}");
    }
}

#[test]
fn row_file_changes_name_the_file_and_the_edit_tool() {
    for (p, scenario) in [(P::Claude, "file_edit"), (P::Codex, "file_change")] {
        assert!(caps(p).file_changes, "{p:?}");
        let r = replay(p, scenario);
        let all = r.all();
        let edit = all
            .iter()
            .find_map(|e| match e {
                Event::ToolStarted {
                    node_id,
                    tool_kind: ToolKind::Edit,
                    ..
                } => Some(node_id.clone()),
                _ => None,
            })
            .unwrap();
        let changed = all
            .iter()
            .find_map(|e| match e {
                Event::FileChanged { path, node_id, .. } => Some((path.clone(), node_id.clone())),
                _ => None,
            })
            .unwrap();
        assert!(changed.0.ends_with("fd-edit.txt"), "{p:?}");
        assert_eq!(changed.1, Some(edit), "{p:?}");
    }
}

#[test]
fn row_approvals_are_answerable_with_the_providers_own_options() {
    for p in PROVIDERS {
        let c = caps(p);
        assert!(c.approvals && c.supports(Operation::Answer), "{p:?}");
        for scenario in ["tool_call_approve", "tool_call_deny"] {
            let r = replay(p, scenario);
            let all = r.all();
            let (options, response) = all
                .iter()
                .find_map(|e| match e {
                    Event::ApprovalRequested {
                        options,
                        response,
                        request_kind: RequestKind::Command,
                        ..
                    } => Some((options.clone(), *response)),
                    _ => None,
                })
                .unwrap();
            assert!(options.len() >= 2, "{p:?} {scenario}");
            let resolved = all
                .iter()
                .find_map(|e| match e {
                    Event::ApprovalResolved { option_id, .. } => Some(option_id.clone()),
                    _ => None,
                })
                .unwrap();
            assert!(
                options.iter().any(|o| o.option_id == resolved),
                "{p:?} {scenario}"
            );
            assert!(c.response_channels.contains(&response), "{p:?} {scenario}");
        }
    }
    assert_eq!(
        Operation::Answer.degradation(),
        Degradation::NoApprovalControls
    );
}

#[test]
fn row_response_channels_cover_every_recorded_request() {
    assert_eq!(
        caps(P::Claude).response_channels,
        vec![ResponseChannel::RpcReply]
    );
    assert_eq!(
        caps(P::Codex).response_channels,
        vec![ResponseChannel::RpcReply, ResponseChannel::Message]
    );
    for p in PROVIDERS {
        let c = caps(p);
        for scenario in fixtures(p) {
            for e in replay(p, scenario).all() {
                if let Event::ApprovalRequested { response, .. } = e {
                    assert!(c.response_channels.contains(response), "{p:?} {scenario}");
                }
            }
        }
    }
}

#[test]
fn row_edit_input_reaches_claude_and_is_refused_for_codex() {
    let claude = caps(P::Claude);
    assert!(claude.edit_input && claude.supports(Operation::EditInput));
    // The edited input is the input that Claude runs.
    let r = replay(P::Claude, "tool_call_edit_input");
    let edited = r
        .writes
        .iter()
        .find(|w| w.to_string().contains("updatedInput"))
        .unwrap();
    assert!(edited.to_string().contains("fd-edited.txt"), "{edited}");
    assert_eq!(messages(&r.processes[0]), vec!["edited-ok".to_owned()]);

    let codex = caps(P::Codex);
    assert!(!codex.edit_input && !codex.supports(Operation::EditInput));
    assert_eq!(
        Operation::EditInput.degradation(),
        Degradation::AllowDenyOnly
    );
    // An allow with an edited input is refused before any write; the plain
    // allow of the same request still works afterwards.
    let mut core: Option<codex::CodexCore> = None;
    let mut refused = false;
    for rec in records(P::Codex, "tool_call_approve") {
        match rec["dir"].as_str().unwrap() {
            "spawn" => core = Some(codex_core()),
            "host" => {
                let cmd: codex::HostCmd = serde_json::from_value(rec["cmd"].clone()).unwrap();
                if let codex::HostCmd::Answer {
                    request_id,
                    option_id,
                    ..
                } = &cmd
                {
                    let edited = codex::HostCmd::Answer {
                        request_id: request_id.clone(),
                        option_id: option_id.clone(),
                        updated_input: Some(serde_json::json!({"command": "echo edited"})),
                        message: None,
                    };
                    let err = core.as_mut().unwrap().apply(&edited).unwrap_err();
                    assert!(
                        matches!(err, ProviderError::Unsupported(u) if u.op == Operation::EditInput),
                        "{err:?}"
                    );
                    refused = true;
                }
                core.as_mut().unwrap().apply(&cmd).unwrap();
            }
            "out" => {
                core.as_mut()
                    .unwrap()
                    .on_frame(codex::rpc::parse_value(rec["frame"].clone()));
            }
            _ => {}
        }
    }
    assert!(refused);
}

#[test]
fn row_permission_modes_are_native_names_and_mode_changes_report_them() {
    assert_eq!(
        caps(P::Claude).modes,
        vec!["default", "plan", "acceptEdits", "dontAsk", "auto"]
    );
    assert!(
        claude::capabilities(true)
            .modes
            .contains(&claude::BYPASS_MODE.to_owned())
    );
    assert!(
        !caps(P::Codex)
            .modes
            .iter()
            .any(|m| m.ends_with(codex::FULL_ACCESS_SANDBOX))
    );
    assert!(
        codex::capabilities(true)
            .modes
            .iter()
            .any(|m| m.ends_with(codex::FULL_ACCESS_SANDBOX))
    );
    assert_eq!(
        caps(P::Codex).modes[0],
        codex::SAFEST_MODE,
        "most restrictive first"
    );
    assert!(
        caps(P::Claude)
            .modes
            .contains(&claude::SAFEST_MODE.to_owned())
    );
    assert_eq!(
        Operation::SetMode.degradation(),
        Degradation::MostRestrictiveMode
    );
    for (p, scenario) in [(P::Claude, "set_permission_mode"), (P::Codex, "set_mode")] {
        let c = caps(p);
        assert!(c.set_mode && c.supports(Operation::SetMode), "{p:?}");
        let r = replay(p, scenario);
        let mut seen = 0;
        for e in r.all() {
            match e {
                Event::SessionStarted {
                    mode: Some(mode), ..
                }
                | Event::ModeChanged { mode } => {
                    assert!(c.modes.contains(mode), "{p:?}: {mode}");
                    seen += 1;
                }
                _ => {}
            }
        }
        assert_eq!(seen, 2, "{p:?}: the start mode and one change");
    }
}

#[test]
fn row_plan_capture_proposes_a_plan_in_the_turn() {
    for p in PROVIDERS {
        assert!(caps(p).plan_capture, "{p:?}");
        let r = replay(p, "plan_capture");
        let plan = r
            .all()
            .into_iter()
            .find_map(|e| match e {
                Event::PlanProposed {
                    turn_id, markdown, ..
                } => Some((turn_id.clone(), markdown.clone())),
                _ => None,
            })
            .unwrap();
        assert!(plan.0.is_some(), "{p:?}");
        assert!(plan.1.contains("hello"), "{p:?}: {}", plan.1);
    }
}

#[test]
fn row_subagents_are_subagent_tool_nodes() {
    for p in PROVIDERS {
        assert!(caps(p).subagents, "{p:?}");
        let r = replay(p, "subagent");
        let all = r.all();
        let node = all
            .iter()
            .find_map(|e| match e {
                Event::ToolStarted {
                    node_id,
                    tool_kind: ToolKind::Subagent,
                    ..
                } => Some(node_id.clone()),
                _ => None,
            })
            .unwrap();
        assert!(
            all.iter()
                .any(|e| matches!(e, Event::ToolCompleted { node_id, .. } if *node_id == node)),
            "{p:?}"
        );
    }
    // Codex reports the child's own message under the subagent node.
    let codex = replay(P::Codex, "subagent");
    assert!(codex.all().iter().any(|e| matches!(
        e,
        Event::MessageCompleted { parent_node_id: Some(_), text, .. } if text == "sub-ok"
    )));
    // Claude runs the subagent as background work of the session.
    let claude = replay(P::Claude, "subagent");
    assert!(claude.all().iter().any(|e| matches!(
        e,
        Event::BackgroundUpdated { tasks } if tasks.iter().any(|t| t.task_kind == "local_agent")
    )));
}

#[test]
fn row_background_work_keeps_the_run_pending_until_it_ends() {
    assert_eq!(caps(P::Claude).background_work, BackgroundReporting::Pushed);
    assert_eq!(caps(P::Codex).background_work, BackgroundReporting::Polled);
    for (p, scenario) in [
        (P::Claude, "background_task"),
        (P::Codex, "background_terminal"),
    ] {
        let r = replay(p, scenario);
        let all = r.all();
        let pending = position(&all, |e| {
            matches!(
                e,
                Event::RunCompleted {
                    background_pending: true,
                    ..
                }
            )
        })
        .unwrap();
        let listed = position(
            &all,
            |e| matches!(e, Event::BackgroundUpdated { tasks } if !tasks.is_empty()),
        )
        .unwrap();
        let cleared = all
            .iter()
            .rposition(|e| matches!(e, Event::BackgroundUpdated { tasks } if tasks.is_empty()))
            .unwrap();
        assert!(cleared > pending && cleared > listed, "{p:?}");
    }
    // Codex hears of its background terminals only when the host polls
    // (DL-024): the list follows the run's end.
    let codex = replay(P::Codex, "background_terminal");
    let all = codex.all();
    let pending = position(&all, |e| matches!(e, Event::RunCompleted { .. })).unwrap();
    let listed = position(&all, |e| matches!(e, Event::BackgroundUpdated { .. })).unwrap();
    assert!(listed > pending);
    assert!(
        codex
            .writes
            .iter()
            .any(|w| w["method"] == "thread/backgroundTerminals/list")
    );
}

#[test]
fn row_native_turn_ids_only_codex() {
    assert!(!caps(P::Claude).native_turn_ids);
    assert!(caps(P::Codex).native_turn_ids);
    for p in PROVIDERS {
        for e in replay(p, "multi_turn").all() {
            if let Event::TurnStarted { provider_refs, .. } = e {
                assert_eq!(
                    provider_refs.contains_key("turn_id"),
                    caps(p).native_turn_ids,
                    "{p:?}"
                );
            }
        }
    }
}

#[test]
fn row_usage_reports_tokens_for_both_and_cost_for_claude_only() {
    assert_eq!(
        caps(P::Claude).usage,
        UsageReporting {
            tokens: true,
            cost: true
        }
    );
    assert_eq!(
        caps(P::Codex).usage,
        UsageReporting {
            tokens: true,
            cost: false
        }
    );
    for p in PROVIDERS {
        let c = caps(p);
        let usages: Vec<_> = replay(p, "multi_turn")
            .all()
            .into_iter()
            .filter_map(|e| match e {
                Event::UsageUpdated { usage, .. } => Some(usage.clone()),
                _ => None,
            })
            .collect();
        assert!(!usages.is_empty(), "{p:?}");
        for u in usages {
            assert_eq!(u.input_tokens.is_some(), c.usage.tokens, "{p:?}");
            // Unknown cost is `None`, never zero.
            assert_eq!(u.cost_usd.is_some(), c.usage.cost, "{p:?}");
        }
    }
}

// ----- operations and degradations -------------------------------------------

#[test]
fn every_operation_is_supported_or_degrades_as_the_matrix_says() {
    for p in PROVIDERS {
        let c = caps(p);
        for op in Operation::ALL {
            match c.check(op) {
                Ok(()) => assert!(c.supports(op), "{p:?} {op:?}"),
                Err(u) => {
                    assert_eq!(u.op, op);
                    assert_eq!(u.degradation, op.degradation(), "{p:?} {op:?}");
                }
            }
        }
        // The only operation that one provider lacks and the other has.
        let missing: Vec<Operation> = Operation::ALL
            .into_iter()
            .filter(|op| !c.supports(*op))
            .collect();
        let expected = match p {
            P::Claude => vec![],
            P::Codex => vec![Operation::EditInput],
        };
        assert_eq!(missing, expected, "{p:?}");
    }
    // A provider with no capabilities keeps prompt, follow-up and stop only.
    let minimal = Capabilities::minimal();
    let left: Vec<Operation> = Operation::ALL
        .into_iter()
        .filter(|op| minimal.supports(*op))
        .collect();
    assert_eq!(
        left,
        vec![Operation::Prompt, Operation::FollowUp, Operation::Stop]
    );
}

#[test]
fn a_session_reports_the_capabilities_of_its_provider() {
    for p in PROVIDERS {
        for (i, events) in replay(p, "simple").processes.iter().enumerate() {
            let Event::SessionStarted { capabilities, .. } = &events[0] else {
                panic!("{p:?} process {i} starts with session.started");
            };
            assert_eq!(**capabilities, caps(p), "{p:?}");
        }
    }
}
