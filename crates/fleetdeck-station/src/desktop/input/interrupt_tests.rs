//! The failures after the injection started and the interruption of
//! `wtype` (R13 P7 §7.5, §10): a preview close, a focus change, a policy
//! change, a lost link and an unreadable poll kill the child and record the
//! fixed outcome; no compensating key follows. A stalled child is killed at
//! its limit with its group.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::future::Future;
use std::sync::atomic::Ordering;
use std::time::Duration;

use super::rig::{Fx, Probe, foreign_active};
use crate::desktop::Fail;
use crate::desktop::authority::Admitted;
use crate::desktop::coordinator::input_off;
use crate::desktop::coordinator_tests::DECK;
use crate::desktop::tests::on;

/// Whether the process is gone. A zombie is dead: it only waits for a reap
/// that a shim of the test session may not do.
fn pid_gone(pid: &str) -> bool {
    let Ok(stat) = std::fs::read_to_string(format!("/proc/{}/stat", pid.trim())) else {
        return true;
    };
    stat.rsplit_once(") ")
        .is_none_or(|(_, rest)| rest.starts_with('Z'))
}

async fn until(what: &str, mut ready: impl FnMut() -> bool) {
    for _ in 0..400 {
        if ready() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("timeout: {what}");
}

/// Runs `action` against a `wtype` that stalls, and `disturb` once the child
/// runs. Returns the failure of the call.
async fn disturbed<F, Fut>(
    fx: &Fx,
    probe: &Probe,
    action: &fleetdeck_proto::desktop::DesktopAction,
    disturb: F,
) -> Fail
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = ()>,
{
    fx.flag("wtype_stall", true);
    let root = fx.root();
    let driver = async {
        until("the child runs", || root.join("wtype.child").exists()).await;
        disturb().await;
    };
    let (result, ()) = tokio::join!(fx.run(action, probe), driver);
    result.unwrap_err()
}

async fn assert_killed_and_quiet(fx: &Fx) {
    // Both the leader and the process of its group are gone.
    let pid = fx.read("wtype.pid");
    let child = fx.read("wtype.child");
    until("the group is gone", || pid_gone(&pid) && pid_gone(&child)).await;
    // One `wtype` run, and no key or dispatch of the station after it: only
    // the polls (and the close that an owner asked for).
    assert_eq!(fx.calls_of("wtype").len(), 1);
    let timeline = fx.timeline();
    let after: Vec<&String> = timeline
        .iter()
        .skip_while(|l| *l != "wtype")
        .skip(1)
        .collect();
    assert!(
        after.iter().all(|l| {
            **l == "hyprctl -j activewindow" || l.starts_with("hyprctl dispatch closewindow")
        }),
        "{timeline:?}"
    );
    assert!(!fx.desktop().input.is_held());
}

#[tokio::test]
async fn a_preview_close_during_the_input_kills_the_child_without_a_key() {
    let fx = Fx::new().await;
    let probe = Probe::new(&fx);
    let desktop = std::sync::Arc::clone(fx.desktop());
    let address = fx.record.address.clone();
    let e = disturbed(&fx, &probe, &fx.text("typing"), || async move {
        // The owner close does not wait behind the input slot: it only
        // removes the record.
        desktop.events.forget(&address);
    })
    .await;
    assert_eq!(e.code, "input_interrupted");
    assert_eq!(
        e.message,
        "Input was interrupted. Some input may already have been sent. Do not retry automatically."
    );
    assert_killed_and_quiet(&fx).await;
}

#[tokio::test]
async fn a_focus_change_during_the_input_is_seen_by_the_poll() {
    let fx = Fx::new().await;
    let probe = Probe::new(&fx);
    let active = fx.root().join("active.json");
    let e = disturbed(&fx, &probe, &fx.text("typing"), || async move {
        std::fs::write(active, foreign_active().to_string()).unwrap();
    })
    .await;
    assert_eq!(e.code, "focus_changed");
    assert_killed_and_quiet(&fx).await;
}

#[tokio::test]
async fn a_board_title_during_the_input_stops_it() {
    let fx = Fx::new().await;
    let probe = Probe::new(&fx);
    let active = fx.root().join("active.json");
    let title = fx.preview_active("Fleet Deck");
    let e = disturbed(&fx, &probe, &fx.key("a"), || async move {
        std::fs::write(active, title.to_string()).unwrap();
    })
    .await;
    assert_eq!(e.code, "input_board_forbidden");
    assert_killed_and_quiet(&fx).await;
}

#[tokio::test]
async fn the_policy_turned_off_during_the_input_stops_it() {
    let fx = Fx::new().await;
    let mut probe = Probe::new(&fx);
    let off = fx.root().join("gates_off");
    let flag = off.clone();
    probe.gates = Some(std::sync::Arc::new(move || {
        if flag.exists() {
            Err(input_off())
        } else {
            Ok(())
        }
    }));
    let e = disturbed(&fx, &probe, &fx.text("typing"), || async move {
        std::fs::write(off, "").unwrap();
    })
    .await;
    assert_eq!(e.code, "input_interrupted");
    assert_killed_and_quiet(&fx).await;
}

#[tokio::test]
async fn a_monitor_list_that_excludes_the_preview_during_the_input_stops_it() {
    let fx = Fx::new().await;
    let mut probe = Probe::new(&fx);
    let list = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let base = probe.live.clone();
    let read = std::sync::Arc::clone(&list);
    probe.live_now = Some(std::sync::Arc::new(move || {
        let mut live = base.clone();
        live.monitors = read.lock().unwrap().clone();
        live
    }));
    let e = disturbed(&fx, &probe, &fx.text("typing"), || async move {
        // The preview lies on DP-1 only: a list without it interrupts.
        *list.lock().unwrap() = vec!["HDMI-A-1".to_owned()];
    })
    .await;
    assert_eq!(e.code, "input_interrupted");
    assert_killed_and_quiet(&fx).await;
}

#[tokio::test]
async fn a_lost_link_during_the_input_stops_it() {
    let fx = Fx::new().await;
    let probe = Probe::new(&fx);
    let alive = std::sync::Arc::clone(&probe.alive);
    let e = disturbed(&fx, &probe, &fx.text("typing"), || async move {
        alive.store(false, Ordering::SeqCst);
    })
    .await;
    assert_eq!(e.code, "input_interrupted");
    assert_killed_and_quiet(&fx).await;
}

#[tokio::test]
async fn a_poll_that_fails_or_is_malformed_stops_the_input() {
    for flag in ["active_fail", "active_garbage"] {
        let fx = Fx::new().await;
        let probe = Probe::new(&fx);
        let root = fx.root();
        let e = disturbed(&fx, &probe, &fx.text("typing"), || async move {
            std::fs::write(root.join(flag), "").unwrap();
        })
        .await;
        assert_eq!(e.code, "input_interrupted", "{flag}");
        assert_killed_and_quiet(&fx).await;
    }
}

#[tokio::test]
async fn a_child_that_passes_its_limit_is_killed_with_its_group() {
    let fx = Fx::new().await;
    let mut probe = Probe::new(&fx);
    probe.wtype_limit = Some(Duration::from_millis(400));
    fx.flag("wtype_stall", true);
    let started = std::time::Instant::now();
    let e = fx.run(&fx.text("slow"), &probe).await.unwrap_err();
    assert_eq!(e.code, "input_timeout");
    assert_eq!(
        e.message,
        "Input did not finish in time. Some input may already have been sent. Do not retry automatically."
    );
    assert!(started.elapsed() < Duration::from_secs(5));
    assert_killed_and_quiet(&fx).await;
}

#[tokio::test]
async fn the_handler_limit_drops_a_stalled_call_and_kills_the_child() {
    let fx = Fx::new().await;
    let mut probe = Probe::new(&fx);
    probe.handler_limit = Some(Duration::from_millis(700));
    fx.flag("wtype_stall", true);
    let e = fx.run(&fx.key("a"), &probe).await.unwrap_err();
    assert_eq!(e.code, "input_timeout");
    assert_killed_and_quiet(&fx).await;
}

#[tokio::test]
async fn a_failing_tool_is_input_failed_and_a_change_after_a_clean_exit_is_seen() {
    let fx = Fx::new().await;
    let probe = Probe::new(&fx);
    fx.flag("wtype_fail", true);
    let e = fx.run(&fx.key("a"), &probe).await.unwrap_err();
    assert_eq!(e.code, "input_failed");
    assert_eq!(
        e.message,
        "The input tool failed. Some input may already have been sent. Do not retry automatically."
    );
    fx.flag("wtype_fail", false);
    // The tool ends cleanly, but the focus moved while it ran: the final
    // observation reports it (and the poll may have seen it first).
    fx.wtype_changes_focus_to(&foreign_active());
    let e = fx.run(&fx.key("a"), &probe).await.unwrap_err();
    assert_eq!(e.code, "focus_changed");
}

#[tokio::test]
async fn a_final_observation_that_cannot_be_made_leaves_the_result_open() {
    let fx = Fx::new().await;
    let probe = Probe::new(&fx);
    fx.flag("wtype_breaks_active", true);
    let e = fx.run(&fx.key("a"), &probe).await.unwrap_err();
    assert_eq!(e.code, "input_interrupted");
    assert_eq!(fx.calls_of("wtype").len(), 1);
}

#[tokio::test]
async fn a_clean_run_leaves_the_preview_and_the_slot_alone() {
    let fx = Fx::new().await;
    let probe = Probe::new(&fx);
    fx.run(&fx.text("fine"), &probe).await.unwrap();
    assert!(fx.desktop().events.preview(&fx.window_id).is_some());
    assert!(!fx.desktop().input.is_held());
    // Only the dispatches of the protocol ran: one focus, nothing to restore.
    let dispatches: Vec<String> = fx
        .calls_of("hyprctl")
        .into_iter()
        .filter(|c| c.args.first().is_some_and(|a| a == "dispatch"))
        .map(|c| c.args.join(" "))
        .collect();
    assert_eq!(
        dispatches,
        [format!(
            "dispatch focuswindow address:{}",
            fx.record.address
        )]
    );
}

#[tokio::test]
async fn the_owner_close_of_the_preview_does_not_wait_behind_the_input_slot() {
    let fx = Fx::new().await;
    let probe = Probe::new(&fx);
    let window_id = fx.window_id.clone();
    let desktop = std::sync::Arc::clone(fx.desktop());
    let e = disturbed(&fx, &probe, &fx.text("typing"), || async move {
        // The slot is held by the stalled input: the owner close still runs
        // and ends the preview.
        let started = std::time::Instant::now();
        desktop
            .act_for_deck(
                &on,
                fleetdeck_proto::desktop::DesktopAction::PreviewClose { window_id },
                Admitted::open(),
                Some(DECK.to_owned()),
            )
            .await
            .unwrap();
        assert!(started.elapsed() < Duration::from_secs(3));
    })
    .await;
    assert_eq!(e.code, "input_interrupted");
    assert_killed_and_quiet(&fx).await;
}
