//! The Omarchy lines of `fleetdeck doctor`.

use std::path::PathBuf;
use std::process::{Command, Output};
use std::time::{Duration, Instant};

use crate::rig::{HOOKS, MANIFEST, Rig, out, sleep_path};

fn git_dir() -> PathBuf {
    std::env::split_paths(&std::env::var_os("PATH").expect("PATH is set"))
        .find(|dir| dir.join("git").is_file())
        .expect("git is on PATH")
}

fn doctor(rig: &Rig, args: &[&str]) -> Output {
    rig.script("claude", "echo '2.1.289 (fake)'\n");
    rig.script("codex", "echo 'codex-cli 0.160.0'\n");
    let path = std::env::join_paths([rig.root().join("bin"), git_dir()]).unwrap();
    rig.command(&["doctor"])
        .args(args)
        .env("PATH", path)
        .output()
        .unwrap()
}

#[test]
fn omarchy_doctor_without_omarchy_says_so() {
    let rig = Rig::new();
    let o = rig
        .command(&["doctor"])
        .env("OMARCHY_PATH", rig.root().join("nowhere"))
        .env(
            "PATH",
            std::env::join_paths([rig.root().join("bin"), git_dir()]).unwrap(),
        )
        .output()
        .unwrap();
    assert!(
        out(&o).contains("ok   omarchy: no Omarchy on this machine"),
        "{}",
        out(&o)
    );
}

#[test]
fn omarchy_doctor_lines_for_each_case() {
    let mut rig = Rig::new();
    rig.station(false);
    let o = doctor(&rig, &[]);
    assert!(
        out(&o).contains(
            "ok   omarchy: Omarchy 4.0.0-test; the integration is not installed (run fleetdeck omarchy install)"
        ),
        "{}",
        out(&o)
    );
    assert_eq!(rig.install().status.code(), Some(0));
    let o = doctor(&rig, &[]);
    assert!(
        out(&o).contains("ok   omarchy: installed, no drift"),
        "{}",
        out(&o)
    );
    std::fs::write(rig.omarchy_dir().join(HOOKS[0]), "changed\n").unwrap();
    std::fs::remove_file(rig.omarchy_dir().join(MANIFEST)).unwrap();
    let o = doctor(&rig, &[]);
    assert!(
        out(&o).contains("warn omarchy: 2 changed or missing files (run fleetdeck omarchy status)"),
        "{}",
        out(&o)
    );
}

#[test]
fn omarchy_doctor_sudo_lines() {
    let rig = Rig::new();
    let o = doctor(&rig, &[]);
    assert!(
        !out(&o).contains("omarchy-sudo"),
        "no script, no line: {}",
        out(&o)
    );
    rig.tool("omarchy-sudo-passwordless", "", 0);
    let o = doctor(&rig, &[]);
    assert!(
        out(&o).contains("warn omarchy-sudo: passwordless sudo is on for this user"),
        "{}",
        out(&o)
    );
    assert_eq!(
        rig.calls("omarchy-sudo-passwordless").last().unwrap(),
        &["--active"]
    );
    rig.tool("omarchy-sudo-passwordless", "", 1);
    let o = doctor(&rig, &[]);
    assert!(out(&o).contains("ok   omarchy-sudo: off"), "{}", out(&o));
}

#[test]
fn omarchy_doctor_sudo_check_times_out_after_5_seconds() {
    let rig = Rig::new();
    rig.script(
        "omarchy-sudo-passwordless",
        &format!("exec {} 30\n", sleep_path()),
    );
    let started = Instant::now();
    let o = doctor(&rig, &[]);
    assert!(started.elapsed() < Duration::from_secs(25));
    assert!(
        out(&o).contains("warn omarchy-sudo: could not check"),
        "{}",
        out(&o)
    );
}

#[test]
fn omarchy_doctor_brief_prints_only_warn_and_fail_lines_and_the_summary() {
    let rig = Rig::new();
    rig.tool("omarchy-sudo-passwordless", "", 0);
    let o = doctor(&rig, &["--brief"]);
    let text = out(&o);
    assert_eq!(o.status.code(), Some(0), "{text}");
    for line in text.lines() {
        assert!(
            line.starts_with("warn ") || line.starts_with("fail ") || line.starts_with("doctor: "),
            "unexpected line `{line}` in:\n{text}"
        );
    }
    assert!(
        text.contains("warn omarchy-sudo: passwordless sudo is on"),
        "{text}"
    );
    assert!(text.trim_end().contains("doctor: "), "{text}");
    assert!(!text.contains("ok   "), "{text}");
    // The exit code is unchanged: 1 with a failure.
    let path = std::env::join_paths([rig.root().join("bin"), git_dir()]).unwrap();
    let o = Command::new(env!("CARGO_BIN_EXE_fleetdeck"))
        .args(["doctor", "--brief", "--data-dir"])
        .arg(rig.root().join("nowhere"))
        .env_clear()
        .env("HOME", rig.root().join("home"))
        .env("PATH", path)
        .env("OMARCHY_PATH", rig.root().join("omarchy-root"))
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(1), "{}", out(&o));
    assert!(out(&o).contains("fail data-dir: "), "{}", out(&o));
}
