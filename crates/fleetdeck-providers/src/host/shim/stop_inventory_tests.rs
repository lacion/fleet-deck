//! Tests of the stop inventory over fake `/proc` roots. No test reads or
//! signals a real process.

#![allow(clippy::unwrap_used)]

use super::*;

fn stat_line(pid: u32, state: char, ppid: u32, pgrp: u32, session: u32, start: u64) -> String {
    format!(
        "{pid} (a b) c) {state} {ppid} {pgrp} {session} 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 {start} 0"
    )
}

/// One fake process: `stat`, `status` with `uid`, and `comm`.
fn put(root: &Path, pid: u32, stat: &str, uid: u32) {
    let dir = root.join(pid.to_string());
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("stat"), stat).unwrap();
    std::fs::write(
        dir.join("status"),
        format!("Name:\tx\nUid:\t{uid}\t{uid}\t{uid}\t{uid}\nGid:\t1\t1\t1\t1\n"),
    )
    .unwrap();
    std::fs::write(dir.join("comm"), "worker\n").unwrap();
}

#[test]
fn the_domain_holds_the_leader_and_the_live_members_of_the_group_and_the_session() {
    let dir = tempfile::tempdir().unwrap();
    put(dir.path(), 100, &stat_line(100, 'S', 1, 100, 100, 5), 1000);
    put(
        dir.path(),
        101,
        &stat_line(101, 'S', 100, 100, 100, 6),
        1000,
    );
    // Another group of the same session, and a zombie of the group.
    put(dir.path(), 103, &stat_line(103, 'S', 1, 103, 100, 8), 1000);
    put(
        dir.path(),
        102,
        &stat_line(102, 'Z', 100, 100, 100, 7),
        1000,
    );
    // A foreign process outside the domain and a member of another UID.
    put(dir.path(), 200, &stat_line(200, 'S', 1, 200, 200, 9), 1000);
    put(dir.path(), 104, &stat_line(104, 'S', 100, 100, 100, 10), 0);
    let inv = inventory_in(dir.path(), 100).unwrap();
    let keys: Vec<_> = inv
        .domain
        .iter()
        .map(|m| (m.pid, m.start_time, m.uid))
        .collect();
    assert_eq!(
        keys,
        [(100, 5, 1000), (101, 6, 1000), (103, 8, 1000), (104, 10, 0)]
    );
    assert_eq!(inv.leader_stat.as_ref().map(|s| s.start_time), Some(5));
    assert_eq!(comm_in(dir.path(), 101), "worker");
    assert_eq!(comm_in(dir.path(), 999), "?");
}

#[test]
fn a_gone_leader_leaves_the_members_and_no_leader_stat() {
    let dir = tempfile::tempdir().unwrap();
    put(dir.path(), 101, &stat_line(101, 'S', 1, 100, 100, 6), 1000);
    let inv = inventory_in(dir.path(), 100).unwrap();
    assert!(inv.leader_stat.is_none());
    assert_eq!(inv.domain.len(), 1);
    // A zombie leader is listed in `leader_stat` and is not a member.
    put(dir.path(), 100, &stat_line(100, 'Z', 1, 100, 100, 5), 1000);
    let inv = inventory_in(dir.path(), 100).unwrap();
    assert!(inv.leader_stat.unwrap().is_zombie());
    assert_eq!(inv.domain.iter().map(|m| m.pid).collect::<Vec<_>>(), [101]);
}

#[test]
fn two_passes_that_differ_are_incomplete() {
    let dir = tempfile::tempdir().unwrap();
    put(dir.path(), 100, &stat_line(100, 'S', 1, 100, 100, 5), 1000);
    let root = dir.path().to_owned();
    let got = inventory_with(dir.path(), 100, &mut || {
        put(&root, 105, &stat_line(105, 'S', 100, 100, 100, 11), 1000);
    });
    assert!(
        matches!(got, Err(StopInventoryError::Incomplete(_))),
        "{got:?}"
    );
    // The same PID with another start time differs too.
    let dir = tempfile::tempdir().unwrap();
    put(dir.path(), 100, &stat_line(100, 'S', 1, 100, 100, 5), 1000);
    let root = dir.path().to_owned();
    let got = inventory_with(dir.path(), 100, &mut || {
        put(&root, 100, &stat_line(100, 'S', 1, 100, 100, 6), 1000);
    });
    assert!(
        matches!(got, Err(StopInventoryError::Incomplete(_))),
        "{got:?}"
    );
}

#[test]
fn an_unreadable_proc_or_uid_is_incomplete() {
    let dir = tempfile::tempdir().unwrap();
    let got = inventory_in(&dir.path().join("missing"), 100);
    assert!(
        matches!(got, Err(StopInventoryError::Incomplete(_))),
        "{got:?}"
    );
    // A member without a readable UID.
    put(dir.path(), 100, &stat_line(100, 'S', 1, 100, 100, 5), 1000);
    std::fs::write(dir.path().join("100").join("status"), "Name:\tx\n").unwrap();
    let got = inventory_in(dir.path(), 100);
    assert!(
        matches!(got, Err(StopInventoryError::Incomplete(_))),
        "{got:?}"
    );
    // A member that ended between the two reads is not an error.
    std::fs::remove_file(dir.path().join("100").join("status")).unwrap();
    let inv = inventory_in(dir.path(), 100).unwrap();
    assert!(inv.domain.is_empty());
}

#[test]
fn the_ancestors_follow_the_parents_up_to_the_top() {
    let dir = tempfile::tempdir().unwrap();
    put(dir.path(), 1, &stat_line(1, 'S', 0, 1, 1, 1), 0);
    put(dir.path(), 50, &stat_line(50, 'S', 1, 50, 50, 2), 1000);
    put(dir.path(), 60, &stat_line(60, 'S', 50, 60, 60, 3), 1000);
    put(dir.path(), 100, &stat_line(100, 'S', 60, 100, 100, 5), 1000);
    let inv = inventory_in(dir.path(), 100).unwrap();
    assert_eq!(inv.ancestors(100), [60, 50, 1]);
    assert_eq!(inv.ancestors(1), Vec::<u32>::new());
    assert_eq!(inv.ancestors(999), Vec::<u32>::new());
}

#[test]
fn stat_now_tells_alive_gone_and_unreadable() {
    let dir = tempfile::tempdir().unwrap();
    put(dir.path(), 100, &stat_line(100, 'S', 1, 100, 100, 5), 1000);
    assert!(matches!(stat_now_in(dir.path(), 100), StatNow::Alive(s) if s.start_time == 5));
    assert_eq!(stat_now_in(dir.path(), 101), StatNow::Gone);
    std::fs::write(dir.path().join("100").join("stat"), "garbage").unwrap();
    assert!(matches!(
        stat_now_in(dir.path(), 100),
        StatNow::Unreadable(_)
    ));
}
