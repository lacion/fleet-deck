//! The station side of the TUI handover (DL-140): `shell.handover` admits an
//! ended session, holds it while the provider TUI runs in a PTY, and ends
//! the hold only when the process is gone; `shell.release` and the start
//! recovery clear holds that the station cannot see. The "provider" is a
//! shell script that is the instance binary.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use fleetdeck_proto::event::{ProviderKind, SessionState};
use fleetdeck_proto::shell::{ShellChange, ShellState};
use fleetdeck_proto::{Event, ProviderInstanceId, SessionCommand, SessionId};
use fleetdeck_station::manager::{Policy, PolicyHub};
use fleetdeck_station::shell::handover::recover_holds;
use fleetdeck_station::shell::{ShellConfig, Shells, dispatch};
use fleetdeck_station::store::OfflineApprovalPolicy;

mod tui_common;
use tui_common::*;

#[tokio::test]
async fn a_handover_of_an_ended_session_holds_it_and_starts_the_tui() {
    let mut rig = Rig::new("echo \"$@\" > args.txt\nexec cat").await;
    let s = rig.ended("s-a", "native-a").await;

    let d = rig.handover("tui-1", &s).await;
    accepted(&d);
    // The admission holds the session before any PTY I/O.
    let hold = rig.summary(&s).await.terminal.expect("held at admission");
    assert_eq!(hold.shell_id, "tui-1");
    assert_eq!(hold.pid, None);
    assert_eq!(hold.reason.as_deref(), Some("handover requested"));
    assert!(rig.shells.list().is_empty());

    assert_eq!(rig.dispatch().await, 1);
    let hold = rig.summary(&s).await.terminal.expect("still held");
    assert!(hold.pid.is_some(), "the pid is in the hold: {hold:?}");
    let shells = rig.shells.list();
    assert_eq!(shells.len(), 1);
    assert_eq!(shells[0].shell_id, "tui-1");
    assert_eq!(shells[0].session_id.as_ref(), Some(&s));
    assert_eq!(shells[0].state, ShellState::Running);
    assert_eq!(rig.effect_state(&d.receipt.command_id).await, "confirmed");

    // The TUI runs in the canonical workspace with the stored mode and model.
    let args = rig.root.join("args.txt");
    eventually("the TUI wrote its arguments", || async {
        args.exists() && std::fs::read_to_string(&args).is_ok_and(|t| t.ends_with('\n'))
    })
    .await;
    assert_eq!(
        std::fs::read_to_string(&args).unwrap().trim(),
        "--resume native-a --permission-mode plan --model opus"
    );
    rig.shells.close_all();
}

#[tokio::test]
async fn a_resume_of_a_held_session_is_refused() {
    let mut rig = Rig::new("exec cat").await;
    let s = rig.ended("s-a", "native-a").await;
    accepted(&rig.handover("tui-1", &s).await);

    let hold = "a terminal holds this session (shell `tui-1`); return it to board control first";
    let d = rig
        .session(SessionCommand::Resume {
            session_id: s.clone(),
        })
        .await;
    assert_eq!(rejected(&d), hold);
    // The auto-continue admits through the same function.
    let d = rig.auto_continue(&s).await;
    assert_eq!(rejected(&d), hold);
}

#[tokio::test]
async fn a_handover_of_a_running_session_is_refused() {
    let mut rig = Rig::new("exec cat").await;
    let s = rig.started("s-a", "native-a").await;
    rig.store
        .append(
            s.clone(),
            vec![
                Event::SessionStateChanged {
                    state: SessionState::Running,
                    detail: None,
                }
                .into(),
            ],
        )
        .await
        .unwrap();

    let d = rig.handover("tui-1", &s).await;
    assert_eq!(rejected(&d), "only an ended session can be resumed");
    assert!(rig.summary(&s).await.terminal.is_none());
}

#[tokio::test]
async fn a_second_handover_of_one_session_is_refused() {
    let mut rig = Rig::new("exec cat").await;
    let s = rig.ended("s-a", "native-a").await;
    accepted(&rig.handover("tui-1", &s).await);

    let d = rig.handover("tui-2", &s).await;
    assert_eq!(
        rejected(&d),
        "a terminal holds this session (shell `tui-1`); return it to board control first"
    );
    assert_eq!(rig.summary(&s).await.terminal.unwrap().shell_id, "tui-1");
}

#[tokio::test]
async fn a_session_with_the_same_native_id_as_a_held_one_cannot_run_either() {
    let mut rig = Rig::new("exec cat").await;
    let a = rig.ended("s-a", "native-x").await;
    let b = rig.ended("s-b", "native-x").await;
    accepted(&rig.handover("tui-1", &a).await);
    let alias = "session `s-a` holds this native session in a terminal";

    let d = rig
        .session(SessionCommand::Resume {
            session_id: b.clone(),
        })
        .await;
    assert_eq!(rejected(&d), alias);
    let d = rig.handover("tui-2", &b).await;
    assert_eq!(rejected(&d), alias);
    let d = rig.session(import("native-x", &rig)).await;
    assert_eq!(rejected(&d), alias);
}

#[tokio::test]
async fn closing_a_tui_releases_the_hold_only_after_the_process_died() {
    // The TUI ignores SIGHUP, so only the 3 s kill of the process group
    // ends it.
    let mut rig = Rig::new("trap '' HUP\nwhile :; do sleep 1; done").await;
    let s = rig.ended("s-a", "native-a").await;
    accepted(&rig.handover("tui-1", &s).await);
    assert_eq!(rig.dispatch().await, 1);

    accepted(&rig.close("tui-1").await);
    assert_eq!(rig.dispatch().await, 1);
    assert!(!rig.shells.exists("tui-1"));
    tokio::time::sleep(Duration::from_millis(700)).await;
    let hold = rig.summary(&s).await.terminal;
    assert!(hold.is_some(), "the process still runs: the hold stays");

    eventually("the hold ends with the process", || async {
        released(&rig, &s).await
    })
    .await;
    let reasons = rig.reasons(&s).await;
    let last = reasons.last().unwrap();
    assert!(
        last.starts_with("the terminal process exited"),
        "{reasons:?}"
    );
}

#[tokio::test]
async fn closing_a_tui_that_hangs_up_releases_its_hold() {
    let mut rig = Rig::new("exec cat").await;
    let s = rig.ended("s-a", "native-a").await;
    accepted(&rig.handover("tui-1", &s).await);
    assert_eq!(rig.dispatch().await, 1);

    accepted(&rig.close("tui-1").await);
    assert_eq!(rig.dispatch().await, 1);
    eventually("the hold ends", || async { released(&rig, &s).await }).await;
    let reasons = rig.reasons(&s).await;
    assert!(
        reasons
            .last()
            .unwrap()
            .starts_with("the terminal process exited"),
        "{reasons:?}"
    );
}

#[tokio::test]
async fn a_tui_that_exits_by_itself_releases_the_hold_with_its_exit_code() {
    let mut rig = Rig::new("exit 3").await;
    let s = rig.ended("s-a", "native-a").await;
    accepted(&rig.handover("tui-1", &s).await);
    assert_eq!(rig.dispatch().await, 1);

    eventually("the hold ends", || async { released(&rig, &s).await }).await;
    let reasons = rig.reasons(&s).await;
    assert_eq!(
        reasons.last().map(String::as_str),
        Some("the terminal process exited (code 3)"),
        "{reasons:?}"
    );
}

#[tokio::test]
async fn a_tui_that_does_not_start_releases_the_hold_and_fails_the_effect() {
    let mut rig = Rig::new("exec cat").await;
    std::fs::remove_file(&rig.binary).unwrap();
    let s = rig.ended("s-a", "native-a").await;
    let d = rig.handover("tui-1", &s).await;
    accepted(&d);

    assert_eq!(rig.dispatch().await, 1);
    assert_eq!(rig.effect_state(&d.receipt.command_id).await, "failed");
    assert!(rig.summary(&s).await.terminal.is_none());
    let reasons = rig.reasons(&s).await;
    assert!(
        reasons
            .last()
            .unwrap()
            .starts_with("the terminal did not start: "),
        "{reasons:?}"
    );
    assert!(rig.shells.list().is_empty());
}

#[tokio::test]
async fn a_handover_whose_folder_left_the_allowed_roots_does_not_start_the_tui() {
    // NV-24: the policy at the dispatch decides, not the one at the admission.
    let mut rig = Rig::new("echo \"$@\" > args.txt\nexec cat").await;
    let s = rig.ended("s-a", "native-a").await;
    let d = rig.handover("tui-1", &s).await;
    accepted(&d);

    let elsewhere = tempfile::tempdir().unwrap();
    let later = Policy::new(
        vec![std::fs::canonicalize(elsewhere.path()).unwrap()],
        4,
        OfflineApprovalPolicy::Wait,
        BTreeMap::from([(ProviderInstanceId::new("claude"), ProviderKind::Claude)]),
    )
    .with_shell(true, 4);
    let done = dispatch(&rig.store, &rig.shells, &PolicyHub::new(later))
        .await
        .unwrap();
    assert_eq!(done, 1);

    assert_eq!(rig.effect_state(&d.receipt.command_id).await, "failed");
    assert!(rig.summary(&s).await.terminal.is_none(), "the hold ends");
    let reasons = rig.reasons(&s).await;
    assert!(
        reasons
            .last()
            .unwrap()
            .starts_with("the terminal did not start: the session directory is not allowed now: "),
        "{reasons:?}"
    );
    assert!(rig.shells.list().is_empty());
    assert!(!rig.root.join("args.txt").exists());
}

#[tokio::test]
async fn a_release_is_refused_while_the_shell_lives() {
    let mut rig = Rig::new("exec cat").await;
    let s = rig.ended("s-a", "native-a").await;
    accepted(&rig.handover("tui-1", &s).await);
    assert_eq!(rig.dispatch().await, 1);

    let d = rig.release("tui-1").await;
    assert_eq!(rejected(&d), "shell `tui-1` still runs; close it first");
    assert!(rig.summary(&s).await.terminal.is_some());
    rig.shells.close_all();
}

#[tokio::test]
async fn a_release_is_refused_while_the_handover_is_not_dispatched() {
    let mut rig = Rig::new("exec cat").await;
    let s = rig.ended("s-a", "native-a").await;
    let d = rig.handover("tui-1", &s).await;
    accepted(&d);
    assert_eq!(rig.effect_state(&d.receipt.command_id).await, "queued");

    let d = rig.release("tui-1").await;
    assert_eq!(
        rejected(&d),
        "shell `tui-1` has a pending change; try again when it settles"
    );
    assert!(rig.summary(&s).await.terminal.is_some(), "the hold stays");

    // Once the handover is done the TUI is the shell that runs.
    assert_eq!(rig.dispatch().await, 1);
    assert!(rig.summary(&s).await.terminal.unwrap().pid.is_some());
    rig.shells.close_all();
}

#[tokio::test]
async fn a_release_without_a_hold_is_refused() {
    let mut rig = Rig::new("exec cat").await;
    let d = rig.release("tui-9").await;
    assert_eq!(rejected(&d), "no session is held by shell `tui-9`");
}

#[tokio::test]
async fn a_handover_of_an_instance_without_a_binary_is_refused() {
    let mut rig = Rig::new("exec cat").await;
    let s = rig.ended("s-a", "native-a").await;
    rig.shells = Arc::new(Shells::with_env(ShellConfig::default(), child_env()));

    let d = rig.handover("tui-1", &s).await;
    assert_eq!(
        rejected(&d),
        "provider instance `claude` has no binary for a terminal"
    );
    assert!(rig.summary(&s).await.terminal.is_none());
    let _ = &rig.det;
}

/// A TUI that ignores SIGHUP and says (a file in its cwd) when it does.

#[tokio::test]
async fn a_release_is_refused_while_a_closed_tui_still_runs() {
    let mut rig = Rig::new(DEAF_TUI).await;
    let s = rig.ended("s-a", "native-a").await;
    accepted(&rig.handover("tui-1", &s).await);
    assert_eq!(rig.dispatch().await, 1);
    let ready = rig.root.join("ready");
    eventually("the TUI ignores the hangup", || async { ready.exists() }).await;

    accepted(&rig.close("tui-1").await);
    assert_eq!(rig.dispatch().await, 1);
    assert!(!rig.shells.exists("tui-1"));

    let d = rig.release("tui-1").await;
    assert_eq!(
        rejected(&d),
        "shell `tui-1` is closing and its process still runs; try again when it has stopped"
    );
    assert!(rig.summary(&s).await.terminal.is_some(), "the hold stays");

    eventually("the hold ends with the process", || async {
        released(&rig, &s).await
    })
    .await;
    let reasons = rig.reasons(&s).await;
    assert!(
        reasons
            .last()
            .unwrap()
            .starts_with("the terminal process exited"),
        "{reasons:?}"
    );
}

#[tokio::test]
async fn a_closing_tui_keeps_its_shell_id_reserved() {
    let mut rig = Rig::new(DEAF_TUI).await;
    let a = rig.ended("s-a", "native-a").await;
    let b = rig.ended("s-b", "native-b").await;
    accepted(&rig.handover("tui-1", &a).await);
    assert_eq!(rig.dispatch().await, 1);
    let ready = rig.root.join("ready");
    eventually("the TUI ignores the hangup", || async { ready.exists() }).await;

    accepted(&rig.close("tui-1").await);
    assert_eq!(rig.dispatch().await, 1);
    assert!(!rig.shells.exists("tui-1"));

    // The id of the closing TUI is not free for another session.
    let d = rig.handover("tui-1", &b).await;
    assert_eq!(rejected(&d), "shell `tui-1` exists");
    assert!(rig.summary(&b).await.terminal.is_none());
    let d = rig.release("tui-1").await;
    assert_eq!(
        rejected(&d),
        "shell `tui-1` is closing and its process still runs; try again when it has stopped"
    );
    assert!(rig.summary(&a).await.terminal.is_some(), "the hold stays");

    eventually("the hold ends with the process", || async {
        released(&rig, &a).await
    })
    .await;
    let reasons = rig.reasons(&a).await;
    assert!(
        reasons
            .last()
            .unwrap()
            .starts_with("the terminal process exited"),
        "{reasons:?}"
    );
}

#[tokio::test]
async fn a_shell_id_that_holds_a_session_is_not_used_again() {
    let mut rig = Rig::new("exec cat").await;
    let a = rig.ended("s-a", "native-a").await;
    let b = rig.ended("s-b", "native-b").await;
    let d = rig.handover("tui-1", &a).await;
    accepted(&d);
    let effect = rig.store.claim_shell_effect().await.unwrap().unwrap();
    rig.store
        .finish_shell_effect(effect.effect_id, Err("shell task: panic".into()))
        .await
        .unwrap();
    let mut rig = rig.restarted().await;
    recover_holds(&rig.store).await.unwrap();
    assert!(rig.summary(&a).await.terminal.is_some(), "the hold stays");

    let refused = "shell `tui-1` holds session `s-a`; release it first";
    assert_eq!(rejected(&rig.handover("tui-1", &b).await), refused);
    let open = ShellChange::Open {
        shell_id: "tui-1".to_owned(),
        cwd: rig.root.to_string_lossy().into_owned(),
        cols: 80,
        rows: 24,
    };
    assert_eq!(rejected(&rig.shell("open", open).await), refused);
    assert!(rig.summary(&b).await.terminal.is_none());
}

#[tokio::test]
async fn the_recovery_reads_only_the_handover_of_the_held_session() {
    let mut rig = Rig::new("exec cat").await;
    let a = rig.ended("s-a", "native-a").await;
    let b = rig.ended("s-b", "native-b").await;
    accepted(&rig.handover("tui-1", &a).await);
    let effect = rig.store.claim_shell_effect().await.unwrap().unwrap();
    rig.store
        .finish_shell_effect(effect.effect_id, Err("shell task: panic".into()))
        .await
        .unwrap();

    let state = |s: &SessionId| rig.store.handover_effect_state("tui-1".into(), s.clone());
    assert_eq!(state(&a).await.unwrap().as_deref(), Some("failed"));
    assert_eq!(state(&b).await.unwrap(), None);
}
