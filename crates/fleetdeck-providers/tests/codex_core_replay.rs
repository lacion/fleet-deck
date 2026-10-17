//! Replays every recorded Codex fixture (`spikes/s2-codex/fixtures`) through
//! the production sans-IO core (M2 P5-1, ADR-002).
//!
//! For each fixture:
//! - `spawn` starts a fresh core; its recorded arguments must equal
//!   `app_server_args`;
//! - `host` applies the recorded host command;
//! - `in` must equal the next frame that the core wrote, byte for byte;
//! - `out` feeds the recorded app-server frame;
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
use fleetdeck_proto::event::{ExpiredReason, RunOrigin, ToolKind, ToolStatus, TurnStatus};
use fleetdeck_providers::codex::rpc::parse_value;
use fleetdeck_providers::codex::{
    ClientInfo, CodexCore, CoreConfig, HostCmd, Output, app_server_args, capabilities,
};
use serde_json::Value;

/// The scenarios that both providers record (PLAN §13).
const SHARED: &[&str] = &[
    "simple",
    "multi_turn",
    "queued_turn",
    "turn_interrupt",
    "message_steering",
    "provider_thread_resume",
    "tool_call_approve",
    "tool_call_deny",
];

fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../spikes/s2-codex/fixtures")
}

fn fixtures() -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(fixture_dir())
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| {
            let n = p.to_string_lossy();
            n.ends_with(".jsonl") && !n.ends_with(".failed.jsonl")
        })
        .collect();
    files.sort();
    files
}

/// The recording client: the fixtures were made by the spike recorder.
fn recorder_core() -> CodexCore {
    CodexCore::new(CoreConfig {
        provider_instance: "codex".into(),
        cwd: "<workdir>".to_owned(),
        capabilities: capabilities(false),
        resumed: None,
        client: ClientInfo {
            name: "fleet-deck-spike".to_owned(),
            version: "0.0.0".to_owned(),
        },
        request_models: false,
    })
}

#[derive(Default)]
struct Report {
    events: Vec<Event>,
    writes_checked: usize,
    processes: usize,
    unknown_frames: usize,
}

fn replay_text(name: &str, text: &str) -> Result<Report, String> {
    let mut report = Report::default();
    let mut core: Option<CodexCore> = None;
    let mut queue: VecDeque<Value> = VecDeque::new();
    for (index, line) in text.lines().filter(|l| !l.trim().is_empty()).enumerate() {
        let rec: Value = serde_json::from_str(line).unwrap();
        let at = format!("{name}:{}", index + 1);
        let Some(dir) = rec.get("dir").and_then(Value::as_str) else {
            continue; // the fixture header
        };
        match dir {
            "spawn" => {
                if !queue.is_empty() {
                    return Err(format!("{at}: unrecorded writes {queue:?}"));
                }
                if let Some(c) = core.take() {
                    report.unknown_frames += c.unknown_frames();
                }
                let recorded: Vec<String> = serde_json::from_value(rec["args"].clone()).unwrap();
                assert_eq!(recorded, app_server_args(), "{at}: spawn arguments");
                core = Some(recorder_core());
                report.processes += 1;
            }
            "host" => {
                let cmd: HostCmd = serde_json::from_value(rec["cmd"].clone())
                    .map_err(|e| format!("{at}: bad host command: {e}"))?;
                let out = core
                    .as_mut()
                    .unwrap()
                    .apply(&cmd)
                    .map_err(|e| format!("{at}: host command {cmd:?} rejected: {e}"))?;
                collect(out, &mut report, &mut queue);
            }
            "in" => {
                let actual = queue
                    .pop_front()
                    .ok_or_else(|| format!("{at}: the core did not write {}", rec["frame"]))?;
                if actual != rec["frame"] {
                    return Err(format!(
                        "{at}: the core wrote {actual}, the recording has {}",
                        rec["frame"]
                    ));
                }
                report.writes_checked += 1;
            }
            "out" => {
                let out = core
                    .as_mut()
                    .unwrap()
                    .on_frame(parse_value(rec["frame"].clone()));
                collect(out, &mut report, &mut queue);
            }
            "exit" => {
                if !queue.is_empty() {
                    return Err(format!("{at}: unrecorded writes {queue:?}"));
                }
                let code = rec
                    .get("code")
                    .and_then(Value::as_i64)
                    .and_then(|c| i32::try_from(c).ok());
                for o in core.as_mut().unwrap().on_exit(code) {
                    if let Output::Emit(e) = o {
                        report.events.push(e);
                    }
                }
            }
            "note" | "raw" => {}
            other => return Err(format!("{at}: unknown record dir {other}")),
        }
    }
    if !queue.is_empty() {
        return Err(format!("{name}: unrecorded writes {queue:?}"));
    }
    if let Some(c) = core.take() {
        report.unknown_frames += c.unknown_frames();
    }
    Ok(report)
}

fn replay(path: &Path) -> Report {
    let name = path.file_name().unwrap().to_string_lossy().into_owned();
    let text = std::fs::read_to_string(path).unwrap();
    replay_text(&name, &text).unwrap_or_else(|e| panic!("{e}"))
}

fn collect(out: Vec<Output>, report: &mut Report, queue: &mut VecDeque<Value>) {
    for o in out {
        match o {
            Output::Emit(e) => report.events.push(e),
            Output::Write(v) => queue.push_back(v),
            Output::NativeIdentity { .. } => {}
        }
    }
}

/// The lifecycle invariants of one process of a replayed fixture.
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
    assert_eq!(ended, 1, "{name}: one session.ended per process");
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

fn replay_named(scenario: &str) -> Vec<Event> {
    replay(&fixture_dir().join(format!("{scenario}.jsonl"))).events
}

#[test]
fn every_recorded_fixture_replays_through_the_production_core() {
    let files = fixtures();
    assert_eq!(files.len(), 18, "the recorded Codex fixtures");
    let mut writes = 0;
    let mut names = Vec::new();
    for path in &files {
        let name = path.file_stem().unwrap().to_string_lossy().into_owned();
        let report = replay(path);
        assert!(report.processes >= 1, "{name}");
        assert!(report.writes_checked > 0, "{name}");
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
        assert_eq!(report.unknown_frames, 0, "{name}: unknown frames");
        writes += report.writes_checked;
        names.push(name);
    }
    for s in SHARED {
        assert!(
            names.iter().any(|n| n == s),
            "shared scenario {s} has no fixture"
        );
    }
    assert!(writes > 50, "every recorded write is compared ({writes})");
}

#[test]
fn a_changed_host_write_fails_the_replay() {
    let text = std::fs::read_to_string(fixture_dir().join("simple.jsonl")).unwrap();
    // Change only the recorded write of `turn/start`, not the host command.
    let changed: Vec<String> = text
        .lines()
        .map(|l| {
            if l.starts_with("{\"dir\":\"in\"") && l.contains("\"turn/start\"") {
                l.replacen("\"text\":", "\"text\":\"x\",\"was\":", 1)
            } else {
                l.to_owned()
            }
        })
        .collect();
    let changed = changed.join("\n");
    assert_ne!(changed, text.trim_end());
    assert!(replay_text("simple(changed)", &changed).is_err());
    assert!(replay_text("simple", &text).is_ok());
}

#[test]
fn simple_replays_to_one_completed_turn_with_the_codex_version() {
    let events = replay_named("simple");
    let Some(Event::SessionStarted {
        provider_version,
        provider,
        ..
    }) = events.first()
    else {
        panic!("no session.started");
    };
    assert_eq!(provider_version.as_deref(), Some("0.160.0"));
    assert_eq!(*provider, fleetdeck_proto::event::ProviderKind::Codex);
    let k = kinds(&events);
    assert!(k.contains(&"message.completed".to_owned()));
    assert!(events.iter().any(|e| matches!(
        e,
        Event::TurnCompleted {
            status: TurnStatus::Done,
            ..
        }
    )));
    assert!(events.iter().any(|e| matches!(
        e,
        Event::RunStarted {
            origin: RunOrigin::Board,
            ..
        }
    )));
    assert_eq!(k.last().map(String::as_str), Some("session.ended"));
}

#[test]
fn approvals_are_answered_with_the_native_decision() {
    let events = replay_named("tool_call_approve");
    let k = kinds(&events);
    let req = k.iter().position(|k| k == "approval.requested").unwrap();
    let res = k.iter().position(|k| k == "approval.resolved").unwrap();
    assert!(req < res);
    assert!(events.iter().any(|e| matches!(
        e,
        Event::ToolCompleted {
            status: ToolStatus::Ok,
            ..
        }
    )));
    let deny = replay_named("tool_call_deny");
    assert!(deny.iter().any(|e| matches!(
        e,
        Event::ToolCompleted {
            status: ToolStatus::Denied,
            ..
        }
    )));
}

#[test]
fn interrupts_end_the_turn_as_interrupted_and_report_the_result() {
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
                    status: TurnStatus::Interrupted,
                    ..
                }
            )),
            "{scenario}"
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e, Event::InterruptResult { accepted: true, .. })),
            "{scenario}: interrupt result"
        );
    }
}

#[test]
fn an_approval_open_at_a_kill_expires_as_process_lost() {
    let events = replay_named("approval_restart");
    assert!(events.iter().any(|e| matches!(
        e,
        Event::ApprovalExpired {
            reason: ExpiredReason::ProcessLost,
            ..
        }
    )));
}

#[test]
fn plans_file_changes_modes_and_background_reach_the_canonical_stream() {
    assert!(kinds(&replay_named("plan_capture")).contains(&"plan.proposed".to_owned()));
    assert!(kinds(&replay_named("file_change")).contains(&"file.changed".to_owned()));
    let mode = replay_named("set_mode");
    assert!(mode.iter().any(|e| matches!(
        e,
        Event::ModeChanged { mode } if mode == "untrusted/read-only"
    )));
    let bg = replay_named("background_terminal");
    assert!(bg.iter().any(|e| matches!(
        e,
        Event::BackgroundUpdated { tasks } if !tasks.is_empty()
    )));
}

#[test]
fn a_subagent_message_belongs_to_its_spawning_tool_call() {
    let events = replay_named("subagent");
    let spawn = events
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
        events.iter().any(|e| matches!(
            e,
            Event::MessageCompleted { parent_node_id: Some(p), .. } if *p == spawn
        )),
        "a child message nests under the spawning call"
    );
}

#[test]
fn resume_and_fork_start_sessions_on_the_recorded_threads() {
    for scenario in ["provider_thread_resume", "fork_thread"] {
        let report = replay(&fixture_dir().join(format!("{scenario}.jsonl")));
        let started = report
            .events
            .iter()
            .filter(|e| matches!(e, Event::SessionStarted { .. }))
            .count();
        assert!(started >= 2, "{scenario}: {started} session.started");
    }
}

/// The board's test fixtures (M2 P5-3): the canonical events of a recorded
/// Codex approval and subagent as the deck sends them on a timeline, in
/// `web/src/testing/fixtures/codex-<scenario>.events.json`. The web tests
/// read them for the capability-driven controls of a Codex session.
/// `FD_UPDATE_FIXTURES=1` rewrites the files.
#[test]
fn the_board_timeline_fixtures_equal_the_canonical_replay() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../web/src/testing/fixtures");
    let update = std::env::var_os("FD_UPDATE_FIXTURES").is_some();
    for scenario in ["subagent", "tool_call_approve", "question_rpc"] {
        let envelopes: Vec<fleetdeck_proto::EventEnvelope> = replay_named(scenario)
            .into_iter()
            .enumerate()
            .map(|(i, event)| {
                let n = i as u64 + 1;
                fleetdeck_proto::EventEnvelope {
                    station_seq: 200 + n,
                    session_id: format!("codex-{scenario}").into(),
                    session_seq: n,
                    at: 1_760_000_000_000 + 1000 * n as i64,
                    proto_version: 1,
                    raw_ref: None,
                    event: event.into(),
                }
            })
            .collect();
        let text = serde_json::to_string_pretty(&envelopes).unwrap() + "\n";
        let path = dir.join(format!("codex-{scenario}.events.json"));
        if update {
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(&path, &text).unwrap();
        } else {
            let found = std::fs::read_to_string(&path).unwrap_or_default();
            assert!(
                found == text,
                "{} is stale: run FD_UPDATE_FIXTURES=1 cargo test -p fleetdeck-providers --test codex_core_replay",
                path.display()
            );
        }
    }
}
