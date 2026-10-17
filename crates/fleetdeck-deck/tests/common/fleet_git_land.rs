//! The land additions of the Fleet Git fake station (FG-8): what it answers to
//! `session.land`, and the land tasks of a test deck.

#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use super::Fixture;

/// What the fake answers to one `session.land` (FG-8). The replies are used
/// in order; the last one stays.
#[derive(Debug, Clone)]
pub enum LandReply {
    /// An accepted receipt.
    Accept,
    /// A rejected receipt with this `RejectCode` name and message.
    Reject(&'static str, &'static str),
    /// A link error: the deck gets no receipt.
    Error(&'static str),
}

/// Runs the three land tasks of the deck of `f` (FG-8): admission and status
/// poll, the outbox sender and the fence sender. `stop.send(true)` ends them.
pub fn spawn_land(f: &Fixture) -> tokio::sync::watch::Sender<bool> {
    use fleetdeck_deck::{fleet_git_land, fleet_git_land_abandon, fleet_git_land_outbox};
    let (stop, rx) = tokio::sync::watch::channel(false);
    let kick = || Arc::clone(&f.ctx.land_kick);
    tokio::spawn(fleet_git_land::run(
        f.store.clone(),
        Arc::clone(&f.ctx),
        kick(),
        rx.clone(),
    ));
    tokio::spawn(fleet_git_land_outbox::run(
        f.store.clone(),
        Arc::clone(&f.ctx),
        kick(),
        rx.clone(),
    ));
    tokio::spawn(fleet_git_land_abandon::run(
        f.store.clone(),
        Arc::clone(&f.ctx),
        kick(),
        rx,
    ));
    stop
}
