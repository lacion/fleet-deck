//! `fake-codex app-server` scenario `sim`: a small reactive model of the
//! Codex app-server JSON-RPC protocol (ADR-002 §2), for tests that need a
//! live Codex session (driver, station, deck and board tests). The recorded
//! fixtures stay the reference for the exact frames; this model only reacts
//! to the host the same way.
//!
//! The text of `turn/start` selects the behaviour of the turn:
//! - `ask ...`: a command that needs an approval
//!   (`item/commandExecution/requestApproval`); accept runs it, decline
//!   reports a declined command, cancel interrupts the turn;
//! - `wait ...`: a turn that streams and waits for `turn/steer` (the turn
//!   then answers `steered: <text>`) or `turn/interrupt`;
//! - `question ...`: a question with the options Red and Blue
//!   (`item/tool/requestUserInput`, question id `choice`); the reply's
//!   answers end the turn with `answered: <answers>`;
//! - `exit-after <text>`: the turn answers and completes, then the process
//!   exits with code 0.
//! - `exit-leaving-child <text>`: the same, and a child of the app-server
//!   (`sleep 30`, in its process group) is still alive at the exit;
//! - `slow <ms> <text>`: the turn runs `ms` without output, then answers
//!   `echo: <text>` (a turn that ends while nobody reads stdout);
//! - `bg ...`: a command that outlives its turn; the first
//!   `thread/backgroundTerminals/list` lists it, the second completes it;
//! - `set-rollback <mode>`: sets how the next `thread/rollback` behaves
//!   (`ok`, `refuse`, `hang`, `exit`, or `ok-write`: a writer changes
//!   `a.txt` in the thread's cwd during an `ok` rollback), then echoes;
//! - anything else: a reply `echo: <text>`.
//!
//! `thread/rollback` drops `numTurns` completed turns when no turn runs.
//!
//! `thread/shellCommand` (B8) answers `{}`, then runs a turn with one
//! `userShell` command item (output `sim: <command>`, exit code 0). The
//! words in the command select: `fail` (exit code 2), `hang` (no end until
//! `turn/interrupt`), `noack` (no reply and no turn), `refuse` (an error
//! reply), `slowend` (1 s between `item/completed` and `turn/completed`),
//! `latestart` (1 s between the reply and `turn/started`, then as `hang`).
//!
//! Scenario `mcp_app` (B15, `sim_codex_mcp.rs`) adds the MCP server `demo` with
//! its app: the three app requests of the host and one `mcpToolCall` item in
//! every turn.
//!
//! `account/read` answers signed in, except under scenario
//! `auth:signed-out`. End of stdin ends the process with exit code 0.

use std::io::{BufRead, Write};
use std::process::ExitCode;

use serde_json::{Value, json};

#[path = "sim_codex_mcp.rs"]
mod mcp;

/// The first line that `sim` writes to the stdin log, before its pid.
pub const PID_LINE: &str = "# fake-codex pid ";

const SIM_MODEL: &str = "gpt-sim-1";

/// What the running turn waits for.
enum Wait {
    /// The host answer of approval request `request` for `item`.
    Approval {
        request: u64,
        item: String,
        command: String,
    },
    /// The host reply to question request `request`.
    Question { request: u64 },
    /// A steer or an interrupt.
    Host,
}

struct Turn {
    id: String,
    wait: Wait,
}

/// A command that outlived its turn.
struct Background {
    process: String,
    item: String,
    command: String,
    turn: String,
    polls: u32,
}

struct Sim<W: Write> {
    out: W,
    signed_in: bool,
    thread: Option<String>,
    model: String,
    cwd: String,
    seq: u64,
    turn: Option<Turn>,
    background: Vec<Background>,
    /// Completed turns of the thread; `thread/rollback` drops from the end.
    turns: u32,
    /// How the next `thread/rollback` behaves: `ok`, `refuse`, `hang` (no
    /// reply) or `exit` (the process ends without a reply). A prompt
    /// `set-rollback <mode>` sets it (DL-087).
    rollback: String,
    /// Scenario `mcp_app` (B15).
    mcp: bool,
}

/// Runs the model until stdin ends. `scenario` is `sim` or `auth:<state>`.
pub fn run(scenario: &str) -> ExitCode {
    let stdin = std::io::stdin();
    let mut sim = Sim {
        out: std::io::stdout().lock(),
        signed_in: scenario != "auth:signed-out",
        thread: None,
        model: SIM_MODEL.to_owned(),
        cwd: std::env::current_dir()
            .map(|d| d.to_string_lossy().into_owned())
            .unwrap_or_default(),
        seq: 0,
        turn: None,
        background: Vec::new(),
        turns: 0,
        rollback: "ok".to_owned(),
        mcp: scenario == mcp::SCENARIO,
    };
    let mut log = std::env::var_os(crate::fake::STDIN_LOG_ENV).and_then(|path| {
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .ok()
    });
    if let Some(file) = log.as_mut() {
        let _ = writeln!(file, "{PID_LINE}{}", std::process::id());
    }
    for line in stdin.lock().lines() {
        let Ok(line) = line else {
            return ExitCode::from(1);
        };
        if let Some(file) = log.as_mut() {
            let _ = file.write_all(format!("{line}\n").as_bytes());
            let _ = file.flush();
        }
        let Ok(frame) = serde_json::from_str::<Value>(&line) else {
            eprintln!("fake-codex sim: not JSON: {line}");
            continue;
        };
        if sim.on_frame(&frame).is_err() {
            return ExitCode::from(1);
        }
    }
    ExitCode::SUCCESS
}

fn str_of<'a>(v: &'a Value, path: &[&str]) -> Option<&'a str> {
    path.iter().try_fold(v, |v, k| v.get(k))?.as_str()
}

/// The text of the first text input of `params.input`.
fn input_text(params: &Value) -> String {
    params
        .get("input")
        .and_then(Value::as_array)
        .and_then(|a| a.iter().find_map(|i| i.get("text").and_then(Value::as_str)))
        .unwrap_or_default()
        .to_owned()
}

impl<W: Write> Sim<W> {
    fn emit(&mut self, frame: &Value) -> std::io::Result<()> {
        writeln!(self.out, "{frame}")?;
        self.out.flush()
    }

    fn next(&mut self) -> u64 {
        self.seq += 1;
        self.seq
    }

    fn thread_id(&self) -> String {
        self.thread.clone().unwrap_or_default()
    }

    fn reply(&mut self, id: &Value, result: Value) -> std::io::Result<()> {
        self.emit(&json!({"id": id, "result": result}))
    }

    fn error(&mut self, id: &Value, message: &str) -> std::io::Result<()> {
        self.emit(&json!({"id": id, "error": {"code": -32600, "message": message}}))
    }

    fn notify(&mut self, method: &str, mut params: Value) -> std::io::Result<()> {
        if let Some(map) = params.as_object_mut() {
            map.entry("threadId").or_insert(json!(self.thread_id()));
        }
        self.emit(&json!({"method": method, "params": params}))
    }

    fn item(&mut self, phase: &str, turn: &str, item: Value) -> std::io::Result<()> {
        self.notify(
            &format!("item/{phase}"),
            json!({"item": item, "turnId": turn}),
        )
    }

    fn on_frame(&mut self, frame: &Value) -> std::io::Result<()> {
        let id = frame.get("id").cloned().unwrap_or(Value::Null);
        let params = frame.get("params").cloned().unwrap_or(Value::Null);
        match frame.get("method").and_then(Value::as_str) {
            None if !id.is_null() => self.on_answer(&id, frame),
            None | Some("initialized") => Ok(()),
            Some("initialize") => self.reply(
                &id,
                json!({
                    "codexHome": "<home>/.codex",
                    "platformFamily": "unix",
                    "platformOs": "linux",
                    "userAgent": "fleetdeck/0.160.999 (sim) unknown",
                }),
            ),
            Some("account/read") => {
                let account = if self.signed_in {
                    json!({"type": "chatgpt", "email": "sim@example.invalid", "planType": "plus"})
                } else {
                    Value::Null
                };
                self.reply(&id, json!({"account": account, "requiresOpenaiAuth": true}))
            }
            Some(m @ ("thread/start" | "thread/resume" | "thread/fork")) => {
                self.on_thread(&id, m, &params)
            }
            Some("turn/start") => self.on_turn_start(&id, &params),
            Some("turn/steer") => self.on_steer(&id, &params),
            Some("turn/interrupt") => self.on_interrupt(&id),
            Some("thread/shellCommand") => self.on_shell(&id, &params),
            Some("thread/backgroundTerminals/list") => self.on_list(&id),
            Some("model/list") => self.reply(
                &id,
                json!({"data": [{
                    "id": SIM_MODEL,
                    "model": SIM_MODEL,
                    "displayName": "Sim model",
                    "description": "The model of the sim",
                    "hidden": false,
                    "isDefault": true,
                }], "nextCursor": null}),
            ),
            Some("thread/rollback") => self.on_rollback(&id, &params),
            Some(
                m @ ("mcpServerStatus/list" | "mcpServer/resource/read" | "mcpServer/tool/call"),
            ) if self.mcp => self.on_mcp(&id, m, &params),
            Some(_) if !id.is_null() => self.error(&id, "method not supported by the sim"),
            Some(_) => Ok(()),
        }
    }

    fn on_thread(&mut self, id: &Value, method: &str, params: &Value) -> std::io::Result<()> {
        let thread = match (method, str_of(params, &["threadId"])) {
            ("thread/resume", Some(_))
                if std::env::var(crate::fake::RESUME_FAULT_ENV).as_deref()
                    == Ok(crate::fake::RESUME_OTHER_ID) =>
            {
                format!("other-{}", std::process::id())
            }
            ("thread/resume", Some(t)) => t.to_owned(),
            _ => format!("sim-thread-{}-{}", std::process::id(), self.next()),
        };
        if let Some(m) = str_of(params, &["model"]) {
            self.model = m.to_owned();
        }
        if let Some(c) = str_of(params, &["cwd"]) {
            self.cwd = c.to_owned();
        }
        self.thread = Some(thread.clone());
        let info = json!({
            "id": thread,
            "cwd": self.cwd,
            "model": self.model,
            "ephemeral": false,
            "forkedFromId": (method == "thread/fork").then(|| str_of(params, &["threadId"])).flatten(),
        });
        self.reply(
            id,
            json!({
                "approvalPolicy": str_of(params, &["approvalPolicy"]).unwrap_or("untrusted"),
                "cwd": self.cwd,
                "model": self.model,
                "thread": info,
            }),
        )?;
        self.notify("thread/started", json!({"thread": info}))
    }

    fn on_rollback(&mut self, id: &Value, params: &Value) -> std::io::Result<()> {
        let n = params.get("numTurns").and_then(Value::as_u64).unwrap_or(0);
        if self.turn.is_some() {
            return self.error(id, "a turn is running");
        }
        match self.rollback.as_str() {
            "refuse" => return self.error(id, "rollback refused by the sim"),
            "hang" => return Ok(()),
            "exit" => return Err(std::io::Error::other("sim exits during a rollback")),
            _ => {}
        }
        if n == 0 || n > u64::from(self.turns) {
            return self.error(id, "numTurns is out of range");
        }
        self.turns -= u32::try_from(n).unwrap_or(u32::MAX);
        if self.rollback == "ok-write" {
            std::fs::write(std::path::Path::new(&self.cwd).join("a.txt"), "a writer\n")?;
        }
        let info = json!({"id": self.thread_id(), "cwd": self.cwd, "model": self.model});
        self.reply(id, json!({"thread": info}))
    }

    fn on_turn_start(&mut self, id: &Value, params: &Value) -> std::io::Result<()> {
        if self.turn.is_some() {
            return self.error(id, "a turn is already running");
        }
        if let Some(m) = str_of(params, &["model"]) {
            self.model = m.to_owned();
        }
        let turn = format!("sim-turn-{}", self.next());
        let text = input_text(params);
        let client = str_of(params, &["clientUserMessageId"]).unwrap_or_default();
        let shape = json!({"id": turn, "status": "inProgress", "items": [], "error": null});
        self.reply(id, json!({"turn": shape}))?;
        self.notify("turn/started", json!({"turn": shape}))?;
        self.user_message(&turn, client, &text)?;
        if let Some(mode) = text.strip_prefix("set-rollback ") {
            mode.trim().clone_into(&mut self.rollback);
        }
        if let Some(command) = text.strip_prefix("ask ") {
            let request = self.next();
            let item = format!("exec-sim-{request}");
            // Codex starts the command item before it asks (fixture
            // `tool_call_approve`).
            let running = command_item(&item, command, &self.cwd, "inProgress", None);
            self.item("started", &turn, running)?;
            self.emit(&json!({
                "id": request,
                "method": "item/commandExecution/requestApproval",
                "params": {
                    "availableDecisions": ["accept", "acceptForSession", "decline", "cancel"],
                    "command": command,
                    "cwd": self.cwd,
                    "itemId": item,
                    "kind": "command",
                    "threadId": self.thread_id(),
                    "turnId": turn,
                },
            }))?;
            self.turn = Some(Turn {
                id: turn,
                wait: Wait::Approval {
                    request,
                    item,
                    command: command.to_owned(),
                },
            });
            Ok(())
        } else if let Some(question) = text.strip_prefix("question ") {
            let request = self.next();
            self.emit(&json!({
                "id": request,
                "method": "item/tool/requestUserInput",
                "params": {
                    "autoResolutionMs": null,
                    "isBlocking": true,
                    "itemId": format!("call-sim-{request}"),
                    "questions": [{
                        "header": "Choice",
                        "id": "choice",
                        "isOther": true,
                        "isSecret": false,
                        "options": [
                            {"description": "Choose red.", "label": "Red"},
                            {"description": "Choose blue.", "label": "Blue"},
                        ],
                        "question": question,
                    }],
                    "threadId": self.thread_id(),
                    "turnId": turn,
                },
            }))?;
            self.turn = Some(Turn {
                id: turn,
                wait: Wait::Question { request },
            });
            Ok(())
        } else if text.starts_with("wait ") {
            let msg = format!("msg-sim-{}", self.next());
            self.item("started", &turn, agent_message(&msg, ""))?;
            self.notify(
                "item/agentMessage/delta",
                json!({"delta": "waiting", "itemId": msg, "turnId": turn}),
            )?;
            self.turn = Some(Turn {
                id: turn,
                wait: Wait::Host,
            });
            Ok(())
        } else if let Some(rest) = text.strip_prefix("slow ") {
            // `slow <ms> <text>`: the turn runs for `ms` without output,
            // then answers (a turn that ends while nobody reads stdout).
            let (ms, said) = rest.split_once(' ').unwrap_or((rest, ""));
            let ms: u64 = ms.parse().unwrap_or(1000);
            std::thread::sleep(std::time::Duration::from_millis(ms));
            self.answer(&turn, &format!("echo: {said}"), "completed")
        } else if let Some(said) = text.strip_prefix("exit-leaving-child ") {
            // `exit-leaving-child <text>`: like `exit-after`, but a child
            // in the app-server's process group (`sleep 30`) is still alive
            // when the app-server exits (DL-120).
            self.answer(&turn, &format!("echo: {said}"), "completed")?;
            let _child = std::process::Command::new("sleep")
                .arg("30")
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()?;
            std::process::exit(0)
        } else if let Some(said) = text.strip_prefix("exit-after ") {
            // `exit-after <text>`: the turn answers and completes, then the
            // app-server exits with code 0 (a provider that ends after a
            // turn while nobody reads stdout).
            self.answer(&turn, &format!("echo: {said}"), "completed")?;
            std::process::exit(0)
        } else if let Some(command) = text.strip_prefix("bg ") {
            let n = self.next();
            let item = format!("exec-sim-{n}");
            let process = format!("sim-{n}");
            let exec = command_item(&item, command, &self.cwd, "inProgress", Some(&process));
            self.item("started", &turn, exec)?;
            self.background.push(Background {
                process,
                item,
                command: command.to_owned(),
                turn: turn.clone(),
                polls: 0,
            });
            self.answer(&turn, "started in background", "completed")
        } else {
            self.answer(&turn, &format!("echo: {text}"), "completed")
        }
    }

    fn user_message(&mut self, turn: &str, client: &str, text: &str) -> std::io::Result<()> {
        let item = json!({
            "type": "userMessage",
            "id": format!("user-sim-{}", self.next()),
            "clientId": client,
            "content": [{"type": "text", "text": text, "text_elements": []}],
        });
        self.item("started", turn, item.clone())?;
        self.item("completed", turn, item)
    }

    /// An agent message, then `turn/completed` with `status`.
    fn answer(&mut self, turn: &str, text: &str, status: &str) -> std::io::Result<()> {
        if self.mcp {
            self.mcp_item(turn)?;
        }
        let msg = format!("msg-sim-{}", self.next());
        self.item("started", turn, agent_message(&msg, ""))?;
        self.notify(
            "item/agentMessage/delta",
            json!({"delta": text, "itemId": msg, "turnId": turn}),
        )?;
        let done = agent_message(&msg, text);
        self.item("completed", turn, done.clone())?;
        self.complete(turn, status, vec![done])
    }

    fn complete(&mut self, turn: &str, status: &str, items: Vec<Value>) -> std::io::Result<()> {
        self.turns += 1;
        self.turn = None;
        self.notify(
            "turn/completed",
            json!({"turn": {"id": turn, "status": status, "items": items, "error": null}}),
        )
    }

    fn on_answer(&mut self, id: &Value, frame: &Value) -> std::io::Result<()> {
        let waits_for = match &self.turn {
            Some(Turn {
                wait: Wait::Approval { request, .. } | Wait::Question { request },
                ..
            }) => Some(*request),
            _ => None,
        };
        if waits_for.is_none() || id.as_u64() != waits_for {
            return Ok(());
        }
        let Some(Turn { id: turn, wait }) = self.turn.take() else {
            return Ok(());
        };
        let (request, item, command) = match wait {
            Wait::Approval {
                request,
                item,
                command,
            } => (request, item, command),
            Wait::Question { request } => {
                self.notify("serverRequest/resolved", json!({"requestId": request}))?;
                let answers: Vec<&str> = frame
                    .pointer("/result/answers/choice/answers")
                    .and_then(Value::as_array)
                    .map(|a| a.iter().filter_map(Value::as_str).collect())
                    .unwrap_or_default();
                let text = format!("answered: {}", answers.join(", "));
                return self.answer(&turn, &text, "completed");
            }
            Wait::Host => return Ok(()),
        };
        self.notify("serverRequest/resolved", json!({"requestId": request}))?;
        let decision = str_of(frame, &["result", "decision"]).unwrap_or("cancel");
        match decision {
            "accept" | "acceptForSession" => {
                let mut done = command_item(&item, &command, &self.cwd, "completed", None);
                done["exitCode"] = json!(0);
                done["aggregatedOutput"] = json!("ok\n");
                self.item("completed", &turn, done)?;
                self.answer(&turn, &format!("ran: {command}"), "completed")
            }
            "decline" => {
                let declined = command_item(&item, &command, &self.cwd, "declined", None);
                self.item("completed", &turn, declined)?;
                self.answer(&turn, "declined", "completed")
            }
            _ => {
                let declined = command_item(&item, &command, &self.cwd, "declined", None);
                self.item("completed", &turn, declined)?;
                self.complete(&turn, "interrupted", Vec::new())
            }
        }
    }

    fn on_steer(&mut self, id: &Value, params: &Value) -> std::io::Result<()> {
        let Some(Turn {
            id: turn,
            wait: Wait::Host,
        }) = self.turn.take()
        else {
            return self.error(id, "no steerable turn");
        };
        let text = input_text(params);
        let client = str_of(params, &["clientUserMessageId"]).unwrap_or_default();
        self.reply(id, json!({"turnId": turn}))?;
        self.user_message(&turn, client, &text)?;
        self.answer(&turn, &format!("steered: {text}"), "completed")
    }

    /// `thread/shellCommand` (B8): see the module documentation.
    fn on_shell(&mut self, id: &Value, params: &Value) -> std::io::Result<()> {
        if self.turn.is_some() {
            return self.error(id, "a turn is already running");
        }
        let command = str_of(params, &["command"]).unwrap_or_default().to_owned();
        if command.contains("refuse") {
            return self.error(id, "refused");
        }
        if command.contains("noack") {
            return Ok(());
        }
        let turn = format!("sim-turn-{}", self.next());
        let shape = json!({"id": turn, "status": "inProgress", "items": [], "error": null});
        self.reply(id, json!({}))?;
        let late = command.contains("latestart");
        if late {
            std::thread::sleep(std::time::Duration::from_secs(1));
        }
        self.notify("turn/started", json!({"turn": shape}))?;
        let item = format!("shell-sim-{}", self.next());
        let mut exec = command_item(&item, &command, &self.cwd, "inProgress", None);
        exec["source"] = json!("userShell");
        self.item("started", &turn, exec.clone())?;
        if late || command.contains("hang") {
            self.turn = Some(Turn {
                id: turn,
                wait: Wait::Host,
            });
            return Ok(());
        }
        let failed = command.contains("fail");
        exec["status"] = json!(if failed { "failed" } else { "completed" });
        exec["aggregatedOutput"] = json!(format!("sim: {command}\n"));
        exec["exitCode"] = json!(if failed { 2 } else { 0 });
        self.item("completed", &turn, exec.clone())?;
        if command.contains("slowend") {
            std::thread::sleep(std::time::Duration::from_secs(1));
        }
        self.complete(&turn, "completed", vec![exec])
    }

    fn on_interrupt(&mut self, id: &Value) -> std::io::Result<()> {
        self.reply(id, json!({}))?;
        let Some(turn) = self.turn.take() else {
            return Ok(());
        };
        if let Wait::Approval { request, .. } | Wait::Question { request } = turn.wait {
            self.notify("serverRequest/resolved", json!({"requestId": request}))?;
        }
        self.complete(&turn.id, "interrupted", Vec::new())
    }

    /// Lists the background commands; each completes on its second poll.
    fn on_list(&mut self, id: &Value) -> std::io::Result<()> {
        let mut listed = Vec::new();
        let mut finished = Vec::new();
        for mut bg in std::mem::take(&mut self.background) {
            bg.polls += 1;
            if bg.polls >= 2 {
                finished.push(bg);
            } else {
                listed.push(bg);
            }
        }
        for bg in &finished {
            let mut done = command_item(
                &bg.item,
                &bg.command,
                &self.cwd,
                "completed",
                Some(&bg.process),
            );
            done["exitCode"] = json!(0);
            done["aggregatedOutput"] = json!("bg-done\n");
            self.item("completed", &bg.turn, done)?;
        }
        let data: Vec<Value> = listed
            .iter()
            .map(|bg| {
                json!({
                    "processId": bg.process,
                    "itemId": bg.item,
                    "command": bg.command,
                    "cwd": self.cwd,
                })
            })
            .collect();
        self.background = listed;
        self.reply(id, json!({"data": data, "nextCursor": null}))
    }
}

fn agent_message(id: &str, text: &str) -> Value {
    json!({
        "type": "agentMessage",
        "id": id,
        "text": text,
        "phase": "final_answer",
        "delivery": null,
        "questions": null,
    })
}

fn command_item(id: &str, command: &str, cwd: &str, status: &str, process: Option<&str>) -> Value {
    json!({
        "type": "commandExecution",
        "id": id,
        "command": command,
        "commandActions": [{"command": command, "type": "unknown"}],
        "cwd": cwd,
        "status": status,
        "exitCode": null,
        "aggregatedOutput": null,
        "processId": process,
        "source": "agent",
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sim() -> Sim<Vec<u8>> {
        Sim {
            out: Vec::new(),
            signed_in: true,
            thread: None,
            model: SIM_MODEL.to_owned(),
            cwd: "/w".to_owned(),
            seq: 0,
            turn: None,
            background: Vec::new(),
            turns: 0,
            rollback: "ok".to_owned(),
            mcp: false,
        }
    }

    fn frames(sim: &mut Sim<Vec<u8>>) -> Vec<Value> {
        let text = String::from_utf8(std::mem::take(&mut sim.out)).unwrap();
        text.lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    fn method(f: &Value) -> &str {
        f["method"].as_str().unwrap_or("")
    }

    fn start(sim: &mut Sim<Vec<u8>>, text: &str) -> Vec<Value> {
        sim.on_frame(&json!({"id": "h1", "method": "thread/start", "params": {"cwd": "/w"}}))
            .unwrap();
        frames(sim);
        sim.on_frame(&json!({"id": "h2", "method": "turn/start", "params": {
            "threadId": sim.thread_id(), "clientUserMessageId": "c1",
            "input": [{"type": "text", "text": text}]}}))
            .unwrap();
        frames(sim)
    }

    #[test]
    fn echo_turn_completes() {
        let mut s = sim();
        let out = start(&mut s, "hello");
        assert_eq!(out[0]["id"], "h2");
        let last = out.last().unwrap();
        assert_eq!(method(last), "turn/completed");
        assert_eq!(last["params"]["turn"]["items"][0]["text"], "echo: hello");
    }

    #[test]
    fn ask_waits_for_the_answer() {
        let mut s = sim();
        let out = start(&mut s, "ask touch x");
        let req = out.last().unwrap();
        assert_eq!(method(req), "item/commandExecution/requestApproval");
        let id = req["id"].clone();
        s.on_frame(&json!({"id": id, "result": {"decision": "accept"}}))
            .unwrap();
        let out = frames(&mut s);
        assert_eq!(method(&out[0]), "serverRequest/resolved");
        assert_eq!(out.last().unwrap()["params"]["turn"]["status"], "completed");
    }

    #[test]
    fn question_waits_for_the_answers() {
        let mut s = sim();
        let out = start(&mut s, "question Which color?");
        let req = out.last().unwrap();
        assert_eq!(method(req), "item/tool/requestUserInput");
        assert_eq!(req["params"]["questions"][0]["question"], "Which color?");
        let id = req["id"].clone();
        s.on_frame(&json!({"id": id, "result": {"answers": {"choice": {"answers": ["Blue"]}}}}))
            .unwrap();
        let out = frames(&mut s);
        assert_eq!(method(&out[0]), "serverRequest/resolved");
        let last = out.last().unwrap();
        assert_eq!(last["params"]["turn"]["status"], "completed");
        assert_eq!(last["params"]["turn"]["items"][0]["text"], "answered: Blue");
    }

    #[test]
    fn background_command_completes_on_the_second_poll() {
        let mut s = sim();
        start(&mut s, "bg sleep 1");
        s.on_frame(&json!({"id": "l1", "method": "thread/backgroundTerminals/list", "params": {}}))
            .unwrap();
        let first = frames(&mut s);
        assert_eq!(first[0]["result"]["data"].as_array().unwrap().len(), 1);
        s.on_frame(&json!({"id": "l2", "method": "thread/backgroundTerminals/list", "params": {}}))
            .unwrap();
        let second = frames(&mut s);
        assert_eq!(method(&second[0]), "item/completed");
        assert_eq!(second[1]["result"]["data"], json!([]));
    }

    #[test]
    fn wait_turn_ends_on_steer_or_interrupt() {
        let mut s = sim();
        start(&mut s, "wait please");
        s.on_frame(&json!({"id": "s1", "method": "turn/steer", "params": {
            "clientUserMessageId": "c2", "input": [{"type": "text", "text": "stop"}]}}))
            .unwrap();
        let out = frames(&mut s);
        assert_eq!(
            out.last().unwrap()["params"]["turn"]["items"][0]["text"],
            "steered: stop"
        );
        s.turn = None;
        s.on_frame(&json!({"id": "h3", "method": "turn/start", "params": {
            "input": [{"type": "text", "text": "wait again"}]}}))
            .unwrap();
        frames(&mut s);
        s.on_frame(&json!({"id": "i1", "method": "turn/interrupt", "params": {}}))
            .unwrap();
        let out = frames(&mut s);
        assert_eq!(
            out.last().unwrap()["params"]["turn"]["status"],
            "interrupted"
        );
    }
}
