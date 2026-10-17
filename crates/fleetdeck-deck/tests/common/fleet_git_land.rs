//! The land additions of the Fleet Git fake station (FG-8): what it answers to
//! `session.land`, and the land tasks of a test deck.

#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use fleetdeck_deck::fleet_git_radar::RadarOptions;
use fleetdeck_proto::link::{Outcome, WireError};
use serde_json::{Value, json};

use super::Fixture;

/// What the fake answers to `project.radar` (FG-7).
#[derive(Debug, Clone)]
pub enum RadarReply {
    /// A `RadarResult` as JSON.
    Answer(Value),
    /// A link error with this code.
    Error(&'static str),
}

impl RadarReply {
    /// No pairs, no code.
    pub fn clear() -> Self {
        Self::Answer(json!({ "checked_at": 5_000, "pairs": [], "skipped": 0 }))
    }

    /// A result with one pair (`b` may be `@default`).
    pub fn pair(a: &str, b: &str, conflict: bool, files: &[&str]) -> Self {
        Self::Answer(json!({
            "checked_at": 5_000, "base_oid": "b".repeat(40), "skipped": 0,
            "pairs": [{
                "a": a, "b": b, "conflict": conflict, "files": files,
                "files_truncated": false
            }]
        }))
    }

    /// A failed run: the `code` and the `text` of the result.
    pub fn failed(code: &str, text: &str) -> Self {
        Self::Answer(json!({
            "checked_at": 5_000, "pairs": [], "skipped": 0, "code": code, "text": text
        }))
    }

    pub fn outcome(&self) -> Outcome {
        match self {
            Self::Answer(v) => Outcome::Ok(v.clone()),
            Self::Error(code) => Outcome::Err(WireError::new(*code, "the fake says no")),
        }
    }
}

/// Runs the radar task of the deck of `f` (FG-7) with `options`.
/// `stop.send(true)` ends it.
pub fn spawn_radar(f: &Fixture, options: RadarOptions) -> tokio::sync::watch::Sender<bool> {
    let (stop, rx) = tokio::sync::watch::channel(false);
    tokio::spawn(fleetdeck_deck::fleet_git_radar::run(
        f.store.clone(),
        Arc::clone(&f.ctx),
        Arc::clone(&f.ctx.radar_state),
        Arc::clone(&f.ctx.radar_kick),
        rx,
        options,
    ));
    stop
}

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
