//! `!` shell commands in the Codex core (B8): the request, the shell turn,
//! the node `shell-<effect id>` and its end.

use fleetdeck_proto::Event;
use fleetdeck_proto::event::{RunOrigin, ToolKind, ToolStatus};
use serde_json::{Value, json};

use super::checkpoint::CHECKPOINT_VERSION;
use super::tests::{T, complete, config, emits, feed, notify, ready_core, reply, writes};
use super::*;
use crate::adapter::ProviderError;
use crate::checkpoint::AdapterCheckpoint;

const NODE: &str = "shell-eff-1";

/// A core with `!pwd` on the wire (the effect `eff-1`).
fn sent(command: &str) -> (CodexCore, Vec<Output>) {
    let mut core = ready_core();
    let out = core.shell_command("eff-1", command, 600_000).unwrap();
    (core, out)
}

fn start_turn(core: &mut CodexCore, native: &str) -> Vec<Event> {
    emits(&notify(
        core,
        "turn/started",
        json!({"threadId": T, "turn": {"id": native}}),
    ))
}

fn shell_item(phase: &str, native: &str, output: Option<&str>, exit: Option<i64>) -> Value {
    json!({"threadId": T, "turnId": native, "item": {
        "type": "commandExecution", "id": "exec-1", "command": "pwd", "source": "userShell",
        "status": if phase == "started" { "inProgress" } else { "completed" },
        "aggregatedOutput": output, "exitCode": exit,
    }})
}

fn refs_of(e: &Event) -> Option<&serde_json::Map<String, Value>> {
    match e {
        Event::ToolCompleted { provider_refs, .. } | Event::ToolStarted { provider_refs, .. } => {
            Some(provider_refs)
        }
        _ => None,
    }
}

#[test]
fn the_request_is_a_thread_shell_command_frame() {
    let (core, out) = sent("pwd");
    let w = writes(&out);
    assert_eq!(w.len(), 1);
    assert_eq!(w[0]["method"], "thread/shellCommand");
    assert_eq!(
        w[0]["params"],
        json!({"threadId": T, "command": "pwd", "timeoutMs": 600_000})
    );
    assert!(!core.is_idle(), "the command waits for its turn");
}

#[test]
fn the_request_carries_the_run_limit_of_the_caller() {
    let mut core = ready_core();
    let out = core.shell_command("eff-1", "pwd", 60_000).unwrap();
    let w = writes(&out);
    assert_eq!(
        w[0]["params"],
        json!({"threadId": T, "command": "pwd", "timeoutMs": 60_000})
    );
}

#[test]
fn a_busy_session_refuses_a_shell_command() {
    let (mut core, _) = sent("pwd");
    let err = core.shell_command("eff-2", "ls", 600_000).unwrap_err();
    assert!(
        matches!(&err, ProviderError::Refused(m) if m == "the session is busy"),
        "{err:?}"
    );
    let mut core = ready_core();
    core.prompt(&super::tests::input("hi", "in-1")).unwrap();
    assert!(matches!(
        core.shell_command("eff-1", "pwd", 600_000),
        Err(ProviderError::Refused(_))
    ));
}

#[test]
fn an_error_response_fails_the_node_and_refuses_a_held_interrupt() {
    let (mut core, out) = sent("pwd");
    core.interrupt(Some("ctl-1".into())).unwrap();
    let id = writes(&out)[0]["id"].clone();
    let ev = emits(&feed(
        &mut core,
        json!({"id": id, "error": {"code": -32600, "message": "no"}}),
    ));
    assert!(
        matches!(&ev[0], Event::ToolStarted { tool_kind: ToolKind::Shell, title, input_summary, .. }
        if title == "Shell command" && input_summary == "pwd"),
        "{ev:?}"
    );
    assert!(
        matches!(&ev[1], Event::ToolOutput { chunk, truncated: false, .. }
        if chunk == "Codex refused the command: no (-32600)"),
        "{ev:?}"
    );
    assert!(
        matches!(&ev[2], Event::ToolCompleted { node_id, status: ToolStatus::Failed, .. }
        if node_id.as_str() == NODE)
    );
    assert_eq!(refs_of(&ev[2]).unwrap()["shell"], "failed");
    assert!(ev.iter().any(|e| matches!(
        e,
        Event::InterruptResult { accepted: false, control_id: Some(c), .. } if c == "ctl-1"
    )));
    assert!(core.is_idle());
    assert!(core.shell_command("eff-2", "ls", 600_000).is_ok());
}

#[test]
fn the_bound_turn_is_a_shell_run() {
    let (mut core, out) = sent("pwd");
    assert!(reply(&mut core, &out, json!({})).is_empty());
    let ev = start_turn(&mut core, "n1");
    assert!(
        matches!(&ev[0], Event::RunStarted { origin: RunOrigin::Shell, input_summary, input_id: None, input_text: None, .. }
        if input_summary == "!pwd"),
        "{ev:?}"
    );
    assert!(matches!(&ev[1], Event::TurnStarted { .. }));
}

#[test]
fn a_user_shell_item_is_the_shell_node() {
    let (mut core, out) = sent("pwd");
    reply(&mut core, &out, json!({}));
    start_turn(&mut core, "n1");
    let ev = emits(&notify(
        &mut core,
        "item/started",
        shell_item("started", "n1", None, None),
    ));
    assert_eq!(ev.len(), 1, "{ev:?}");
    assert!(
        matches!(&ev[0], Event::ToolStarted { node_id, parent_node_id: None, tool_kind: ToolKind::Shell, title, input_summary, files, .. }
        if node_id.as_str() == NODE && title == "Shell command" && input_summary == "pwd" && files.is_empty())
    );
    assert_eq!(refs_of(&ev[0]).unwrap()["item_id"], "exec-1");
    // The output is cut to the last 16 KiB at a character boundary.
    let text = format!("start{}", "é".repeat(9000));
    let ev = emits(&notify(
        &mut core,
        "item/completed",
        shell_item("completed", "n1", Some(&text), Some(0)),
    ));
    let Event::ToolOutput {
        chunk, truncated, ..
    } = &ev[0]
    else {
        panic!("{ev:?}")
    };
    assert!(*truncated);
    assert!(chunk.len() <= 16 * 1024 && chunk.len() > 16 * 1024 - 2);
    assert!(chunk.chars().all(|c| c == 'é'));
    // (B8 PRE-PR r1) The item completion gives the output and no completion.
    assert_eq!(ev.len(), 1, "{ev:?}");
    assert!(
        core.pending_shell.is_some(),
        "the command waits for its turn end"
    );
    assert!(core.shell_command("eff-2", "ls", 600_000).is_err());
    // The turn ends: the node completes once, `done`, with the exit code.
    let end = emits(&complete(&mut core, "n1", "completed"));
    let done: Vec<_> = end
        .iter()
        .filter(|e| matches!(e, Event::ToolCompleted { .. }))
        .collect();
    assert_eq!(done.len(), 1, "{end:?}");
    assert!(
        matches!(done[0], Event::ToolCompleted { node_id, status: ToolStatus::Ok, exit_code: Some(0), .. }
        if node_id.as_str() == NODE)
    );
    assert_eq!(refs_of(done[0]).unwrap()["shell"], "done");
    let pos = |f: fn(&Event) -> bool| end.iter().position(f).unwrap();
    assert!(
        pos(|e| matches!(e, Event::ToolCompleted { .. }))
            < pos(|e| matches!(e, Event::TurnCompleted { .. }))
    );
    assert!(core.is_idle(), "the shell turn leaves the core idle");
    assert!(core.shell_command("eff-2", "ls", 600_000).is_ok());
}

#[test]
fn a_failed_command_and_a_completed_item_without_a_started_one() {
    let (mut core, out) = sent("pwd");
    reply(&mut core, &out, json!({}));
    start_turn(&mut core, "n1");
    let ev = emits(&notify(
        &mut core,
        "item/completed",
        shell_item("completed", "n1", Some(""), Some(2)),
    ));
    // started first, no output event for an empty text, no completion yet
    assert!(matches!(&ev[0], Event::ToolStarted { .. }), "{ev:?}");
    assert_eq!(ev.len(), 1, "{ev:?}");
    let end = emits(&complete(&mut core, "n1", "completed"));
    assert!(
        end.iter().any(|e| matches!(
            e,
            Event::ToolCompleted {
                status: ToolStatus::Failed,
                exit_code: Some(2),
                ..
            }
        )),
        "{end:?}"
    );
}

#[test]
fn an_agent_command_item_keeps_the_generic_mapping() {
    let (mut core, out) = sent("pwd");
    reply(&mut core, &out, json!({}));
    start_turn(&mut core, "n1");
    let ev = emits(&notify(
        &mut core,
        "item/started",
        json!({"threadId": T, "turnId": "n1", "item": {
            "type": "commandExecution", "id": "exec-9", "command": "ls", "source": "agent"}}),
    ));
    assert!(
        matches!(&ev[0], Event::ToolStarted { node_id, title, .. }
        if node_id.as_str() == "node-2" && title == "Shell"),
        "{ev:?}"
    );
}

#[test]
fn an_interrupt_before_the_turn_starts_is_held_and_sent_at_turn_started() {
    let (mut core, out) = sent("pwd");
    assert!(core.interrupt(Some("ctl-1".into())).unwrap().is_empty());
    reply(&mut core, &out, json!({}));
    let after = notify(
        &mut core,
        "turn/started",
        json!({"threadId": T, "turn": {"id": "n1"}}),
    );
    let w = writes(&after);
    assert_eq!(w.len(), 1);
    assert_eq!(w[0]["method"], "turn/interrupt");
    assert_eq!(w[0]["params"]["turnId"], "n1");
}

#[test]
fn an_interrupted_turn_without_the_item_completion_cancels_the_node() {
    let (mut core, out) = sent("pwd");
    reply(&mut core, &out, json!({}));
    start_turn(&mut core, "n1");
    notify(
        &mut core,
        "item/started",
        shell_item("started", "n1", None, None),
    );
    let ev = emits(&complete(&mut core, "n1", "interrupted"));
    let done: Vec<_> = ev
        .iter()
        .filter(|e| matches!(e, Event::ToolCompleted { .. }))
        .collect();
    assert_eq!(done.len(), 1, "closed once: {ev:?}");
    assert!(
        matches!(done[0], Event::ToolCompleted { node_id, status: ToolStatus::Failed, exit_code: None, .. }
        if node_id.as_str() == NODE)
    );
    assert_eq!(refs_of(done[0]).unwrap()["shell"], "cancelled");
    let pos = |f: fn(&Event) -> bool| ev.iter().position(f).unwrap();
    assert!(
        pos(|e| matches!(e, Event::ToolCompleted { .. }))
            < pos(|e| matches!(e, Event::TurnCompleted { .. }))
    );
    assert!(core.is_idle());
}

#[test]
fn a_failed_turn_without_the_item_completion_fails_the_node() {
    let (mut core, out) = sent("pwd");
    reply(&mut core, &out, json!({}));
    start_turn(&mut core, "n1");
    let ev = emits(&complete(&mut core, "n1", "failed"));
    // The node never started: it starts and fails in one step.
    assert!(matches!(&ev[0], Event::ToolStarted { node_id, .. } if node_id.as_str() == NODE));
    assert_eq!(refs_of(&ev[1]).unwrap()["shell"], "failed");
}

#[test]
fn a_process_exit_ends_the_node_unknown_or_done_when_the_item_completed() {
    let (mut core, _) = sent("pwd");
    let ev = emits(&core.on_exit(Some(1)));
    let done = ev
        .iter()
        .find(|e| matches!(e, Event::ToolCompleted { .. }))
        .unwrap();
    assert_eq!(refs_of(done).unwrap()["shell"], "unknown");

    let (mut core, out) = sent("pwd");
    reply(&mut core, &out, json!({}));
    start_turn(&mut core, "n1");
    notify(
        &mut core,
        "item/completed",
        shell_item("completed", "n1", Some("x"), Some(0)),
    );
    let ev = emits(&core.on_exit(Some(1)));
    let done: Vec<_> = ev
        .iter()
        .filter(|e| matches!(e, Event::ToolCompleted { .. }))
        .collect();
    assert_eq!(done.len(), 1, "{ev:?}");
    assert_eq!(refs_of(done[0]).unwrap()["shell"], "done");
}

#[test]
fn a_nonzero_exit_code_fails_the_node_at_the_turn_end() {
    let (mut core, out) = sent("pwd");
    reply(&mut core, &out, json!({}));
    start_turn(&mut core, "n1");
    notify(
        &mut core,
        "item/completed",
        shell_item("completed", "n1", Some("x"), Some(2)),
    );
    let end = emits(&complete(&mut core, "n1", "completed"));
    let done = end
        .iter()
        .find(|e| matches!(e, Event::ToolCompleted { .. }))
        .unwrap();
    assert!(matches!(
        done,
        Event::ToolCompleted {
            status: ToolStatus::Failed,
            exit_code: Some(2),
            ..
        }
    ));
    assert_eq!(refs_of(done).unwrap()["shell"], "done");
}

#[test]
fn a_checkpoint_keeps_the_exit_code_of_a_completed_command() {
    let (mut core, out) = sent("pwd");
    reply(&mut core, &out, json!({}));
    start_turn(&mut core, "n1");
    notify(
        &mut core,
        "item/completed",
        shell_item("completed", "n1", Some("x"), Some(2)),
    );
    let text = core.checkpoint().unwrap().to_json().unwrap();
    let core =
        CodexCore::restore(config("/w"), &AdapterCheckpoint::from_json(&text).unwrap()).unwrap();
    let pending = core.pending_shell.as_ref().unwrap();
    assert!(pending.completed);
    assert_eq!(pending.exit_code, Some(2));
}

#[test]
fn a_checkpoint_keeps_the_pending_shell_and_an_old_one_has_none() {
    let (core, out) = sent("pwd");
    let checkpoint = core.checkpoint().unwrap();
    assert_eq!(checkpoint.version, CHECKPOINT_VERSION);
    let text = checkpoint.to_json().unwrap();
    let mut core =
        CodexCore::restore(config("/w"), &AdapterCheckpoint::from_json(&text).unwrap()).unwrap();
    assert_eq!(core.pending_shell.as_ref().unwrap().effect_id, "eff-1");
    // The reply of the restored request still finds its correlation.
    assert!(reply(&mut core, &out, json!({})).is_empty());
    let ev = start_turn(&mut core, "n1");
    assert!(matches!(
        &ev[0],
        Event::RunStarted {
            origin: RunOrigin::Shell,
            ..
        }
    ));

    let mut old = ready_core().checkpoint().unwrap();
    old.state.as_object_mut().unwrap().remove("pending_shell");
    let text = old.to_json().unwrap();
    let core =
        CodexCore::restore(config("/w"), &AdapterCheckpoint::from_json(&text).unwrap()).unwrap();
    assert!(core.pending_shell.is_none());
}
