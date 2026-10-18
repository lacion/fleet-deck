//! The durable state of the bootstrap (design `omarchy-p8.md` 3.1): the
//! checkpoint file `provision-state.json` in the station data directory.

use std::io;
use std::path::{Path, PathBuf};

use fleetdeck_core::private_file::{
    PrivateReadError, PrivateWriteError, read_private, replace_private_below,
};
use serde::{Deserialize, Serialize};

/// The state file is read with this limit.
pub const MAX_BYTES: u64 = 16 * 1024;
/// The prefix of the temporary leaf of a state write.
const TEMP_PREFIX: &str = ".provision-state.";

/// How far the bootstrap got.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Validated,
    Service,
    Paired,
    Link,
    Integration,
    Complete,
}

impl Phase {
    /// The fixed word of the phase on stderr (the same as in the file).
    #[must_use]
    pub fn word(self) -> &'static str {
        match self {
            Self::Validated => "validated",
            Self::Service => "service",
            Self::Paired => "paired",
            Self::Link => "link",
            Self::Integration => "integration",
            Self::Complete => "complete",
        }
    }
}

/// The state file. It is a checkpoint, not proof that an effect exists.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProvisionState {
    pub version: u32,
    pub config_sha256: String,
    pub station_fingerprint: Option<String>,
    pub phase: Phase,
    pub error: Option<String>,
}

impl ProvisionState {
    /// A new state at `Validated`.
    #[must_use]
    pub fn new(config_sha256: String) -> Self {
        Self {
            version: 1,
            config_sha256,
            station_fingerprint: None,
            phase: Phase::Validated,
            error: None,
        }
    }
}

/// The state file could not be used. The run exits 1 with `state_invalid`
/// and never overwrites the file.
#[derive(Debug)]
pub enum StateError {
    /// A link, or a file or parent that is not private.
    Unsafe,
    /// Too large, bad JSON or an unsupported version.
    Invalid,
    Io(io::Error),
}

impl std::fmt::Display for StateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unsafe => f.write_str("the state file is not private or is a link"),
            Self::Invalid => f.write_str("the state file is not valid"),
            Self::Io(e) => write!(f, "the state file could not be read: {e}"),
        }
    }
}

impl std::error::Error for StateError {}

/// `<station data>/provision-state.json`.
#[must_use]
pub fn state_path(dir: &Path) -> PathBuf {
    dir.join("provision-state.json")
}

/// `<station data>/provision.lock`.
#[must_use]
pub fn lock_path(dir: &Path) -> PathBuf {
    dir.join("provision.lock")
}

/// Reads the state. An absent file is `None`.
///
/// # Errors
/// [`StateError`]: an unsafe file, a file over 16 KiB, bad JSON, a version
/// other than 1, an I/O error.
pub fn load(path: &Path) -> Result<Option<ProvisionState>, StateError> {
    let bytes = match read_private(path, MAX_BYTES) {
        Ok(bytes) => bytes,
        Err(PrivateReadError::Io(e)) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(PrivateReadError::Unsafe) => return Err(StateError::Unsafe),
        Err(PrivateReadError::TooLarge) => return Err(StateError::Invalid),
        Err(PrivateReadError::Io(e)) => return Err(StateError::Io(e)),
    };
    let state: ProvisionState = serde_json::from_slice(&bytes).map_err(|_| StateError::Invalid)?;
    if state.version != 1 {
        return Err(StateError::Invalid);
    }
    Ok(Some(state))
}

/// Writes the state: a temporary 0600 file, sync, atomic replacement, sync of
/// the directory.
///
/// # Errors
/// The write failed or the path is not private.
pub fn save(root: &Path, path: &Path, state: &ProvisionState) -> Result<(), PrivateWriteError> {
    let json = serde_json::to_vec(state).map_err(io::Error::from)?;
    replace_private_below(root, path, &json, TEMP_PREFIX)
}
