//! The gates right before the spawn of a Claude `!` command (B8 PRE-PR r1):
//! a cancel or a policy change that arrives during the `ToolStarted` append
//! ends the command without a process.

use super::*;
/// The text of a refused command while `allow_shell` is off.
const SHELLS_OFF: &str = "Shells are off on this station (allow_shell = false).";

/// A command that writes `ran.txt` when it runs.
const MARKER_COMMAND: &str = "touch ran.txt";

async fn started_and_cancelled(rig: &Rig, id: &str) {
    rig.until("tool.started", |e| {
        matches!(e, Event::ToolStarted { node_id, .. } if node_id.as_str() == format!("shell-{id}"))
            .then_some(())
    })
    .await;
    let end = rig
        .until("tool.completed", |e| {
            completed(std::slice::from_ref(e), id).map(|c| c.2)
        })
        .await;
    assert_eq!(end, "cancelled");
}

#[tokio::test]
async fn a_cancel_stored_after_the_tool_started_append_ends_cancelled_without_a_process() {
    let mut rig = Rig::new().await;
    let s = rig.session().await;
    let store = rig.manager.store().clone();
    rig.manager.target.set_shell_pre_spawn(move |id| {
        let store = store.clone();
        Box::pin(async move {
            store.shell_request_cancel(id).await.unwrap();
        })
    });
    let id = rig.bang(&s, MARKER_COMMAND).await;
    let row = rig
        .row_is(&id, |r| r.state.is_terminal() && !r.advance_pending)
        .await;
    assert_eq!(row.state, ShellState::Cancelled);
    assert!(row.cancel_requested);
    started_and_cancelled(&rig, &id).await;
    assert!(!rig.app().join("ran.txt").exists(), "no process started");
    rig.manager.shutdown(StopMode::Kill).await;
}

#[tokio::test]
async fn allow_shell_turned_off_after_the_tool_started_append_ends_cancelled_with_the_text() {
    let mut rig = Rig::new().await;
    let s = rig.session().await;
    let hub = Arc::clone(rig.manager.policy_hub());
    let policy = hub.current().as_ref().clone();
    rig.manager.target.set_shell_pre_spawn(move |_| {
        let (hub, policy) = (Arc::clone(&hub), policy.clone());
        Box::pin(async move { hub.swap(policy.with_shell(false, 8)) })
    });
    let id = rig.bang(&s, MARKER_COMMAND).await;
    let row = rig
        .row_is(&id, |r| r.state.is_terminal() && !r.advance_pending)
        .await;
    assert_eq!(row.state, ShellState::Cancelled);
    assert_eq!(row.stderr_tail.as_deref(), Some(SHELLS_OFF));
    started_and_cancelled(&rig, &id).await;
    assert!(!rig.app().join("ran.txt").exists(), "no process started");
    rig.manager.shutdown(StopMode::Kill).await;
}
