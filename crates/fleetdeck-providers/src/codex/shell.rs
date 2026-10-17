//! `!` shell commands through `thread/shellCommand` (B8, DL-196).
//!
//! The core sends the request when it is idle and keeps one [`PendingShell`].
//! The next turn is the shell turn (`RunOrigin::Shell`); its `userShell`
//! `commandExecution` item becomes the node `shell-<effect id>`. The node
//! ends with `provider_refs.shell` = `done`, `cancelled`, `failed` or
//! `unknown`; the station settles the `shell_runs` row from that event.

use fleetdeck_proto::Event;
use fleetdeck_proto::NodeId;
use fleetdeck_proto::event::{ToolKind, ToolStatus};
use serde_json::{Value, json};

use super::core::{CodexCore, HostRequest, Output, ToolNode};
use super::map::{refs, str_at};
use super::rpc;
use crate::adapter::{ProviderError, Result};

/// The most bytes of `tool.output` of a shell node (B8).
const OUTPUT_BYTES: usize = 16 * 1024;
/// The run limit that the request names, in ms.
const TIMEOUT_MS: u64 = 600_000;

/// A `!` command that was sent and has not ended (B8).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(super) struct PendingShell {
    pub(super) effect_id: String,
    pub(super) command: String,
    /// The native turn id, once the shell turn started.
    pub(super) turn: Option<String>,
    /// The `tool.started` of the node was emitted.
    pub(super) started: bool,
    /// The `item/completed` of the command was mapped.
    #[serde(default)]
    pub(super) completed: bool,
    /// The exit code of the completed command (B8 PRE-PR r1).
    #[serde(default)]
    pub(super) exit_code: Option<i32>,
}

/// How a shell command ended without its own `item/completed`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ShellEnd {
    Cancelled,
    Failed,
    Unknown,
}

impl ShellEnd {
    fn name(self) -> &'static str {
        match self {
            Self::Cancelled => "cancelled",
            Self::Failed => "failed",
            Self::Unknown => "unknown",
        }
    }
}

/// The last `max` bytes of `text`, cut at a character boundary.
fn tail_bytes(text: &str, max: usize) -> (&str, bool) {
    if text.len() <= max {
        return (text, false);
    }
    let mut start = text.len() - max;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    (&text[start..], true)
}

fn node_of(effect_id: &str) -> NodeId {
    NodeId(format!("shell-{effect_id}"))
}

fn started_event(p: &PendingShell, item_id: Option<&str>) -> Event {
    Event::ToolStarted {
        node_id: node_of(&p.effect_id),
        parent_node_id: None,
        tool_kind: ToolKind::Shell,
        title: "Shell command".to_owned(),
        input_summary: p.command.clone(),
        files: Vec::new(),
        provider_refs: refs(&[("item_id", item_id)]),
    }
}

impl CodexCore {
    /// `thread/shellCommand` for the shell effect `effect_id` (B8).
    ///
    /// # Errors
    /// No thread yet, an ended session, a rollback without an answer, or a
    /// session that is not idle ([`ProviderError::Refused`]).
    pub fn shell_command(&mut self, effect_id: &str, command: &str) -> Result<Vec<Output>> {
        let thread = self.thread()?;
        self.no_rollback_outstanding()?;
        if self.turn.is_some()
            || self.turn_requested
            || !self.follow_ups.is_empty()
            || self.pending_shell.is_some()
        {
            return Err(ProviderError::Refused("the session is busy".into()));
        }
        let id = self.host_id(HostRequest::ShellCommand {
            effect_id: effect_id.to_owned(),
        });
        self.pending_shell = Some(PendingShell {
            effect_id: effect_id.to_owned(),
            command: command.to_owned(),
            turn: None,
            started: false,
            completed: false,
            exit_code: None,
        });
        // An interrupt before `turn/started` is held by the existing rule.
        self.turn_requested = true;
        Ok(vec![Output::Write(rpc::request(
            &id,
            "thread/shellCommand",
            json!({"threadId": thread, "command": command, "timeoutMs": TIMEOUT_MS}),
        ))])
    }

    /// The error reply of `thread/shellCommand`: the command never ran.
    pub(super) fn shell_refused(&mut self, code: i64, message: &str) -> Vec<Output> {
        let text = format!("Codex refused the command: {message} ({code})");
        let mut out = self.shell_end(ShellEnd::Failed, None, Some(text));
        self.turn_requested = false;
        out.extend(self.refuse_held_interrupt("the shell command was refused"));
        out
    }

    /// Binds the turn `native` to the pending command, when there is one that
    /// has no turn yet. Returns the input summary of the run.
    pub(super) fn bind_shell_turn(&mut self, native: &str) -> Option<String> {
        let pending = self.pending_shell.as_mut().filter(|p| p.turn.is_none())?;
        pending.turn = Some(native.to_owned());
        let summary = format!("!{}", pending.command);
        // The turn runs now: the request no longer waits for it.
        self.turn_requested = false;
        Some(summary)
    }

    /// Maps the `userShell` item of the shell turn. `None` when the item is
    /// not the one of the pending command (the generic mapping then applies).
    pub(super) fn shell_item(
        &mut self,
        p: &Value,
        item: &Value,
        item_id: &str,
        completed: bool,
    ) -> Option<Vec<Output>> {
        let pending = self.pending_shell.as_ref()?;
        if str_at(item, "type") != Some("commandExecution")
            || str_at(item, "source") != Some("userShell")
            || pending.turn.is_none()
            || str_at(p, "turnId") != pending.turn.as_deref()
        {
            return None;
        }
        let mut out = Vec::new();
        if pending.completed {
            return Some(out);
        }
        let pending = pending.clone();
        let node_id = node_of(&pending.effect_id);
        if !pending.started {
            out.push(Output::Emit(started_event(&pending, Some(item_id))));
            let order = self.tool_order();
            self.tools.insert(
                item_id.to_owned(),
                ToolNode {
                    node_id: node_id.clone(),
                    order,
                    item_type: "commandExecution".to_owned(),
                    done: false,
                    denied: false,
                    background: false,
                },
            );
            if let Some(p) = self.pending_shell.as_mut() {
                p.started = true;
            }
        }
        if !completed {
            return Some(out);
        }
        if let Some(text) = str_at(item, "aggregatedOutput").filter(|t| !t.is_empty()) {
            let (tail, cut) = tail_bytes(text, OUTPUT_BYTES);
            out.push(Output::Emit(Event::ToolOutput {
                node_id: node_id.clone(),
                chunk: tail.to_owned(),
                truncated: cut,
            }));
        }
        let exit_code = item
            .get("exitCode")
            .and_then(Value::as_i64)
            .and_then(|c| i32::try_from(c).ok());
        // (B8 PRE-PR r1) The `tool.completed` waits for the end of the turn:
        // the station releases the folder from it. The node stays open
        // until `shell_turn_end` or `shell_exit` closes it.
        if let Some(p) = self.pending_shell.as_mut() {
            p.completed = true;
            p.exit_code = exit_code;
        }
        Some(out)
    }

    /// The end of a command that did not complete: its node starts if it did
    /// not, shows `text` and fails with `end` (B8).
    pub(super) fn shell_end(
        &mut self,
        end: ShellEnd,
        exit_code: Option<i32>,
        text: Option<String>,
    ) -> Vec<Output> {
        let Some(pending) = self.pending_shell.take() else {
            return Vec::new();
        };
        let node_id = node_of(&pending.effect_id);
        let mut out = Vec::new();
        if !pending.started {
            out.push(Output::Emit(started_event(&pending, None)));
        }
        if let Some(chunk) = text {
            out.push(Output::Emit(Event::ToolOutput {
                node_id: node_id.clone(),
                chunk,
                truncated: false,
            }));
        }
        for node in self.tools.values_mut().filter(|n| n.node_id == node_id) {
            node.done = true;
        }
        out.push(Output::Emit(Event::ToolCompleted {
            node_id,
            status: ToolStatus::Failed,
            files: Vec::new(),
            exit_code,
            provider_refs: refs(&[("shell", Some(end.name()))]),
        }));
        out
    }

    /// The end of a completed command (B8 PRE-PR r1): its `tool.completed`
    /// with `done` and the exit code of the item.
    fn shell_done(&mut self) -> Vec<Output> {
        let Some(pending) = self.pending_shell.take() else {
            return Vec::new();
        };
        let node_id = node_of(&pending.effect_id);
        for node in self.tools.values_mut().filter(|n| n.node_id == node_id) {
            node.done = true;
        }
        vec![Output::Emit(Event::ToolCompleted {
            node_id,
            status: if pending.exit_code == Some(0) {
                ToolStatus::Ok
            } else {
                ToolStatus::Failed
            },
            files: Vec::new(),
            exit_code: pending.exit_code,
            provider_refs: refs(&[("shell", Some("done"))]),
        })]
    }

    /// The bound turn ended (call before the open tools close): a completed
    /// command is done; any other ends `cancelled` (interrupted) or `failed`.
    pub(super) fn shell_turn_end(&mut self, interrupted: bool) -> Vec<Output> {
        let Some(pending) = self.pending_shell.as_ref() else {
            return Vec::new();
        };
        if pending.turn.is_none() || pending.turn.as_deref() != self.native_turn.as_deref() {
            return Vec::new();
        }
        if pending.completed {
            return self.shell_done();
        }
        let end = if interrupted {
            ShellEnd::Cancelled
        } else {
            ShellEnd::Failed
        };
        self.shell_end(end, None, None)
    }

    /// The process ended (call before all tools close).
    pub(super) fn shell_exit(&mut self) -> Vec<Output> {
        match self.pending_shell.as_ref() {
            Some(p) if p.completed => self.shell_done(),
            Some(_) => self.shell_end(ShellEnd::Unknown, None, None),
            None => Vec::new(),
        }
    }
}
