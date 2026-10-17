//! `fleetdeck confine-exec` (B9, DL-200): the Landlock helper that confines
//! the writes of a provisioning command. A test skips with a printed reason
//! when the kernel does not enforce Landlock (the status line is
//! `not_enforced`).

#![cfg(target_os = "linux")]
#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test code")]

use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// A canonical temp directory (the temp dir itself may sit below a link).
fn canonical(tmp: &tempfile::TempDir) -> PathBuf {
    std::fs::canonicalize(tmp.path()).unwrap()
}

/// Runs `sh -c <script>` confined to `roots`.
fn confined(roots: &[&Path], script: &str) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_fleetdeck"));
    cmd.arg("confine-exec");
    for root in roots {
        cmd.arg("--write-root").arg(root);
    }
    cmd.args(["--", "/bin/sh", "-c", script]).output().unwrap()
}

fn status_line(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr)
        .lines()
        .find(|l| l.starts_with("fleetdeck-confine: "))
        .unwrap_or_default()
        .to_owned()
}

/// True (and a printed reason) when this kernel does not enforce Landlock.
fn skip(out: &Output) -> bool {
    let line = status_line(out);
    if line == "fleetdeck-confine: not_enforced" {
        eprintln!("skipped: Landlock is not enforced on this kernel ({line})");
        return true;
    }
    false
}

#[test]
fn the_status_line_is_printed_once_before_the_program() {
    let tmp = tempfile::tempdir().unwrap();
    let root = canonical(&tmp);
    let out = confined(&[&root], "echo ran");
    let stderr = String::from_utf8_lossy(&out.stderr);
    let lines: Vec<&str> = stderr
        .lines()
        .filter(|l| l.starts_with("fleetdeck-confine: "))
        .collect();
    assert_eq!(lines.len(), 1, "{stderr}");
    assert!(
        matches!(
            lines[0],
            "fleetdeck-confine: fully_enforced"
                | "fleetdeck-confine: partially_enforced"
                | "fleetdeck-confine: not_enforced"
        ),
        "{stderr}"
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "ran\n");
}

#[test]
fn a_write_below_the_root_works() {
    let tmp = tempfile::tempdir().unwrap();
    let root = canonical(&tmp);
    let file = root.join("sub/out.txt");
    std::fs::create_dir(root.join("sub")).unwrap();
    let out = confined(&[&root], &format!("echo hi > {}", file.display()));
    if skip(&out) {
        return;
    }
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "hi\n");
}

#[test]
fn a_write_outside_the_roots_fails() {
    let tmp = tempfile::tempdir().unwrap();
    let base = canonical(&tmp);
    let root = base.join("root");
    let outside = base.join("outside");
    std::fs::create_dir(&root).unwrap();
    std::fs::create_dir(&outside).unwrap();
    let file = outside.join("out.txt");
    let out = confined(&[&root], &format!("echo hi > {}", file.display()));
    if skip(&out) {
        return;
    }
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("Permission denied"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!file.exists());
}

#[test]
fn a_write_through_a_link_that_points_outside_fails() {
    let tmp = tempfile::tempdir().unwrap();
    let base = canonical(&tmp);
    let root = base.join("root");
    let outside = base.join("outside");
    std::fs::create_dir(&root).unwrap();
    std::fs::create_dir(&outside).unwrap();
    symlink(&outside, root.join("link")).unwrap();
    let out = confined(
        &[&root],
        &format!("echo hi > {}/link/out.txt", root.display()),
    );
    if skip(&out) {
        return;
    }
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("Permission denied"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!outside.join("out.txt").exists());
}

#[test]
fn dev_null_is_writable() {
    let tmp = tempfile::tempdir().unwrap();
    let root = canonical(&tmp);
    let out = confined(&[&root], "echo hi > /dev/null");
    if skip(&out) {
        return;
    }
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn the_exit_code_of_the_program_is_passed_through() {
    let tmp = tempfile::tempdir().unwrap();
    let root = canonical(&tmp);
    let out = confined(&[&root], "exit 7");
    assert_eq!(out.status.code(), Some(7));
}

#[test]
fn a_root_that_cannot_be_opened_exits_125() {
    let tmp = tempfile::tempdir().unwrap();
    let missing = canonical(&tmp).join("missing");
    let out = confined(&[&missing], "echo ran");
    assert_eq!(out.status.code(), Some(125));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains(&format!(
            "fleetdeck-confine: cannot open {}",
            missing.display()
        )),
        "{stderr}"
    );
    assert!(!String::from_utf8_lossy(&out.stdout).contains("ran"));
}

#[test]
fn a_program_that_cannot_run_exits_126() {
    let tmp = tempfile::tempdir().unwrap();
    let root = canonical(&tmp);
    let out = Command::new(env!("CARGO_BIN_EXE_fleetdeck"))
        .args(["confine-exec", "--write-root"])
        .arg(&root)
        .args(["--", "/nonexistent/program"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(126));
}
