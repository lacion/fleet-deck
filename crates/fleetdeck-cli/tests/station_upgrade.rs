//! Station drain and self-upgrade end to end (M3 7-3, DL-128): the CLI
//! drains a running station, refuses to prepare while it runs, prepares
//! (snapshot, trial by the new binary, install), activates the stable
//! launcher, and rolls back: binary-only after the new station opened the
//! live database, a snapshot restore with a new incarnation before.

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test code")]

mod common;

use common::{Station, open_store};
use fleetdeck_proto::session::Activity;
use fleetdeck_station::upgrade::{Launcher, UpdateRecord, UpdateState};

const BIN: &str = env!("CARGO_BIN_EXE_fleetdeck");

fn prepare(station: &Station) -> String {
    let out = station.ok(&[
        "station",
        "upgrade",
        "prepare",
        "--binary",
        BIN,
        "--to-version",
        "9.9.9",
    ]);
    out.lines()
        .rev()
        .find(|l| l.starts_with("upd-"))
        .expect("an update id")
        .to_owned()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_drained_station_upgrades_and_rolls_back_binary_only_after_it_went_live() {
    let mut station = Station::start();
    let session = station.run("hello");
    station.wait_for("the first turn to end", &session, |s| {
        s.current_turn.is_none() && s.activity == Activity::Idle
    });
    let drained = station.ok(&["station", "drain", "--timeout-secs", "20"]);
    assert!(drained.contains("drained"), "{drained}");
    // A draining station refuses new work.
    let refused = station.fails(&[
        "station",
        "run",
        "--provider",
        "claude",
        "--cwd",
        station.workspace.to_str().unwrap(),
        "more",
    ]);
    assert!(refused.contains("draining"), "{refused}");
    let running = station.fails(&[
        "station",
        "upgrade",
        "prepare",
        "--binary",
        BIN,
        "--to-version",
        "9.9.9",
    ]);
    assert!(running.contains("running"), "{running}");
    station.stop();

    let update = prepare(&station);
    let upgrades = station.node.dirs().station().join("upgrades");
    let record = UpdateRecord::load(&upgrades, &update).unwrap();
    assert_eq!(record.state, UpdateState::Trialed);
    let activated = station.ok(&["station", "upgrade", "activate", &update]);
    assert!(activated.contains("activated"), "{activated}");
    let launcher = Launcher::new(station.node.dirs().station().join("bin"));
    assert_eq!(
        launcher.current(),
        Some(launcher.release_path("9.9.9", &update))
    );

    // The new station opens the live database: the update is live.
    station.spawn(None);
    assert_eq!(station.sessions().len(), 1);
    station.stop();
    assert!(
        UpdateRecord::load(&upgrades, &update)
            .unwrap()
            .live_advanced
    );
    let incarnation = open_store(&station.db()).await.incarnation().clone();

    let rolled = station.ok(&["station", "upgrade", "rollback", &update]);
    assert!(rolled.contains("binary only (live_advanced)"), "{rolled}");
    assert_eq!(open_store(&station.db()).await.incarnation(), &incarnation);
    assert_eq!(
        UpdateRecord::load(&upgrades, &update).unwrap().state,
        UpdateState::RolledBack
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_rollback_before_the_new_station_ran_restores_the_snapshot_with_a_new_incarnation() {
    let mut station = Station::start();
    let session = station.run("hello");
    station.wait_for("the first turn to end", &session, |s| {
        s.current_turn.is_none() && s.activity == Activity::Idle
    });
    station.stop();
    let before = open_store(&station.db()).await.incarnation().clone();

    let update = prepare(&station);
    let rolled = station.ok(&["station", "upgrade", "rollback", &update]);
    assert!(rolled.contains("snapshot restored"), "{rolled}");
    let after = open_store(&station.db()).await.incarnation().clone();
    assert_ne!(after, before, "a restored database gets a new incarnation");

    // The restored station starts and still knows the session.
    station.spawn(None);
    assert_eq!(station.sessions().len(), 1);
    station.stop();
}
