//! The B9 PRE-PR round 1 fixes: a provisioned folder takes its identity
//! right after the Git work, the preparation Git runs in the checked source
//! repository, and the checkpoint start-up runs in the checked folder.

use std::path::{Path, PathBuf};
use std::process::Command;

use fleetdeck_core::launch_dir::{CHANGED, LaunchDir};
use fleetdeck_proto::SessionId;
use fleetdeck_proto::command::{SessionCommand, StopMode, WorkspaceRequest};
use fleetdeck_proto::event::{CheckpointSide, Event};
use fleetdeck_proto::{EventBody, launch::LaunchIdentity};

use crate::manager::tests::setup::station;
use crate::manager::tests::{Station, WAIT, spec, text};
use crate::workspace::checkpoint::CheckpointError;
use crate::workspace::{GitOptions, Worktree};

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .current_dir(dir)
        .args(["-c", "user.name=t", "-c", "user.email=t@t"])
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .unwrap();
    assert!(out.status.success(), "{args:?}: {out:?}");
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

fn init_repo(dir: &Path) {
    git(dir, &["init", "-q", "-b", "main"]);
    std::fs::write(dir.join("a.txt"), "one\n").unwrap();
    git(dir, &["add", "a.txt"]);
    git(dir, &["commit", "-qm", "one"]);
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

fn worktree_start(cwd: &str, setup_cmd: Option<&str>) -> SessionCommand {
    SessionCommand::Start {
        spec: spec(cwd, "claude"),
        prompt: Some(text("hello")),
        workspace: Some(Box::new(WorkspaceRequest::Worktree {
            branch: None,
            base: None,
        })),
        setup_cmd: setup_cmd.map(str::to_owned),
        batch: None,
        issue: None,
        origin_text: false,
        fleet_git: None,
    }
}

/// The worktree path and the branch that the decide planned for `session`.
async fn planned(st: &Station, session: &SessionId) -> (PathBuf, String) {
    let summary = st
        .manager
        .store()
        .summary(session.clone())
        .await
        .unwrap()
        .unwrap();
    (
        PathBuf::from(summary.worktree.unwrap()),
        summary.branch.unwrap(),
    )
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn a_provisioned_folder_replaced_by_another_plain_folder_after_the_git_work_does_not_start() {
    let mut st = station(true).await;
    let repo = st.root.path().join("projects/app");
    init_repo(&repo);
    st.manager.launch_hook().arm();
    let d = st
        .accepted(worktree_start(
            &repo.to_string_lossy(),
            Some("touch ran.txt"),
        ))
        .await;
    let s = d.receipt.result.unwrap().session_id.unwrap();
    let (path, _branch) = planned(&st, &s).await;
    // The holds, in order: the base read in the source repository, the
    // parent and the source repository of the Git work, the identity of the
    // new folder, and the `child_dir` of the setup command.
    reached(&st).await;
    for _ in 0..4 {
        st.manager.launch_hook().arm();
        st.manager.launch_hook().release.notify_one();
        reached(&st).await;
    }
    assert!(path.is_dir(), "the Git work did not create the worktree");
    let moved = path.with_file_name("moved-away");
    std::fs::rename(&path, &moved).unwrap();
    std::fs::create_dir(&path).unwrap();
    st.manager.launch_hook().release.notify_one();
    assert_eq!(start_failure(&st).await, CHANGED);
    st.settled(&s).await;
    assert!(!path.join("ran.txt").exists(), "the command ran");
    assert!(!moved.join("ran.txt").exists(), "the command ran");
    st.manager.shutdown(StopMode::Kill).await;
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn a_worktree_is_provisioned_from_the_checked_repository() {
    let mut st = station(true).await;
    let repo = st.root.path().join("projects/app");
    init_repo(&repo);
    st.manager.launch_hook().arm();
    let d = st
        .accepted(worktree_start(&repo.to_string_lossy(), None))
        .await;
    let s = d.receipt.result.unwrap().session_id.unwrap();
    let (path, branch) = planned(&st, &s).await;
    // The base read, the parent, then the source repository of the Git work.
    reached(&st).await;
    for _ in 0..2 {
        st.manager.launch_hook().arm();
        st.manager.launch_hook().release.notify_one();
        reached(&st).await;
    }
    st.manager.launch_hook().release.notify_one();
    st.wait_for("started", |e| {
        matches!(e, Event::SessionStarted { .. }).then_some(())
    })
    .await;
    assert!(path.join("a.txt").is_file());
    assert_eq!(git(&path, &["rev-parse", "--abbrev-ref", "HEAD"]), branch);
    st.manager.shutdown(StopMode::Kill).await;
}

/// The identity of `dir` below the rig's root.
fn identity_of(st: &Station, dir: &Path) -> LaunchIdentity {
    let roots = [std::fs::canonicalize(st.root.path().join("projects")).unwrap()];
    LaunchDir::open_in(&roots, dir).unwrap().identity().clone()
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn checkpoints_open_in_the_checked_folder() {
    let mut st = Station::new(4).await;
    let ws = st.workspace();
    init_repo(Path::new(&ws));
    // A start: the first turn gets an available checkpoint side.
    let s = st.start(&ws, Some("hello")).await;
    let deadline = tokio::time::Instant::now() + WAIT;
    loop {
        let available = st.events().await.iter().any(|e| {
            e.session_id == s
                && matches!(
                    &e.event,
                    EventBody::Known(Event::TurnFinalized {
                        checkpoint: Some(c),
                        ..
                    }) if matches!(c.start, CheckpointSide::Available { .. })
                )
        });
        if available {
            break;
        }
        assert!(tokio::time::Instant::now() < deadline, "no checkpoint");
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    // The start-up of the checkpoints: the reopened folder, the worktree root
    // is the canonical folder.
    let mut with_id = spec(&ws, "claude");
    with_id.launch = Some(Box::new(identity_of(&st, Path::new(&ws))));
    let target = std::sync::Arc::clone(&st.manager.target);
    let (dir, pinned) = target.turns_dir(&with_id).expect("the folder reopens");
    let roots = target.current_policy().unwrap().allowed_roots().to_vec();
    let worktree = Worktree::open_in_roots(&pinned, GitOptions::isolated(), Some(&roots))
        .await
        .unwrap();
    assert_eq!(worktree.root(), std::fs::canonicalize(&ws).unwrap());
    drop(dir);
    assert!(
        target
            .open_turns(&"s-fix".into(), &with_id)
            .await
            .has_worktree()
    );
    // A folder that cannot be reopened: no checkpoints.
    let mut gone = with_id.clone();
    if let Some(id) = gone.launch.as_mut() {
        id.ino += 1;
    }
    assert!(target.turns_dir(&gone).is_none());
    assert!(
        !target
            .open_turns(&"s-fix2".into(), &gone)
            .await
            .has_worktree()
    );
    st.manager.shutdown(StopMode::Kill).await;
}

#[tokio::test]
async fn a_toplevel_outside_the_roots_gives_no_worktree() {
    let repo = tempfile::tempdir().unwrap();
    let other = tempfile::tempdir().unwrap();
    init_repo(repo.path());
    let inside = [std::fs::canonicalize(repo.path()).unwrap()];
    let outside = [std::fs::canonicalize(other.path()).unwrap()];
    let ok = Worktree::open_in_roots(repo.path(), GitOptions::isolated(), Some(&inside)).await;
    assert!(ok.is_ok());
    assert!(matches!(
        Worktree::open_in_roots(repo.path(), GitOptions::isolated(), Some(&outside)).await,
        Err(CheckpointError::NotAWorktree(_))
    ));
    // `None` keeps today's behavior.
    assert!(
        Worktree::open(repo.path(), GitOptions::isolated())
            .await
            .is_ok()
    );
}
