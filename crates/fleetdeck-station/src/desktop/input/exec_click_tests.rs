//! The click of the input protocol (R13 P7b §10.2, §10.3, §11, §13): the
//! order of the steps and the argv, the refusals before the claim (the tool
//! and the compositor), every refusal after the movement (no `wlrctl` run),
//! the supervision of `wlrctl`, and the single dispatch of a click.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use fleetdeck_proto::coordinator_desktop_wire::AuthorityPhase;
use fleetdeck_proto::desktop::DesktopAction as A;
use serde_json::json;

use super::Claimed;
use super::rig::{Fx, Probe, foreign_active, nothing, other_client, pair, refused};
use crate::desktop::Fail;
use crate::desktop::coordinator::{Live, check_live_input, lost};

const MOVE: &str = "hyprctl dispatch movecursor 105 105";
const ACTIVE: &str = "hyprctl -j activewindow";

/// The timeline of a click in the preview at `(5, 5)`.
fn full_timeline(fx: &Fx) -> Vec<String> {
    [
        "claim",
        "check",
        "notify-send",
        "check",
        &fx.focus_line(),
        "check",
        ACTIVE,
        MOVE,
        "check",
        ACTIVE,
        "wlrctl",
        ACTIVE,
    ]
    .map(str::to_owned)
    .to_vec()
}

/// A desktop whose shell window `0xbeef` is active and listed.
async fn listed_shell() -> (Fx, String) {
    let fx = Fx::new().await;
    fx.set_active(&foreign_active());
    let handle = fx.other_handle().await;
    fx.clear_logs();
    (fx, handle)
}

/// A refusal after the movement: the movement ran once, `wlrctl` never.
fn moved_but_not_clicked(fx: &Fx) {
    let moves = fx.timeline().iter().filter(|l| *l == MOVE).count();
    assert_eq!(moves, 1, "{:?}", fx.timeline());
    assert!(fx.calls_of("wlrctl").is_empty(), "{:?}", fx.timeline());
    assert!(!fx.desktop().input.is_held());
}

// ------------------------------------------------------- order and argv

#[tokio::test]
async fn a_click_runs_the_steps_in_order_with_the_fixed_argv() {
    let fx = Fx::new().await;
    let probe = Probe::new(&fx);
    fx.run(&fx.click(5, 5), &probe).await.unwrap();
    assert_eq!(fx.timeline(), full_timeline(&fx));
    assert_eq!(
        *probe.check.phases.lock().unwrap(),
        [AuthorityPhase::Execute; 4]
    );
    assert_eq!(fx.calls_of("wlrctl")[0].args, ["pointer", "click", "left"]);
    let moves = fx.calls_of("hyprctl");
    let movement = moves
        .iter()
        .find(|c| c.args.get(1).is_some_and(|a| a == "movecursor"))
        .unwrap();
    assert_eq!(movement.args, ["dispatch", "movecursor", "105", "105"]);
    // The compositor query and the bundle are reads.
    let reads: Vec<String> = fx
        .rig
        .reads()
        .into_iter()
        .map(|c| c.args.join(" "))
        .collect();
    assert!(reads.contains(&"instances -j".to_owned()), "{reads:?}");
    assert!(reads.contains(&"-j cursorpos".to_owned()), "{reads:?}");
    assert_eq!(probe.claim.calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        fx.calls_of("notify-send")[0].args.last().unwrap(),
        "Fleet Deck is clicking for bold-fox in an agent preview."
    );
    assert!(!fx.desktop().input.is_held());
}

#[tokio::test]
async fn a_click_in_a_listed_window_has_its_own_notification_and_point() {
    let (fx, handle) = listed_shell().await;
    let probe = Probe::new(&fx);
    fx.run(&fx.click_any(&handle, 10, 20), &probe)
        .await
        .unwrap();
    let timeline = fx.timeline();
    assert!(
        timeline.contains(&"hyprctl dispatch movecursor 2010 120".to_owned()),
        "{timeline:?}"
    );
    assert!(timeline.contains(&"wlrctl".to_owned()), "{timeline:?}");
    assert_eq!(
        fx.calls_of("notify-send")[0].args.last().unwrap(),
        "Fleet Deck is clicking for bold-fox in an application window."
    );
    assert_eq!(fx.calls_of("wlrctl").len(), 1);
}

#[tokio::test]
async fn an_invalid_click_is_refused_before_anything_runs() {
    let fx = Fx::new().await;
    let probe = Probe::new(&fx);
    nothing(&fx, &probe, &fx.click(32_768, 0), "invalid_params").await;
    nothing(&fx, &probe, &fx.click(0, 32_768), "invalid_params").await;
}

// -------------------------------------------- refusals before the claim

#[tokio::test]
async fn a_missing_wlrctl_refuses_the_click_before_the_claim_but_not_the_keys() {
    let fx = Fx::new().await;
    let probe = Probe::new(&fx);
    std::fs::remove_file(fx.root().join("bin/wlrctl")).unwrap();
    for action in [fx.click(5, 5), fx.click_any(&fx.window_id, 5, 5)] {
        let e = refused(&fx, &probe, &action).await;
        assert_eq!(
            pair(&e),
            (
                "input_click_tool_missing",
                "The pointer click tool wlrctl is not installed on the target station.".into()
            )
        );
        assert!(fx.calls_of("notify-send").is_empty());
        assert_eq!(super::rig::calls(&fx), 0);
        assert_eq!(probe.claim.calls.load(Ordering::SeqCst), 0);
    }
    fx.run(&fx.key("a"), &probe).await.unwrap();
    fx.run(&fx.pointer(1, 1), &probe).await.unwrap();
}

#[tokio::test]
async fn a_compositor_that_does_not_correspond_refuses_the_click_before_the_claim() {
    const SIG: &str = crate::desktop::fake::SIGNATURE;
    let cases: [(&str, Vec<(&str, &str)>); 5] = [
        ("no entry", vec![]),
        ("another instance only", vec![("other", "wayland-1")]),
        ("two entries", vec![(SIG, "wayland-1"), (SIG, "wayland-1")]),
        ("a socket mismatch", vec![(SIG, "wayland-9")]),
        ("an empty socket", vec![(SIG, ""), ("other", "wayland-1")]),
    ];
    for (what, entries) in cases {
        let fx = Fx::new().await;
        let probe = Probe::new(&fx);
        fx.set_instances_later(&entries);
        let e = refused(&fx, &probe, &fx.click(5, 5)).await;
        assert_eq!(
            pair(&e),
            (
                "input_click_tool_unsupported",
                "The target desktop cannot use the configured pointer click tool.".into()
            ),
            "{what}"
        );
        assert_eq!(super::rig::calls(&fx), 0, "{what}");
        assert_eq!(probe.claim.calls.load(Ordering::SeqCst), 0, "{what}");
    }
}

#[tokio::test]
async fn an_instances_query_that_cannot_be_read_is_input_failed_before_the_claim() {
    for body in ["not json", "{}", "[1, 2"] {
        let fx = Fx::new().await;
        let probe = Probe::new(&fx);
        fx.instances_later_text(body);
        nothing(&fx, &probe, &fx.click(5, 5), "input_failed").await;
    }
}

// --------------------------------------------- refusals after the move

#[tokio::test]
async fn a_withdrawn_authority_stops_the_click_at_the_third_and_the_fourth_check() {
    // The third check: no movement.
    let fx = Fx::new().await;
    let mut probe = Probe::new(&fx);
    probe.check = fx.check(Some((3, lost())), None);
    let e = refused(&fx, &probe, &fx.click(5, 5)).await;
    assert_eq!(e.code, "authority_lost");
    assert!(!fx.timeline().iter().any(|l| l.contains("movecursor")));
    assert!(fx.calls_of("wlrctl").is_empty());
    // The fourth check: the pointer moved, the button did not start, and no
    // bundle was read.
    let fx = Fx::new().await;
    let mut probe = Probe::new(&fx);
    probe.check = fx.check(Some((4, lost())), None);
    let e = refused(&fx, &probe, &fx.click(5, 5)).await;
    assert_eq!(e.code, "authority_lost");
    moved_but_not_clicked(&fx);
    assert_eq!(fx.timeline().last().unwrap(), "check");
    assert_eq!(fx.timeline().iter().filter(|l| *l == ACTIVE).count(), 1);
}

#[tokio::test]
async fn a_cursor_that_drifted_refuses_the_button() {
    let fx = Fx::new().await;
    let probe = Probe::new(&fx);
    fx.flag("cursor_drift", true);
    let e = refused(&fx, &probe, &fx.click(5, 5)).await;
    assert_eq!(e.code, "input_destination_unavailable");
    moved_but_not_clicked(&fx);
}

#[tokio::test]
async fn a_changed_rectangle_another_client_or_a_layer_refuses_the_button() {
    let changes = [
        ("a resized target", {
            let fx = Fx::new().await;
            let mut resized = fx.preview_client();
            resized["size"] = json!([700, 600]);
            fx.write("clients_after.json", &json!([resized, other_client()]));
            fx.write_text(
                "on_move.sh",
                &format!(
                    "/bin/cp {0}/clients_after.json {0}/clients.json",
                    fx.root().display()
                ),
            );
            fx
        }),
        ("a client over the point", {
            let fx = Fx::new().await;
            let mut over = other_client();
            over["at"] = json!([100, 100]);
            over["size"] = json!([50, 50]);
            fx.write("clients_after.json", &json!([fx.preview_client(), over]));
            fx.write_text(
                "on_move.sh",
                &format!(
                    "/bin/cp {0}/clients_after.json {0}/clients.json",
                    fx.root().display()
                ),
            );
            fx
        }),
        ("a layer over the point", {
            let fx = Fx::new().await;
            fx.write(
                "layers_after.json",
                &json!({"DP-1": {"levels": {"3": [{"x": 100, "y": 100, "w": 50, "h": 50}]}}}),
            );
            fx.write_text(
                "on_move.sh",
                &format!(
                    "/bin/cp {0}/layers_after.json {0}/layers.json",
                    fx.root().display()
                ),
            );
            fx
        }),
    ];
    for (what, fx) in changes {
        let probe = Probe::new(&fx);
        let e = refused(&fx, &probe, &fx.click(5, 5)).await;
        assert_eq!(e.code, "input_destination_unavailable", "{what}");
        moved_but_not_clicked(&fx);
    }
}

#[tokio::test]
async fn another_active_window_or_a_board_title_refuses_the_button() {
    let fx = Fx::new().await;
    let probe = Probe::new(&fx);
    fx.write("after_move.json", &foreign_active());
    fx.write_text(
        "on_move.sh",
        &format!(
            "/bin/cp {0}/after_move.json {0}/active.json",
            fx.root().display()
        ),
    );
    let e = refused(&fx, &probe, &fx.click(5, 5)).await;
    assert_eq!(e.code, "focus_changed");
    moved_but_not_clicked(&fx);
    // The target itself now looks like a board window.
    let fx = Fx::new().await;
    fx.write("after_move.json", &fx.preview_active("Fleet Deck"));
    fx.write_text(
        "on_move.sh",
        &format!(
            "/bin/cp {0}/after_move.json {0}/active.json",
            fx.root().display()
        ),
    );
    let e = refused(&fx, &Probe::new(&fx), &fx.click(5, 5)).await;
    assert_eq!(e.code, "input_board_forbidden");
    moved_but_not_clicked(&fx);
}

#[tokio::test]
async fn a_bundle_query_that_stalls_or_cannot_be_read_gives_focus_unknown() {
    // A cursor query over the 250 ms of the whole bundle.
    let fx = Fx::new().await;
    let probe = Probe::new(&fx);
    fx.flag("cursor_stall", true);
    let started = Instant::now();
    let e = refused(&fx, &probe, &fx.click(5, 5)).await;
    assert_eq!(e.code, "input_focus_unknown");
    assert!(started.elapsed() < Duration::from_secs(3));
    moved_but_not_clicked(&fx);
    // A cursor query that fails or answers with garbage.
    for flag in ["cursor_fail", "cursor_garbage"] {
        let fx = Fx::new().await;
        let probe = Probe::new(&fx);
        fx.flag(flag, true);
        let e = refused(&fx, &probe, &fx.click(5, 5)).await;
        assert_eq!(e.code, "input_focus_unknown", "{flag}");
        moved_but_not_clicked(&fx);
    }
    // An active-window query that fails inside the bundle (the first read of
    // the third check passes).
    let fx = Fx::new().await;
    let probe = Probe::new(&fx);
    fx.write_text(
        "on_move.sh",
        &format!(": > {}/active_fail", fx.root().display()),
    );
    let e = refused(&fx, &probe, &fx.click(5, 5)).await;
    assert_eq!(e.code, "input_focus_unknown");
    moved_but_not_clicked(&fx);
}

/// A clock that adds `extra` to the real time once the rig has the file
/// `late`. The fake `cursorpos` creates it, so that the dispatch time of the
/// bundle is before the jump and the time of the spawn checks is after it.
fn late_clock(fx: &Fx, extra: Duration) -> super::Clock {
    let late = fx.root().join("late");
    fx.write_text("on_cursor.sh", &format!(": > {}", late.display()));
    Arc::new(move || {
        let now = Instant::now();
        if late.exists() { now + extra } else { now }
    })
}

/// Runs `hook` once, at the first read of the local gates after the cursor
/// query of the final bundle (the fake `cursorpos` creates the file `late`).
/// The gates are read before the first synchronous check of the button.
fn hook_after_bundle(
    fx: &Fx,
    probe: &mut Probe,
    action: &A,
    hook: impl Fn() + Send + Sync + 'static,
) {
    let late = fx.root().join("late");
    fx.write_text("on_cursor.sh", &format!(": > {}", late.display()));
    let (action, done) = (action.clone(), AtomicBool::new(false));
    probe.gates = Some(Arc::new(move || {
        if late.exists() && !done.swap(true, Ordering::SeqCst) {
            hook();
        }
        check_live_input(&super::rig::all_on(), &action)
    }));
}

#[tokio::test]
async fn an_old_observation_or_an_old_authority_answer_refuses_the_button() {
    // An observation 150 ms after its dispatch (the limit is 100 ms).
    let fx = Fx::new().await;
    let mut probe = Probe::new(&fx);
    probe.clock = Some(late_clock(&fx, Duration::from_millis(150)));
    let e = refused(&fx, &probe, &fx.click(5, 5)).await;
    assert_eq!(e.code, "input_focus_unknown");
    moved_but_not_clicked(&fx);
    // An authority answer 600 ms old (the limit is 500 ms); it is read first.
    let fx = Fx::new().await;
    let mut probe = Probe::new(&fx);
    probe.clock = Some(late_clock(&fx, Duration::from_millis(600)));
    let e = refused(&fx, &probe, &fx.click(5, 5)).await;
    assert_eq!(e.code, "authority_unknown");
    moved_but_not_clicked(&fx);
    // 90 ms is inside both limits.
    let fx = Fx::new().await;
    let mut probe = Probe::new(&fx);
    probe.clock = Some(late_clock(&fx, Duration::from_millis(90)));
    fx.run(&fx.click(5, 5), &probe).await.unwrap();
    assert_eq!(fx.calls_of("wlrctl").len(), 1);
}

#[tokio::test]
async fn the_click_gate_turned_off_between_the_move_and_the_spawn_refuses_the_button() {
    let fx = Fx::new().await;
    let mut probe = Probe::new(&fx);
    let off = fx.root().join("gates_off");
    let (flag, action) = (off.clone(), fx.click(5, 5));
    probe.gates = Some(Arc::new(move || {
        let mut live: Live = super::rig::all_on();
        live.desktop_click = !flag.exists();
        check_live_input(&live, &action)
    }));
    // The cursor query (inside the bundle) turns the click gate off.
    fx.write_text("on_cursor.sh", &format!(": > {}", off.display()));
    let e = refused(&fx, &probe, &fx.click(5, 5)).await;
    assert_eq!(e.code, "input_click_disabled");
    moved_but_not_clicked(&fx);
}

#[tokio::test]
async fn a_start_deadline_that_passed_between_the_move_and_the_spawn_refuses_the_button() {
    let fx = Fx::new().await;
    let mut probe = Probe::new(&fx);
    let harness = Arc::clone(&probe.harness);
    hook_after_bundle(&fx, &mut probe, &fx.click(5, 5), move || {
        harness.advance_mono(11_000);
    });
    let e = refused(&fx, &probe, &fx.click(5, 5)).await;
    assert_eq!(e.code, "failed");
    assert_eq!(
        e.message,
        "The authorization of this desktop action expired."
    );
    moved_but_not_clicked(&fx);
}

#[tokio::test]
async fn an_expired_handle_or_a_new_epoch_between_the_move_and_the_spawn_refuses_the_button() {
    // The expiry of the handle.
    let (fx, handle) = listed_shell().await;
    let mut probe = Probe::new(&fx);
    probe.clock = Some(late_clock(&fx, Duration::from_secs(61)));
    let e = refused(&fx, &probe, &fx.click_any(&handle, 10, 20)).await;
    assert_eq!(
        pair(&e),
        (
            "desktop_window_not_found",
            "No input window available to this grant matches that handle.".into()
        )
    );
    assert_eq!(fx.calls_of("wlrctl").len(), 0);
    // A new epoch of the event stream.
    let (fx, handle) = listed_shell().await;
    let events = fx.desktop().events.clone();
    let mut probe = Probe::new(&fx);
    let action = fx.click_any(&handle, 10, 20);
    hook_after_bundle(&fx, &mut probe, &action, move || events.inject_gap());
    let e = refused(&fx, &probe, &action).await;
    assert_eq!(e.code, "desktop_window_not_found");
    assert_eq!(fx.calls_of("wlrctl").len(), 0);
    assert!(!fx.desktop().input.is_held());
}

// ----------------------------------------------------- during wlrctl

async fn until(what: &str, mut ready: impl FnMut() -> bool) {
    for _ in 0..400 {
        if ready() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("timeout: {what}");
}

/// Whether the process is gone. A zombie is dead.
fn pid_gone(pid: &str) -> bool {
    let Ok(stat) = std::fs::read_to_string(format!("/proc/{}/stat", pid.trim())) else {
        return true;
    };
    stat.rsplit_once(") ")
        .is_none_or(|(_, rest)| rest.starts_with('Z'))
}

/// Runs a click against a `wlrctl` that hangs, and `disturb` once it runs.
async fn disturbed(fx: &Fx, probe: &Probe, disturb: impl FnOnce()) -> Fail {
    fx.flag("wlrctl_stall", true);
    let root = fx.root();
    let driver = async {
        until("wlrctl runs", || root.join("wlrctl.child").exists()).await;
        disturb();
    };
    let action = fx.click(5, 5);
    let (result, ()) = tokio::join!(fx.run(&action, probe), driver);
    result.unwrap_err()
}

/// `wlrctl` and its group are gone; it ran once; only polls followed it.
async fn killed_and_quiet(fx: &Fx) {
    let (pid, child) = (fx.read("wlrctl.pid"), fx.read("wlrctl.child"));
    until("the group is gone", || pid_gone(&pid) && pid_gone(&child)).await;
    assert_eq!(fx.calls_of("wlrctl").len(), 1);
    let timeline = fx.timeline();
    let after: Vec<&String> = timeline
        .iter()
        .skip_while(|l| *l != "wlrctl")
        .skip(1)
        .collect();
    assert!(after.iter().all(|l| **l == ACTIVE), "{timeline:?}");
    assert!(!fx.desktop().input.is_held());
}

#[tokio::test]
async fn a_focus_change_or_a_gate_off_during_wlrctl_kills_it_without_a_second_command() {
    let fx = Fx::new().await;
    let probe = Probe::new(&fx);
    let active = fx.root().join("active.json");
    let e = disturbed(&fx, &probe, || {
        std::fs::write(active, foreign_active().to_string()).unwrap();
    })
    .await;
    assert_eq!(e.code, "focus_changed");
    killed_and_quiet(&fx).await;
    // The click gate turns off.
    let fx = Fx::new().await;
    let mut probe = Probe::new(&fx);
    let off = fx.root().join("gates_off");
    let (flag, action) = (off.clone(), fx.click(5, 5));
    probe.gates = Some(Arc::new(move || {
        let mut live: Live = super::rig::all_on();
        live.desktop_click = !flag.exists();
        check_live_input(&live, &action)
    }));
    let e = disturbed(&fx, &probe, || std::fs::write(off, "").unwrap()).await;
    assert_eq!(e.code, "input_interrupted");
    killed_and_quiet(&fx).await;
}

#[tokio::test]
async fn a_wlrctl_that_hangs_is_killed_at_its_limit_of_one_second() {
    let fx = Fx::new().await;
    let probe = Probe::new(&fx);
    fx.flag("wlrctl_stall", true);
    let started = Instant::now();
    let e = refused(&fx, &probe, &fx.click(5, 5)).await;
    assert_eq!(e.code, "input_timeout");
    assert!(started.elapsed() >= Duration::from_millis(900));
    assert!(started.elapsed() < Duration::from_secs(6));
    killed_and_quiet(&fx).await;
}

#[tokio::test]
async fn a_failing_wlrctl_or_a_focus_change_after_a_clean_exit_is_never_replayed() {
    let fx = Fx::new().await;
    let probe = Probe::new(&fx);
    fx.flag("wlrctl_fail", true);
    let e = refused(&fx, &probe, &fx.click(5, 5)).await;
    assert_eq!(e.code, "input_failed");
    assert_eq!(fx.calls_of("wlrctl").len(), 1);
    // Clean exit, but the focus moved while it ran.
    let fx = Fx::new().await;
    let probe = Probe::new(&fx);
    fx.flag("wlrctl_changes_focus", true);
    let e = refused(&fx, &probe, &fx.click(5, 5)).await;
    assert_eq!(e.code, "focus_changed");
    assert_eq!(fx.calls_of("wlrctl").len(), 1);
    // The final observation cannot be made: the result stays open.
    let fx = Fx::new().await;
    let probe = Probe::new(&fx);
    fx.write_text("after_wlrctl.json", "{\"address\": \"\", \"class\": 5}");
    fx.flag("wlrctl_changes_focus", true);
    let e = refused(&fx, &probe, &fx.click(5, 5)).await;
    assert_eq!(e.code, "input_interrupted");
    assert_eq!(fx.calls_of("wlrctl").len(), 1);
}

// ----------------------------------------------------------- one click

#[tokio::test]
async fn a_done_click_stations_one_claim_and_a_duplicate_claim_does_not_click_again() {
    let fx = Fx::new().await;
    let probe = Probe::new(&fx);
    fx.run(&fx.click(5, 5), &probe).await.unwrap();
    assert_eq!(probe.claim.calls.load(Ordering::SeqCst), 1);
    assert_eq!(fx.calls_of("wlrctl").len(), 1);
    // Only the dispatches of the protocol: one focus and one movement; no
    // release, no Escape, no focus or cursor restore.
    let dispatches: Vec<String> = fx
        .calls_of("hyprctl")
        .into_iter()
        .filter(|c| c.args.first().is_some_and(|a| a == "dispatch"))
        .map(|c| c.args.join(" "))
        .collect();
    assert_eq!(
        dispatches,
        [
            format!("dispatch focuswindow address:{}", fx.record.address),
            "dispatch movecursor 105 105".to_owned()
        ]
    );
    // The same operation again: the claim answers a replay.
    fx.clear_logs();
    let again = fx.claim(Claimed::Replay);
    fx.run_with(&fx.click(5, 5), &probe, again.as_ref())
        .await
        .unwrap();
    assert_eq!(again.calls.load(Ordering::SeqCst), 1);
    assert_eq!(fx.timeline(), ["claim"]);
}
