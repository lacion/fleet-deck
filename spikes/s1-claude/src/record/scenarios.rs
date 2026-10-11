//! Scripted live scenarios: the PLAN §13 shared scenarios plus the S1 extras of
//! PLAN §14 Phase 0 (edited input, interrupt during a tool, set permission mode,
//! open request across a restart).

use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use anyhow::{Result, bail};
use spike_canon::CanonEvent;

use super::Ctx;
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

/// All scenarios, in recording order.
pub const SCENARIOS: &[Scenario] = &[
    scenario!(
        "simple",
        true,
        "One prompt, one turn, partial deltas, result.",
        simple
    ),
    scenario!(
        "multi_turn",
        true,
        "Two turns in one process; the second turn uses the first.",
        multi_turn
    ),
    scenario!(
        "queued_turn",
        true,
        "Two prompts written back to back before the first turn ends: does the CLI queue the second as its own turn?",
        queued_turn
    ),
    scenario!(
        "turn_interrupt",
        true,
        "Interrupt during streamed text, then a follow-up turn.",
        turn_interrupt
    ),
    scenario!(
        "message_steering",
        true,
        "A second message written while an approved tool runs: steer into the running turn or queue?",
        message_steering
    ),
    scenario!(
        "provider_thread_resume",
        true,
        "Process 1 runs a turn and exits; process 2 starts with --resume and uses the first turn.",
        provider_thread_resume
    ),
    scenario!(
        "tool_call_approve",
        true,
        "Bash in manual mode: can_use_tool, allow.",
        tool_call_approve
    ),
    scenario!(
        "tool_call_deny",
        true,
        "Bash in manual mode: can_use_tool, deny with a message.",
        tool_call_deny
    ),
    scenario!(
        "tool_call_edit_input",
        false,
        "Bash in manual mode: allow with an edited command (updatedInput).",
        tool_call_edit_input
    ),
    scenario!(
        "interrupt_during_tool",
        false,
        "Interrupt while an approved long Bash command runs.",
        interrupt_during_tool
    ),
    scenario!(
        "set_permission_mode",
        false,
        "set_permission_mode to plan during the session, then a turn.",
        set_permission_mode
    ),
    scenario!(
        "approval_restart",
        false,
        "An approval is open when the process is killed; process 2 resumes the session: is the request issued again?",
        approval_restart
    ),
];

/// A command that writes a file, so the default mode asks for approval
/// (read-only commands such as a plain `echo` are allowed without a request).
const BASH_ECHO: &str = "Use the Bash tool to run exactly this command: touch fd-approve.txt && echo approve-ok\nRun it in the foreground. Then reply with the command output only.";

async fn simple(c: &mut Ctx) -> Result<()> {
    let mut s = c.start("default", None).await?;
    c.prompt(&mut s, "Reply with exactly the word: pong")
        .await?;
    c.turn_done(&mut s).await?;
    c.close(s).await
}

async fn multi_turn(c: &mut Ctx) -> Result<()> {
    let mut s = c.start("default", None).await?;
    c.prompt(&mut s, "Remember the number 42. Reply with exactly: ok")
        .await?;
    c.turn_done(&mut s).await?;
    c.prompt(
        &mut s,
        "Which number did I ask you to remember? Reply with the number only.",
    )
    .await?;
    c.turn_done(&mut s).await?;
    c.close(s).await
}

async fn queued_turn(c: &mut Ctx) -> Result<()> {
    let mut s = c.start("default", None).await?;
    c.prompt(&mut s, "Reply with exactly the word: first")
        .await?;
    c.prompt(&mut s, "Reply with exactly the word: second")
        .await?;
    c.idle(&mut s).await?;
    let turns = count_turns(&s.events);
    c.note(format!("turns completed: {turns}"));
    c.close(s).await
}

async fn turn_interrupt(c: &mut Ctx) -> Result<()> {
    let mut s = c.start("default", None).await?;
    c.prompt(
        &mut s,
        "Do not use any tools. Write the numbers from 1 to 400 as plain text, one number per line, and nothing else.",
    )
    .await?;
    s.until(super::TURN_LIMIT, &mut c.log, |e| {
        matches!(e, CanonEvent::MessageDelta { .. })
    })
    .await?;
    c.note(format!("interrupt at {} ms", s.elapsed_ms()));
    s.cmd(HostCmd::Interrupt, &mut c.log).await?;
    let status = c.turn_done(&mut s).await?;
    c.note(format!("interrupted turn status: {status:?}"));
    c.prompt(&mut s, "Reply with exactly the word: after")
        .await?;
    c.turn_done(&mut s).await?;
    c.close(s).await
}

async fn message_steering(c: &mut Ctx) -> Result<()> {
    let mut s = c.start("default", None).await?;
    c.prompt(
        &mut s,
        "Use the Bash tool to run exactly this command in the foreground (not in the background): touch fd-steer.txt && python3 -c \"import time; time.sleep(6); print('step-one')\"\nThen reply with the command output.",
    )
    .await?;
    let (req, _) = c.approval(&mut s).await?;
    answer(c, &mut s, &req, "allow", None).await?;
    c.note(format!("steer message at {} ms", s.elapsed_ms()));
    c.prompt(&mut s, "Also end your reply with the word: banana")
        .await?;
    c.idle(&mut s).await?;
    let turns = count_turns(&s.events);
    c.note(format!("turns completed: {turns}"));
    c.close(s).await
}

async fn provider_thread_resume(c: &mut Ctx) -> Result<()> {
    let mut s = c.start("default", None).await?;
    c.prompt(
        &mut s,
        "Remember the code word: walnut. Reply with exactly: ok",
    )
    .await?;
    c.turn_done(&mut s).await?;
    let Some(session) = s.core.native_session_id().map(str::to_owned) else {
        bail!("no session id after the first turn");
    };
    c.close(s).await?;
    let mut s = c.start("default", Some(session)).await?;
    c.prompt(&mut s, "What was the code word? Reply with the word only.")
        .await?;
    c.turn_done(&mut s).await?;
    c.close(s).await
}

async fn answer(
    c: &mut Ctx,
    s: &mut crate::driver::LiveSession,
    req: &str,
    option: &str,
    updated_input: Option<serde_json::Value>,
) -> Result<()> {
    let message = (option == "deny").then(|| "The owner denied this command.".to_owned());
    s.cmd(
        HostCmd::Answer {
            request_id: req.to_owned(),
            option_id: option.to_owned(),
            updated_input,
            message,
        },
        &mut c.log,
    )
    .await?;
    Ok(())
}

async fn tool_call_approve(c: &mut Ctx) -> Result<()> {
    let mut s = c.start("default", None).await?;
    c.prompt(&mut s, BASH_ECHO).await?;
    let (req, _) = c.approval(&mut s).await?;
    answer(c, &mut s, &req, "allow", None).await?;
    c.idle(&mut s).await?;
    c.close(s).await
}

async fn tool_call_deny(c: &mut Ctx) -> Result<()> {
    let mut s = c.start("default", None).await?;
    c.prompt(&mut s, BASH_ECHO).await?;
    let (req, _) = c.approval(&mut s).await?;
    answer(c, &mut s, &req, "deny", None).await?;
    c.idle(&mut s).await?;
    c.close(s).await
}

async fn tool_call_edit_input(c: &mut Ctx) -> Result<()> {
    let mut s = c.start("default", None).await?;
    c.prompt(&mut s, BASH_ECHO).await?;
    let (req, mut input) = c.approval(&mut s).await?;
    if let Some(obj) = input.as_object_mut() {
        obj.insert(
            "command".to_owned(),
            "touch fd-edited.txt && echo edited-ok".into(),
        );
    }
    answer(c, &mut s, &req, "allow", Some(input)).await?;
    c.idle(&mut s).await?;
    c.close(s).await
}

async fn interrupt_during_tool(c: &mut Ctx) -> Result<()> {
    let mut s = c.start("default", None).await?;
    c.prompt(
        &mut s,
        "Use the Bash tool to run exactly this command in the foreground (not in the background): touch fd-long.txt && python3 -c \"import time; time.sleep(40); print('late')\"\nThen reply with the output.",
    )
    .await?;
    let (req, _) = c.approval(&mut s).await?;
    answer(c, &mut s, &req, "allow", None).await?;
    s.drain(Duration::from_secs(2), &mut c.log).await?;
    c.note(format!("interrupt at {} ms", s.elapsed_ms()));
    s.cmd(HostCmd::Interrupt, &mut c.log).await?;
    let status = c.turn_done(&mut s).await?;
    c.note(format!("turn status after interrupt: {status:?}"));
    c.idle(&mut s).await?;
    c.close(s).await
}

async fn set_permission_mode(c: &mut Ctx) -> Result<()> {
    let mut s = c.start("default", None).await?;
    s.cmd(
        HostCmd::SetMode {
            mode: "plan".to_owned(),
        },
        &mut c.log,
    )
    .await?;
    s.until(super::TURN_LIMIT, &mut c.log, |e| {
        matches!(e, CanonEvent::ModeChanged { .. })
    })
    .await?;
    c.prompt(&mut s, "Reply with exactly the word: planned")
        .await?;
    c.turn_done(&mut s).await?;
    c.close(s).await
}

async fn approval_restart(c: &mut Ctx) -> Result<()> {
    let mut s = c.start("default", None).await?;
    c.prompt(&mut s, BASH_ECHO).await?;
    let (req, _) = c.approval(&mut s).await?;
    let native = s.core.native_request_id(&req).map(str::to_owned);
    let Some(session) = s.core.native_session_id().map(str::to_owned) else {
        bail!("no session id");
    };
    c.note(format!("open request {req} (native {native:?}) at kill"));
    s.kill(&mut c.log).await?;
    let mut s = c.start("default", Some(session)).await?;
    // Does the resumed process ask again without new input?
    s.drain(Duration::from_secs(8), &mut c.log).await?;
    let reissued = s
        .events
        .iter()
        .any(|e| matches!(e, CanonEvent::ApprovalRequested { .. }));
    c.note(format!("request issued again without input: {reissued}"));
    c.prompt(&mut s, "Continue with the task.").await?;
    let mut asked = false;
    let deadline = tokio::time::Instant::now() + super::TURN_LIMIT;
    while !s.core.is_idle() && tokio::time::Instant::now() < deadline {
        let Some(events) = s.step(super::TURN_LIMIT, &mut c.log).await? else {
            break;
        };
        for e in events {
            if let CanonEvent::ApprovalRequested { request_id, .. } = e {
                asked = true;
                answer(c, &mut s, &request_id, "deny", None).await?;
            }
        }
    }
    c.note(format!("new request after a new prompt: {asked}"));
    c.idle(&mut s).await?;
    c.close(s).await
}

fn count_turns(events: &[CanonEvent]) -> usize {
    events
        .iter()
        .filter(|e| matches!(e, CanonEvent::TurnCompleted { .. }))
        .count()
}
