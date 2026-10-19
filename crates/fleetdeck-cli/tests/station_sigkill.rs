//! GATE-M2 carry-over: what the direct-child host leaves behind when the
//! station gets an uncatchable SIGKILL (no `Drop`, no `kill_on_drop`, no
//! graceful stop runs).
//!
//! The behavior that this test proves (`crates/fleetdeck-providers/src/host/direct.rs`):
//! each provider runs in its own process group, and its only link to the
//! station is its stdio pipes. The kill closes the station's pipe ends, so
//! the provider reads the end of its input and exits, idle or in the middle
//! of a turn. No process of a provider process group survives, and a new
//! station process reports both sessions `lost` and never reattaches or
//! respawns them. A provider that ignored the end of its input would
//! survive until its next stdout write (EPIPE); DL-074 records that limit.
//! The durable `ShimHost` (ADR-005) is the intended exception.

#![cfg(target_os = "linux")]
#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test code")]

mod common;

use std::time::{Duration, Instant};

use common::{Station, WAIT};
use fleetdeck_proto::event::SessionState;
use fleetdeck_proto::session::Activity;

/// SIGKILL.
const SIGKILL: i32 = 9;

/// The members of process group `pgid` that are not zombies.
#[cfg(target_os = "linux")]
fn group_members(pgid: u32) -> Vec<u32> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir("/proc").unwrap().flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|n| n.parse::<u32>().ok())
        else {
            continue;
        };
        let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
            continue;
        };
        let Some((_, rest)) = stat.rsplit_once(')') else {
            continue;
        };
        let fields: Vec<&str> = rest.split_whitespace().collect();
        // state, ppid, pgrp
        if fields.len() > 2 && fields[0] != "Z" && fields[2] == pgid.to_string() {
            out.push(pid);
        }
    }
    out
}

#[cfg(target_os = "linux")]
fn wait_group_gone(pgid: u32) {
    let deadline = Instant::now() + WAIT;
    loop {
        let members = group_members(pgid);
        if members.is_empty() {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "process group {pgid} survives the station kill: {members:?}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

// The process-group check reads /proc: the test exists on Linux only, so a
// macOS run reports no result rather than a pass (owner/platform validation).
#[cfg(target_os = "linux")]
#[test]
fn providers_of_a_sigkilled_station_exit_and_the_restarted_station_reports_them_lost() {
    let mut station = Station::new();
    station.spawn(None);
    let idle = station.run("hello idle");
    station.wait_for("the idle session", &idle, |s| {
        s.state == SessionState::Running && s.activity == Activity::Idle
    });
    let busy = station.run("wait for the host");
    station.wait_for("the busy session", &busy, |s| {
        s.state == SessionState::Running && s.activity == Activity::Working
    });
    let pids = station.stdin().pids;
    assert_eq!(pids.len(), 2, "two provider processes");
    for pid in &pids {
        assert_eq!(group_members(*pid), vec![*pid], "a group of its own");
    }

    station.signal("KILL");
    assert_eq!(station.wait_crash(), SIGKILL);
    for pid in &pids {
        wait_group_gone(*pid);
    }

    station.spawn(None);
    for session in [&idle, &busy] {
        let s = station.wait_for("the lost session", session, |_| true);
        assert_eq!(s.state, SessionState::Lost, "{session}");
        assert!(s.current_turn.is_none() && s.current_run.is_none());
    }
    station.stop();
    assert_eq!(
        station.stdin().pids,
        pids,
        "no replacement provider process"
    );
}
