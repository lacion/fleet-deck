//! ShimHost agents survive a station self-upgrade (ADR-005 §1, PLAN
//! Phase 7; DL-134): a `host = "shim"` station runs one idle and one busy
//! session, stops (the shims keep the providers running, DL-113), prepares
//! and activates an update, and the installed release binary starts as the
//! new station. It reattaches both providers: the sessions stay `running`
//! (never `lost`), the busy turn still runs and takes an interrupt, and no
//! replacement provider process starts.

#![cfg(target_os = "linux")]
#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test code")]

mod common;
mod shim_reaper;

use std::time::{Duration, Instant};

use common::{Station, WAIT};
use fleetdeck_proto::event::SessionState;
use fleetdeck_proto::session::Activity;
use fleetdeck_station::upgrade::{Launcher, UpdateRecord, UpdateState};

const BIN: &str = env!("CARGO_BIN_EXE_fleetdeck");

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
fn providers_under_shims_survive_a_station_upgrade_and_the_new_release_reattaches_them() {
    let mut station = Station::new();
    let config = station.node.dirs().station_config();
    let text = std::fs::read_to_string(&config).unwrap();
    station
        .node
        .write_station_toml(&text.replacen("host = \"direct\"", "host = \"shim\"", 1))
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

    // The old station stops; its shim sessions keep running (DL-113).
    station.stop();
    for pid in &pids {
        assert!(alive(*pid), "provider {pid} died with the station stop");
    }

    // Prepare (snapshot, trial on a copy, install) and activate.
    let out = station.ok(&[
        "station",
        "upgrade",
        "prepare",
        "--binary",
        BIN,
        "--to-version",
        "9.9.9",
    ]);
    let update = out
        .lines()
        .rev()
        .find(|l| l.starts_with("upd-"))
        .expect("an update id")
        .to_owned();
    let upgrades = station.node.dirs().station().join("upgrades");
    assert_eq!(
        UpdateRecord::load(&upgrades, &update).unwrap().state,
        UpdateState::Trialed
    );
    for pid in &pids {
        assert!(alive(*pid), "provider {pid} died in the trial");
    }
    let activated = station.ok(&["station", "upgrade", "activate", &update]);
    assert!(activated.contains("activated"), "{activated}");
    let launcher = Launcher::new(station.node.dirs().station().join("bin"));
    let release = launcher.current().expect("an active release");
    assert_eq!(release, launcher.release_path("9.9.9", &update));

    // The installed release is the new station and reattaches both shims.
    station.spawn_binary(&release);
    let s = station.wait_for("the reattached idle session", &idle, |_| true);
    assert_eq!(s.state, SessionState::Running, "reattached, not lost");
    let s = station.wait_for("the reattached busy session", &busy, |_| true);
    assert_eq!(s.state, SessionState::Running, "reattached, not lost");
    assert_eq!(s.activity, Activity::Working, "its turn still runs");

    let sent = station.ok(&["station", "interrupt", &busy]);
    assert!(sent.contains("interrupt sent"), "{sent}");
    let deadline = Instant::now() + WAIT;
    station.wait_for("the interrupted turn to end", &busy, |s| {
        assert!(Instant::now() < deadline);
        s.current_turn.is_none() && s.activity == Activity::Idle
    });
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(
        station.stdin().pids,
        pids,
        "no replacement provider process"
    );
    for pid in &pids {
        assert!(alive(*pid), "provider {pid} still runs");
    }
    station.stop();
    assert!(
        UpdateRecord::load(&upgrades, &update)
            .unwrap()
            .live_advanced,
        "the new release opened the live database"
    );
}
