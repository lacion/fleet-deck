//! Tests of the limits of one call (four children, five seconds for each
//! child, a total time) and of the real runner.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::ffi::OsString;
use std::path::Path;
use std::time::{Duration, Instant};

use fleetdeck_proto::desktop_tools::{FactStatus, Probe, Switch};

use super::facts::collect;
use super::probe::{
    Exec, MAX_CHILDREN, MAX_STDOUT, PROBE_LIMIT, ProbeRun, Probes, RealExec, TOTAL_LIMIT,
};
use super::test_support::{FakeExec, Rig, exit};

fn paths(n: usize) -> Vec<OsString> {
    (0..n).map(|i| OsString::from(format!("a{i}"))).collect()
}

#[test]
fn the_limits_are_the_designed_ones() {
    assert_eq!(MAX_CHILDREN, 4);
    assert_eq!(PROBE_LIMIT, Duration::from_secs(5));
    assert_eq!(TOTAL_LIMIT, Duration::from_secs(20));
    assert_eq!(MAX_STDOUT, 16 * 1024);
}

#[tokio::test]
async fn at_most_four_children_run_at_the_same_time() {
    let exec = FakeExec::new(|_| exit(0, "")).delayed(Duration::from_millis(40));
    let probes = Probes::new(&exec, TOTAL_LIMIT);
    let program = Path::new("/bin/x");
    let args = paths(1);
    let runs = futures_util::future::join_all(
        (0..12).map(|_| probes.run(program, &args, &[], PROBE_LIMIT)),
    )
    .await;
    assert!(runs.iter().all(|r| *r == exit(0, "")));
    assert_eq!(exec.calls().len(), 12);
    assert_eq!(exec.max_running(), MAX_CHILDREN);
}

#[tokio::test]
async fn a_whole_call_never_runs_more_than_four_children() {
    let rig = Rig::new();
    let found = rig.omarchy();
    for name in [
        "wtype",
        "grim",
        "jq",
        "busctl",
        "chromium",
        "wlrctl",
        "Hyprland",
        "notify-send",
    ] {
        rig.tool(name);
    }
    rig.omarchy_exe("hyprctl");
    rig.omarchy_exe("omarchy-sudo-passwordless");
    rig.omarchy_exe("omarchy-plugin-validate");
    rig.plugin_dir();
    let exec = FakeExec::new(|_| exit(1, "")).delayed(Duration::from_millis(30));
    let tools = collect(&exec, &rig.base(), Some(&found), TOTAL_LIMIT).await;
    assert!(exec.calls().len() >= 11, "{}", exec.calls().len());
    assert!(exec.max_running() <= MAX_CHILDREN);
    assert_eq!(tools.passwordless_sudo, Switch::Off);
}

#[tokio::test]
async fn the_limit_of_a_child_is_the_smaller_of_five_seconds_and_the_time_left() {
    let exec = FakeExec::new(|_| exit(0, ""));
    let long = Probes::new(&exec, TOTAL_LIMIT);
    long.run(Path::new("/bin/x"), &[], &[], PROBE_LIMIT).await;
    let short = Probes::new(&exec, Duration::from_secs(2));
    short.run(Path::new("/bin/x"), &[], &[], PROBE_LIMIT).await;
    let calls = exec.calls();
    assert_eq!(calls[0].limit, PROBE_LIMIT);
    assert!(calls[1].limit <= Duration::from_secs(2));
    assert!(calls[1].limit > Duration::from_millis(1500));
}

#[tokio::test]
async fn a_call_with_no_time_left_is_a_timeout_and_starts_no_child() {
    let exec = FakeExec::new(|_| exit(0, ""));
    let probes = Probes::new(&exec, Duration::ZERO);
    let run = probes.run(Path::new("/bin/x"), &[], &[], PROBE_LIMIT).await;
    assert_eq!(run, ProbeRun::Timeout);
    assert!(exec.calls().is_empty());
}

#[tokio::test]
async fn the_total_time_ends_the_whole_call() {
    let rig = Rig::new();
    let found = rig.omarchy();
    for name in ["wtype", "grim", "jq", "busctl", "chromium", "wlrctl"] {
        rig.tool(name);
    }
    rig.omarchy_exe("hyprctl");
    rig.omarchy_exe("omarchy-sudo-passwordless");
    // Every child needs one second; the call has about 100 ms.
    let exec = FakeExec::new(|_| exit(0, "")).delayed(Duration::from_secs(1));
    let start = Instant::now();
    let tools = collect(&exec, &rig.base(), Some(&found), Duration::from_millis(100)).await;
    assert!(
        start.elapsed() < Duration::from_millis(900),
        "{:?}",
        start.elapsed()
    );
    let calls = exec.calls();
    assert!(calls.len() >= MAX_CHILDREN);
    assert!(calls.iter().all(|c| c.limit <= Duration::from_millis(100)));
    for t in tools.tools.iter().filter(|t| t.present) {
        assert_eq!(t.probe, Probe::Timeout, "{}", t.id);
    }
    assert_eq!(tools.keyboard.status, FactStatus::Failed);
    assert_eq!(tools.passwordless_sudo, Switch::Unknown);
}

fn sh(script: &str) -> Vec<OsString> {
    vec![OsString::from("-c"), OsString::from(script)]
}

#[tokio::test]
async fn the_real_runner_returns_the_exit_code_and_stdout() {
    let run = RealExec
        .run(
            Path::new("/bin/sh"),
            &sh("printf hello; exit 3"),
            &[],
            Duration::from_secs(5),
        )
        .await;
    assert_eq!(
        run,
        ProbeRun::Exit {
            code: 3,
            stdout: b"hello".to_vec()
        }
    );
}

#[tokio::test]
async fn the_real_runner_clears_the_environment() {
    let env = [(OsString::from("ONLY"), OsString::from("1"))];
    let run = RealExec
        .run(
            Path::new("/bin/sh"),
            &sh("printf %s \"$ONLY$HOME\""),
            &env,
            Duration::from_secs(5),
        )
        .await;
    assert_eq!(
        run,
        ProbeRun::Exit {
            code: 0,
            stdout: b"1".to_vec()
        }
    );
}

#[tokio::test]
async fn the_real_runner_fails_for_more_than_16_kib_of_stdout() {
    let script = format!("head -c {} /dev/zero", MAX_STDOUT + 1);
    let run = RealExec
        .run(
            Path::new("/bin/sh"),
            &sh(&script),
            &[],
            Duration::from_secs(5),
        )
        .await;
    assert_eq!(run, ProbeRun::Failed);
    let script = format!("head -c {} /dev/zero", MAX_STDOUT);
    let run = RealExec
        .run(
            Path::new("/bin/sh"),
            &sh(&script),
            &[],
            Duration::from_secs(5),
        )
        .await;
    assert!(matches!(run, ProbeRun::Exit { code: 0, ref stdout } if stdout.len() == MAX_STDOUT));
}

#[tokio::test]
async fn the_real_runner_times_out_and_fails_for_a_missing_program() {
    let run = RealExec
        .run(
            Path::new("/bin/sh"),
            &sh("sleep 5"),
            &[],
            Duration::from_millis(100),
        )
        .await;
    assert_eq!(run, ProbeRun::Timeout);
    let run = RealExec
        .run(
            Path::new("/nonexistent/program"),
            &[],
            &[],
            Duration::from_secs(1),
        )
        .await;
    assert_eq!(run, ProbeRun::Failed);
    let run = RealExec
        .run(
            Path::new("/bin/sh"),
            &sh("kill -9 $$"),
            &[],
            Duration::from_secs(5),
        )
        .await;
    assert_eq!(run, ProbeRun::Failed);
}
