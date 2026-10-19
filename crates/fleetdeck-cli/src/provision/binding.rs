//! The terminal binding and the retry authorization of a setup report
//! (design `board-gaps.md` 4.2 and 4.3).
//!
//! The binding ties one observed terminal condition to a random
//! `terminal_id` and to a digest of the state that produced it and of the
//! provisioning file as read now (a change of the file gives a new digest,
//! so an older authorization is `report_changed`). The
//! supervisor keeps it in `<station data>/provision-terminal.json`. A retry
//! authorization (`<station data>/provision-retry.json`) is valid only for
//! the exact stored binding, and only when the stored digest is still the
//! digest of the state.

use std::io;
use std::path::{Path, PathBuf};

use fleetdeck_core::private_file::{PrivateWriteError, read_private, replace_private_below};
use fleetdeck_link::report_accept::is_lower_hex;
use ring::rand::{SecureRandom, SystemRandom};
use serde::{Deserialize, Serialize};

use super::sha256_hex;
use super::state::Phase;

/// Both files are read with this limit.
pub const MAX_BYTES: u64 = 1024;
/// The prefix of the temporary leaf of a binding write.
const BINDING_TEMP_PREFIX: &str = ".provision-terminal.";
/// The prefix of the temporary leaf of an authorization write.
const AUTHORIZATION_TEMP_PREFIX: &str = ".provision-retry.";

/// `<station data>/provision-terminal.json`.
#[must_use]
pub fn binding_path(dir: &Path) -> PathBuf {
    dir.join("provision-terminal.json")
}

/// `<station data>/provision-retry.json`.
#[must_use]
pub fn authorization_path(dir: &Path) -> PathBuf {
    dir.join("provision-retry.json")
}

/// `<station data>/provision-supervise.lock`.
#[must_use]
pub fn supervise_lock_path(dir: &Path) -> PathBuf {
    dir.join("provision-supervise.lock")
}

/// The digest of a terminal condition: the lowercase hex SHA-256 over the
/// lines `v1`, the retained error code, the phase word, the hash of the
/// provisioning file as read now (`config_sha256`), the deck fingerprint pin
/// of the provisioning file and the station fingerprint of the state (`-`
/// when it has none), each followed by a newline.
#[must_use]
pub fn digest(
    error: &str,
    phase: Phase,
    config_sha256: &str,
    deck_fingerprint: &str,
    station_fingerprint: Option<&str>,
) -> String {
    let text = format!(
        "v1\n{error}\n{}\n{config_sha256}\n{deck_fingerprint}\n{}\n",
        phase.word(),
        station_fingerprint.unwrap_or("-"),
    );
    sha256_hex(text.as_bytes())
}

/// One observed terminal condition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    /// 32 lowercase hex digits.
    pub terminal_id: String,
    /// 64 lowercase hex digits.
    pub state_digest: String,
}

impl Binding {
    fn is_valid(&self) -> bool {
        is_lower_hex(&self.terminal_id, 32) && is_lower_hex(&self.state_digest, 64)
    }
}

/// The stored binding. A missing, unsafe, too large, malformed or invalid
/// file is `None`.
#[must_use]
pub fn load(path: &Path) -> Option<Binding> {
    let bytes = read_private(path, MAX_BYTES).ok()?;
    let binding: Binding = serde_json::from_slice(&bytes).ok()?;
    binding.is_valid().then_some(binding)
}

/// 32 lowercase hex digits from 128 random bits.
fn random_id() -> io::Result<String> {
    let mut bytes = [0_u8; 16];
    SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| io::Error::other("the random source failed"))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// The binding of `digest`: the stored one when its digest is the same,
/// else a new one with a new `terminal_id`, stored privately (0600) first.
///
/// # Errors
/// The new binding could not be written.
pub fn ensure(root: &Path, dir: &Path, digest: &str) -> Result<Binding, PrivateWriteError> {
    let path = binding_path(dir);
    if let Some(stored) = load(&path)
        && stored.state_digest == digest
    {
        return Ok(stored);
    }
    let binding = Binding {
        terminal_id: random_id()?,
        state_digest: digest.to_owned(),
    };
    let json = serde_json::to_vec(&binding).map_err(io::Error::from)?;
    replace_private_below(root, &path, &json, BINDING_TEMP_PREFIX)?;
    Ok(binding)
}

/// The permission that the deck gave for one retry (`retry` reply).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Authorization {
    /// 32 lowercase hex digits.
    pub retry_id: String,
    /// 32 lowercase hex digits.
    pub terminal_id: String,
    /// 64 lowercase hex digits.
    pub state_digest: String,
}

impl Authorization {
    /// The authorization in `bytes`. `None` for bad JSON, an unknown key or
    /// a field of the wrong form.
    #[must_use]
    pub fn parse(bytes: &[u8]) -> Option<Self> {
        let parsed: Self = serde_json::from_slice(bytes).ok()?;
        (is_lower_hex(&parsed.retry_id, 32)
            && is_lower_hex(&parsed.terminal_id, 32)
            && is_lower_hex(&parsed.state_digest, 64))
        .then_some(parsed)
    }

    /// Writes the authorization privately (0600) and atomically.
    ///
    /// # Errors
    /// The write failed or the path is not private.
    pub fn write(&self, root: &Path, path: &Path) -> Result<(), PrivateWriteError> {
        let json = serde_json::to_vec(self).map_err(io::Error::from)?;
        replace_private_below(root, path, &json, AUTHORIZATION_TEMP_PREFIX)
    }
}
