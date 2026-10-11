//! Scripted live scenarios: the PLAN §13 shared scenarios plus the S2 extras of
//! PLAN §14 Phase 0 (questions over both channels, interrupt during a tool,
//! set mode, open approval across a restart, diffs).

use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use anyhow::{Result, bail, ensure};
use spike_canon::{CanonEvent, TurnStatus};

use super::Ctx;
use super::extra::{
    approval_restart, file_change, interrupt_during_tool, interrupt_right_after_send, plan_capture,
    question_rpc, set_mode,
};
use crate::adapter::HostCmd;

type Run = for<'a> fn(&'a mut Ctx) -> Pin<Box<dyn Future<Output = Result<()>> + 'a>>;

/// One live scenario.
pub struct Scenario {
    pub name: &'static str,
    /// True for a PLAN §13 shared scenario.
    pub shared: bool,
    pub notes: &'static str,
    pub run: Run,
}

macro_rules! scenario {
    ($name:literal, $shared:expr, $notes:literal, $f:ident) => {
        Scenario {
            name: $name,
            shared: $shared,
            notes: $notes,
            run: |c| Box::pin($f(c)),
        }
    };
}

/// The approval policy and sandbox of most scenarios: every command that is
/// not known to be read-only asks first.
pub const MODE: &str = "untrusted/workspace-write";

/// A command that writes, so `untrusted` asks for an approval.
pub const TOUCH_ECHO: &str = "Use the shell tool to run exactly this command: touch fd-approve.txt && echo approve-ok\nThen reply with the command output only.";

/// A turn long enough to interrupt or steer.
pub const COUNT: &str =
    "Do not use any tools. Count from 1 to 60, one number per line, then reply done.";

/// All scenarios, in recording order.
pub const SCENARIOS: &[Scenario] = &[
    scenario!(
        "simple",
        true,
        "One prompt, one turn, deltas, turn/completed.",
        simple
    ),
    scenario!(
        "multi_turn",
        true,
        "Two turns on one thread; the second turn uses the first.",
        multi_turn
    ),
    scenario!(
        "queued_turn",
        true,
        "A second prompt while the first turn runs: the core holds it host-side (turn/start during a turn would join the running turn) and starts it after turn/completed.",
        queued_turn
    ),
    scenario!(
        "turn_interrupt",
        true,
        "turn/interrupt during streamed text, then a follow-up turn.",
        turn_interrupt
    ),
    scenario!(
        "message_steering",
        true,
        "turn/steer with expectedTurnId while the turn streams: the text joins the running turn.",
        message_steering
    ),
    scenario!(
        "provider_thread_resume",
        true,
        "Process 1 runs a turn and exits; process 2 sends thread/resume and uses the first turn.",
        provider_thread_resume
    ),
    scenario!(
        "tool_call_approve",
        true,
        "untrusted policy: a writing command asks item/commandExecution/requestApproval; the host answers accept.",
        tool_call_approve
    ),
    scenario!(
        "tool_call_deny",
        true,
        "untrusted policy: the host answers decline; the command item ends declined.",
        tool_call_deny
    ),
    scenario!(
        "question_rpc",
        false,
        "Plan collaboration mode: item/tool/requestUserInput, answered by JSON-RPC reply (L4 rpc_reply channel).",
        question_rpc
    ),
    scenario!(
        "plan_capture",
        false,
        "Plan collaboration mode: the plan arrives as a plan item.",
        plan_capture
    ),
    scenario!(
        "file_change",
        false,
        "The model edits a file with apply_patch: fileChange items, turn/diff/updated, file.changed.",
        file_change
    ),
    scenario!(
        "interrupt_during_tool",
        false,
        "An approved long command runs; turn/interrupt ends the turn and the command item.",
        interrupt_during_tool
    ),
    scenario!(
        "interrupt_right_after_send",
        false,
        "Trap 3: interrupt before the turn id is known; the core holds it until the turn/start reply.",
        interrupt_right_after_send
    ),
    scenario!(
        "set_mode",
        false,
        "The mode changes between turns; turn/start carries approvalPolicy + sandboxPolicy.",
        set_mode
    ),
    scenario!(
        "approval_restart",
        false,
        "Kill app-server while a command approval is open; a new process resumes the thread: is the request asked again?",
        approval_restart
    ),
];

async fn simple(c: &mut Ctx) -> Result<()> {
    let mut s = c.start(MODE, None).await?;
    c.prompt(&mut s, "Reply with exactly: ok").await?;
    c.turn_done(&mut s).await?;
    c.idle(&mut s).await?;
    c.close(s).await
}

async fn multi_turn(c: &mut Ctx) -> Result<()> {
    let mut s = c.start(MODE, None).await?;
    c.prompt(
        &mut s,
        "Remember the word maple. Do not use tools. Reply with exactly: noted",
    )
    .await?;
    c.turn_done(&mut s).await?;
    c.prompt(
        &mut s,
        "Which word did I ask you to remember? Reply with the word only.",
    )
    .await?;
    c.turn_done(&mut s).await?;
    c.idle(&mut s).await?;
    c.close(s).await
}

async fn queued_turn(c: &mut Ctx) -> Result<()> {
    let mut s = c.start(MODE, None).await?;
    c.prompt(&mut s, COUNT).await?;
    c.prompt(&mut s, "Do not use tools. Reply with exactly: second")
        .await?;
    let pending = s.core.pending_inputs();
    c.note(format!("pending inputs after two prompts: {pending}"));
    let first = c.turn_done(&mut s).await?;
    let second = c.turn_done(&mut s).await?;
    c.note(format!("turns: {first:?}, {second:?}"));
    c.idle(&mut s).await?;
    c.close(s).await
}

async fn turn_interrupt(c: &mut Ctx) -> Result<()> {
    let mut s = c.start(MODE, None).await?;
    c.prompt(&mut s, COUNT).await?;
    c.wait_for(&mut s, |e| matches!(e, CanonEvent::MessageDelta { .. }))
        .await?;
    s.cmd(HostCmd::Interrupt, &mut c.log).await?;
    let status = c.turn_done(&mut s).await?;
    ensure!(status == TurnStatus::Interrupted, "status {status:?}");
    c.prompt(&mut s, "Do not use tools. Reply with exactly: after")
        .await?;
    c.turn_done(&mut s).await?;
    c.idle(&mut s).await?;
    c.close(s).await
}

async fn message_steering(c: &mut Ctx) -> Result<()> {
    let mut s = c.start(MODE, None).await?;
    c.prompt(&mut s, COUNT).await?;
    c.wait_for(&mut s, |e| matches!(e, CanonEvent::MessageDelta { .. }))
        .await?;
    c.steer(
        &mut s,
        "Change of plan: stop counting now and reply with exactly: banana",
    )
    .await?;
    c.turn_done(&mut s).await?;
    c.idle(&mut s).await?;
    let turns = s
        .events
        .iter()
        .filter(|e| matches!(e, CanonEvent::TurnStarted { .. }))
        .count();
    c.note(format!("turns started: {turns}"));
    c.close(s).await
}

async fn provider_thread_resume(c: &mut Ctx) -> Result<()> {
    let mut s = c.start(MODE, None).await?;
    c.prompt(
        &mut s,
        "Remember the word maple. Do not use tools. Reply with exactly: noted",
    )
    .await?;
    c.turn_done(&mut s).await?;
    c.idle(&mut s).await?;
    let Some(thread) = s.core.thread_id().map(str::to_owned) else {
        bail!("no thread id");
    };
    c.close(s).await?;
    let mut s = c.start(MODE, Some(thread)).await?;
    c.prompt(
        &mut s,
        "Which word did I ask you to remember? Reply with the word only.",
    )
    .await?;
    c.turn_done(&mut s).await?;
    c.idle(&mut s).await?;
    c.close(s).await
}

async fn tool_call(c: &mut Ctx, option: &str) -> Result<()> {
    let mut s = c.start(MODE, None).await?;
    c.prompt(&mut s, TOUCH_ECHO).await?;
    let (req, params) = c.approval(&mut s).await?;
    let option = deny_or(option, &params);
    c.note(format!(
        "offered decisions: {}",
        params["availableDecisions"]
    ));
    c.answer(&mut s, &req, option, None).await?;
    c.idle_answering(&mut s, Some(option)).await?;
    let exists = c.workdir.join("fd-approve.txt").exists();
    c.note(format!("fd-approve.txt exists after {option}: {exists}"));
    c.close(s).await
}

/// The deny option of a command approval: `decline` (deny, the turn goes on)
/// when offered, else `cancel` (deny and end the turn). Codex 0.160.0 offers no
/// `decline` under the `untrusted` policy (fixture `tool_call_deny`).
pub(super) fn deny_or<'a>(option: &'a str, params: &serde_json::Value) -> &'a str {
    if option != "decline" {
        return option;
    }
    let offered = params["availableDecisions"]
        .as_array()
        .is_some_and(|a| a.iter().any(|d| d == "decline"));
    if offered { "decline" } else { "cancel" }
}

async fn tool_call_approve(c: &mut Ctx) -> Result<()> {
    tool_call(c, "accept").await
}

async fn tool_call_deny(c: &mut Ctx) -> Result<()> {
    tool_call(c, "decline").await
}

/// Waits `d` for frames the server sends on its own.
pub(super) async fn listen(c: &mut Ctx, s: &mut crate::driver::LiveSession, d: Duration) {
    // `drain` returns at the first quiet period of `d`.
    let _ = s.drain(d, &mut c.log).await;
}
