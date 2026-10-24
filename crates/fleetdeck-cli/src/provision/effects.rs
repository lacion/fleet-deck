//! The effects of the provisioning bootstrap (P8 T3a, design
//! `omarchy-p8.md` 3): everything that touches the machine sits behind the
//! [`Effects`] trait, so the state machine of `run.rs` is pure logic that a
//! fake can drive. Task T3b writes the real effects.

use std::future::Future;
use std::io;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::time::{Duration, Instant};

use fleetdeck_link::{ReportReply, ValidReport};
use fleetdeck_proto::link::StationProvider;

/// A future that an effect returns (the crate has no async trait macro).
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// A terminal condition: a fixed code that the state keeps and stderr shows.
/// The run exits with 2 after it (design 3.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Terminal(pub &'static str);

impl Terminal {
    pub const FILE_INVALID: Self = Self("file_invalid");
    pub const RELEASE_UNTRUSTED: Self = Self("release_untrusted");
    pub const BUILD_MISMATCH: Self = Self("build_mismatch");
    pub const CONFIG_CONFLICT: Self = Self("config_conflict");
    pub const CONFIG_CHANGED: Self = Self("config_changed");
    pub const SERVICE_CONFLICT: Self = Self("service_conflict");
    pub const STATION_IDENTITY_CHANGED: Self = Self("station_identity_changed");
    pub const PAIRED_ELSEWHERE: Self = Self("paired_elsewhere");
    pub const DENIED: Self = Self("denied");
    pub const CANCELLED: Self = Self("cancelled");
    pub const REVOKED: Self = Self("revoked");
    pub const FINGERPRINT_MISMATCH: Self = Self("fingerprint_mismatch");
    pub const REQUEST_ID_CONFLICT: Self = Self("request_id_conflict");
    pub const PENDING: Self = Self("pending");
    pub const UNSUPPORTED: Self = Self("unsupported");
    pub const INVALID_REQUEST: Self = Self("invalid_request");
    pub const RETRY_FILE: Self = Self("retry_file");
    pub const PAIRING_FAILED: Self = Self("pairing_failed");
    pub const PROVIDER_PROBE_FAILED: Self = Self("provider_probe_failed");
    pub const INTEGRATION_FAILED: Self = Self("integration_failed");
    pub const UNKNOWN_ERROR: Self = Self("unknown_error");

    /// Every code of the fixed list (D17).
    pub const ALL: [Self; 21] = [
        Self::FILE_INVALID,
        Self::RELEASE_UNTRUSTED,
        Self::BUILD_MISMATCH,
        Self::CONFIG_CONFLICT,
        Self::CONFIG_CHANGED,
        Self::SERVICE_CONFLICT,
        Self::STATION_IDENTITY_CHANGED,
        Self::PAIRED_ELSEWHERE,
        Self::DENIED,
        Self::CANCELLED,
        Self::REVOKED,
        Self::FINGERPRINT_MISMATCH,
        Self::REQUEST_ID_CONFLICT,
        Self::PENDING,
        Self::UNSUPPORTED,
        Self::INVALID_REQUEST,
        Self::RETRY_FILE,
        Self::PAIRING_FAILED,
        Self::PROVIDER_PROBE_FAILED,
        Self::INTEGRATION_FAILED,
        Self::UNKNOWN_ERROR,
    ];

    /// The terminal of a code that a call returned: the code itself when it
    /// is in the fixed list, else `unknown_error`. Text from a peer never
    /// reaches the state or stderr.
    #[must_use]
    pub fn from_code(code: &str) -> Self {
        Self::ALL
            .into_iter()
            .find(|t| t.0 == code)
            .unwrap_or(Self::UNKNOWN_ERROR)
    }
}

/// The identity of a build (`fleetdeck build-info`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Build {
    pub version: String,
    pub commit: String,
    pub release: bool,
}

/// A file that the trust check reads, with what it needs to judge it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RootFile {
    pub bytes: Vec<u8>,
    pub uid: u32,
    /// The permission bits of the file.
    pub mode: u32,
    /// The path itself is a symlink.
    pub symlink: bool,
    pub parent_uid: u32,
    pub parent_mode: u32,
}

/// The station configuration (`station.toml`), as far as bootstrap cares.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigState {
    Absent,
    Named(String),
    /// The file exists and sets no name.
    Unnamed,
    /// The file exists and cannot be loaded.
    Invalid,
}

/// The installed station service (D13).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceState {
    Absent,
    Matching { active: bool },
    Conflict,
}

/// The answer of `station fingerprint`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StationView {
    pub fingerprint: String,
    pub paired_deck: Option<String>,
    pub link_up: bool,
    pub build: Build,
    pub binary_sha256: String,
    pub providers: Option<Vec<StationProvider>>,
}

/// Why a call to the station gave no view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallError {
    Unavailable,
    Timeout,
    Invalid,
}

/// The result of the tailnet join: paired, or the stable code of the refusal
/// or failure (`JoinError::code`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JoinOutcome {
    Paired,
    Code(String),
}

/// Why a setup report got no reply: a fixed word (`fingerprint_mismatch`,
/// `unsupported`, `timeout`, `no_identity`, `no_address`, `failed`). Text
/// from a peer never becomes the word.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReportFailure(pub &'static str);

/// What the bootstrap does to the machine.
pub trait Effects {
    /// The station data directory (`<data>/station`).
    fn station_dir(&self) -> PathBuf;
    /// The directory from which the ancestor rules of the private files
    /// apply (`/` for the real effects).
    fn trust_root(&self) -> PathBuf;
    /// Creates `station_dir()` (mode 0700) when it is absent; an existing
    /// one must pass the private-directory rules.
    ///
    /// # Errors
    /// The directory cannot be created or is not safe.
    fn ensure_station_dir(&self) -> io::Result<()>;
    fn now(&self) -> Instant;
    fn sleep(&self, d: Duration) -> BoxFuture<'_, ()>;

    /// The bytes of the provisioning file (private file rules, 16 KiB).
    ///
    /// # Errors
    /// [`Terminal`] when the file is not readable under the rules.
    fn read_provision(&self, path: &Path) -> Result<Vec<u8>, Terminal>;
    /// A file of the release prerequisite, with its owner and mode.
    ///
    /// # Errors
    /// The file cannot be read.
    fn root_file(&self, path: &Path) -> io::Result<RootFile>;
    /// The SHA-256 (64 lowercase hex digits) of the running binary.
    ///
    /// # Errors
    /// The binary cannot be read.
    fn own_binary_sha256(&self) -> io::Result<String>;
    fn build(&self) -> Build;
    /// The SHA-256 of the binary that the checked manifest names. The calls to
    /// the station trust only a process that runs this binary.
    fn set_trusted_binary(&self, sha256: &str);

    fn station_config(&self) -> ConfigState;
    /// Creates the station configuration privately, with no replace.
    ///
    /// # Errors
    /// The file cannot be created.
    fn create_station_config(&self, name: &str) -> io::Result<()>;

    fn service(&self) -> ServiceState;
    /// # Errors
    /// The installation failed.
    fn install_service(&self) -> io::Result<()>;
    /// # Errors
    /// The start failed.
    fn start_service(&self) -> io::Result<()>;

    /// `station fingerprint` (2 s per call, 35 s with `probe`).
    fn fingerprint(&self, probe: bool) -> BoxFuture<'_, Result<StationView, CallError>>;
    /// The tailnet join with the deck.
    fn join(
        &self,
        host: &str,
        port: u16,
        deck_fingerprint: &str,
        name: &str,
    ) -> BoxFuture<'_, JoinOutcome>;

    /// `omarchy install --yes` (60 s limit): the exit code.
    fn omarchy_install(&self) -> BoxFuture<'_, io::Result<i32>>;
    /// `omarchy status --json` (60 s limit): the exit code and the stdout
    /// (at most 64 KiB).
    fn omarchy_status_json(&self) -> BoxFuture<'_, io::Result<(i32, Vec<u8>)>>;

    /// The first-login notification with the grouped fingerprint.
    fn notify(&self, grouped_fingerprint: &str);
    /// One stderr line.
    fn progress(&self, line: &str);

    /// True when the key of this station can be loaded. It never creates a
    /// key.
    fn identity_loads(&self) -> bool;
    /// Sends one setup report to the deck pinned by `deck_fingerprint`
    /// (30 s limit). It uses the key of this station and never creates one.
    fn report(
        &self,
        host: &str,
        port: u16,
        deck_fingerprint: &str,
        report: &ValidReport,
    ) -> BoxFuture<'_, Result<ReportReply, ReportFailure>>;
    /// A random duration from zero to `max`.
    fn jitter(&self, max: Duration) -> Duration;
    /// The bytes of a retry authorization (private file rules, 1 KiB).
    ///
    /// # Errors
    /// The file is missing, not private, too large or unreadable.
    fn read_authorization(&self, path: &Path) -> io::Result<Vec<u8>>;
    /// Removes the retry authorization file.
    ///
    /// # Errors
    /// The file could not be removed.
    fn delete_authorization(&self, path: &Path) -> io::Result<()>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_listed_code_is_kept_and_any_other_is_unknown() {
        for t in Terminal::ALL {
            assert_eq!(Terminal::from_code(t.0), t);
        }
        for other in [
            "client_gone",
            "probe_failed",
            "self_connect",
            "x y",
            "",
            "Denied",
        ] {
            assert_eq!(
                Terminal::from_code(other),
                Terminal::UNKNOWN_ERROR,
                "{other}"
            );
        }
    }

    #[test]
    fn the_fixed_codes_are_unique_lowercase_words() {
        let mut seen = std::collections::BTreeSet::new();
        for t in Terminal::ALL {
            assert!(t.0.bytes().all(|b| b.is_ascii_lowercase() || b == b'_'));
            assert!(seen.insert(t.0), "{}", t.0);
        }
    }
}
