//! Replays the frames that the TypeScript Agent SDK recorded (D8,
//! `spikes/s1-claude/fixtures/sdk`) through the production Claude core
//! (M2 P2-3; ported from the spike's `reference.rs`).
//!
//! An SDK fixture has no `host` records: the SDK wrote the `in` frames. The
//! test infers the host command from each `in` frame, applies it to the core,
//! and compares the frame that the core writes with the SDK frame on the
//! fields that change the CLI's behaviour ([`semantic`]). SDK request ids are
//! random, so CLI responses are re-addressed to the core's `fd-N` ids.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "a test harness: a panic is the failure report"
)]

use std::collections::{BTreeSet, HashMap, VecDeque};
use std::path::{Path, PathBuf};

use fleetdeck_proto::Event;
use fleetdeck_providers::claude::frames::parse_value;
use fleetdeck_providers::claude::{ClaudeCore, CoreConfig, HostCmd, Output, capabilities};
use serde_json::{Map, Value, json};

fn sdk_fixtures() -> Vec<PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../spikes/s1-claude/fixtures/sdk");
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "jsonl"))
        .collect();
    files.sort();
    files
}

struct Process {
    core: ClaudeCore,
    /// SDK request id → core request id.
    ids: HashMap<String, String>,
    /// SDK-only requests: their CLI responses are not fed.
    skipped: BTreeSet<String>,
    /// Writes that the core made on its own (a fail-closed `control_error`).
    spontaneous: VecDeque<Value>,
    inputs: usize,
}

impl Process {
    fn new() -> Self {
        Self {
            core: ClaudeCore::new(CoreConfig {
                provider_instance: "claude".into(),
                cwd: "/w".to_owned(),
                capabilities: capabilities(false),
                resumed: None,
            }),
            ids: HashMap::new(),
            skipped: BTreeSet::new(),
            spontaneous: VecDeque::new(),
            inputs: 0,
        }
    }
}

#[derive(Default)]
struct Report {
    events: Vec<Event>,
    writes_checked: usize,
    processes: usize,
    unknown_frames: usize,
}

fn replay(path: &Path) -> Report {
    let name = path.file_name().unwrap().to_string_lossy().into_owned();
    let text = std::fs::read_to_string(path).unwrap();
    let mut report = Report::default();
    let mut proc: Option<Process> = None;
    for (n, line) in text.lines().enumerate() {
        let at = format!("{name}:{}", n + 1);
        let rec: Value = serde_json::from_str(line).unwrap();
        match rec.get("dir").and_then(Value::as_str) {
            Some("spawn") => {
                finish(proc.take(), &mut report, &at);
                proc = Some(Process::new());
                report.processes += 1;
            }
            Some("in") => on_in(proc.as_mut().unwrap(), &rec["frame"], &at, &mut report),
            Some("out") => {
                let p = proc.as_mut().unwrap();
                let Some(frame) = readdress(p, &rec["frame"]) else {
                    continue;
                };
                for o in p.core.on_frame(parse_value(frame).unwrap()) {
                    match o {
                        Output::Emit(e) => report.events.push(e),
                        Output::Write(v) => p.spontaneous.push_back(v),
                    }
                }
            }
            Some("exit") => {
                let p = proc.as_mut().unwrap();
                assert!(p.spontaneous.is_empty(), "{at}: {:?}", p.spontaneous);
                let code = rec.get("code").and_then(Value::as_i64).map(|c| c as i32);
                for o in p.core.on_exit(code) {
                    if let Output::Emit(e) = o {
                        report.events.push(e);
                    }
                }
            }
            Some("host") => panic!("{at}: an SDK fixture has no host records"),
            _ => {}
        }
    }
    finish(proc.take(), &mut report, &name);
    report
}

fn finish(proc: Option<Process>, report: &mut Report, at: &str) {
    if let Some(p) = proc {
        assert!(
            p.spontaneous.is_empty(),
            "{at}: extra writes {:?}",
            p.spontaneous
        );
        report.unknown_frames += p.core.unknown_frames();
    }
}

fn readdress(p: &Process, frame: &Value) -> Option<Value> {
    if frame["type"] != "control_response" {
        return Some(frame.clone());
    }
    let sdk_id = frame
        .pointer("/response/request_id")
        .and_then(Value::as_str)?;
    if p.skipped.contains(sdk_id) {
        return None;
    }
    let mut out = frame.clone();
    if let Some(core_id) = p.ids.get(sdk_id)
        && let Some(slot) = out.pointer_mut("/response/request_id")
    {
        *slot = Value::String(core_id.clone());
    }
    Some(out)
}

fn on_in(p: &mut Process, frame: &Value, at: &str, report: &mut Report) {
    let kind = frame["type"].as_str().unwrap_or("");
    let cmd = match kind {
        "user" => {
            let text = frame.pointer("/message/content").and_then(Value::as_str);
            let text = text.unwrap_or_else(|| panic!("{at}: a user message without text"));
            p.inputs += 1;
            let input_id = frame["uuid"].as_str().map_or_else(
                // The SDK sends no uuid: a ULID-shaped id gets a v4 uuid.
                || ulid::Ulid::from_parts(p.inputs as u64, 0).to_string(),
                str::to_owned,
            );
            HostCmd::Prompt {
                text: text.to_owned(),
                input_id,
            }
        }
        "control_request" => {
            let sdk_id = frame["request_id"].as_str().unwrap_or("").to_owned();
            match frame.pointer("/request/subtype").and_then(Value::as_str) {
                Some("initialize") => HostCmd::Initialize,
                Some("interrupt") => HostCmd::Interrupt,
                Some("set_permission_mode") => HostCmd::SetMode {
                    mode: frame
                        .pointer("/request/mode")
                        .unwrap()
                        .as_str()
                        .unwrap()
                        .to_owned(),
                },
                _ => {
                    p.skipped.insert(sdk_id);
                    return;
                }
            }
        }
        "control_response" => return on_sdk_answer(p, frame, at, report),
        other => panic!("{at}: unexpected SDK write of type {other:?}"),
    };
    let outs = p
        .core
        .apply(&cmd)
        .unwrap_or_else(|e| panic!("{at}: {cmd:?} rejected: {e}"));
    let actual = take_write(outs, report, at);
    if kind == "control_request"
        && let (Some(sdk_id), Some(core_id)) =
            (frame["request_id"].as_str(), actual["request_id"].as_str())
    {
        p.ids.insert(sdk_id.to_owned(), core_id.to_owned());
    }
    compare(frame, &actual, at, report);
}

/// An SDK answer to `can_use_tool` becomes an answer command; an answer to
/// any other request must equal the core's own fail-closed answer.
fn on_sdk_answer(p: &mut Process, frame: &Value, at: &str, report: &mut Report) {
    let native = frame
        .pointer("/response/request_id")
        .and_then(Value::as_str)
        .unwrap_or("");
    if let Some(request) = p.core.request_for_native(native).cloned() {
        let body = frame
            .pointer("/response/response")
            .cloned()
            .unwrap_or_default();
        let option_id = match body["behavior"].as_str() {
            Some("allow") if body.get("updatedPermissions").is_some() => "allow_always",
            Some("allow") => "allow",
            _ => "deny",
        };
        let cmd = HostCmd::Answer {
            request_id: request.0,
            option_id: option_id.to_owned(),
            updated_input: body.get("updatedInput").cloned(),
            message: body["message"].as_str().map(str::to_owned),
        };
        let outs = p
            .core
            .apply(&cmd)
            .unwrap_or_else(|e| panic!("{at}: {cmd:?} rejected: {e}"));
        let actual = take_write(outs, report, at);
        compare(frame, &actual, at, report);
        return;
    }
    let pos = p
        .spontaneous
        .iter()
        .position(|w| w.pointer("/response/request_id").and_then(Value::as_str) == Some(native))
        .unwrap_or_else(|| panic!("{at}: the core did not answer {native}"));
    let actual = p.spontaneous.remove(pos).unwrap();
    compare(frame, &actual, at, report);
}

fn take_write(outs: Vec<Output>, report: &mut Report, at: &str) -> Value {
    let mut writes = Vec::new();
    for o in outs {
        match o {
            Output::Emit(e) => report.events.push(e),
            Output::Write(v) => writes.push(v),
        }
    }
    assert_eq!(
        writes.len(),
        1,
        "{at}: one write per command, got {writes:?}"
    );
    writes.remove(0)
}

fn compare(sdk: &Value, core: &Value, at: &str, report: &mut Report) {
    assert_eq!(semantic(sdk), semantic(core), "{at}: written frame");
    report.writes_checked += 1;
}

/// The behaviour-relevant projection of a host → CLI frame.
fn semantic(frame: &Value) -> Value {
    let get = |p: &str| frame.pointer(p).cloned().unwrap_or(Value::Null);
    match frame["type"].as_str() {
        Some("user") => json!({
            "type": "user",
            "content": get("/message/content"),
            "parent_tool_use_id": get("/parent_tool_use_id"),
        }),
        Some("control_request") => json!({
            "type": "control_request",
            "subtype": get("/request/subtype"),
            "mode": get("/request/mode"),
        }),
        Some("control_response") => {
            let body = get("/response/response");
            let mut b = Map::new();
            for key in [
                "behavior",
                "updatedInput",
                "message",
                "toolUseID",
                "updatedPermissions",
            ] {
                b.insert(
                    key.to_owned(),
                    body.get(key).cloned().unwrap_or(Value::Null),
                );
            }
            let interrupt = body["interrupt"].as_bool().unwrap_or(false);
            b.insert("interrupt".to_owned(), Value::Bool(interrupt));
            json!({
                "type": "control_response",
                "subtype": get("/response/subtype"),
                "body": Value::Object(b),
            })
        }
        _ => frame.clone(),
    }
}

#[test]
fn every_sdk_reference_fixture_replays_through_the_production_core() {
    let files = sdk_fixtures();
    assert_eq!(files.len(), 10, "the SDK reference fixtures");
    let mut writes = 0;
    for path in &files {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let report = replay(path);
        assert!(report.processes >= 1, "{name}");
        assert_eq!(report.unknown_frames, 0, "{name}: unknown frames");
        let started = report
            .events
            .iter()
            .filter(|e| matches!(e, Event::SessionStarted { .. }))
            .count();
        let ended = report
            .events
            .iter()
            .filter(|e| matches!(e, Event::SessionEnded { .. }))
            .count();
        assert_eq!(
            started, report.processes,
            "{name}: one session.started per process"
        );
        assert_eq!(
            ended, report.processes,
            "{name}: one session.ended per process"
        );
        writes += report.writes_checked;
    }
    assert_eq!(writes, 32, "every SDK write is compared");
}
