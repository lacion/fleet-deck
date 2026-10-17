//! The other launches of a checked folder (B9 part 3, DL-200): the setup
//! command, the Claude `!` command and a Codex session start in exactly the
//! checked folder object, and a Codex folder that changed after the spawn
//! stops the session.

use fleetdeck_core::launch_dir::CHANGED;
use fleetdeck_proto::command::{SessionCommand, StopMode};
use fleetdeck_proto::event::{Event, SessionState};

use crate::manager::shell_run_tests::rig::Rig;
use crate::manager::tests::codex::codex_spec;
use crate::manager::tests::setup::{start_with_setup, station};
use crate::manager::tests::{Station, WAIT};
use crate::store::shell_runs::ShellState;

/// Creates `projects/<name>` below the rig's root and returns its path.
fn project(st: &Station, name: &str) -> std::path::PathBuf {
    let dir = st.root.path().join("projects").join(name);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Replaces `dir` by a link to its renamed folder; returns the renamed one.
fn swap_for_a_link(dir: &std::path::Path) -> std::path::PathBuf {
    let moved = dir.with_file_name(format!(
        "{}-real",
        dir.file_name().unwrap().to_string_lossy()
    ));
    std::fs::rename(dir, &moved).unwrap();
    std::os::unix::fs::symlink(&moved, dir).unwrap();
    moved
}

/// Waits until a task reaches the launch hook.
async fn reached(st: &Station) {
    tokio::time::timeout(WAIT, st.manager.launch_hook().reached.notified())
        .await
        .expect("the task did not reach the launch hook");
}

async fn start_failure(st: &Station) -> String {
    st.wait_for("start_failed", |e| match e {
        Event::Error { code, message, .. } if code == "start_failed" => Some(message.clone()),
        _ => None,
    })
    .await
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn a_setup_command_in_a_swapped_folder_does_not_run() {
    let mut st = station(true).await;
    let proj = project(&st, "proj");
    st.manager.launch_hook().arm();
    let d = st
        .accepted(start_with_setup(&proj.to_string_lossy(), "touch ran.txt"))
        .await;
    let s = d.receipt.result.unwrap().session_id.unwrap();
    // The dispatch takes the launch; the setup task reopens it right before
    // the command.
    reached(&st).await;
    st.manager.launch_hook().arm();
    st.manager.launch_hook().release.notify_one();
    reached(&st).await;
    let moved = swap_for_a_link(&proj);
    st.manager.launch_hook().release.notify_one();
    assert_eq!(start_failure(&st).await, CHANGED);
    st.settled(&s).await;
    assert!(!proj.join("ran.txt").exists(), "the command ran");
    assert!(!moved.join("ran.txt").exists(), "the command ran");
    st.manager.shutdown(StopMode::Kill).await;
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn a_bang_command_runs_in_the_checked_folder() {
    let mut rig = Rig::new().await;
    let s = rig.session().await;
    let row = rig.run(&s, "pwd -P").await;
    assert_eq!(row.state, ShellState::Done);
    assert_eq!(row.stdout_tail.unwrap().trim(), rig.app_root());
    rig.shutdown().await;
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn a_bang_command_in_a_swapped_folder_is_refused() {
    let mut rig = Rig::new().await;
    let s = rig.session().await;
    let moved = swap_for_a_link(&rig.app());
    let id = rig.bang(&s, "touch ran.txt").await;
    let row = rig
        .row_is(&id, |r| r.state.is_terminal() && !r.advance_pending)
        .await;
    assert_eq!(row.state, ShellState::Cancelled);
    assert_eq!(row.stderr_tail.as_deref(), Some(CHANGED));
    assert!(!moved.join("ran.txt").exists(), "the command ran");
    rig.shutdown().await;
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn a_codex_session_whose_folder_changed_after_the_spawn_is_stopped() {
    let mut st = Station::new(2).await;
    let proj = project(&st, "codex");
    st.manager.launch_hook().arm();
    let started = st
        .accepted(SessionCommand::Start {
            spec: codex_spec(&proj.to_string_lossy()),
            prompt: None,
            workspace: None,
            setup_cmd: None,
            batch: None,
            issue: None,
            origin_text: false,
        })
        .await;
    let s = started.receipt.result.unwrap().session_id.unwrap();
    // The dispatch takes the launch; the start then reopens the identity
    // once more after `session.started`.
    reached(&st).await;
    st.manager.launch_hook().arm();
    st.manager.launch_hook().release.notify_one();
    reached(&st).await;
    swap_for_a_link(&proj);
    st.manager.launch_hook().release.notify_one();
    assert_eq!(start_failure(&st).await, CHANGED);
    st.wait_for("session.ended", |e| match e {
        Event::SessionEnded { reason, .. } if reason == "start_failed" => Some(()),
        _ => None,
    })
    .await;
    st.settled(&s).await;
    assert_eq!(st.state(&s).await, SessionState::Exited);
    st.manager.shutdown(StopMode::Kill).await;
}
