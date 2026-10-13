//! Replays of the recorded fixtures for the capability matrix test.

use std::path::Path;

use fleetdeck_proto::Event;
use fleetdeck_proto::capability::Capabilities;
use fleetdeck_providers::{claude, codex};
use serde_json::Value;

/// The two providers of v2 (D14).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum P {
    Claude,
    Codex,
}

pub(crate) const PROVIDERS: [P; 2] = [P::Claude, P::Codex];

/// The capabilities of a new session with the station defaults (no bypass,
/// no full access).
pub(crate) fn caps(p: P) -> Capabilities {
    match p {
        P::Claude => claude::capabilities(false),
        P::Codex => codex::capabilities(false),
    }
}

/// One replayed fixture: the events of each process, in order.
pub(crate) struct Replay {
    pub(crate) processes: Vec<Vec<Event>>,
    /// Every frame that the core wrote to the provider, in order.
    pub(crate) writes: Vec<Value>,
}

impl Replay {
    pub(crate) fn all(&self) -> Vec<&Event> {
        self.processes.iter().flatten().collect()
    }
}

pub(crate) fn fixture(p: P, scenario: &str) -> std::path::PathBuf {
    let dir = match p {
        P::Claude => "spikes/s1-claude/fixtures/rust",
        P::Codex => "spikes/s2-codex/fixtures",
    };
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(dir)
        .join(format!("{scenario}.jsonl"))
}

pub(crate) fn records(p: P, scenario: &str) -> Vec<Value> {
    let text = std::fs::read_to_string(fixture(p, scenario)).unwrap();
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str::<Value>(l).unwrap())
        .filter(|r| r.get("dir").is_some())
        .collect()
}

/// Replays a fixture through the production core of its provider. The byte
/// checks of the host writes are in `*_core_replay.rs`; this replay keeps
/// the events only.
pub(crate) fn replay(p: P, scenario: &str) -> Replay {
    match p {
        P::Claude => replay_claude(scenario),
        P::Codex => replay_codex(scenario),
    }
}

pub(crate) fn replay_claude(scenario: &str) -> Replay {
    use claude::{ClaudeCore, CoreConfig, HostCmd, Output};
    let mut processes: Vec<Vec<Event>> = Vec::new();
    let mut writes: Vec<Value> = Vec::new();
    let mut core: Option<ClaudeCore> = None;
    let emit = |out: Vec<Output>, events: &mut Vec<Event>, writes: &mut Vec<Value>| {
        for o in out {
            match o {
                Output::Emit(e) => events.push(e),
                Output::Write(w) => writes.push(w),
            }
        }
    };
    for rec in records(P::Claude, scenario) {
        match rec["dir"].as_str().unwrap() {
            "spawn" => {
                let args: Vec<&str> = rec["args"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|a| a.as_str().unwrap())
                    .collect();
                let bypass = args
                    .windows(2)
                    .any(|w| w == ["--permission-mode", claude::BYPASS_MODE]);
                core = Some(ClaudeCore::new(CoreConfig {
                    provider_instance: "claude".into(),
                    cwd: "/w".to_owned(),
                    capabilities: claude::capabilities(bypass),
                    resumed: args.contains(&"--resume").then_some(true),
                }));
                processes.push(Vec::new());
            }
            "host" => {
                let cmd: HostCmd = serde_json::from_value(rec["cmd"].clone()).unwrap();
                let out = core.as_mut().unwrap().apply(&cmd).unwrap();
                emit(out, processes.last_mut().unwrap(), &mut writes);
            }
            "out" => {
                let frame = claude::frames::parse_value(rec["frame"].clone()).unwrap();
                let out = core.as_mut().unwrap().on_frame(frame);
                emit(out, processes.last_mut().unwrap(), &mut writes);
            }
            "exit" => {
                let code = rec
                    .get("code")
                    .and_then(Value::as_i64)
                    .and_then(|c| i32::try_from(c).ok());
                let out = core.as_mut().unwrap().on_exit(code);
                emit(out, processes.last_mut().unwrap(), &mut writes);
            }
            _ => {}
        }
    }
    Replay { processes, writes }
}

pub(crate) fn codex_core() -> codex::CodexCore {
    codex::CodexCore::new(codex::CoreConfig {
        provider_instance: "codex".into(),
        cwd: "<workdir>".to_owned(),
        capabilities: codex::capabilities(false),
        resumed: None,
        client: codex::ClientInfo {
            name: "fleet-deck-spike".to_owned(),
            version: "0.0.0".to_owned(),
        },
        request_models: false,
    })
}

pub(crate) fn replay_codex(scenario: &str) -> Replay {
    let mut processes: Vec<Vec<Event>> = Vec::new();
    let mut writes: Vec<Value> = Vec::new();
    let mut core: Option<codex::CodexCore> = None;
    for rec in records(P::Codex, scenario) {
        let out = match rec["dir"].as_str().unwrap() {
            "spawn" => {
                core = Some(codex_core());
                processes.push(Vec::new());
                continue;
            }
            "host" => {
                let cmd: codex::HostCmd = serde_json::from_value(rec["cmd"].clone()).unwrap();
                core.as_mut().unwrap().apply(&cmd).unwrap()
            }
            "out" => core
                .as_mut()
                .unwrap()
                .on_frame(codex::rpc::parse_value(rec["frame"].clone())),
            "exit" => {
                let code = rec
                    .get("code")
                    .and_then(Value::as_i64)
                    .and_then(|c| i32::try_from(c).ok());
                core.as_mut().unwrap().on_exit(code)
            }
            _ => continue,
        };
        for o in out {
            match o {
                codex::Output::Emit(e) => processes.last_mut().unwrap().push(e),
                codex::Output::Write(w) => writes.push(w),
            }
        }
    }
    Replay { processes, writes }
}

#[test]
#[ignore = "debug helper: prints the event kinds of a fixture"]
fn dump() {
    let p = match std::env::var("FD_P").as_deref() {
        Ok("codex") => P::Codex,
        _ => P::Claude,
    };
    let s = std::env::var("FD_S").unwrap();
    for (i, proc) in replay(p, &s).processes.iter().enumerate() {
        for e in proc {
            println!("{i} {}", serde_json::to_string(e).unwrap());
        }
    }
}

// ----- helpers over replayed events -----------------------------------------

/// The provider's native session id of a process (`session_id` for Claude,
/// `thread_id` for Codex), from `session.started`.
pub(crate) fn native_session(events: &[Event]) -> String {
    events
        .iter()
        .find_map(|e| match e {
            Event::SessionStarted { provider_refs, .. } => provider_refs
                .get("session_id")
                .or_else(|| provider_refs.get("thread_id"))
                .and_then(Value::as_str)
                .map(str::to_owned),
            _ => None,
        })
        .unwrap()
}

pub(crate) fn messages(events: &[Event]) -> Vec<String> {
    events
        .iter()
        .filter_map(|e| match e {
            Event::MessageCompleted {
                text,
                parent_node_id: None,
                ..
            } => Some(text.clone()),
            _ => None,
        })
        .collect()
}

pub(crate) fn count(events: &[&Event], kind: &str) -> usize {
    events.iter().filter(|e| e.kind_name() == kind).count()
}

pub(crate) fn position(events: &[&Event], f: impl Fn(&Event) -> bool) -> Option<usize> {
    events.iter().position(|e| f(e))
}

pub(crate) const ALL_FIXTURES: [&[&str]; 2] = [
    &[
        "approval_restart",
        "ask_user_question",
        "background_task",
        "file_edit",
        "fork_session",
        "interrupt_during_tool",
        "interrupt_right_after_send",
        "message_steering",
        "multi_turn",
        "plan_capture",
        "provider_thread_resume",
        "queued_turn",
        "set_permission_mode",
        "simple",
        "subagent",
        "tool_call_approve",
        "tool_call_deny",
        "tool_call_edit_input",
        "turn_interrupt",
    ],
    &[
        "approval_restart",
        "background_terminal",
        "file_change",
        "fork_thread",
        "interrupt_during_tool",
        "interrupt_right_after_send",
        "message_steering",
        "multi_turn",
        "plan_capture",
        "provider_thread_resume",
        "question_rpc",
        "queued_turn",
        "set_mode",
        "simple",
        "subagent",
        "tool_call_approve",
        "tool_call_deny",
        "turn_interrupt",
    ],
];

pub(crate) fn fixtures(p: P) -> &'static [&'static str] {
    match p {
        P::Claude => ALL_FIXTURES[0],
        P::Codex => ALL_FIXTURES[1],
    }
}
