//! The step 1 stub of the wake task (R13 P2b/P3b): the interface is final and
//! the task only waits for the shutdown until the wake row is built. The relay
//! stub became real in the P3b relay task (`board_desktop.rs`).

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::time::Duration;

use common::*;
use fleetdeck_deck::live::{DeckLive, LiveCell};
use tokio::sync::watch;

fn live() -> LiveCell {
    LiveCell::new(DeckLive {
        name: "deck".into(),
        conflict_mail: true,
        fleet_control: false,
        desktop_control: true,
        tailscale_cert: false,
        tailscale_bin: None,
        knobs: fleetdeck_deck::knobs::DeckKnobs::default(),
        worker_grant: fleetdeck_core::config_worker_grant::WorkerGrantConfig {
            enabled: false,
            ..fleetdeck_core::config_worker_grant::WorkerGrantConfig::default()
        },
    })
}

#[tokio::test]
async fn the_wake_stub_runs_until_the_shutdown_signal() {
    let f = fixture().await;
    let (tx, rx) = watch::channel(false);
    let task =
        fleetdeck_deck::wake::spawn(f.store.clone(), std::sync::Arc::clone(&f.ctx), live(), rx);
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(!task.is_finished());
    tx.send(true).unwrap();
    tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .expect("the wake task stops")
        .unwrap();
}
