//! Offline replay of frames that the TypeScript Agent SDK recorded (D8).
//!
//! An SDK fixture has no `host` records: the SDK, not our host, wrote the `in`
//! frames. This replay infers the host command from each `in` frame, applies it to a
//! fresh [`ClaudeCore`], and compares the frame that the core writes with the frame
//! that the SDK wrote. The comparison is semantic (see [`semantic`]): the wire
//! fields that change the CLI's behaviour must be equal. Key-set differences that
//! do not change behaviour (for example our `uuid` on user messages) go into
//! [`ReferenceReport::key_differences`] as evidence for ADR-001.
//!
//! Request ids differ by design (the SDK uses random ids, the core uses `fd-N`), so
//! CLI responses to SDK requests are re-addressed to the core's ids before the core
//! sees them.

use std::collections::{BTreeSet, HashMap, VecDeque};

use serde_json::{Map, Value, json};
use spike_canon::CanonEvent;
use spike_canon::fixture::{Fixture, Record};

use crate::adapter::{ClaudeCore, HostCmd, Output};
use crate::frames::parse_value;
use crate::replay::ReplayError;

/// The result of a successful reference replay.
#[derive(Debug, Default)]
pub struct ReferenceReport {
    /// Canonical events in order, over all processes of the fixture.
    pub events: Vec<CanonEvent>,
    /// Frames that the parser or the core did not know.
    pub unknown_frames: usize,
    /// SDK writes compared with a core write.
    pub writes_checked: usize,
    /// Number of processes (`spawn` records).
    pub processes: usize,
    /// `kind: field` entries for wire keys that only one side sends.
    pub key_differences: BTreeSet<String>,
    /// SDK control requests that the host model has no command for (skipped).
    pub sdk_only_requests: Vec<String>,
}

#[derive(Default)]
struct Process {
    core: ClaudeCore,
    /// SDK request id → core request id.
    ids: HashMap<String, String>,
    /// SDK request ids of skipped requests: their CLI responses are not fed.
    skipped: BTreeSet<String>,
    /// Writes that the core made without a host command (for example a fail-closed
    /// `control_error`). The SDK must have written an answer to the same request.
    spontaneous: VecDeque<Value>,
    inputs: usize,
}

/// Replays an SDK fixture through a fresh core per process.
///
/// # Errors
/// See [`ReplayError`]; a semantic difference is a [`ReplayError::WriteMismatch`]
/// whose values are the semantic projections.
pub fn replay_reference(fx: &Fixture) -> Result<ReferenceReport, ReplayError> {
    let mut report = ReferenceReport::default();
    let mut proc: Option<Process> = None;
    for (index, record) in fx.records.iter().enumerate() {
        match record {
            Record::Spawn { .. } => {
                finish(&mut proc, &mut report, index)?;
                proc = Some(Process::default());
                report.processes += 1;
            }
            // The product core's `get_usage` write (R4.5) is not part of the
            // SDK or spike behavior.
            Record::In { frame } if crate::replay::is_usage_probe(frame) => {}
            Record::In { frame } => {
                let p = proc.as_mut().ok_or(ReplayError::NoProcess { index })?;
                on_in(p, frame, index, &mut report)?;
            }
            Record::Out { frame } => {
                let p = proc.as_mut().ok_or(ReplayError::NoProcess { index })?;
                let Some(frame) = readdress(p, frame) else {
                    continue;
                };
                let parsed = parse_value(frame).map_err(|e| ReplayError::BadRecord {
                    index,
                    message: e.to_string(),
                })?;
                let outs = p.core.on_frame(parsed);
                for o in outs {
                    match o {
                        Output::Emit(e) => report.events.push(e),
                        Output::Write(v) => p.spontaneous.push_back(v),
                    }
                }
            }
            Record::Exit { code } => {
                let p = proc.as_mut().ok_or(ReplayError::NoProcess { index })?;
                ensure_no_spontaneous(p, index)?;
                for o in p.core.on_exit(*code) {
                    if let Output::Emit(e) = o {
                        report.events.push(e);
                    }
                }
            }
            Record::Host { .. } => {
                return Err(ReplayError::BadRecord {
                    index,
                    message: "an SDK fixture has no host records".to_owned(),
                });
            }
            Record::Raw { .. } | Record::Note { .. } => {}
        }
    }
    finish(&mut proc, &mut report, fx.records.len())?;
    Ok(report)
}

fn finish(
    proc: &mut Option<Process>,
    report: &mut ReferenceReport,
    index: usize,
) -> Result<(), ReplayError> {
    if let Some(mut p) = proc.take() {
        ensure_no_spontaneous(&mut p, index)?;
        report.unknown_frames += p.core.unknown_frames();
    }
    Ok(())
}

fn ensure_no_spontaneous(p: &mut Process, index: usize) -> Result<(), ReplayError> {
    if p.spontaneous.is_empty() {
        return Ok(());
    }
    Err(ReplayError::ExtraWrites {
        index,
        extra: p.spontaneous.drain(..).collect(),
    })
}

/// Rewrites the request id of a CLI `control_response` from the SDK's id to the
/// core's id. Returns `None` for a response to a skipped SDK-only request.
fn readdress(p: &Process, frame: &Value) -> Option<Value> {
    if frame.get("type").and_then(Value::as_str) != Some("control_response") {
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

fn on_in(
    p: &mut Process,
    frame: &Value,
    index: usize,
    report: &mut ReferenceReport,
) -> Result<(), ReplayError> {
    let bad = |message: String| ReplayError::BadRecord { index, message };
    let kind = frame.get("type").and_then(Value::as_str).unwrap_or("");
    let cmd = match kind {
        "user" => {
            let text = frame
                .pointer("/message/content")
                .and_then(Value::as_str)
                .ok_or_else(|| bad("an SDK user message without text content".to_owned()))?;
            p.inputs += 1;
            let input_id = frame
                .get("uuid")
                .and_then(Value::as_str)
                .map_or_else(|| format!("sdk-input-{}", p.inputs), str::to_owned);
            Some(HostCmd::Prompt {
                text: text.to_owned(),
                input_id,
            })
        }
        "control_request" => {
            let sdk_id = frame
                .get("request_id")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
            let subtype = frame
                .pointer("/request/subtype")
                .and_then(Value::as_str)
                .unwrap_or("");
            let cmd = match subtype {
                "initialize" => Some(HostCmd::Initialize),
                "interrupt" => Some(HostCmd::Interrupt),
                "set_permission_mode" => Some(HostCmd::SetMode {
                    mode: frame
                        .pointer("/request/mode")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_owned(),
                }),
                other => {
                    report.sdk_only_requests.push(other.to_owned());
                    p.skipped.insert(sdk_id.clone());
                    None
                }
            };
            if cmd.is_some() {
                // The core's next host request id is assigned by `apply`; it is
                // read from the written frame below.
                p.ids.insert(sdk_id, String::new());
            }
            cmd
        }
        "control_response" => return on_sdk_answer(p, frame, index, report),
        other => return Err(bad(format!("unexpected SDK write of type {other:?}"))),
    };
    let Some(cmd) = cmd else {
        return Ok(());
    };
    let outs = p.core.apply(&cmd).map_err(|e| ReplayError::HostRejected {
        index,
        message: e.to_string(),
    })?;
    let actual = take_write(outs, report, index, frame)?;
    if kind == "control_request"
        && let (Some(sdk_id), Some(core_id)) = (
            frame.get("request_id").and_then(Value::as_str),
            actual.get("request_id").and_then(Value::as_str),
        )
    {
        p.ids.insert(sdk_id.to_owned(), core_id.to_owned());
    }
    compare(frame, &actual, index, report)
}

/// An SDK `control_response` answers a CLI request: a `can_use_tool` request
/// becomes an [`HostCmd::Answer`]; any other request was answered by the core on
/// its own (a fail-closed error), and that write must match.
fn on_sdk_answer(
    p: &mut Process,
    frame: &Value,
    index: usize,
    report: &mut ReferenceReport,
) -> Result<(), ReplayError> {
    let native = frame
        .pointer("/response/request_id")
        .and_then(Value::as_str)
        .unwrap_or("");
    if let Some(request_id) = p.core.app_request_id(native).map(str::to_owned) {
        let body = frame
            .pointer("/response/response")
            .cloned()
            .unwrap_or(Value::Null);
        let behavior = body.get("behavior").and_then(Value::as_str).unwrap_or("");
        let option_id = match behavior {
            "allow" if body.get("updatedPermissions").is_some() => "allow_always",
            "allow" => "allow",
            _ => "deny",
        };
        let cmd = HostCmd::Answer {
            request_id,
            option_id: option_id.to_owned(),
            updated_input: body.get("updatedInput").cloned(),
            message: body
                .get("message")
                .and_then(Value::as_str)
                .map(str::to_owned),
        };
        let outs = p.core.apply(&cmd).map_err(|e| ReplayError::HostRejected {
            index,
            message: e.to_string(),
        })?;
        let actual = take_write(outs, report, index, frame)?;
        return compare(frame, &actual, index, report);
    }
    let pos = p
        .spontaneous
        .iter()
        .position(|w| w.pointer("/response/request_id").and_then(Value::as_str) == Some(native))
        .ok_or_else(|| ReplayError::MissingWrite {
            index,
            expected: frame.clone(),
        })?;
    let actual = p.spontaneous.remove(pos).unwrap_or(Value::Null);
    compare(frame, &actual, index, report)
}

/// Takes the single write of a host command and keeps its events.
fn take_write(
    outs: Vec<Output>,
    report: &mut ReferenceReport,
    index: usize,
    expected: &Value,
) -> Result<Value, ReplayError> {
    let mut writes = Vec::new();
    for o in outs {
        match o {
            Output::Emit(e) => report.events.push(e),
            Output::Write(v) => writes.push(v),
        }
    }
    let mut it = writes.into_iter();
    let first = it.next().ok_or_else(|| ReplayError::MissingWrite {
        index,
        expected: expected.clone(),
    })?;
    let extra: Vec<Value> = it.collect();
    if !extra.is_empty() {
        return Err(ReplayError::ExtraWrites { index, extra });
    }
    Ok(first)
}

fn compare(
    sdk: &Value,
    core: &Value,
    index: usize,
    report: &mut ReferenceReport,
) -> Result<(), ReplayError> {
    let (expected, actual) = (semantic(sdk), semantic(core));
    if expected != actual {
        return Err(ReplayError::WriteMismatch {
            index,
            expected,
            actual,
        });
    }
    report.writes_checked += 1;
    key_differences(sdk, core, &mut report.key_differences);
    Ok(())
}

/// The behaviour-relevant projection of a host → CLI frame.
///
/// - `user`: text content and `parent_tool_use_id`.
/// - `control_request`: subtype and, for `set_permission_mode`, the mode.
/// - `control_response`: subtype and the permission body (`behavior`,
///   `updatedInput`, `message`, `toolUseID`, `updatedPermissions`). A missing
///   `interrupt` equals `interrupt: false` (the CLI default).
#[must_use]
pub fn semantic(frame: &Value) -> Value {
    let get = |p: &str| frame.pointer(p).cloned().unwrap_or(Value::Null);
    match frame.get("type").and_then(Value::as_str) {
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
            let interrupt = body
                .get("interrupt")
                .and_then(Value::as_bool)
                .unwrap_or(false);
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

/// Records wire keys that only one side sends, as `<frame kind>: <side> only <path>`.
fn key_differences(sdk: &Value, core: &Value, out: &mut BTreeSet<String>) {
    let kind = sdk.get("type").and_then(Value::as_str).unwrap_or("?");
    let kind = match kind {
        "control_request" => format!(
            "control_request/{}",
            sdk.pointer("/request/subtype")
                .and_then(Value::as_str)
                .unwrap_or("?")
        ),
        other => other.to_owned(),
    };
    for (path, s, c) in [
        ("", Some(sdk), Some(core)),
        ("request.", sdk.get("request"), core.get("request")),
        ("response.", sdk.get("response"), core.get("response")),
        (
            "response.response.",
            sdk.pointer("/response/response"),
            core.pointer("/response/response"),
        ),
    ] {
        let keys = |v: Option<&Value>| -> BTreeSet<String> {
            v.and_then(Value::as_object)
                .map(|m| m.keys().cloned().collect())
                .unwrap_or_default()
        };
        let (ks, kc) = (keys(s), keys(c));
        for k in ks.difference(&kc) {
            out.insert(format!("{kind}: sdk only {path}{k}"));
        }
        for k in kc.difference(&ks) {
            out.insert(format!("{kind}: core only {path}{k}"));
        }
    }
}
