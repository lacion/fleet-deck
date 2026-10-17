//! Scenarios that verify the PLAN §6.2 capability matrix rows beyond the §14 S1
//! must-prove list: fork, plan capture, subagent parent link, background work,
//! file-change telemetry, questions, and "stop right after send" (trap 3).

use std::time::Duration;

use anyhow::{Result, bail};
use serde_json::Value;
use spike_canon::CanonEvent;

use super::Ctx;
use super::scenarios::answer;
use crate::adapter::HostCmd;

pub(super) async fn fork_session(c: &mut Ctx) -> Result<()> {
    let mut s = c.start("default", None).await?;
    c.prompt(
        &mut s,
        "Remember the code word: maple. Reply with exactly: ok",
    )
    .await?;
    c.turn_done(&mut s).await?;
    let Some(session) = s.core.native_session_id().map(str::to_owned) else {
        bail!("no session id after the first turn");
    };
    c.close(s).await?;
    let mut s = c.start_with("default", Some(session.clone()), true).await?;
    c.prompt(&mut s, "What was the code word? Reply with the word only.")
        .await?;
    c.turn_done(&mut s).await?;
    let forked = s.core.native_session_id().map(str::to_owned);
    c.note(format!(
        "fork: new session id differs from the source: {}",
        forked.as_deref() != Some(session.as_str())
    ));
    c.close(s).await
}

pub(super) async fn plan_capture(c: &mut Ctx) -> Result<()> {
    let mut s = c.start("plan", None).await?;
    c.prompt(
        &mut s,
        "Plan how to create a file named fd-plan.txt that contains the word hello. Do not explore the directory. Present the plan now with the ExitPlanMode tool; the plan is one short line. If I reject the plan, do not ask questions; reply with exactly: plan-denied",
    )
    .await?;
    let (req, input) = c.approval(&mut s).await?;
    c.note(format!(
        "approval for plan exit: input keys {:?}",
        input
            .as_object()
            .map(|o| o.keys().cloned().collect::<Vec<_>>())
    ));
    // Keep the session in plan mode: deny the exit, so nothing is implemented.
    // The model can ask again (ExitPlanMode or AskUserQuestion): deny those too.
    answer(c, &mut s, &req, "deny", None).await?;
    c.idle_answering(&mut s, "deny").await?;
    c.close(s).await
}

pub(super) async fn subagent(c: &mut Ctx) -> Result<()> {
    let mut s = c.start("default", None).await?;
    c.prompt(
        &mut s,
        "Use the Task tool to start a general-purpose subagent with this prompt: 'Do not use any tools. Reply with exactly: sub-ok'. Then reply with the subagent's answer only.",
    )
    .await?;
    c.idle_allowing(&mut s).await?;
    c.close(s).await
}

pub(super) async fn background_task(c: &mut Ctx) -> Result<()> {
    let mut s = c.start("default", None).await?;
    c.prompt(
        &mut s,
        "Use the Bash tool with run_in_background set to true to run exactly: touch fd-bg.txt && python3 -c \"import time; time.sleep(8); print('bg-done')\"\nDo not wait for it. Reply with exactly: started",
    )
    .await?;
    let (req, _) = c.approval(&mut s).await?;
    answer(c, &mut s, &req, "allow", None).await?;
    c.turn_done(&mut s).await?;
    c.note(format!("first turn ended at {} ms", s.elapsed_ms()));
    // The background command finishes after the turn: does the CLI report it,
    // and does it start a turn without host input (trap 4)?
    s.drain(Duration::from_secs(30), &mut c.log).await?;
    c.idle_allowing(&mut s).await?;
    c.close(s).await
}

pub(super) async fn file_edit(c: &mut Ctx) -> Result<()> {
    let mut s = c.start("acceptEdits", None).await?;
    c.prompt(
        &mut s,
        "Use the Write tool to create the file fd-edit.txt with the content: hello. Then reply with exactly: written",
    )
    .await?;
    c.idle_allowing(&mut s).await?;
    c.close(s).await
}

pub(super) async fn ask_user_question(c: &mut Ctx) -> Result<()> {
    let mut s = c.start("default", None).await?;
    c.prompt(
        &mut s,
        "Use the AskUserQuestion tool to ask me one question: 'Which color?' with the options red and blue. Then reply with my answer only.",
    )
    .await?;
    let (req, input) = c.approval(&mut s).await?;
    // The SDK answers a question with `allow` and `updatedInput` that adds an
    // `answers` map (question text → chosen label).
    let mut updated = input.clone();
    let question = input["questions"][0]["question"]
        .as_str()
        .unwrap_or("Which color?")
        .to_owned();
    if let Some(obj) = updated.as_object_mut() {
        let mut answers = serde_json::Map::new();
        answers.insert(question, Value::from("blue"));
        obj.insert("answers".to_owned(), Value::Object(answers));
    }
    answer(c, &mut s, &req, "allow", Some(updated)).await?;
    c.idle(&mut s).await?;
    c.close(s).await
}

pub(super) async fn interrupt_right_after_send(c: &mut Ctx) -> Result<()> {
    let mut s = c.start("default", None).await?;
    c.prompt(&mut s, "Reply with exactly the word: early")
        .await?;
    c.note("interrupt written right after the prompt, before any turn frame");
    s.cmd(HostCmd::Interrupt, &mut c.log).await?;
    s.drain(Duration::from_secs(10), &mut c.log).await?;
    let turns = s
        .events
        .iter()
        .filter(|e| matches!(e, CanonEvent::TurnCompleted { .. }))
        .count();
    c.note(format!(
        "turns completed after the early interrupt: {turns}"
    ));
    c.prompt(&mut s, "Reply with exactly the word: after")
        .await?;
    c.idle(&mut s).await?;
    c.close(s).await
}
