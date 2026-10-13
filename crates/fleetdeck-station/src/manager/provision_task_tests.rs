//! The Git work of a start (a clone or a worktree) runs in the start task, not
//! in the effect worker: the other sessions go on, a stop cancels the work
//! and removes what it made, and the restart consent lists the work.

use std::os::unix::fs::PermissionsExt;
use std::process::Command;

use fleetdeck_proto::command::WorkspaceRequest;
use tokio::sync::watch;

use super::*;
use crate::store::{EffectPayload, WorkspacePlan};
use crate::workspace::provision::{self, ProvisionError};
use crate::workspace::setup::STOPPED_MESSAGE;

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .current_dir(dir)
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "-c",
            "init.defaultBranch=main",
        ])
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .unwrap();
    assert!(out.status.success(), "{args:?}: {out:?}");
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

fn init_repo(dir: &Path) {
    git(dir, &["init", "-q"]);
    std::fs::write(dir.join("a.txt"), "one\n").unwrap();
    git(dir, &["add", "a.txt"]);
    git(dir, &["commit", "-qm", "one"]);
}

fn start_cmd(cwd: &str, workspace: WorkspaceRequest) -> SessionCommand {
    SessionCommand::Start {
        spec: spec(cwd, "claude"),
        prompt: Some(text("hello")),
        workspace: Some(Box::new(workspace)),
        setup_cmd: None,
        batch: None,
        issue: None,
        origin_text: false,
    }
}

fn worktree() -> WorkspaceRequest {
    WorkspaceRequest::Worktree {
        branch: None,
        base: None,
    }
}

async fn session_of(st: &mut Station, command: SessionCommand) -> SessionId {
    st.accepted(command)
        .await
        .receipt
        .result
        .unwrap()
        .session_id
        .unwrap()
}

async fn done_turns(st: &Station, session: &SessionId) -> usize {
    st.events()
        .await
        .into_iter()
        .filter(|e| e.session_id == *session)
        .filter(|e| {
            matches!(
                &e.event,
                EventBody::Known(Event::TurnCompleted {
                    status: TurnStatus::Done,
                    ..
                })
            )
        })
        .count()
}

async fn wait_done(st: &Station, session: &SessionId, n: usize) {
    let deadline = tokio::time::Instant::now() + WAIT;
    while done_turns(st, session).await < n {
        assert!(tokio::time::Instant::now() < deadline, "no turn {n}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn wait_started(st: &Station, session: &SessionId) {
    let deadline = tokio::time::Instant::now() + WAIT;
    loop {
        let started = st.events().await.iter().any(|e| {
            e.session_id == *session
                && matches!(&e.event, EventBody::Known(Event::SessionStarted { .. }))
        });
        if started {
            return;
        }
        assert!(tokio::time::Instant::now() < deadline, "not started");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn wait_ended(st: &Station, session: &SessionId) {
    let deadline = tokio::time::Instant::now() + WAIT;
    while st.state(session).await != SessionState::Exited {
        assert!(tokio::time::Instant::now() < deadline, "not ended");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Waits until the stop effect of `session` is dispatched, that is until the
/// start task has the cancel.
async fn wait_stop_dispatched(st: &Station, session: &SessionId) {
    let deadline = tokio::time::Instant::now() + WAIT;
    loop {
        let states = st.effect_states(session).await;
        if states
            .iter()
            .any(|(kind, state)| *kind == "stop" && *state == EffectState::Confirmed)
        {
            return;
        }
        assert!(tokio::time::Instant::now() < deadline, "{states:?}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Starts `workspace` and holds the start task right before the Git work.
async fn held_start(st: &mut Station, cwd: &str, workspace: WorkspaceRequest) -> SessionId {
    let target = Arc::clone(&st.manager.target);
    let hook = &target.hooks().provision;
    hook.arm();
    let session = session_of(st, start_cmd(cwd, workspace)).await;
    tokio::time::timeout(WAIT, hook.reached.notified())
        .await
        .unwrap();
    session
}

fn release(st: &Station) {
    st.manager.target.hooks().provision.release.notify_one();
}

/// The `Git work` of a start does not hold the effect worker: a prompt to
/// another session is delivered while a clone waits.
#[tokio::test]
async fn a_start_that_prepares_its_workspace_does_not_block_the_other_sessions() {
    let mut st = Station::new(4).await;
    let ws = st.workspace();
    init_repo(Path::new(&ws));
    let first = st.start(&ws, Some("hello")).await;
    wait_done(&st, &first, 1).await;

    let target = Path::new(&ws).parent().unwrap().join("copy");
    let url = format!("file://{ws}");
    let second = held_start(
        &mut st,
        &target.to_string_lossy(),
        WorkspaceRequest::Clone { url },
    )
    .await;
    st.accepted(SessionCommand::Prompt {
        session_id: first.clone(),
        input: text("again"),
    })
    .await;
    wait_done(&st, &first, 2).await;
    assert!(
        !target.exists(),
        "the clone is still held: the prompt did not wait for it"
    );

    release(&st);
    wait_started(&st, &second).await;
    assert!(target.join("a.txt").is_file());
    st.manager.shutdown(StopMode::Kill).await;
}

/// Stops the held start of `session`, releases the task, and checks that the
/// start ended with the stop text and the effect is retired as stopped.
async fn stop_held(st: &mut Station, session: &SessionId) {
    st.accepted(SessionCommand::Stop {
        session_id: session.clone(),
        how: StopMode::Graceful,
    })
    .await;
    wait_stop_dispatched(st, session).await;
    release(st);
    let message = st
        .wait_for("start failure", |e| match e {
            Event::Error { code, message, .. } if code == "start_failed" => Some(message.clone()),
            _ => None,
        })
        .await;
    assert_eq!(message, STOPPED_MESSAGE);
    wait_ended(st, session).await;
    st.settled(session).await;
    let effects = st
        .manager
        .store()
        .session_effects(session.clone())
        .await
        .unwrap();
    let start = effects
        .iter()
        .find(|e| matches!(e.payload, EffectPayload::Start { .. }))
        .unwrap();
    assert_eq!(start.state, EffectState::RetiredNotDispatched);
    assert_eq!(start.reason.as_deref(), Some("setup_stopped"));
}

#[tokio::test]
async fn a_stop_during_the_clone_is_accepted_and_leaves_no_directory() {
    let mut st = Station::new(4).await;
    let ws = st.workspace();
    init_repo(Path::new(&ws));
    let target = Path::new(&ws).parent().unwrap().join("copy");
    let url = format!("file://{ws}");
    let session = held_start(
        &mut st,
        &target.to_string_lossy(),
        WorkspaceRequest::Clone { url },
    )
    .await;
    stop_held(&mut st, &session).await;
    assert!(!target.exists(), "the clone directory is not there");
    st.manager.shutdown(StopMode::Kill).await;
}

#[tokio::test]
async fn a_stop_during_the_worktree_add_leaves_no_worktree_and_no_branch() {
    let mut st = Station::new(4).await;
    let ws = st.workspace();
    init_repo(Path::new(&ws));
    let session = held_start(&mut st, &ws, worktree()).await;
    let summary = st
        .manager
        .store()
        .summary(session.clone())
        .await
        .unwrap()
        .unwrap();
    let path = summary.worktree.clone().unwrap();
    let branch = summary.branch.clone().unwrap();
    stop_held(&mut st, &session).await;
    assert!(!Path::new(&path).exists(), "the worktree directory");
    assert_eq!(git(Path::new(&ws), &["branch", "--list", &branch]), "");
    st.manager.shutdown(StopMode::Kill).await;
}

#[tokio::test]
async fn a_worktree_start_records_its_base_once() {
    let mut st = Station::new(4).await;
    let ws = st.workspace();
    init_repo(Path::new(&ws));
    let session = session_of(&mut st, start_cmd(&ws, worktree())).await;
    st.wait_for("started", |e| {
        matches!(e, Event::SessionStarted { .. }).then_some(())
    })
    .await;
    st.settled(&session).await;
    let bases = st
        .events()
        .await
        .into_iter()
        .filter(|e| e.session_id == session)
        .filter(|e| matches!(&e.event, EventBody::Known(Event::SessionBase { .. })))
        .count();
    assert_eq!(bases, 1);
    st.manager.shutdown(StopMode::Kill).await;
}

#[tokio::test]
async fn the_restart_consent_lists_a_start_that_prepares_its_workspace() {
    use crate::shell::{ShellConfig, Shells};
    let mut st = Station::new(4).await;
    let ws = st.workspace();
    init_repo(Path::new(&ws));
    assert!(
        st.manager
            .store()
            .provisioning_starts()
            .await
            .unwrap()
            .is_empty()
    );
    let session = held_start(&mut st, &ws, worktree()).await;
    assert_eq!(
        st.manager.store().provisioning_starts().await.unwrap(),
        vec![session.clone()]
    );
    let callsign = st
        .manager
        .store()
        .summary(session.clone())
        .await
        .unwrap()
        .unwrap()
        .callsign
        .unwrap();
    let listed = st
        .manager
        .restart_would_stop(&Shells::new(ShellConfig::default()))
        .await
        .unwrap();
    assert_eq!(listed.len(), 1, "{listed:?}");
    assert_eq!(listed[0].id, session.as_str());
    assert_eq!(listed[0].title, format!("Workspace of {callsign}"));
    release(&st);
    st.wait_for("started", |e| {
        matches!(e, Event::SessionStarted { .. }).then_some(())
    })
    .await;
    st.settled(&session).await;
    assert!(
        st.manager
            .store()
            .provisioning_starts()
            .await
            .unwrap()
            .is_empty()
    );
    st.manager.shutdown(StopMode::Kill).await;
}

/// A `git` that waits 60 s after a clone or a worktree add that worked, so a
/// cancel finds the new directory.
fn slow_git(dir: &Path) -> std::path::PathBuf {
    let script = dir.join("slow-git");
    std::fs::write(
        &script,
        "#!/bin/sh\ngit \"$@\"\nrc=$?\ncase \" $* \" in\n  *\" worktree add \"*|*\" clone \"*)\n    [ $rc -eq 0 ] && exec sleep 60 ;;\nesac\nexit $rc\n",
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    script
}

#[tokio::test]
async fn a_cancel_during_the_worktree_add_removes_the_new_worktree_and_branch() {
    let root = tempfile::tempdir().unwrap();
    let repo = root.path().join("app");
    std::fs::create_dir_all(&repo).unwrap();
    init_repo(&repo);
    git(&repo, &["branch", "keep"]);
    let path = root.path().join("app--fd-test");
    let plan = WorkspacePlan::Worktree {
        repo: repo.to_string_lossy().into_owned(),
        path: path.to_string_lossy().into_owned(),
        branch: "fd/test".to_owned(),
        base: None,
    };
    let program = slow_git(root.path());
    let before = provision::before(&plan, &program).await;
    let (cancel, rx) = watch::channel(false);
    let run = provision::provision_cancellable(&plan, &program, rx);
    let stopper = async {
        let deadline = tokio::time::Instant::now() + WAIT;
        while !path.exists() {
            assert!(tokio::time::Instant::now() < deadline, "no worktree");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        cancel.send_replace(true);
    };
    let (result, ()) = tokio::join!(run, stopper);
    assert_eq!(result, Err(ProvisionError::Stopped));
    provision::undo_partial(&plan, &program, &before).await;
    assert!(!path.exists());
    assert_eq!(git(&repo, &["branch", "--list", "fd/test"]), "");
    assert!(git(&repo, &["branch", "--list", "keep"]).contains("keep"));
    assert!(!git(&repo, &["worktree", "list"]).contains("app--fd-test"));
}

#[tokio::test]
async fn a_cancel_keeps_a_branch_that_existed_before() {
    let root = tempfile::tempdir().unwrap();
    let repo = root.path().join("app");
    std::fs::create_dir_all(&repo).unwrap();
    init_repo(&repo);
    git(&repo, &["branch", "old"]);
    let path = root.path().join("app--fd-old");
    let plan = WorkspacePlan::Worktree {
        repo: repo.to_string_lossy().into_owned(),
        path: path.to_string_lossy().into_owned(),
        branch: "old".to_owned(),
        base: None,
    };
    let program = slow_git(root.path());
    let before = provision::before(&plan, &program).await;
    let (cancel, rx) = watch::channel(false);
    let run = provision::provision_cancellable(&plan, &program, rx);
    let stopper = async {
        let deadline = tokio::time::Instant::now() + WAIT;
        while !path.exists() {
            assert!(tokio::time::Instant::now() < deadline, "no worktree");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        cancel.send_replace(true);
    };
    let (result, ()) = tokio::join!(run, stopper);
    assert_eq!(result, Err(ProvisionError::Stopped));
    provision::undo_partial(&plan, &program, &before).await;
    assert!(!path.exists());
    assert!(git(&repo, &["branch", "--list", "old"]).contains("old"));
}

#[tokio::test]
async fn a_cancel_during_the_clone_removes_the_new_directory() {
    let root = tempfile::tempdir().unwrap();
    let repo = root.path().join("app");
    std::fs::create_dir_all(&repo).unwrap();
    init_repo(&repo);
    let path = root.path().join("copy");
    let plan = WorkspacePlan::Clone {
        url: format!("file://{}", repo.display()),
        path: path.to_string_lossy().into_owned(),
    };
    let program = slow_git(root.path());
    let before = provision::before(&plan, &program).await;
    let (cancel, rx) = watch::channel(false);
    let run = provision::provision_cancellable(&plan, &program, rx);
    let stopper = async {
        let deadline = tokio::time::Instant::now() + WAIT;
        while !path.join("a.txt").exists() {
            assert!(tokio::time::Instant::now() < deadline, "no clone");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        cancel.send_replace(true);
    };
    let (result, ()) = tokio::join!(run, stopper);
    assert_eq!(result, Err(ProvisionError::Stopped));
    provision::undo_partial(&plan, &program, &before).await;
    assert!(!path.exists());
}

#[tokio::test]
async fn a_directory_that_existed_before_is_not_removed_after_a_cancel() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("copy");
    std::fs::create_dir_all(&path).unwrap();
    std::fs::write(path.join("mine.txt"), "mine").unwrap();
    let plan = WorkspacePlan::Clone {
        url: "file:///nonexistent".to_owned(),
        path: path.to_string_lossy().into_owned(),
    };
    let program = Path::new("git");
    let before = provision::before(&plan, program).await;
    let (_cancel, rx) = watch::channel(true);
    assert_eq!(
        provision::provision_cancellable(&plan, program, rx).await,
        Err(ProvisionError::Stopped)
    );
    provision::undo_partial(&plan, program, &before).await;
    assert!(path.join("mine.txt").is_file());
}
