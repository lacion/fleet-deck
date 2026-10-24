//! The census with a pinned leader: the zombie of the leader itself does
//! not make a pass unclean (DL-431). A fake `/proc` in a temporary directory.
#![allow(clippy::unwrap_used)]
#![cfg(target_os = "linux")]

use super::*;

const LEADER: u32 = 100;

fn stat_line(pid: u32, state: char, ppid: u32, pgrp: u32, session: u32, start: u64) -> String {
    format!("{pid} (a) {state} {ppid} {pgrp} {session} 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 {start} 0")
}

fn put(root: &Path, pid: u32, stat: &str) {
    let dir = root.join(pid.to_string());
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("stat"), stat).unwrap();
}

fn count(root: &Path, pinned: bool) -> Option<usize> {
    live_with(root, LEADER, pinned, Duration::ZERO, &mut || {}, &mut || {}).map(|m| m.len())
}

#[test]
fn a_pinned_zombie_leader_does_not_make_a_pass_unclean() {
    let dir = tempfile::tempdir().unwrap();
    put(
        dir.path(),
        LEADER,
        &stat_line(LEADER, 'Z', 50, LEADER, LEADER, 7),
    );
    put(dir.path(), 400, &stat_line(400, 'S', 1, 400, 400, 8));
    assert_eq!(count(dir.path(), true), Some(0));
    assert_eq!(
        count(dir.path(), false),
        None,
        "an unpinned zombie leader proves nothing"
    );
}

#[test]
fn a_zombie_member_still_makes_a_pinned_pass_unclean() {
    let dir = tempfile::tempdir().unwrap();
    put(
        dir.path(),
        LEADER,
        &stat_line(LEADER, 'Z', 50, LEADER, LEADER, 7),
    );
    put(dir.path(), 102, &stat_line(102, 'Z', 50, LEADER, LEADER, 8));
    assert_eq!(count(dir.path(), true), None);
    // A zombie member in another group of the session counts as well.
    let session = tempfile::tempdir().unwrap();
    put(
        session.path(),
        LEADER,
        &stat_line(LEADER, 'Z', 50, LEADER, LEADER, 7),
    );
    put(
        session.path(),
        103,
        &stat_line(103, 'Z', 50, 103, LEADER, 8),
    );
    assert_eq!(count(session.path(), true), None);
    // A vanished pid makes a pinned pass unclean too.
    let gone = tempfile::tempdir().unwrap();
    put(
        gone.path(),
        LEADER,
        &stat_line(LEADER, 'Z', 50, LEADER, LEADER, 7),
    );
    std::fs::create_dir_all(gone.path().join("300")).unwrap();
    assert_eq!(count(gone.path(), true), None);
}

#[test]
fn a_live_member_is_counted_with_a_pinned_leader() {
    let dir = tempfile::tempdir().unwrap();
    put(
        dir.path(),
        LEADER,
        &stat_line(LEADER, 'Z', 50, LEADER, LEADER, 7),
    );
    put(dir.path(), 101, &stat_line(101, 'S', 50, LEADER, LEADER, 8));
    put(dir.path(), 103, &stat_line(103, 'S', 50, 103, LEADER, 9));
    put(dir.path(), 400, &stat_line(400, 'S', 1, 400, 400, 10));
    let members = live_with(
        dir.path(),
        LEADER,
        true,
        Duration::ZERO,
        &mut || {},
        &mut || {},
    )
    .unwrap();
    let mut pids: Vec<u32> = members.iter().map(|p| p.pid).collect();
    pids.sort_unstable();
    assert_eq!(
        pids,
        [101, 103],
        "the leader and the other session are not members"
    );
    let member = members.iter().find(|p| p.pid == 103).unwrap();
    assert_eq!(
        (member.pgrp, member.session, member.start_time),
        (103, LEADER, 9)
    );
}
