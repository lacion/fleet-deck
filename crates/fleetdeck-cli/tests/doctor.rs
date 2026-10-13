//! `fleetdeck doctor` (v1 parity row 136) on a temporary node: one line per
//! check, a summary, and exit code 1 only when a check fails. The commands
//! run with an explicit `PATH`: a temp `bin/` with fake `claude` and `codex`
//! plus the directory of the real `git`.

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test code")]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use fleetdeck_testkit::TempNode;

fn git_dir() -> PathBuf {
    std::env::split_paths(&std::env::var_os("PATH").expect("PATH is set"))
        .find(|dir| dir.join("git").is_file())
        .expect("git is on PATH")
}

/// A `bin/` directory with a fake `claude` and `codex`. Their versions are
/// the tested minimums of the adapters (R8.2).
fn fake_bin(node: &TempNode) -> PathBuf {
    fake_bin_with(node, "2.1.289 (fake)", "codex-cli 0.160.0")
}

/// A `bin/` directory whose fakes print the given version lines.
fn fake_bin_with(node: &TempNode, claude: &str, codex: &str) -> PathBuf {
    let bin = node.root().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    for (name, line) in [("claude", claude), ("codex", codex)] {
        let path = bin.join(name);
        std::fs::write(&path, format!("#!/bin/sh\necho '{line}'\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    bin
}

fn doctor(node: &TempNode, data_dir: &Path) -> Output {
    doctor_with(node, data_dir, fake_bin(node))
}

fn doctor_with(node: &TempNode, data_dir: &Path, bin: PathBuf) -> Output {
    let path = std::env::join_paths([bin, git_dir()]).unwrap();
    Command::new(env!("CARGO_BIN_EXE_fleetdeck"))
        .env_clear()
        .env("PATH", path)
        .env("HOME", node.home())
        .env("RUST_LOG", "warn")
        .arg("--data-dir")
        .arg(data_dir)
        .arg("doctor")
        .output()
        .unwrap()
}

fn stdout(out: &Output) -> String {
    String::from_utf8(out.stdout.clone()).unwrap()
}

#[test]
fn a_missing_data_dir_fails_and_exits_1() {
    let node = TempNode::new("doctor-missing").unwrap();
    let out = doctor(&node, &node.root().join("nowhere"));
    let text = stdout(&out);
    assert_eq!(out.status.code(), Some(1), "{text}");
    assert!(text.contains("fail data-dir: "), "{text}");
    assert!(text.contains("does not exist"), "{text}");
    assert!(text.contains(" 1 fail"), "{text}");
}

#[test]
fn a_minimal_setup_without_deck_or_station_config_is_ok() {
    let node = TempNode::new("doctor-minimal").unwrap();
    let out = doctor(&node, node.data_dir());
    let text = stdout(&out);
    assert_eq!(out.status.code(), Some(0), "{text}");
    for line in [
        "ok   data-dir: ",
        "ok   station.toml: not configured",
        "ok   deck.toml: not configured",
        "ok   git: ",
        "ok   provider claude: 2.1.289 (fake)",
        "ok   provider codex: codex-cli 0.160.0",
        "warn shells: shells off: the board offers no terminal",
        "ok   v1-plugin: not installed",
    ] {
        assert!(text.contains(line), "missing `{line}` in:\n{text}");
    }
    assert!(!text.contains("deck-socket"), "{text}");
    assert!(
        !text.contains("ok   board") && !text.contains("warn board"),
        "{text}"
    );
    assert!(
        text.trim_end().ends_with("doctor: 7 ok, 1 warn, 0 fail"),
        "{text}"
    );
}

#[test]
fn a_provider_older_than_the_tested_version_is_a_warning() {
    let node = TempNode::new("doctor-old-provider").unwrap();
    let bin = fake_bin_with(&node, "2.1.100 (fake)", "codex-cli 0.150.0");
    let out = doctor_with(&node, node.data_dir(), bin);
    let text = stdout(&out);
    assert_eq!(out.status.code(), Some(0), "{text}");
    for line in [
        "warn provider claude: 2.1.100 is older than the tested 2.1.289",
        "warn provider codex: 0.150.0 is older than the tested 0.160.0",
    ] {
        assert!(text.contains(line), "missing `{line}` in:\n{text}");
    }
}

#[test]
fn a_station_toml_with_a_bad_key_fails_with_the_parser_message() {
    let node = TempNode::new("doctor-badkey").unwrap();
    node.write_station_toml("bogus_key = \"secret-value\"\n")
        .unwrap();
    let out = doctor(&node, node.data_dir());
    let text = stdout(&out);
    assert_eq!(out.status.code(), Some(1), "{text}");
    assert!(
        text.contains("fail station.toml: ") && text.contains("unknown field"),
        "{text}"
    );
    assert!(
        !text.contains("secret-value") && !text.contains("bogus_key"),
        "{text}"
    );
    assert!(
        text.contains("warn provider: skipped: station.toml does not load"),
        "{text}"
    );
    assert!(
        text.contains("warn shells: skipped: station.toml does not load"),
        "{text}"
    );
}

#[test]
fn a_station_toml_with_mode_0644_is_a_warning() {
    let node = TempNode::new("doctor-mode").unwrap();
    let path = node.write_station_toml("name = \"alpha\"\n").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    let out = doctor(&node, node.data_dir());
    let text = stdout(&out);
    assert_eq!(out.status.code(), Some(0), "{text}");
    assert!(
        text.contains("warn station.toml: mode 0644, use 0600"),
        "{text}"
    );
}
