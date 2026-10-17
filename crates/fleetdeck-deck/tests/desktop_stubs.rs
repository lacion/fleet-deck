//! The step 1 stubs of the desktop relay and the wake task (R13 P2b/P3b):
//! the interface is final and the defined refusal holds until step 2.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::time::Duration;

use common::*;
use fleetdeck_deck::desktop_relay::{Actor, RelayCtx, RelayError, act, info};
use fleetdeck_deck::live::{DeckLive, LiveCell};
use fleetdeck_proto::desktop::DesktopAction;
use tokio::sync::watch;

fn live() -> LiveCell {
    LiveCell::new(DeckLive {
        name: "deck".into(),
        conflict_mail: true,
        fleet_control: false,
        desktop_control: true,
        tailscale_cert: false,
        tailscale_bin: None,
    })
}

#[tokio::test]
async fn the_relay_stub_refuses_every_action_and_every_info_as_unsupported() {
    let f = fixture().await;
    let ctx = RelayCtx {
        store: f.store.clone(),
        link: std::sync::Arc::clone(&f.ctx),
        live: live(),
    };
    let r = act(
        &ctx,
        f.station.clone(),
        DesktopAction::ScreenLock,
        Actor::Owner,
    )
    .await;
    assert_eq!(r.unwrap_err(), RelayError::Unsupported);
    let r = act(
        &ctx,
        f.station.clone(),
        DesktopAction::ScreenPower {
            monitor: Some("DP-1".into()),
            on: true,
        },
        Actor::Wake {
            display_id: "d".into(),
        },
    )
    .await;
    assert_eq!(r.unwrap_err(), RelayError::Unsupported);
    let r = info(&ctx, f.station.clone(), true).await;
    assert_eq!(r.unwrap_err(), RelayError::Unsupported);
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
