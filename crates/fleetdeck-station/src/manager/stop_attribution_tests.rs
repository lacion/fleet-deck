//! The witness rules over fake `/proc` roots (BG-4 §5.3). No test reads or
//! signals a real process.

#![allow(clippy::unwrap_used)]

use std::path::{Path, PathBuf};

use fleetdeck_providers::host::shim::stop_inventory::{inventory_in, inventory_with};

use super::*;
use crate::store::ShimState;

const BOOT: &str = "boot-1";
/// The station of the tests: PID 5000 under 4000 under PID 1.
const STATION: Station = Station {
    pid: 5000,
    uid: 1000,
};

fn stat_line(pid: u32, state: char, ppid: u32, pgrp: u32, session: u32, start: u64) -> String {
    format!("{pid} (x) {state} {ppid} {pgrp} {session} 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 {start} 0")
}

/// A fake process: `Z` for a zombie.
fn put(root: &Path, (pid, ppid): (u32, u32), (pgrp, session): (u32, u32), start: u64, uid: u32) {
    put_state(root, 'S', (pid, ppid), (pgrp, session), start, uid);
}

fn put_state(
    root: &Path,
    state: char,
    (pid, ppid): (u32, u32),
    (pgrp, session): (u32, u32),
    start: u64,
    uid: u32,
) {
    let dir = root.join(pid.to_string());
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("stat"),
        stat_line(pid, state, ppid, pgrp, session, start),
    )
    .unwrap();
    std::fs::write(
        dir.join("status"),
        format!("Uid:\t{uid}\t{uid}\t{uid}\t{uid}\n"),
    )
    .unwrap();
    std::fs::write(dir.join("comm"), "worker\n").unwrap();
}

/// The station and its ancestors, outside any domain.
fn machine(root: &Path) {
    put(root, (1, 0), (1, 1), 1, 0);
    put(root, (4000, 1), (4000, 4000), 2, 1000);
    put(root, (5000, 4000), (4000, 4000), 3, 1000);
}

fn identity() -> ProviderIdentity {
    ProviderIdentity {
        pid: 100,
        start_time: 5,
        boot_id: BOOT.to_owned(),
    }
}

fn row(identity: Option<ProviderIdentity>) -> ShimRow {
    ShimRow {
        shim_id: "shim".to_owned(),
        session_id: "s".to_owned().into(),
        incarnation: "inc".to_owned(),
        state: ShimState::Lost,
        generation: 1,
        socket_path: PathBuf::from("/nonexistent/shim.s"),
        provider_identity: identity,
        cursor: 0,
        next_write_seq: 0,
        reason: Some("execution_unknown".to_owned()),
        inspection_version: 0,
        shim_pid: None,
        proof: None,
        proof_boot_id: None,
        launch_boot_id: Some(BOOT.to_owned()),
        native_target: None,
        created_at: 1,
    }
}

fn witness(pid: u32, start_time: u64) -> Witness {
    Witness {
        pid,
        start_time,
        boot_id: BOOT.to_owned(),
    }
}

fn attribute_in(root: &Path, witnesses: &[Witness]) -> Result<Attribution, Refused> {
    let inv = inventory_in(root, 100).unwrap();
    attribute(
        &row(Some(identity())),
        &identity(),
        &inv,
        witnesses,
        STATION,
    )
}

fn pids(attribution: &Attribution) -> Vec<u32> {
    attribution.targets.iter().map(|m| m.pid).collect()
}

#[test]
fn with_the_leader_alive_every_owned_member_is_a_target() {
    let dir = tempfile::tempdir().unwrap();
    machine(dir.path());
    put(dir.path(), (100, 1), (100, 100), 5, 1000);
    put(dir.path(), (101, 100), (100, 100), 6, 1000);
    put(dir.path(), (102, 100), (102, 100), 7, 1000);
    // Not in the domain, whoever owns it.
    put(dir.path(), (200, 1), (200, 200), 9, 1000);
    let got = attribute_in(dir.path(), &[]).unwrap();
    assert!(got.rule_a);
    assert_eq!(pids(&got), [100, 101, 102]);
    assert_eq!(got.domain, [(100, 5), (101, 6), (102, 7)]);
}

#[test]
fn with_the_leader_gone_only_a_recorded_witness_is_a_target() {
    let dir = tempfile::tempdir().unwrap();
    machine(dir.path());
    put(dir.path(), (101, 1), (100, 100), 6, 1000);
    put(dir.path(), (102, 1), (100, 100), 7, 1000);
    put(dir.path(), (103, 1), (100, 100), 8, 1000);
    // 102 was recorded with another start time: not the same process.
    let mut other_boot = witness(103, 8);
    other_boot.boot_id = "boot-0".to_owned();
    let got = attribute_in(dir.path(), &[witness(101, 6), witness(102, 99), other_boot]).unwrap();
    assert!(!got.rule_a);
    assert_eq!(pids(&got), [101], "a witness authorizes only itself");
    assert_eq!(got.domain.len(), 3);
}

#[test]
fn a_zombie_leader_counts_as_gone() {
    let dir = tempfile::tempdir().unwrap();
    machine(dir.path());
    put_state(dir.path(), 'Z', (100, 1), (100, 100), 5, 1000);
    put(dir.path(), (101, 1), (100, 100), 6, 1000);
    let got = attribute_in(dir.path(), &[]).unwrap_err();
    assert_eq!(got.code, code::IDENTITY_UNPROVEN, "{got:?}");
    let got = attribute_in(dir.path(), &[witness(101, 6)]).unwrap();
    assert!(!got.rule_a);
    assert_eq!(pids(&got), [101]);
}

#[test]
fn a_recycled_domain_number_and_an_escaped_witness_are_identity_unproven() {
    // The leader number belongs to another process now (another start time),
    // and its group holds a process of a later, unrelated run.
    let dir = tempfile::tempdir().unwrap();
    machine(dir.path());
    put(dir.path(), (100, 1), (100, 100), 99, 1000);
    put(dir.path(), (101, 100), (100, 100), 100, 1000);
    let got = attribute_in(dir.path(), &[]).unwrap_err();
    assert_eq!(got.code, code::IDENTITY_UNPROVEN, "{got:?}");
    // A witness that escaped (it has its own group and session) is not a
    // target, and the unrelated member stays without proof.
    let dir = tempfile::tempdir().unwrap();
    machine(dir.path());
    put(dir.path(), (101, 1), (100, 100), 6, 1000);
    put(dir.path(), (102, 1), (102, 102), 7, 1000);
    let got = attribute_in(dir.path(), &[witness(102, 7)]).unwrap_err();
    assert_eq!(got.code, code::IDENTITY_UNPROVEN, "{got:?}");
    // The same number with an empty domain has nothing to stop.
    let dir = tempfile::tempdir().unwrap();
    machine(dir.path());
    put(dir.path(), (102, 1), (102, 102), 7, 1000);
    let got = attribute_in(dir.path(), &[witness(102, 7)]).unwrap();
    assert!(got.targets.is_empty() && got.domain.is_empty());
}

#[test]
fn a_leader_that_is_not_the_leader_of_its_group_and_session_is_identity_unproven() {
    let dir = tempfile::tempdir().unwrap();
    machine(dir.path());
    // The same process (PID and start time), now in the group of another.
    put(dir.path(), (100, 1), (300, 100), 5, 1000);
    let got = attribute_in(dir.path(), &[]).unwrap_err();
    assert_eq!(got.code, code::IDENTITY_UNPROVEN, "{got:?}");
}

#[test]
fn a_member_of_another_uid_refuses_the_plan_with_permission_denied() {
    let dir = tempfile::tempdir().unwrap();
    machine(dir.path());
    put(dir.path(), (100, 1), (100, 100), 5, 1000);
    put(dir.path(), (101, 100), (100, 100), 6, 0);
    let got = attribute_in(dir.path(), &[]).unwrap_err();
    assert_eq!(got.code, code::PERMISSION_DENIED, "{got:?}");
    // A foreign member without a witness is not a candidate of rule (b).
    let dir = tempfile::tempdir().unwrap();
    machine(dir.path());
    put(dir.path(), (101, 1), (100, 100), 6, 1000);
    put(dir.path(), (102, 1), (100, 100), 7, 0);
    let got = attribute_in(dir.path(), &[witness(101, 6)]).unwrap();
    assert_eq!(pids(&got), [101]);
}

#[test]
fn a_protected_member_is_identity_unproven() {
    for (pid, parent) in [(1, 0), (5000, 4000), (4000, 1)] {
        let dir = tempfile::tempdir().unwrap();
        machine(dir.path());
        put(dir.path(), (100, 1), (100, 100), 5, 1000);
        // The protected process joined the group of the run.
        put(
            dir.path(),
            (pid, parent),
            (100, 100),
            u64::from(pid) + 20,
            1000,
        );
        let got = attribute_in(dir.path(), &[]).unwrap_err();
        assert_eq!(got.code, code::IDENTITY_UNPROVEN, "{pid}: {got:?}");
    }
    // The shim of the row.
    let dir = tempfile::tempdir().unwrap();
    machine(dir.path());
    put(dir.path(), (100, 1), (100, 100), 5, 1000);
    put(dir.path(), (777, 1), (100, 100), 8, 1000);
    let inv = inventory_in(dir.path(), 100).unwrap();
    let mut shimmed = row(Some(identity()));
    shimmed.shim_pid = Some(777);
    let got = attribute(&shimmed, &identity(), &inv, &[], STATION).unwrap_err();
    assert_eq!(got.code, code::IDENTITY_UNPROVEN);
}

#[test]
fn a_legacy_row_or_a_changed_row_is_identity_unproven() {
    let ok = check_row(&row(Some(identity())), "inc", Some(BOOT)).unwrap();
    assert_eq!(ok, identity());
    let refused = |row: &ShimRow, inc: &str, boot: Option<&str>| {
        let got = check_row(row, inc, boot).unwrap_err();
        assert_eq!(got.code, code::IDENTITY_UNPROVEN, "{got:?}");
    };
    // A legacy run without a recorded leader identity.
    refused(&row(None), "inc", Some(BOOT));
    // Another incarnation, another boot, an unreadable boot.
    refused(&row(Some(identity())), "other", Some(BOOT));
    refused(&row(Some(identity())), "inc", Some("boot-2"));
    refused(&row(Some(identity())), "inc", None);
    refused(&row(Some(identity())), "inc", Some(""));
    let mut no_launch = row(Some(identity()));
    no_launch.launch_boot_id = None;
    refused(&no_launch, "inc", Some(BOOT));
    // A running shim and a run whose end is proven.
    let mut running = row(Some(identity()));
    running.state = ShimState::Running;
    refused(&running, "inc", Some(BOOT));
    let mut ended = row(Some(identity()));
    ended.proof = Some("reconciled_by_owner".to_owned());
    refused(&ended, "inc", Some(BOOT));
}

#[test]
fn two_passes_that_differ_are_inspection_incomplete() {
    let dir = tempfile::tempdir().unwrap();
    machine(dir.path());
    put(dir.path(), (100, 1), (100, 100), 5, 1000);
    let root = dir.path().to_owned();
    let failed = inventory_with(dir.path(), 100, &mut || {
        put(&root, (101, 100), (100, 100), 6, 1000);
    })
    .unwrap_err();
    match inventory_failure(&failed) {
        InventoryFailure::Refused(refused) => {
            assert_eq!(refused.code, code::INSPECTION_INCOMPLETE);
        }
        InventoryFailure::Unsupported => panic!("not unsupported"),
    }
}

#[test]
fn more_than_128_targets_are_inspection_incomplete_and_never_truncated() {
    let dir = tempfile::tempdir().unwrap();
    machine(dir.path());
    put(dir.path(), (100, 1), (100, 100), 5, 1000);
    for n in 1..MAX_TARGETS {
        let pid = 100 + u32::try_from(n).unwrap();
        put(dir.path(), (pid, 100), (100, 100), 5 + u64::from(pid), 1000);
    }
    let got = attribute_in(dir.path(), &[]).unwrap();
    assert_eq!(got.targets.len(), MAX_TARGETS);
    put(dir.path(), (900, 100), (100, 100), 905, 1000);
    let got = attribute_in(dir.path(), &[]).unwrap_err();
    assert_eq!(got.code, code::INSPECTION_INCOMPLETE, "{got:?}");
}
