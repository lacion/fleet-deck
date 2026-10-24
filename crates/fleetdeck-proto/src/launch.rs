//! The launch identity of a session folder (B9, DL-200): plain data. The
//! station takes it with `fleetdeck_core::launch_dir::LaunchDir`; the hosts
//! reopen it right before a spawn.

use serde::{Deserialize, Serialize};

/// The directory object that the station checked inside the allowed roots.
/// Station-local: it is not exported to TypeScript and never comes from the
/// deck.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaunchIdentity {
    /// The canonical allowed root that holds the folder.
    pub root: String,
    /// The relative rest: normal components joined by `/`; empty for the root.
    pub rel: String,
    pub dev: u64,
    pub ino: u64,
}
