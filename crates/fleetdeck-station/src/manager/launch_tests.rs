//! The launch identity at the dispatch (B9, DL-200): a command never brings
//! one, and a folder that changed after the decide fails the start or the
//! resume.

use std::time::Duration;

use fleetdeck_core::launch_dir::CHANGED;
use fleetdeck_proto::command::{SessionCommand, StopMode};
use fleetdeck_proto::event::{Event, SessionState, TurnStatus};
use fleetdeck_proto::launch::LaunchIdentity;
use fleetdeck_proto::{EventBody, SessionId};

use super::super::tests::{Station, WAIT, spec, text};

/// Creates `projects/<name>` below the rig's root and returns its path.
fn project(st: &Station, name: &str) -> std::path::PathBuf {
    let dir = st.root.path().join("projects").join(name);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Replaces `dir` by a link to its renamed folder.
#[cfg(target_os = "linux")]
fn swap_for_a_link(dir: &std::path::Path) {
    let moved = dir.with_file_name(format!(
        "{}-real",
        dir.file_name().unwrap().to_string_lossy()
    ));
    std::fs::rename(dir, &moved).unwrap();
    std::os::unix::fs::symlink(&moved, dir).unwrap();
}

/// Waits until a task reaches the launch hook.
async fn reached(st: &Station) {
    tokio::time::timeout(WAIT, st.manager.launch_hook().reached.notified())
        .await
        .expect("the dispatch did not reach the launch hook");
}

async fn start_failure(st: &Station) -> String {
    st.wait_for("start_failed", |e| match e {
        Event::Error { code, message, .. } if code == "start_failed" => Some(message.clone()),
        _ => None,
    })
    .await
}

async fn started_events(st: &Station) -> usize {
    st.events()
        .await
        .iter()
        .filter(|e| matches!(e.event, EventBody::Known(Event::SessionStarted { .. })))
        .count()
}

#[tokio::test]
async fn a_launch_identity_in_a_command_is_cleared_at_the_decide() {
    let mut st = Station::new(2).await;
    let ws = st.workspace();
    let mut forged = spec(&ws, "claude");
    forged.launch = Some(Box::new(LaunchIdentity {
        root: "/".into(),
        rel: String::new(),
        dev: 0,
        ino: 0,
    }));
    let started = st
        .accepted(SessionCommand::Start {
            spec: forged,
            prompt: Some(text("hello")),
            workspace: None,
            setup_cmd: None,
            batch: None,
            issue: None,
            origin_text: false,
            fleet_git: None,
        })
        .await;
    let s = started.receipt.result.unwrap().session_id.unwrap();
    st.wait_for("a finished turn", |e| match e {
        Event::TurnCompleted {
            status: TurnStatus::Done,
            ..
        } => Some(()),
        _ => None,
    })
    .await;
    assert_eq!(started_events(&st).await, 1);
    assert_ne!(st.state(&s).await, SessionState::Exited);
    st.manager.shutdown(StopMode::Kill).await;
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn a_folder_swapped_between_decide_and_dispatch_fails_the_start() {
    let mut st = Station::new(2).await;
    let proj = project(&st, "proj");
    st.manager.launch_hook().arm();
    let s = st.start(&proj.to_string_lossy(), Some("hello")).await;
    reached(&st).await;
    swap_for_a_link(&proj);
    st.manager.launch_hook().release.notify_one();
    assert_eq!(start_failure(&st).await, CHANGED);
    st.settled(&s).await;
    assert_eq!(started_events(&st).await, 0);
    st.manager.shutdown(StopMode::Kill).await;
}

/// Starts a session in `dir`, runs one turn and stops it.
#[cfg(target_os = "linux")]
async fn ended_session(st: &mut Station, dir: &std::path::Path) -> SessionId {
    let s = st.start(&dir.to_string_lossy(), Some("hello")).await;
    st.wait_for("a finished turn", |e| match e {
        Event::TurnCompleted {
            status: TurnStatus::Done,
            ..
        } => Some(()),
        _ => None,
    })
    .await;
    st.accepted(SessionCommand::Stop {
        session_id: s.clone(),
        how: StopMode::Graceful,
    })
    .await;
    let deadline = tokio::time::Instant::now() + WAIT;
    while st.state(&s).await != SessionState::Exited {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the stop did not end"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    st.settled(&s).await;
    s
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn a_resume_in_a_swapped_folder_fails() {
    let mut st = Station::new(2).await;
    let proj = project(&st, "resumable");
    let s = ended_session(&mut st, &proj).await;
    assert_eq!(started_events(&st).await, 1);
    st.manager.launch_hook().arm();
    st.accepted(SessionCommand::Resume {
        session_id: s.clone(),
    })
    .await;
    reached(&st).await;
    swap_for_a_link(&proj);
    st.manager.launch_hook().release.notify_one();
    assert_eq!(start_failure(&st).await, CHANGED);
    st.settled(&s).await;
    assert_eq!(started_events(&st).await, 1, "the resume must not start");
    st.manager.shutdown(StopMode::Kill).await;
}

#[cfg(target_os = "linux")]
#[test]
fn a_launch_without_an_identity_is_refused() {
    use super::launch::{LAUNCH_MISSING, LAUNCH_MISSING_CODE, launch_refusal};

    let mut plain = spec("/w", "claude");
    assert_eq!(
        launch_refusal(&plain),
        Some((LAUNCH_MISSING_CODE, LAUNCH_MISSING))
    );
    assert_eq!(LAUNCH_MISSING_CODE, "launch_dir_missing");
    plain.launch = Some(Box::new(LaunchIdentity {
        root: "/".into(),
        rel: String::new(),
        dev: 0,
        ino: 0,
    }));
    assert_eq!(launch_refusal(&plain), None);
}
