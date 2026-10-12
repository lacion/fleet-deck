//! The ShimHost exception to `station_sigkill.rs` (ADR-005; DL-116): a
//! `fleetdeck station serve` process with `host = "shim"` gets an
//! uncatchable SIGKILL while one session is idle and one is in a turn. The
//! providers keep running under their shims; a new station process
//! reattaches both, the sessions stay `running` (never `lost`), the
//! running turn is interrupted from the CLI after the restart, and no
//! replacement provider process starts.

#![cfg(target_os = "linux")]
#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test code")]

mod common;
mod shim_reaper;

use std::time::{Duration, Instant};

use common::{Station, WAIT};
use fleetdeck_proto::event::SessionState;
use fleetdeck_proto::session::Activity;

/// SIGKILL.
const SIGKILL: i32 = 9;

/// Stops every shim of the node when the test ends (a shim outlives its
/// station by design).
struct Reap(std::path::PathBuf);

impl Drop for Reap {
    fn drop(&mut self) {
        shim_reaper::reap(&self.0);
    }
}

fn alive(pid: u32) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|s| {
        s.rsplit_once(')')
            .is_some_and(|(_, rest)| !rest.trim_start().starts_with('Z'))
    })
}

#[test]
fn providers_under_shims_survive_a_station_sigkill_and_the_restarted_station_reattaches_them() {
    let mut station = Station::new();
    let config = station.node.dirs().station_config();
    let text = std::fs::read_to_string(&config).unwrap();
    // Top-level keys come before the provider tables.
    station
        .node
        .write_station_toml(&format!("host = \"shim\"\n{text}"))
        .unwrap();
    let _reap = Reap(station.node.dirs().station().join("shims"));
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

    station.signal("KILL");
    assert_eq!(station.wait_crash(), SIGKILL);
    // The providers outlive the station (the shim exception).
    std::thread::sleep(Duration::from_millis(500));
    for pid in &pids {
        assert!(alive(*pid), "provider {pid} died with the station");
    }

    station.spawn(None);
    let s = station.wait_for("the restored idle session", &idle, |_| true);
    assert_eq!(s.state, SessionState::Running, "reattached, not lost");
    let s = station.wait_for("the restored busy session", &busy, |_| true);
    assert_eq!(s.state, SessionState::Running, "reattached, not lost");
    assert_eq!(s.activity, Activity::Working, "its turn still runs");

    // The restored session takes commands: the CLI interrupts its turn.
    let sent = station.ok(&["station", "interrupt", &busy]);
    assert!(sent.contains("interrupt sent"), "{sent}");
    let deadline = Instant::now() + WAIT;
    station.wait_for("the interrupted turn to end", &busy, |s| {
        assert!(Instant::now() < deadline);
        s.current_turn.is_none() && s.activity == Activity::Idle
    });
    assert_eq!(
        station.stdin().pids,
        pids,
        "no replacement provider process"
    );
    for pid in &pids {
        assert!(alive(*pid), "provider {pid} still runs");
    }
    // A station stop leaves shim sessions running (DL-113).
    station.stop();
}
