//! S2 extra scenarios (PLAN §14 Phase 0 S2 row beyond the shared ones).

use std::time::Duration;

use anyhow::{Result, bail, ensure};
use serde_json::{Map, Value, json};
use spike_canon::{CanonEvent, TurnStatus};

use super::Ctx;
use super::scenarios::{COUNT, MODE, TOUCH_ECHO, listen};
use crate::adapter::HostCmd;

const LONG: &str = "Use the shell tool to run exactly this command in the foreground: touch fd-long.txt && python3 -c \"import time; time.sleep(40); print('late')\"\nThen reply with the output.";

pub(super) async fn question_rpc(c: &mut Ctx) -> Result<()> {
    let mut s = c.start(MODE, None).await?;
    s.cmd(
        HostCmd::SetCollaboration {
            mode: "plan".into(),
        },
        &mut c.log,
    )
    .await?;
    c.prompt(
        &mut s,
        "Ask me one question with your user-input question tool: which color do I prefer, red or blue? Then reply with exactly the chosen color.",
    )
    .await?;
    let (req, params) = c.approval(&mut s).await?;
    let mut answers = Map::new();
    for q in params
        .get("questions")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        if let Some(id) = q.get("id").and_then(Value::as_str) {
            answers.insert(id.to_owned(), json!({"answers": ["blue"]}));
        }
    }
    ensure!(!answers.is_empty(), "no question ids in {params}");
    c.answer(&mut s, &req, "answer", Some(json!({"answers": answers})))
        .await?;
    c.turn_done(&mut s).await?;
    c.idle(&mut s).await?;
    c.close(s).await
}

pub(super) async fn plan_capture(c: &mut Ctx) -> Result<()> {
    let mut s = c.start(MODE, None).await?;
    s.cmd(
        HostCmd::SetCollaboration {
            mode: "plan".into(),
        },
        &mut c.log,
    )
    .await?;
    c.prompt(
        &mut s,
        "Plan how to add a file hello.txt that contains the word hello. Do not ask questions and do not run tools. Give a plan with two steps.",
    )
    .await?;
    c.turn_done(&mut s).await?;
    c.idle_answering(&mut s, Some("decline")).await?;
    let plans = s
        .events
        .iter()
        .filter(|e| matches!(e, CanonEvent::PlanProposed { .. }))
        .count();
    c.note(format!("plan.proposed events: {plans}"));
    c.close(s).await
}

pub(super) async fn file_change(c: &mut Ctx) -> Result<()> {
    let mut s = c.start(MODE, None).await?;
    c.prompt(
        &mut s,
        "Create the file fd-edit.txt with the single line hello. Use your file editing tool (apply_patch), not a shell command. Then reply with exactly: edited",
    )
    .await?;
    c.idle_answering(&mut s, Some("accept")).await?;
    let exists = c.workdir.join("fd-edit.txt").exists();
    c.note(format!("fd-edit.txt exists: {exists}"));
    c.close(s).await
}

pub(super) async fn interrupt_during_tool(c: &mut Ctx) -> Result<()> {
    let mut s = c.start(MODE, None).await?;
    c.prompt(&mut s, LONG).await?;
    let (req, _) = c.approval(&mut s).await?;
    // The commandExecution item starts before the approval request, so the
    // command runs as soon as the answer arrives.
    c.answer(&mut s, &req, "accept", None).await?;
    // Let the command run for a moment.
    listen(c, &mut s, Duration::from_secs(3)).await;
    s.cmd(HostCmd::Interrupt, &mut c.log).await?;
    let status = c.turn_done(&mut s).await?;
    c.note(format!("turn status after interrupt: {status:?}"));
    c.idle(&mut s).await?;
    c.close(s).await
}

pub(super) async fn interrupt_right_after_send(c: &mut Ctx) -> Result<()> {
    let mut s = c.start(MODE, None).await?;
    c.prompt(&mut s, COUNT).await?;
    s.cmd(HostCmd::Interrupt, &mut c.log).await?;
    let status = c.turn_done(&mut s).await?;
    c.note(format!("turn status: {status:?}"));
    c.prompt(&mut s, "Do not use tools. Reply with exactly: after")
        .await?;
    c.turn_done(&mut s).await?;
    c.idle(&mut s).await?;
    c.close(s).await
}

pub(super) async fn set_mode(c: &mut Ctx) -> Result<()> {
    let mut s = c.start(MODE, None).await?;
    c.prompt(&mut s, "Do not use tools. Reply with exactly: one")
        .await?;
    c.turn_done(&mut s).await?;
    s.cmd(
        HostCmd::SetMode {
            mode: "untrusted/read-only".into(),
        },
        &mut c.log,
    )
    .await?;
    c.prompt(&mut s, "Do not use tools. Reply with exactly: two")
        .await?;
    c.wait_for(&mut s, |e| matches!(e, CanonEvent::ModeChanged { .. }))
        .await?;
    c.turn_done(&mut s).await?;
    c.idle(&mut s).await?;
    c.close(s).await
}

pub(super) async fn approval_restart(c: &mut Ctx) -> Result<()> {
    let mut s = c.start(MODE, None).await?;
    c.prompt(&mut s, TOUCH_ECHO).await?;
    let (req, _) = c.approval(&mut s).await?;
    let Some(thread) = s.core.thread_id().map(str::to_owned) else {
        bail!("no thread id");
    };
    c.note(format!("open request {req} at kill"));
    s.kill(&mut c.log).await?;
    let mut s = c.start(MODE, Some(thread)).await?;
    // Does the new process ask the open request again on its own?
    listen(c, &mut s, Duration::from_secs(10)).await;
    let reasked = s
        .events
        .iter()
        .any(|e| matches!(e, CanonEvent::ApprovalRequested { .. }));
    c.note(format!(
        "approval asked again by the resumed process without input: {reasked}"
    ));
    c.prompt(
        &mut s,
        "The previous command did not run. Use the shell tool again to run exactly: touch fd-approve.txt && echo approve-ok",
    )
    .await?;
    let (req2, _) = c.approval(&mut s).await?;
    c.answer(&mut s, &req2, "accept", None).await?;
    let status = c.turn_done(&mut s).await?;
    ensure!(status == TurnStatus::Done, "status {status:?}");
    c.idle_answering(&mut s, Some("accept")).await?;
    c.close(s).await
}
