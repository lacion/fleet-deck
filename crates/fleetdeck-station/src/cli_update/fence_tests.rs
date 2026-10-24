//! Tests of the fence (Linux). Each test uses its own marker.
#![allow(clippy::expect_used)]

use std::os::unix::process::CommandExt as _;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

use super::*;

const SHORT: Duration = Duration::from_millis(300);

fn marker() -> String {
    static N: AtomicU32 = AtomicU32::new(0);
    format!(
        "fence-test-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    )
}

/// Starts `sh -c script` in its own process group, with the marker when
/// given. A thread reaps the leader. Returns the pid, which is the group id.
fn spawn(script: &str, marker: Option<&str>) -> i32 {
    let mut cmd = Command::new("/bin/sh");
    cmd.arg("-c")
        .arg(script)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0);
    if let Some(m) = marker {
        cmd.env(MARKER_VAR, m);
    }
    let mut child = cmd.spawn().expect("spawn sh");
    let pid = i32::try_from(child.id()).expect("pid");
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    std::thread::sleep(Duration::from_millis(300));
    pid
}

/// The pids of the processes that carry the marker.
fn marked(marker: &str) -> Vec<i32> {
    let want = format!("{MARKER_VAR}={marker}");
    let mut out = Vec::new();
    for entry in std::fs::read_dir("/proc").expect("proc").flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|n| n.parse::<i32>().ok())
        else {
            continue;
        };
        let alive = std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|s| {
            s.rsplit(')')
                .next()
                .is_some_and(|r| !r.trim_start().starts_with('Z'))
        });
        let has = std::fs::read(format!("/proc/{pid}/environ"))
            .is_ok_and(|e| e.split(|b| *b == 0).any(|v| v == want.as_bytes()));
        if alive && has {
            out.push(pid);
        }
    }
    out
}

fn group(pid: i32) -> Pid {
    Pid::from_raw(pid).expect("pid")
}

#[test]
fn a_group_child_and_a_grandchild_are_gone_after_the_fence() {
    let m = marker();
    let pgid = spawn("sleep 60 & sleep 60 & wait", Some(&m));
    assert!(marked(&m).len() >= 3, "{:?}", marked(&m));
    assert_eq!(fence_with(&m, Some(pgid), SHORT), Ok(()));
    assert!(gone(group(pgid)));
    assert!(marked(&m).is_empty());
}

#[test]
fn a_marked_child_that_left_the_group_is_gone_after_the_fence() {
    let m = marker();
    let pgid = spawn("setsid sleep 60 & wait", Some(&m));
    assert!(!marked(&m).is_empty());
    assert_eq!(fence_with(&m, Some(pgid), SHORT), Ok(()));
    assert!(marked(&m).is_empty(), "{:?}", marked(&m));
}

#[test]
fn a_group_with_an_unmarked_process_is_not_signalled() {
    let m = marker();
    let other = marker();
    let pgid = spawn("sleep 60 & wait", Some(&other));
    assert_eq!(fence_with(&m, Some(pgid), SHORT), Err(reason::PROCESS_LEFT));
    assert!(!gone(group(pgid)));
    assert!(!marked(&other).is_empty());
    let _ = kill_process_group(group(pgid), Signal::KILL);
}

#[test]
fn a_process_that_ignores_term_is_killed_by_kill() {
    let m = marker();
    let pgid = spawn("trap '' TERM; sleep 60 & wait", Some(&m));
    assert_eq!(fence_with(&m, Some(pgid), SHORT), Ok(()));
    assert!(gone(group(pgid)));
    assert!(marked(&m).is_empty());
}

#[test]
fn no_group_runs_the_marker_scan_only() {
    let m = marker();
    let _ = spawn("setsid sleep 60 & wait", Some(&m));
    assert_eq!(fence_with(&m, None, SHORT), Ok(()));
    assert!(marked(&m).is_empty());
}
