//! Provider CLI updates by the station (B28, design `cli-update.md`).
//!
//! This part holds the pure and the network-facing checks that run before
//! any byte of a new version runs: the install detection (§3.3), the gate
//! (§3.4) and the target with its signature verification (§3.5).

pub mod activate;
pub mod channel;
pub mod detect;
pub mod fence;
pub mod gate;
pub mod mutations;
pub mod recover;
pub mod run;
pub mod schedule;
pub mod service;
pub mod stage;
pub mod staged;
pub mod tree_digest;
pub mod verify;
