//! S2 capability scenarios for the PLAN §6.2 matrix rows that the shared
//! scenarios do not exercise: fork, child work with a parent link, and
//! background work after the turn ends.

use std::time::Duration;

use anyhow::{Result, bail};
use spike_canon::{CanonEvent, ToolKind};

use super::Ctx;
use super::scenarios::{MODE, listen};
use crate::adapter::HostCmd;

/// `thread/fork` of a thread with one turn: the fork is a new thread with
/// the old context.
pub(super) async fn fork_thread(c: &mut Ctx) -> Result<()> {
    let mut s = c.start(MODE, None).await?;
    c.prompt(
        &mut s,
        "Remember the word maple. Do not use tools. Reply with exactly: noted",
    )
    .await?;
    c.turn_done(&mut s).await?;
    c.idle(&mut s).await?;
    let Some(source) = s.core.thread_id().map(str::to_owned) else {
        bail!("no thread id");
    };
    c.close(s).await?;
    let mut spec = c.spec(MODE, Some(source.clone()));
    spec.fork = true;
    let mut s = c.start_with(spec).await?;
    let forked = s.core.thread_id().map(str::to_owned);
    c.note(format!(
        "fork: new thread id differs from the source: {}",
        forked.as_deref() != Some(source.as_str())
    ));
    c.prompt(
        &mut s,
        "Which word did I ask you to remember? Reply with the word only.",
    )
    .await?;
    c.turn_done(&mut s).await?;
    c.idle(&mut s).await?;
    c.close(s).await
}

/// A sub-agent (multi-agent tools): does Codex report the child's work, and
/// how does it link to the parent tool call?
pub(super) async fn subagent(c: &mut Ctx) -> Result<()> {
    let mut s = c.start(MODE, None).await?;
    c.prompt(
        &mut s,
        "Spawn exactly one sub-agent with your agent tools. Give it this task: \"Do not use tools. Reply with exactly: sub-ok\". Wait for it to finish, then reply with its answer only.",
    )
    .await?;
    c.idle_answering(&mut s, Some("accept")).await?;
    let subagent_tools = s
        .events
        .iter()
        .filter(|e| {
            matches!(
                e,
                CanonEvent::ToolStarted {
                    tool_kind: ToolKind::Subagent,
                    ..
                }
            )
        })
        .count();
    c.note(format!("subagent tool calls: {subagent_tools}"));
    c.close(s).await
}

/// A command that keeps running after the turn ends. Codex sends no
/// notification for it, so the host asks with
/// `thread/backgroundTerminals/list` after the turn and again later.
pub(super) async fn background_terminal(c: &mut Ctx) -> Result<()> {
    let mut s = c.start(MODE, None).await?;
    c.prompt(
        &mut s,
        "Run this command with your exec_command tool and a yield_time_ms of 1000, so that the tool returns while the command is still running: python3 -c \"import time; time.sleep(25); print('bg-done')\"\nDo not use nohup, &, or any other way to detach it, and do not poll or wait for it. Then reply with exactly: started",
    )
    .await?;
    c.idle_answering(&mut s, Some("accept")).await?;
    for round in 0..2 {
        s.cmd(HostCmd::ListBackground, &mut c.log).await?;
        let ev = c
            .wait_for(&mut s, |e| {
                matches!(e, CanonEvent::BackgroundUpdated { .. })
            })
            .await?;
        if let CanonEvent::BackgroundUpdated { tasks } = ev {
            c.note(format!(
                "background terminals after the turn (poll {round}): {}",
                tasks.len()
            ));
        }
        if round == 0 {
            listen(c, &mut s, Duration::from_secs(30)).await;
        }
    }
    c.close(s).await
}
