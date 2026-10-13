//! The station side of the TUI handover (DL-140) after a restart: the start
//! recovery of holds, and the station stop that ends a TUI. The "provider" is
//! a shell script that is the instance binary.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use fleetdeck_station::shell::handover::{Liveness, liveness, recover_holds};

mod tui_common;
use tui_common::*;

#[tokio::test]
async fn after_a_restart_a_hold_with_a_dead_process_is_released() {
    let mut rig = Rig::new("exec cat").await;
    let s = rig.ended("s-a", "native-a").await;
    accepted(&rig.handover("tui-1", &s).await);
    // A process id that is gone: a child that has ended and was reaped.
    let mut gone = std::process::Command::new("true").spawn().unwrap();
    let dead = gone.id();
    gone.wait().unwrap();
    assert!(
        rig.store
            .record_terminal_pid(s.clone(), "tui-1".to_owned(), dead)
            .await
            .unwrap()
    );

    let rig = rig.restarted().await;
    assert!(rig.summary(&s).await.terminal.is_some());
    recover_holds(&rig.store).await.unwrap();
    assert!(rig.summary(&s).await.terminal.is_none());
    assert_eq!(
        rig.reasons(&s).await.last().map(String::as_str),
        Some("the station restarted; the terminal process is gone")
    );
}

#[tokio::test]
async fn after_a_restart_a_hold_with_a_live_process_stays_until_the_owner_releases_it() {
    let mut rig = Rig::new("exec cat").await;
    let s = rig.ended("s-a", "native-a").await;
    accepted(&rig.handover("tui-1", &s).await);
    let mut alive = std::process::Command::new("sleep")
        .arg("30")
        .spawn()
        .unwrap();
    let pid = alive.id();
    assert!(
        rig.store
            .record_terminal_pid(s.clone(), "tui-1".to_owned(), pid)
            .await
            .unwrap()
    );

    let mut rig = rig.restarted().await;
    recover_holds(&rig.store).await.unwrap();
    let hold = rig.summary(&s).await.terminal.expect("the hold stays");
    assert_eq!(hold.pid, Some(pid));
    assert_eq!(
        hold.reason,
        Some(format!(
            "the station restarted; process {pid} may still use this session"
        ))
    );
    // It is still held: nothing resumes it or hands it over again.
    let d = rig.handover("tui-2", &s).await;
    assert!(rejected(&d).starts_with("a terminal holds this session (shell `tui-1`)"));

    let d = rig.release("tui-1").await;
    accepted(&d);
    assert!(rig.summary(&s).await.terminal.is_none());
    assert_eq!(
        rig.reasons(&s).await.last().map(String::as_str),
        Some("the owner released the terminal hold")
    );
    alive.kill().unwrap();
    alive.wait().unwrap();
}

#[tokio::test]
async fn after_a_restart_a_handover_that_never_ran_releases_its_hold() {
    let mut rig = Rig::new("exec cat").await;
    let s = rig.ended("s-a", "native-a").await;
    let d = rig.handover("tui-1", &s).await;
    accepted(&d);
    assert_eq!(rig.effect_state(&d.receipt.command_id).await, "queued");

    let rig = rig.restarted().await;
    assert_eq!(
        rig.effect_state(&d.receipt.command_id).await,
        "retired_not_dispatched"
    );
    recover_holds(&rig.store).await.unwrap();
    assert!(rig.summary(&s).await.terminal.is_none());
    assert_eq!(
        rig.reasons(&s).await.last().map(String::as_str),
        Some("the station restarted before the terminal started")
    );
}

#[tokio::test]
async fn after_a_restart_a_handover_that_may_have_started_keeps_its_hold() {
    let mut rig = Rig::new("exec cat").await;
    let s = rig.ended("s-a", "native-a").await;
    accepted(&rig.handover("tui-1", &s).await);
    // Claimed, and the station stopped before it knew the outcome.
    assert!(rig.store.claim_shell_effect().await.unwrap().is_some());

    let rig = rig.restarted().await;
    recover_holds(&rig.store).await.unwrap();
    let hold = rig.summary(&s).await.terminal.expect("the hold stays");
    assert_eq!(hold.pid, None);
    assert_eq!(
        hold.reason.as_deref(),
        Some("the station restarted; a terminal process may still use this session")
    );
}

#[tokio::test]
async fn after_a_restart_a_hold_whose_process_cannot_be_checked_stays() {
    // Pid 1 is a process that a non-root user cannot signal (`EPERM`).
    if rustix::process::geteuid().is_root() || liveness(1) != Liveness::Unknown {
        eprintln!("skipped: pid 1 can be probed by this user");
        return;
    }
    let mut rig = Rig::new("exec cat").await;
    let s = rig.ended("s-a", "native-a").await;
    accepted(&rig.handover("tui-1", &s).await);
    assert!(
        rig.store
            .record_terminal_pid(s.clone(), "tui-1".to_owned(), 1)
            .await
            .unwrap()
    );

    let rig = rig.restarted().await;
    recover_holds(&rig.store).await.unwrap();
    let hold = rig.summary(&s).await.terminal.expect("the hold stays");
    assert_eq!(hold.pid, Some(1));
    assert_eq!(
        rig.reasons(&s).await.last().map(String::as_str),
        Some("the station restarted; it cannot check process 1, so the hold stays")
    );
}

#[tokio::test]
async fn station_stop_kills_a_tui_that_ignores_the_hangup() {
    let mut rig = Rig::new(DEAF_TUI).await;
    let s = rig.ended("s-a", "native-a").await;
    accepted(&rig.handover("tui-1", &s).await);
    assert_eq!(rig.dispatch().await, 1);
    let ready = rig.root.join("ready");
    eventually("the TUI ignores the hangup", || async { ready.exists() }).await;
    let pid = rig.summary(&s).await.terminal.unwrap().pid.unwrap();
    assert_ne!(liveness(pid), Liveness::Gone);

    let shells = Arc::clone(&rig.shells);
    tokio::task::spawn_blocking(move || shells.close_all())
        .await
        .unwrap();
    // The pump reaps the zombie right after close_all saw it.
    eventually("the TUI process is gone", || async {
        liveness(pid) != Liveness::Running
    })
    .await;
}

#[tokio::test]
async fn after_a_restart_a_failed_handover_with_a_hold_keeps_it() {
    let mut rig = Rig::new("exec cat").await;
    let s = rig.ended("s-a", "native-a").await;
    let d = rig.handover("tui-1", &s).await;
    accepted(&d);
    // Claimed and finished as failed, but the hold was not released: the
    // start was uncertain.
    let effect = rig.store.claim_shell_effect().await.unwrap().unwrap();
    rig.store
        .finish_shell_effect(effect.effect_id, Err("shell task: panic".into()))
        .await
        .unwrap();
    assert_eq!(rig.effect_state(&d.receipt.command_id).await, "failed");

    let rig = rig.restarted().await;
    recover_holds(&rig.store).await.unwrap();
    let hold = rig.summary(&s).await.terminal.expect("the hold stays");
    assert_eq!(hold.pid, None);
    assert_eq!(
        hold.reason.as_deref(),
        Some("the station restarted; a terminal process may still use this session")
    );
}
