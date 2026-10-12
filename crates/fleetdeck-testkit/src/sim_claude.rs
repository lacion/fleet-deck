//! `fake-claude` scenario `sim`: a small reactive model of the Claude CLI
//! stream-json protocol (ADR-001 §2), for tests that mint their own ids
//! (session manager, local CLI, deck and board end-to-end tests). The
//! recorded fixtures stay the reference for the exact frames; this model
//! only reacts to the host the same way.
//!
//! The prompt text selects the behaviour of the turn:
//! - `ask ...`: a `Bash` tool call that needs an approval; allow runs it,
//!   deny reports a denied tool;
//! - `wait ...`: a turn that streams and waits until the host interrupts;
//! - anything else: a reply `echo: <text>`.
//!
//! Inputs that arrive while a turn runs are queued and run in order.
//! End of stdin ends the process with exit code 0, after the linger time of
//! [`crate::fake::LINGER_ENV`] if that variable is set.

use std::collections::VecDeque;
use std::io::{BufRead, Write};
use std::process::ExitCode;

use serde_json::{Value, json};

const SIM_MODEL: &str = "claude-sim-1";

/// One user message that waits for its turn.
struct Input {
    uuid: String,
    text: String,
}

/// What the current turn waits for.
enum Turn {
    /// An approval of `tool_use_id` (control request `request_id`).
    Approval {
        input: Input,
        request_id: String,
        tool_use_id: String,
        command: String,
    },
    /// A host interrupt.
    Interrupt { input: Input },
}

struct Sim<W: Write> {
    out: W,
    session_id: String,
    mode: String,
    model: String,
    started: bool,
    seq: u64,
    turn: Option<Turn>,
    queue: VecDeque<Input>,
}

/// The first line that `sim` writes to the stdin log, before its pid.
pub const PID_LINE: &str = "# fake-claude pid ";

/// Runs the model until stdin ends.
pub fn run() -> ExitCode {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mode = flag_value("--permission-mode").unwrap_or_else(|| "default".to_owned());
    let model = flag_value("--model").unwrap_or_else(|| SIM_MODEL.to_owned());
    let mut sim = Sim {
        out: stdout.lock(),
        // `--resume <id>` continues that native session (DL-118).
        session_id: flag_value("--resume").unwrap_or_else(|| format!("sim-{}", std::process::id())),
        mode,
        model,
        started: false,
        seq: 0,
        turn: None,
        queue: VecDeque::new(),
    };
    let mut log = std::env::var_os(crate::fake::STDIN_LOG_ENV).and_then(|path| {
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .ok()
    });
    if let Some(file) = log.as_mut() {
        // Fault tests find the process to kill here.
        let _ = writeln!(file, "{PID_LINE}{}", std::process::id());
    }
    for line in stdin.lock().lines() {
        let Ok(line) = line else {
            return ExitCode::from(1);
        };
        if let Some(file) = log.as_mut() {
            // One write per line, flushed before the model reacts.
            let _ = file.write_all(format!("{line}\n").as_bytes());
            let _ = file.flush();
        }
        let Ok(frame) = serde_json::from_str::<Value>(&line) else {
            eprintln!("fake-claude sim: not JSON: {line}");
            continue;
        };
        if sim.on_frame(&frame).is_err() {
            return ExitCode::from(1);
        }
    }
    linger();
    ExitCode::SUCCESS
}

/// Waits the time of [`crate::fake::LINGER_ENV`], if it is set to a number.
fn linger() {
    let Ok(value) = std::env::var(crate::fake::LINGER_ENV) else {
        return;
    };
    match value.trim().parse::<u64>() {
        Ok(ms) => std::thread::sleep(std::time::Duration::from_millis(ms)),
        Err(_) => eprintln!(
            "fake-claude sim: {} is not a number: {value}",
            crate::fake::LINGER_ENV
        ),
    }
}

fn flag_value(flag: &str) -> Option<String> {
    let args: Vec<String> = std::env::args().collect();
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

fn str_of<'a>(v: &'a Value, path: &[&str]) -> Option<&'a str> {
    path.iter().try_fold(v, |v, k| v.get(k))?.as_str()
}

impl<W: Write> Sim<W> {
    fn emit(&mut self, frame: &Value) -> std::io::Result<()> {
        writeln!(self.out, "{frame}")?;
        self.out.flush()
    }

    fn next_id(&mut self, prefix: &str) -> String {
        self.seq += 1;
        format!("{prefix}{}", self.seq)
    }

    fn on_frame(&mut self, frame: &Value) -> std::io::Result<()> {
        match frame.get("type").and_then(Value::as_str) {
            Some("control_request") => self.on_control_request(frame),
            Some("control_response") => self.on_control_response(frame),
            Some("user") => {
                let input = Input {
                    uuid: str_of(frame, &["uuid"]).unwrap_or_default().to_owned(),
                    text: str_of(frame, &["message", "content"])
                        .unwrap_or_default()
                        .to_owned(),
                };
                self.lifecycle(&input.uuid, "queued")?;
                self.queue.push_back(input);
                self.next_turn()
            }
            _ => Ok(()),
        }
    }

    fn control_response(&mut self, request_id: &str, response: Value) -> std::io::Result<()> {
        self.emit(&json!({"type": "control_response", "response": {
            "subtype": "success", "request_id": request_id, "response": response}}))
    }

    fn on_control_request(&mut self, frame: &Value) -> std::io::Result<()> {
        let id = str_of(frame, &["request_id"])
            .unwrap_or_default()
            .to_owned();
        match str_of(frame, &["request", "subtype"]).unwrap_or_default() {
            "initialize" => {
                let mode = self.mode.clone();
                self.control_response(
                    &id,
                    json!({
                        "models": [
                            {"value": "default", "displayName": "Default"},
                            {"value": "haiku", "resolvedModel": SIM_MODEL},
                        ],
                        "current_permission_mode": mode,
                    }),
                )
            }
            "interrupt" => {
                self.control_response(&id, json!({"still_queued": []}))?;
                match self.turn.take() {
                    Some(Turn::Interrupt { input }) => self.interrupted(&input),
                    Some(Turn::Approval {
                        input, request_id, ..
                    }) => {
                        self.emit(&json!({"type": "control_cancel_request",
                                          "request_id": request_id}))?;
                        self.interrupted(&input)
                    }
                    None => Ok(()),
                }
            }
            "set_permission_mode" => {
                self.mode = str_of(frame, &["request", "mode"])
                    .unwrap_or_default()
                    .to_owned();
                self.control_response(&id, json!({}))
            }
            "set_model" => {
                if let Some(model) = str_of(frame, &["request", "model"]) {
                    self.model = model.to_owned();
                }
                self.control_response(&id, json!({}))
            }
            other => self.emit(&json!({"type": "control_response", "response": {
                "subtype": "error", "request_id": id,
                "error": format!("unknown control request `{other}`")}})),
        }
    }

    fn on_control_response(&mut self, frame: &Value) -> std::io::Result<()> {
        let id = str_of(frame, &["response", "request_id"]).unwrap_or_default();
        let Some(Turn::Approval { request_id, .. }) = &self.turn else {
            return Ok(());
        };
        if id != request_id {
            return Ok(());
        }
        let Some(Turn::Approval {
            input,
            tool_use_id,
            command,
            ..
        }) = self.turn.take()
        else {
            return Ok(());
        };
        let allowed = str_of(frame, &["response", "response", "behavior"]) == Some("allow");
        let (content, is_error) = if allowed {
            (format!("ran: {command}"), false)
        } else {
            ("The user denied this tool use.".to_owned(), true)
        };
        let uuid = self.next_id("sim-msg-");
        let session = self.session_id.clone();
        self.emit(&json!({"type": "user", "session_id": session, "uuid": uuid,
            "parent_tool_use_id": null,
            "message": {"role": "user", "content": [{"type": "tool_result",
                "tool_use_id": tool_use_id, "content": content, "is_error": is_error}]}}))?;
        let reply = if allowed { "done" } else { "denied" };
        self.finish(&input, reply)
    }

    fn lifecycle(&mut self, uuid: &str, state: &str) -> std::io::Result<()> {
        let session = self.session_id.clone();
        let id = self.next_id("sim-lc-");
        self.emit(&json!({"type": "command_lifecycle", "command_uuid": uuid,
            "state": state, "session_id": session, "uuid": id}))
    }

    fn next_turn(&mut self) -> std::io::Result<()> {
        if self.turn.is_some() {
            return Ok(());
        }
        let Some(input) = self.queue.pop_front() else {
            return Ok(());
        };
        self.lifecycle(&input.uuid, "started")?;
        let session = self.session_id.clone();
        if !self.started {
            self.started = true;
            let (model, mode) = (self.model.clone(), self.mode.clone());
            let cwd = std::env::current_dir()
                .map(|d| d.to_string_lossy().into_owned())
                .unwrap_or_default();
            self.emit(
                &json!({"type": "system", "subtype": "init", "session_id": session,
                "model": model, "permissionMode": mode, "cwd": cwd,
                "claude_code_version": "2.1.999", "tools": ["Bash"]}),
            )?;
        }
        self.emit(
            &json!({"type": "user", "isReplay": true, "session_id": session,
            "uuid": input.uuid, "parent_tool_use_id": null,
            "message": {"role": "user", "content": input.text}}),
        )?;
        if let Some(rest) = input.text.strip_prefix("ask") {
            let command = match rest.trim() {
                "" => "echo ok".to_owned(),
                c => c.to_owned(),
            };
            let tool_use_id = self.next_id("toolu_sim_");
            let request_id = self.next_id("sim-req-");
            self.assistant(
                json!([{"type": "tool_use", "id": tool_use_id, "name": "Bash",
                "input": {"command": command}}]),
            )?;
            self.emit(&json!({"type": "control_request", "request_id": request_id,
                "request": {"subtype": "can_use_tool", "tool_name": "Bash",
                    "input": {"command": command}, "tool_use_id": tool_use_id}}))?;
            self.turn = Some(Turn::Approval {
                input,
                request_id,
                tool_use_id,
                command,
            });
            Ok(())
        } else if input.text.starts_with("wait") {
            self.emit(&json!({"type": "stream_event", "parent_tool_use_id": null,
                "session_id": session, "event": {"type": "content_block_delta",
                "index": 0, "delta": {"type": "text_delta", "text": "working"}}}))?;
            self.turn = Some(Turn::Interrupt { input });
            Ok(())
        } else {
            let reply = format!("echo: {}", input.text);
            self.finish(&input, &reply)
        }
    }

    fn assistant(&mut self, content: Value) -> std::io::Result<()> {
        let session = self.session_id.clone();
        let (id, uuid) = (self.next_id("msg_sim_"), self.next_id("sim-msg-"));
        let model = self.model.clone();
        self.emit(
            &json!({"type": "assistant", "session_id": session, "uuid": uuid,
            "parent_tool_use_id": null, "message": {"id": id, "role": "assistant",
            "model": model, "content": content}}),
        )
    }

    fn result(&mut self, subtype: &str, reason: &str, text: &str) -> std::io::Result<()> {
        let session = self.session_id.clone();
        let is_error = subtype != "success";
        self.emit(
            &json!({"type": "result", "subtype": subtype, "is_error": is_error,
            "session_id": session, "stop_reason": null, "terminal_reason": reason,
            "total_cost_usd": 0.0, "num_turns": 1, "result": text,
            "usage": {"input_tokens": 3, "output_tokens": 2}}),
        )
    }

    fn finish(&mut self, input: &Input, reply: &str) -> std::io::Result<()> {
        self.assistant(json!([{"type": "text", "text": reply}]))?;
        self.result("success", "completed", reply)?;
        self.lifecycle(&input.uuid, "completed")?;
        self.next_turn()
    }

    fn interrupted(&mut self, input: &Input) -> std::io::Result<()> {
        self.result("error_during_execution", "aborted_streaming", "")?;
        self.lifecycle(&input.uuid, "cancelled")?;
        self.next_turn()
    }
}

/// What the `sim` processes that shared one stdin log received.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StdinLog {
    /// The process ids, in start order.
    pub pids: Vec<u32>,
    /// Every stdin line of every process.
    pub lines: Vec<String>,
}

impl StdinLog {
    /// Reads a log; a missing file is an empty log.
    #[must_use]
    pub fn read(path: &std::path::Path) -> Self {
        let text = std::fs::read_to_string(path).unwrap_or_default();
        let mut log = Self::default();
        for line in text.lines() {
            match line.strip_prefix(PID_LINE) {
                Some(pid) => log.pids.extend(pid.trim().parse::<u32>().ok()),
                None => log.lines.push(line.to_owned()),
            }
        }
        log
    }

    /// The number of stdin lines that contain `needle`.
    #[must_use]
    pub fn count(&self, needle: &str) -> usize {
        self.lines.iter().filter(|l| l.contains(needle)).count()
    }
}
