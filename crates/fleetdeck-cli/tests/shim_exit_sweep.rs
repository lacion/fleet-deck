//! The exit sweep of the shim (DL-431): at a provider exit the shim stops
//! the members that are left in the provider's group and session, with the
//! leader pinned as a zombie, and only then counts. Real `fleetdeck shim`
//! processes with `/bin/sh` providers; the test process plays the station.

#![cfg(target_os = "linux")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::Path;
use std::time::{Duration, Instant};

use fleetdeck_providers::host::shim::client::{Attachment, Launched};
use fleetdeck_providers::host::shim::wire::{ExitState, FromShim, ToShim};

mod shim_rig;
use shim_rig::*;

/// Attaches (generation 1) and returns the exit report, with the stdout
/// text that came before it. `on_text` sees the text each time it grows.
async fn report(rig: &Rig, mut on_text: impl FnMut(&str)) -> (Attachment, ExitState, String) {
    let (mut att, attached) = rig.attach(1).await;
    let FromShim::Attached { exit, .. } = attached else {
        panic!("{attached:?}");
    };
    if let Some(exit) = exit {
        return (att, exit, String::new());
    }
    att.send(
        &ToShim::Replay {
            generation: 1,
            from: 0,
        },
        &[],
    )
    .await
    .unwrap();
    let seen = read_until(&mut att, |s| {
        on_text(&s.text());
        exited(s).is_some()
    })
    .await;
    (att, exited(&seen).unwrap().clone(), seen.text())
}

/// Acknowledges the exit and waits until the shim ends.
async fn finish(mut launched: Launched, mut att: Attachment) {
    att.send(&ToShim::AckExit { generation: 1 }, &[])
        .await
        .unwrap();
    let status = tokio::time::timeout(WAIT, launched.child.wait())
        .await
        .unwrap()
        .unwrap();
    assert!(status.success(), "{status:?}");
}

fn pid_of(text: &str) -> u32 {
    text.trim().parse().unwrap_or_else(|_| panic!("{text:?}"))
}

/// The process group and the session of `pid`.
fn group_and_session(pid: u32) -> (u32, u32) {
    let text = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap();
    let rest: Vec<&str> = text[text.rfind(')').unwrap() + 1..]
        .split_whitespace()
        .collect();
    (rest[2].parse().unwrap(), rest[3].parse().unwrap())
}

fn kill9(pid: u32) {
    let _ = std::process::Command::new("kill")
        .args(["-KILL", &pid.to_string()])
        .status();
}

#[tokio::test]
async fn a_member_that_outlives_the_provider_is_stopped_before_the_report() {
    let rig = Rig::new("sleep 30 </dev/null >/dev/null 2>&1 & echo $!");
    let (launched, _) = rig.ready().await;
    let (att, exit, text) = report(&rig, |_| {}).await;
    assert_eq!(exit.descendants_remaining, Some(0), "{exit:?}");
    assert_eq!(exit.inspection, 1);
    assert_eq!(exit.cause, "exited");
    assert!(!alive(pid_of(&text)), "the member was stopped");
    finish(launched, att).await;
}

#[tokio::test]
async fn a_member_that_ignores_term_is_killed() {
    let rig = Rig::new(r#"sh -c 'trap "" TERM; sleep 30' </dev/null >/dev/null 2>&1 & echo $!"#);
    let (launched, _) = rig.ready().await;
    let started = Instant::now();
    let (att, exit, text) = report(&rig, |_| {}).await;
    assert_eq!(exit.descendants_remaining, Some(0), "{exit:?}");
    assert!(
        started.elapsed() < Duration::from_secs(7),
        "the report took {:?}",
        started.elapsed()
    );
    assert!(!alive(pid_of(&text)), "the member was killed");
    finish(launched, att).await;
}

#[tokio::test]
async fn a_session_member_in_another_group_is_stopped() {
    // A child in its own process group, in the provider's session (Codex
    // starts its MCP servers like this). `/bin/sh` here has no job control.
    let rig = Rig::new(
        r#"perl -e 'setpgrp(0,0); exec "sleep", "30"' </dev/null >/dev/null 2>&1 & p=$!; sleep 0.3; echo $p"#,
    );
    let (launched, provider) = rig.ready().await;
    let mut checked = false;
    let (att, exit, text) = report(&rig, |text| {
        if !checked && text.contains('\n') {
            checked = true;
            let (group, session) = group_and_session(pid_of(text));
            assert_ne!(group, provider, "the member is in another group");
            assert_eq!(session, provider, "the member is in the provider's session");
        }
    })
    .await;
    assert!(checked, "the member was seen alive in the session");
    assert_eq!(exit.descendants_remaining, Some(0), "{exit:?}");
    assert!(!alive(pid_of(&text)), "the member was stopped");
    finish(launched, att).await;
}

#[tokio::test]
async fn an_escaped_process_is_not_signalled() {
    let rig = Rig::new("setsid sleep 30 </dev/null >/dev/null 2>&1 & p=$!; sleep 0.4; echo $p");
    let (launched, _) = rig.ready().await;
    let (att, exit, text) = report(&rig, |_| {}).await;
    let escaped = pid_of(&text);
    assert_eq!(exit.descendants_remaining, Some(0), "{exit:?}");
    assert!(alive(escaped), "an escaped process is not touched");
    kill9(escaped);
    finish(launched, att).await;
}

#[tokio::test]
async fn the_leader_status_is_kept() {
    let rig = Rig::new("exit 3");
    let (launched, _) = rig.ready().await;
    let (att, exit, _) = report(&rig, |_| {}).await;
    assert_eq!(exit.termination, "code:3", "{exit:?}");
    assert_eq!(exit.cause, "exited");
    assert_eq!(exit.descendants_remaining, Some(0));
    finish(launched, att).await;
}

/// The reap ended the leader while the output waits for `ack`s: a late
/// `Interrupt` or `Kill` frame sends nothing (the pid number may be reused)
/// and does not rename the exit.
#[tokio::test]
async fn late_kill_and_interrupt_after_the_reap_change_nothing() {
    // About 6000 bytes in lines: 4 KiB fill the small shim buffer, the rest
    // fits in the pipe (also when the kernel gives a pipe one page only), so
    // the provider exits while the output waits for `ack`s.
    let rig = Rig::with(
        "head -c 6000 /dev/zero | tr '\\0' a | fold -w 100; exit 0",
        |spec| spec.buffer_bytes = 4096,
    );
    let (launched, provider) = rig.ready().await;
    let (mut att, _) = rig.attach(1).await;
    let gone = Instant::now() + WAIT;
    while Path::new(&format!("/proc/{provider}")).exists() {
        assert!(Instant::now() < gone, "the provider was not reaped");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    att.send(&ToShim::Interrupt { generation: 1 }, &[])
        .await
        .unwrap();
    att.send(&ToShim::Kill { generation: 1 }, &[])
        .await
        .unwrap();
    att.send(
        &ToShim::Replay {
            generation: 1,
            from: 0,
        },
        &[],
    )
    .await
    .unwrap();
    let exit = loop {
        let (frame, payload) = att.recv(WAIT).await.unwrap().expect("the shim is open");
        match frame {
            FromShim::Output { offset, .. } => {
                let cursor = offset + payload.len() as u64;
                att.send(
                    &ToShim::Ack {
                        generation: 1,
                        cursor,
                    },
                    &[],
                )
                .await
                .unwrap();
            }
            FromShim::Exited { exit } => break exit,
            _ => {}
        }
    };
    assert_eq!(exit.cause, "exited", "{exit:?}");
    assert_eq!(exit.termination, "code:0");
    assert_eq!(exit.descendants_remaining, Some(0));
    finish(launched, att).await;
}

#[tokio::test]
async fn a_kill_during_the_sweep_skips_the_grace() {
    let rig =
        Rig::new(r#"sh -c 'trap "" TERM; sleep 30' </dev/null >/dev/null 2>&1 & echo started"#);
    let (launched, provider) = rig.ready().await;
    let (mut att, _) = rig.attach(1).await;
    // The provider is a zombie (pinned) while the sweep waits for the member.
    let zombie = Instant::now() + WAIT;
    loop {
        let text = std::fs::read_to_string(format!("/proc/{provider}/stat")).unwrap_or_default();
        if text
            .rfind(')')
            .is_some_and(|i| text[i + 1..].trim_start().starts_with('Z'))
        {
            break;
        }
        assert!(Instant::now() < zombie, "the provider did not end");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
    let sent = Instant::now();
    att.send(&ToShim::Kill { generation: 1 }, &[])
        .await
        .unwrap();
    att.send(
        &ToShim::Replay {
            generation: 1,
            from: 0,
        },
        &[],
    )
    .await
    .unwrap();
    let seen = read_until(&mut att, |s| exited(s).is_some()).await;
    let exit = exited(&seen).unwrap();
    assert!(
        sent.elapsed() < Duration::from_secs(2),
        "the report took {:?}",
        sent.elapsed()
    );
    assert_eq!(exit.descendants_remaining, Some(0), "{exit:?}");
    assert_eq!(exit.cause, "exited", "a Kill after the end keeps the cause");
    finish(launched, att).await;
}
