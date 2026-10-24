//! Tests of the leader wait and of the unpinned exit path (real children
//! of the test process; Linux).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

use rustix::process::Signal;

use super::leader::{Sent, Target, wait_leader};
use super::*;

fn quiet(script: &str) -> Command {
    let mut command = Command::new("sh");
    command
        .args(["-c", script])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    command
}

fn state_of(pid: u32) -> char {
    let text = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap();
    text[text.rfind(')').unwrap() + 1..]
        .trim_start()
        .chars()
        .next()
        .unwrap()
}

#[test]
fn leader_wait_keeps_the_zombie() {
    let mut child = quiet("exit 3").spawn().unwrap();
    let pid = child.id();
    assert_eq!(wait_leader(pid), LeaderWait::Ended);
    assert_eq!(state_of(pid), 'Z', "the leader is not reaped");
    // A second wait still sees it: nothing consumed the status.
    assert_eq!(wait_leader(pid), LeaderWait::Ended);
    let status = child.wait().unwrap();
    assert_eq!(status.code(), Some(3));
}

#[test]
fn leader_wait_after_a_foreign_reap_fails() {
    let mut child = quiet("true").spawn().unwrap();
    let pid = child.id();
    child.wait().unwrap();
    assert_eq!(
        wait_leader(pid),
        LeaderWait::Failed(rustix::io::Errno::CHILD)
    );
}

#[test]
fn unpinned_exit_sends_no_numeric_signal() {
    let log: Arc<Mutex<Vec<(Target, Signal)>>> = Arc::default();
    let seen = Arc::clone(&log);
    let leader = Leader::with_sender(
        4242,
        None,
        Box::new(move |target, sig| seen.lock().unwrap().push((target, sig))),
    );
    leader.set(Lifecycle::Unpinned);
    leader.set(Lifecycle::Reaped);
    assert_eq!(unpinned_count(&leader, || Some(1)), Some(1));
    assert!(log.lock().unwrap().is_empty());
    assert_eq!(leader.signal(Signal::KILL, || {}), Sent::Nothing);
    assert_eq!(leader.signal(Signal::INT, || {}), Sent::Nothing);
    assert!(log.lock().unwrap().is_empty(), "no signal after the reap");
}
