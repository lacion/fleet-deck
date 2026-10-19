//! Tests of the runner, of the status JSON parser and of the tick.

use std::ffi::OsString;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use fleetdeck_core::childenv::ChildEnv;
use tokio::sync::watch;

use super::child::{self, RunResult, STDOUT_CAP, Spec};
use super::status::parse_status;
use super::tests::{Rig, STATUS_OK, act, result, until};
use super::{Holder, tick};

fn sh(script: &str) -> Vec<OsString> {
    vec!["-c".into(), script.into()]
}

async fn run_sh(script: &str, limit: Duration) -> std::io::Result<RunResult> {
    let args = sh(script);
    let env = vec![("PATH".into(), "/usr/bin:/bin".into())];
    child::run(&Spec {
        program: Path::new("/bin/sh"),
        args: &args,
        env: &env,
        extra: &[],
        stdin: None,
        limit,
    })
    .await
}

#[tokio::test]
async fn omarchy_runner_returns_the_exit_code_and_both_outputs() {
    let r = run_sh("echo out; echo err >&2; exit 3", Duration::from_secs(5))
        .await
        .unwrap();
    match r {
        RunResult::Exit {
            code,
            stdout,
            stderr,
        } => {
            assert_eq!(code, Some(3));
            assert_eq!(stdout, b"out\n");
            assert_eq!(stderr, b"err\n");
        }
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn omarchy_runner_gives_stdin_as_dev_null() {
    let r = run_sh("cat; echo done", Duration::from_secs(5))
        .await
        .unwrap();
    match r {
        RunResult::Exit { stdout, .. } => assert_eq!(stdout, b"done\n"),
        other => panic!("{other:?}"),
    }
}

async fn run_stdin(script: &str, stdin: Vec<u8>) -> std::io::Result<RunResult> {
    let args = sh(script);
    let env = vec![("PATH".into(), "/usr/bin:/bin".into())];
    child::run(&Spec {
        program: Path::new("/bin/sh"),
        args: &args,
        env: &env,
        extra: &[],
        stdin: Some(stdin),
        limit: Duration::from_secs(5),
    })
    .await
}

#[tokio::test]
async fn omarchy_runner_gives_the_stdin_bytes_exactly_and_closes_the_pipe() {
    // `cat` ends only at the end of file: the pipe is closed after the write.
    let bytes: Vec<u8> = (0..=255_u8).chain(b"\n{\"a\":1}".iter().copied()).collect();
    let RunResult::Exit { stdout, code, .. } = run_stdin("cat", bytes.clone()).await.unwrap()
    else {
        panic!("no exit");
    };
    assert_eq!(code, Some(0));
    assert_eq!(stdout, bytes);
    // The whole cap fits.
    let full = vec![b'x'; child::STDIN_CAP];
    let RunResult::Exit { stdout, .. } = run_stdin("cat", full.clone()).await.unwrap() else {
        panic!("no exit");
    };
    assert_eq!(stdout.len(), full.len());
}

#[tokio::test]
async fn omarchy_runner_refuses_a_stdin_over_the_cap_before_it_starts_the_child() {
    let too_long = vec![b'x'; child::STDIN_CAP + 1];
    let e = run_stdin("exit 0", too_long).await.unwrap_err();
    assert_eq!(e.kind(), std::io::ErrorKind::InvalidInput);
}

#[tokio::test]
async fn omarchy_runner_a_child_that_ignores_its_stdin_still_ends_with_its_code() {
    let big = vec![b'x'; child::STDIN_CAP];
    let RunResult::Exit { code, stdout, .. } = run_stdin("echo bye; exit 4", big).await.unwrap()
    else {
        panic!("no exit");
    };
    assert_eq!(code, Some(4));
    assert_eq!(stdout, b"bye\n");
}

#[tokio::test]
async fn omarchy_runner_puts_the_child_in_its_own_group() {
    let r = run_sh(
        "cut -d' ' -f5 /proc/$$/stat; echo $$",
        Duration::from_secs(5),
    )
    .await
    .unwrap();
    let RunResult::Exit { stdout, .. } = r else {
        panic!("no exit");
    };
    let text = String::from_utf8(stdout).unwrap();
    let mut lines = text.lines().map(|l| l.trim().to_owned());
    let (pgid, pid) = (lines.next().unwrap(), lines.next().unwrap());
    assert_eq!(pgid, pid, "the child leads its own group");
}

#[tokio::test]
async fn omarchy_runner_kills_the_group_at_the_limit_and_on_too_much_output() {
    let started = std::time::Instant::now();
    let r = run_sh("sleep 30 & wait", Duration::from_millis(300))
        .await
        .unwrap();
    assert!(matches!(r, RunResult::TimedOut), "{r:?}");
    assert!(started.elapsed() < Duration::from_secs(8));
    // A child that floods stdout and then sleeps is killed at the cap.
    let script = format!("head -c {} /dev/zero; sleep 30", STDOUT_CAP + 1);
    let r = run_sh(&script, Duration::from_secs(20)).await.unwrap();
    assert!(matches!(r, RunResult::TooLarge), "{r:?}");
    assert!(started.elapsed() < Duration::from_secs(15));
}

#[tokio::test]
async fn omarchy_runner_reports_the_exit_code_after_the_group_kill_and_reap() {
    let r = run_sh("exit 3", Duration::from_secs(5)).await.unwrap();
    let RunResult::Exit { code, .. } = r else {
        panic!("no exit");
    };
    assert_eq!(code, Some(3));
}

#[tokio::test]
async fn omarchy_runner_kills_a_subprocess_left_behind_after_a_normal_exit() {
    // The background `sleep` keeps no pipe of the station, so the run ends at
    // the exit of the shell. The shell leads the group, so its pid is the pgid.
    let r = run_sh(
        "sleep 30 >/dev/null 2>&1 </dev/null & echo $$; exit 0",
        Duration::from_secs(5),
    )
    .await
    .unwrap();
    let RunResult::Exit { code, stdout, .. } = r else {
        panic!("no exit");
    };
    assert_eq!(code, Some(0));
    let pgid: i32 = String::from_utf8(stdout).unwrap().trim().parse().unwrap();
    let pgid = rustix::process::Pid::from_raw(pgid).unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(1);
    loop {
        match rustix::process::test_kill_process_group(pgid) {
            Err(rustix::io::Errno::SRCH) => break,
            other => {
                assert!(
                    std::time::Instant::now() < deadline,
                    "the group is still alive: {other:?}"
                );
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        }
    }
}

#[tokio::test]
async fn omarchy_runner_reports_a_program_that_does_not_start() {
    let env = Vec::new();
    let r = child::run(&Spec {
        program: Path::new("/nonexistent/program"),
        args: &[],
        env: &env,
        extra: &[],
        stdin: None,
        limit: Duration::from_secs(1),
    })
    .await;
    assert!(r.is_err());
}

#[tokio::test]
async fn omarchy_runner_extra_variables_win_over_the_base() {
    let args = sh("echo $A-$B");
    let env: Vec<(OsString, OsString)> =
        vec![("A".into(), "base".into()), ("B".into(), "base".into())];
    let extra = [("B", OsString::from("extra"))];
    let r = child::run(&Spec {
        program: Path::new("/bin/sh"),
        args: &args,
        env: &env,
        extra: &extra,
        stdin: None,
        limit: Duration::from_secs(5),
    })
    .await
    .unwrap();
    let RunResult::Exit { stdout, .. } = r else {
        panic!("no exit");
    };
    assert_eq!(stdout, b"base-extra\n");
}

#[test]
fn omarchy_status_json_parses_the_facts_of_the_contract() {
    let (install, saver) = parse_status(STATUS_OK.as_bytes()).unwrap();
    assert!(install.installed && install.plugin_enabled && install.hyprland_line);
    assert!(!install.outdated);
    assert_eq!(
        (
            install.files_ok,
            install.files_changed,
            install.files_missing
        ),
        (2, 1, 0)
    );
    let saver = saver.unwrap();
    assert!(saver.enabled);
    assert_eq!((saver.file_restored, saver.blocked), (None, None));
}

fn json(edit: impl FnOnce(&mut serde_json::Value)) -> Vec<u8> {
    let mut v: serde_json::Value = serde_json::from_str(STATUS_OK).unwrap();
    edit(&mut v);
    serde_json::to_vec(&v).unwrap()
}

#[test]
fn omarchy_status_json_edge_cases() {
    // An unknown file state is not counted; unknown keys are ignored.
    let bytes = json(|v| {
        v["files"] = serde_json::json!([{"state": "weird"}, {"state": "missing"}]);
        v["extra"] = serde_json::json!(1);
        v["record_version"] = serde_json::json!("x");
    });
    let (install, _) = parse_status(&bytes).unwrap();
    assert_eq!(
        (
            install.files_ok,
            install.files_changed,
            install.files_missing
        ),
        (0, 0, 1)
    );
    // `hyprland_line` other than "ok" is false.
    let bytes = json(|v| v["hyprland_line"] = serde_json::json!("missing"));
    assert!(!parse_status(&bytes).unwrap().0.hyprland_line);
    // `screensaver: null` is none; `blocked` is kept only when known.
    let bytes = json(|v| v["screensaver"] = serde_json::Value::Null);
    assert_eq!(parse_status(&bytes).unwrap().1, None);
    let bytes = json(|v| {
        v["screensaver"] = serde_json::json!(
            {"enabled": true, "file_restored": false, "blocked": "kept_original_differs"}
        );
    });
    let saver = parse_status(&bytes).unwrap().1.unwrap();
    assert_eq!(saver.file_restored, Some(false));
    assert_eq!(saver.blocked.as_deref(), Some("kept_original_differs"));
    let bytes = json(|v| {
        v["screensaver"] =
            serde_json::json!({"enabled": false, "file_restored": null, "blocked": "other"});
    });
    assert_eq!(parse_status(&bytes).unwrap().1.unwrap().blocked, None);
    // A missing key or a wrong type is a failure.
    for key in [
        "installed",
        "files",
        "plugin_enabled",
        "hyprland_line",
        "outdated",
        "screensaver",
    ] {
        let bytes = json(|v| {
            v.as_object_mut().unwrap().remove(key);
        });
        assert!(parse_status(&bytes).is_none(), "missing {key}");
    }
    for (key, bad) in [
        ("installed", serde_json::json!("yes")),
        ("files", serde_json::json!({})),
        ("plugin_enabled", serde_json::json!(1)),
        ("hyprland_line", serde_json::json!(true)),
        ("outdated", serde_json::json!(null)),
        ("screensaver", serde_json::json!(3)),
    ] {
        let bytes = json(|v| v[key] = bad.clone());
        assert!(parse_status(&bytes).is_none(), "wrong type {key}");
    }
    let bytes = json(|v| v["files"] = serde_json::json!([{"state": 4}]));
    assert!(parse_status(&bytes).is_none());
    assert!(parse_status(b"").is_none());
    assert!(parse_status(b"not json").is_none());
}

fn write_record(rig: &Rig) {
    std::fs::write(rig.path("config/fleetdeck/omarchy-install.json"), "{}").unwrap();
}

fn tick_runs(rig: &Rig) -> usize {
    rig.runs()
        .iter()
        .filter(|r| r.args.last().map(String::as_str) == Some("screensaver-tick"))
        .count()
}

#[tokio::test]
async fn omarchy_tick_is_skipped_without_a_record_or_omarchy_or_config_home() {
    let rig = Rig::new("exit 0");
    rig.admin.tick_once().await;
    assert_eq!(tick_runs(&rig), 0, "no record");
    write_record(&rig);
    let none = Rig::with("exit 0", |cfg| cfg.omarchy_env = ChildEnv::empty());
    write_record(&none);
    none.admin.tick_once().await;
    assert_eq!(tick_runs(&none), 0, "no Omarchy");
    let home = Rig::with("exit 0", |cfg| cfg.config_home = None);
    home.admin.tick_once().await;
    assert_eq!(tick_runs(&home), 0, "no config home");
}

#[tokio::test]
async fn omarchy_tick_runs_the_fixed_child_when_a_record_exists() {
    let rig = Rig::new("exit 1");
    write_record(&rig);
    rig.admin.tick_once().await;
    let runs = rig.runs();
    assert_eq!(runs.len(), 1, "{runs:?}");
    let data = rig.path("data").to_string_lossy().into_owned();
    assert_eq!(
        runs[0].args,
        ["--data-dir", &data, "omarchy", "screensaver-tick"]
    );
    assert_eq!(
        runs[0].env.get("WAYLAND_DISPLAY").map(String::as_str),
        Some("wayland-fake")
    );
    // A failing tick frees the slot.
    assert!(rig.admin.try_hold(Holder::Tick).is_some());
}

#[tokio::test]
async fn omarchy_tick_counts_a_dangling_link_as_a_record() {
    let rig = Rig::new("exit 0");
    std::os::unix::fs::symlink(
        "/nonexistent/target",
        rig.path("config/fleetdeck/omarchy-install.json"),
    )
    .unwrap();
    rig.admin.tick_once().await;
    assert_eq!(tick_runs(&rig), 1);
}

#[tokio::test]
async fn omarchy_tick_is_skipped_while_an_action_runs_and_blocks_actions_while_it_runs() {
    let rig = Rig::new(r#"case "$*" in *screensaver-tick*) sleep 1;; esac"#);
    write_record(&rig);
    {
        let _held = rig
            .admin
            .try_hold(Holder::Act(
                fleetdeck_proto::omarchy_admin::OmarchyAction::Install,
            ))
            .unwrap();
        rig.admin.tick_once().await;
        assert_eq!(tick_runs(&rig), 0, "an action holds the slot");
    }
    let admin = Arc::clone(&rig.admin);
    let ticking = tokio::spawn(async move { admin.tick_once().await });
    until(|| rig.admin.try_hold(Holder::Tick).is_none()).await;
    let outcome = act(&rig.admin, "install", true).await;
    assert!(matches!(
        outcome,
        fleetdeck_proto::link::Outcome::Err(ref e) if e.code == "busy"
    ));
    ticking.await.unwrap();
    assert_eq!(tick_runs(&rig), 1);
    assert_eq!(
        result(act(&rig.admin, "install", true).await).outcome,
        fleetdeck_proto::omarchy_admin::ActOutcome::Done
    );
}

#[tokio::test]
async fn omarchy_tick_scheduler_runs_every_interval_and_stops_with_the_station() {
    let rig = Rig::new("exit 0");
    write_record(&rig);
    let (stop_tx, stop_rx) = watch::channel(false);
    let task = tokio::spawn(tick::run(
        Arc::clone(&rig.admin),
        Duration::from_millis(100),
        stop_rx,
    ));
    until(|| tick_runs(&rig) >= 2).await;
    stop_tx.send(true).unwrap();
    tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn omarchy_tick_scheduler_ends_when_the_sender_is_gone() {
    let rig = Rig::new("exit 0");
    let (stop_tx, stop_rx) = watch::channel(false);
    let task = tokio::spawn(tick::run(
        Arc::clone(&rig.admin),
        Duration::from_secs(3600),
        stop_rx,
    ));
    drop(stop_tx);
    tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .unwrap()
        .unwrap();
}
