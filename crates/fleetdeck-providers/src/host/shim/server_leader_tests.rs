//! The signal gate of the leader (DL-431). A spy replaces the kernel: no
//! real signal is sent.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::{Arc, Mutex};

use rustix::process::Signal;

use super::*;

const PID: u32 = 4242;

type Log = Arc<Mutex<Vec<(Target, Signal)>>>;

fn spy(fd: bool) -> (Leader, Log) {
    let log: Log = Arc::default();
    let seen = Arc::clone(&log);
    let fd = fd.then(|| OwnedFd::from(std::fs::File::open("/dev/null").unwrap()));
    let leader = Leader::with_sender(
        PID,
        fd,
        Box::new(move |target, sig| seen.lock().unwrap().push((target, sig))),
    );
    (leader, log)
}

fn sent(log: &Log) -> Vec<(Target, Signal)> {
    log.lock().unwrap().clone()
}

const ALL: [Signal; 3] = [Signal::TERM, Signal::INT, Signal::KILL];

#[test]
fn leader_running_signals_the_group() {
    let (leader, log) = spy(true);
    assert_eq!(leader.lifecycle(), Lifecycle::Running);
    assert_eq!(leader.signal(Signal::TERM, || {}), Sent::Group);
    assert_eq!(leader.signal(Signal::KILL, || {}), Sent::Group);
    assert_eq!(
        sent(&log),
        [
            (Target::Group(PID), Signal::TERM),
            (Target::Group(PID), Signal::KILL)
        ]
    );
}

#[test]
fn leader_ended_kill_escalates_and_sends_nothing() {
    let (leader, log) = spy(true);
    let escalated = leader.escalated();
    leader.set(Lifecycle::Ended);
    assert!(!*escalated.borrow());
    assert_eq!(leader.signal(Signal::KILL, || {}), Sent::Escalated);
    assert!(*escalated.borrow());
    assert!(sent(&log).is_empty());
}

#[test]
fn leader_ended_term_and_int_send_nothing() {
    let (leader, log) = spy(true);
    let escalated = leader.escalated();
    leader.set(Lifecycle::Ended);
    assert_eq!(leader.signal(Signal::TERM, || {}), Sent::Nothing);
    assert_eq!(leader.signal(Signal::INT, || {}), Sent::Nothing);
    assert!(!*escalated.borrow(), "only a Kill escalates the sweep");
    assert!(sent(&log).is_empty());
}

#[test]
fn leader_unpinned_signals_only_through_the_pidfd() {
    let (leader, log) = spy(true);
    leader.set(Lifecycle::Unpinned);
    for sig in ALL {
        assert_eq!(leader.signal(sig, || {}), Sent::LeaderFd);
    }
    assert_eq!(
        sent(&log),
        [
            (Target::LeaderFd, Signal::TERM),
            (Target::LeaderFd, Signal::INT),
            (Target::LeaderFd, Signal::KILL)
        ]
    );
}

#[test]
fn leader_unpinned_without_a_pidfd_sends_nothing() {
    let (leader, log) = spy(false);
    leader.set(Lifecycle::Unpinned);
    for sig in ALL {
        assert_eq!(leader.signal(sig, || {}), Sent::Nothing);
    }
    assert!(sent(&log).is_empty());
}

#[test]
fn leader_reaped_sends_nothing_for_every_signal() {
    let (leader, log) = spy(true);
    leader.set(Lifecycle::Reaped);
    for sig in ALL {
        assert_eq!(leader.signal(sig, || {}), Sent::Nothing);
    }
    assert!(sent(&log).is_empty());
}

#[test]
fn leader_kill_marks_the_cause_only_while_running() {
    for (state, runs) in [
        (Lifecycle::Running, true),
        (Lifecycle::Ended, false),
        (Lifecycle::Unpinned, false),
        (Lifecycle::Reaped, false),
    ] {
        let (leader, _) = spy(true);
        leader.set(state);
        let mut ran = false;
        leader.signal(Signal::KILL, || ran = true);
        assert_eq!(ran, runs, "{state:?}");
    }
}
