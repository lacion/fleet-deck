//! Provider CLI updates by the station (B28, design `cli-update.md`).
//!
//! This part holds the pure and the network-facing checks that run before
//! any byte of a new version runs: the install detection (§3.3), the gate
//! (§3.4) and the target with its signature verification (§3.5).

pub mod channel;
pub mod detect;
pub mod gate;
pub mod staged;
pub mod tree_digest;
pub mod verify;
