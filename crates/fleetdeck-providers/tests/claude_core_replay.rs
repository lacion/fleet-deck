//! Replays every recorded Claude fixture (`spikes/s1-claude/fixtures/rust`)
//! through the production sans-IO core (M2 P2-2).
//!
//! For each fixture:
//! - `spawn` starts a fresh core; its recorded arguments must equal
//!   `cli_args` for the same launch (mode, model, resume, fork);
//! - `host` applies the recorded host command;
//! - `in` must equal the next frame that the core wrote, byte for byte;
//! - `out` feeds the recorded CLI frame;
//! - `exit` ends the process.
//!
//! Then the canonical stream must keep the invariants of PLAN §6.3: it opens
//! with `session.started`, every turn and run that starts also ends, every
//! approval request is resolved or expired, and every tool completes.

#![allow(
    clippy::unwrap_used,
    reason = "a test harness: a panic is the failure report"
)]

use std::collections::{BTreeSet, VecDeque};
use std::path::{Path, PathBuf};

use fleetdeck_proto::Event;
use fleetdeck_providers::NativeSessionId;
use fleetdeck_providers::claude::frames::parse_value;
use fleetdeck_providers::claude::{
    BYPASS_MODE, ClaudeCore, CoreConfig, HostCmd, Launch, Output, capabilities, cli_args,
};
use serde_json::Value;

fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../spikes/s1-claude/fixtures/rust")
}

fn fixtures() -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(fixture_dir())
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "jsonl"))
        .collect();
    files.sort();
    files
}

#[derive(Default)]
struct Report {
    events: Vec<Event>,
    writes_checked: usize,
    processes: usize,
    unknown_frames: usize,
}

fn launch_of(args: &[Value]) -> Launch {
    let args: Vec<&str> = args.iter().map(|a| a.as_str().unwrap()).collect();
    let after = |flag: &str| {
        args.iter()
            .position(|a| *a == flag)
            .map(|i| args[i + 1].to_owned())
    };
    Launch {
        model: after("--model"),
        mode: after("--permission-mode"),
        resume: after("--resume").map(NativeSessionId),
        fork: args.contains(&"--fork-session"),
        extra_args: vec!["--safe-mode".to_owned()],
    }
}

fn replay(path: &Path) -> Report {
    let name = path.file_name().unwrap().to_string_lossy().into_owned();
    let text = std::fs::read_to_string(path).unwrap();
    let mut report = Report::default();
    let mut core: Option<ClaudeCore> = None;
    let mut queue: VecDeque<Value> = VecDeque::new();
    for (index, line) in text.lines().enumerate() {
        let rec: Value = serde_json::from_str(line).unwrap();
        let at = format!("{name}:{}", index + 1);
        let Some(dir) = rec.get("dir").and_then(Value::as_str) else {
            continue; // the fixture header
        };
        match dir {
            "spawn" => {
                assert!(queue.is_empty(), "{at}: unrecorded writes {queue:?}");
                if let Some(c) = core.take() {
                    report.unknown_frames += c.unknown_frames();
                }
                let args = rec["args"].as_array().unwrap();
                let launch = launch_of(args);
                let caps = capabilities(launch.mode.as_deref() == Some(BYPASS_MODE));
                let expected: Vec<String> = cli_args(&launch, &caps);
                let recorded: Vec<&str> = args.iter().map(|a| a.as_str().unwrap()).collect();
                assert_eq!(expected, recorded, "{at}: spawn arguments");
                core = Some(ClaudeCore::new(CoreConfig {
                    provider_instance: "claude".into(),
                    cwd: "/w".to_owned(),
                    capabilities: caps,
                    resumed: launch.resume.as_ref().map(|_| true),
                }));
                report.processes += 1;
            }
            "host" => {
                let cmd: HostCmd = serde_json::from_value(rec["cmd"].clone())
                    .unwrap_or_else(|e| panic!("{at}: bad host command: {e}"));
                let c = core.as_mut().unwrap();
                let out = c
                    .apply(&cmd)
                    .unwrap_or_else(|e| panic!("{at}: host command {cmd:?} rejected: {e}"));
                collect(out, &mut report, &mut queue);
            }
            "in" => {
                let actual = queue
                    .pop_front()
                    .unwrap_or_else(|| panic!("{at}: the core did not write {}", rec["frame"]));
                assert_eq!(actual, rec["frame"], "{at}: written frame");
                report.writes_checked += 1;
            }
            "out" => {
                let frame = parse_value(rec["frame"].clone())
                    .unwrap_or_else(|e| panic!("{at}: unparsable frame: {e}"));
                let out = core.as_mut().unwrap().on_frame(frame);
                collect(out, &mut report, &mut queue);
            }
            "exit" => {
                assert!(queue.is_empty(), "{at}: unrecorded writes {queue:?}");
                let code = rec.get("code").and_then(Value::as_i64).map(|c| c as i32);
                for o in core.as_mut().unwrap().on_exit(code) {
                    if let Output::Emit(e) = o {
                        report.events.push(e);
                    }
                }
            }
            "note" | "raw" => {}
            other => panic!("{at}: unknown record dir {other}"),
        }
    }
    assert!(queue.is_empty(), "{name}: unrecorded writes {queue:?}");
    if let Some(c) = core.take() {
        report.unknown_frames += c.unknown_frames();
    }
    report
}

fn collect(out: Vec<Output>, report: &mut Report, queue: &mut VecDeque<Value>) {
    for o in out {
        match o {
            Output::Emit(e) => report.events.push(e),
            Output::Write(v) => queue.push_back(v),
        }
    }
}

/// The lifecycle invariants of one replayed fixture.
fn check_invariants(name: &str, events: &[Event]) {
    assert!(
        matches!(events.first(), Some(Event::SessionStarted { .. })),
        "{name}: the stream opens with session.started"
    );
    let mut open_turns = BTreeSet::new();
    let mut open_runs = BTreeSet::new();
    let mut open_requests = BTreeSet::new();
    let mut open_tools = BTreeSet::new();
    let mut ended = 0;
    for e in events {
        match e {
            Event::RunStarted { run_id, .. } => {
                assert!(
                    open_runs.insert(run_id.clone()),
                    "{name}: run {run_id} twice"
                );
            }
            Event::TurnStarted {
                run_id, turn_id, ..
            } => {
                assert!(open_runs.contains(run_id), "{name}: turn outside its run");
                assert!(open_turns.is_empty(), "{name}: two turns at once");
                open_turns.insert(turn_id.clone());
            }
            Event::TurnCompleted { turn_id, .. } => {
                assert!(
                    open_turns.remove(turn_id),
                    "{name}: turn {turn_id} not open"
                );
            }
            Event::RunCompleted { run_id, .. } => {
                assert!(open_turns.is_empty(), "{name}: run ends before its turn");
                assert!(open_runs.remove(run_id), "{name}: run {run_id} not open");
            }
            Event::ApprovalRequested { request_id, .. } => {
                assert!(
                    open_requests.insert(request_id.clone()),
                    "{name}: request twice"
                );
            }
            Event::ApprovalResolved { request_id, .. }
            | Event::ApprovalExpired { request_id, .. } => {
                assert!(
                    open_requests.remove(request_id),
                    "{name}: request {request_id} closed twice"
                );
            }
            Event::ToolStarted { node_id, .. } => {
                assert!(open_tools.insert(node_id.clone()), "{name}: tool twice");
            }
            Event::ToolCompleted { node_id, .. } => {
                assert!(
                    open_tools.remove(node_id),
                    "{name}: tool {node_id} not open"
                );
            }
            Event::SessionEnded { .. } => ended += 1,
            _ => {}
        }
    }
    assert!(open_turns.is_empty(), "{name}: open turns {open_turns:?}");
    assert!(open_runs.is_empty(), "{name}: open runs {open_runs:?}");
    assert!(
        open_requests.is_empty(),
        "{name}: open requests {open_requests:?}"
    );
    assert!(open_tools.is_empty(), "{name}: open tools {open_tools:?}");
    assert!(ended >= 1, "{name}: no session.ended");
}

fn kinds(events: &[Event]) -> Vec<String> {
    events
        .iter()
        .map(|e| {
            serde_json::to_value(e).unwrap()["kind"]
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect()
}

#[test]
fn every_recorded_fixture_replays_through_the_production_core() {
    let files = fixtures();
    assert_eq!(files.len(), 19, "the recorded Rust fixtures");
    let mut writes = 0;
    for path in &files {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let report = replay(path);
        assert!(report.processes >= 1, "{name}");
        // Every process in a fixture starts its own stream; check each.
        let mut start = 0;
        for (i, e) in report.events.iter().enumerate() {
            if matches!(e, Event::SessionEnded { .. }) {
                check_invariants(&name, &report.events[start..=i]);
                start = i + 1;
            }
        }
        assert_eq!(
            start,
            report.events.len(),
            "{name}: events after the last exit"
        );
        writes += report.writes_checked;
        assert_eq!(report.unknown_frames, 0, "{name}: unknown frames");
    }
    assert_eq!(writes, 62, "every recorded write is compared");
}

fn replay_named(scenario: &str) -> Vec<Event> {
    replay(&fixture_dir().join(format!("{scenario}.jsonl"))).events
}

#[test]
fn an_approval_is_requested_answered_and_the_tool_runs() {
    let events = replay_named("tool_call_approve");
    let k = kinds(&events);
    let req = k.iter().position(|k| k == "approval.requested").unwrap();
    let res = k.iter().position(|k| k == "approval.resolved").unwrap();
    assert!(req < res);
    assert!(events.iter().any(|e| matches!(
        e,
        Event::ToolCompleted {
            status: fleetdeck_proto::event::ToolStatus::Ok,
            ..
        }
    )));
}

#[test]
fn an_interrupt_ends_the_turn_as_interrupted() {
    for scenario in [
        "turn_interrupt",
        "interrupt_during_tool",
        "interrupt_right_after_send",
    ] {
        let events = replay_named(scenario);
        assert!(
            events.iter().any(|e| matches!(
                e,
                Event::TurnCompleted {
                    status: fleetdeck_proto::event::TurnStatus::Interrupted,
                    ..
                }
            )),
            "{scenario}"
        );
    }
}

#[test]
fn a_background_task_leaves_the_run_pending_and_ends_in_a_provider_turn() {
    let events = replay_named("background_task");
    assert!(events.iter().any(|e| matches!(
        e,
        Event::RunCompleted {
            background_pending: true,
            ..
        }
    )));
    assert!(events.iter().any(|e| matches!(
        e,
        Event::RunStarted {
            origin: fleetdeck_proto::event::RunOrigin::Provider,
            ..
        }
    )));
}

#[test]
fn an_approval_open_at_a_kill_expires_as_process_lost() {
    let events = replay_named("approval_restart");
    assert!(events.iter().any(|e| matches!(
        e,
        Event::ApprovalExpired {
            reason: fleetdeck_proto::event::ExpiredReason::ProcessLost,
            ..
        }
    )));
}

#[test]
fn plan_mode_and_file_edits_reach_the_canonical_stream() {
    assert!(kinds(&replay_named("plan_capture")).contains(&"plan.proposed".to_owned()));
    assert!(kinds(&replay_named("file_edit")).contains(&"file.changed".to_owned()));
}
