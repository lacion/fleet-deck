//! The provisioning state of the station (design `omarchy-admin.md` §3.2,
//! §3.3): a safe read of `<station data>/provision-state.json` and a probe of
//! `<station data>/provision.lock`.
//!
//! The station reads only `phase` and `error`. No read follows a link or
//! blocks on a special file.

use std::io::Read;
use std::os::fd::OwnedFd;
use std::path::Path;

use fleetdeck_proto::omarchy_admin::ProvisionView;
use rustix::fs::{FileType, FlockOperation, Mode, OFlags};
use serde::Deserialize;

/// The state file name, in the station data directory.
pub const STATE_FILE: &str = "provision-state.json";
/// The lock file name, in the station data directory.
pub const LOCK_FILE: &str = "provision.lock";
/// The most bytes of the state file.
const MAX_STATE: u64 = 16 * 1024;

/// The phases of the bootstrap.
const PHASES: [&str; 6] = [
    "validated",
    "service",
    "paired",
    "link",
    "integration",
    "complete",
];

/// The 21 terminal codes of the bootstrap (`Terminal::ALL` of the CLI).
pub(super) const TERMINAL_CODES: [&str; 21] = [
    "file_invalid",
    "release_untrusted",
    "build_mismatch",
    "config_conflict",
    "config_changed",
    "service_conflict",
    "station_identity_changed",
    "paired_elsewhere",
    "denied",
    "cancelled",
    "revoked",
    "fingerprint_mismatch",
    "request_id_conflict",
    "pending",
    "unsupported",
    "invalid_request",
    "retry_file",
    "pairing_failed",
    "provider_probe_failed",
    "integration_failed",
    "unknown_error",
];

/// What the state file holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StateRead {
    /// There is no file.
    Absent,
    /// Any other refusal, or a parse failure.
    Unreadable,
    Found {
        phase: String,
        error: Option<String>,
    },
}

#[derive(Deserialize)]
struct RawState {
    phase: String,
    error: Option<String>,
}

pub(super) enum OpenError {
    Missing,
    Refused,
}

/// Opens `path` without following a link or blocking, and checks the
/// descriptor: a regular file, owned by the station user, not writable by
/// group or others, and at most `max` bytes when `max` is given.
pub(super) fn open_private(path: &Path, max: Option<u64>) -> Result<OwnedFd, OpenError> {
    let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
    let fd = rustix::fs::open(path, flags, Mode::empty()).map_err(|e| {
        if e == rustix::io::Errno::NOENT {
            OpenError::Missing
        } else {
            OpenError::Refused
        }
    })?;
    let stat = rustix::fs::fstat(&fd).map_err(|_| OpenError::Refused)?;
    let regular = FileType::from_raw_mode(stat.st_mode) == FileType::RegularFile;
    let mine = stat.st_uid == rustix::process::getuid().as_raw();
    let private = stat.st_mode & 0o022 == 0;
    let small = max.is_none_or(|m| u64::try_from(stat.st_size).is_ok_and(|s| s <= m));
    if regular && mine && private && small {
        Ok(fd)
    } else {
        Err(OpenError::Refused)
    }
}

/// Reads the state file of the station data directory `dir`.
#[must_use]
pub fn read_state(dir: &Path) -> StateRead {
    let fd = match open_private(&dir.join(STATE_FILE), Some(MAX_STATE)) {
        Ok(fd) => fd,
        Err(OpenError::Missing) => return StateRead::Absent,
        Err(OpenError::Refused) => return StateRead::Unreadable,
    };
    let mut bytes = Vec::new();
    let read = std::fs::File::from(fd)
        .take(MAX_STATE + 1)
        .read_to_end(&mut bytes);
    if read.is_err() || bytes.len() as u64 > MAX_STATE {
        return StateRead::Unreadable;
    }
    let Ok(raw) = serde_json::from_slice::<RawState>(&bytes) else {
        return StateRead::Unreadable;
    };
    if !PHASES.contains(&raw.phase.as_str()) {
        return StateRead::Unreadable;
    }
    let error = raw.error.map(|e| {
        if TERMINAL_CODES.contains(&e.as_str()) {
            e
        } else {
            "unknown_error".to_owned()
        }
    });
    StateRead::Found {
        phase: raw.phase,
        error,
    }
}

/// Whether a bootstrap holds `provision.lock`: a non-blocking exclusive lock
/// attempt that is released at once. It fails closed: when the state is not
/// clear (a refused file, an unexpected error) the answer is `true`.
#[must_use]
pub fn bootstrap_running(dir: &Path) -> bool {
    let fd = match open_private(&dir.join(LOCK_FILE), None) {
        Ok(fd) => fd,
        Err(OpenError::Missing) => return false,
        Err(OpenError::Refused) => return true,
    };
    // The lock goes with the descriptor, at the end of this function.
    rustix::fs::flock(&fd, FlockOperation::NonBlockingLockExclusive).is_err()
}

/// The provisioning view of the station data directory `dir`, with the
/// `last_retry` of this process. `None` when there is no state file.
#[must_use]
pub fn view(dir: &Path, last_retry: Option<&str>) -> Option<ProvisionView> {
    let (phase, error) = match read_state(dir) {
        StateRead::Absent => return None,
        StateRead::Unreadable => (None, Some("unknown_error".to_owned())),
        StateRead::Found { phase, error } => (Some(phase), error),
    };
    Some(ProvisionView {
        phase,
        error,
        bootstrap_running: bootstrap_running(dir),
        last_retry: last_retry.map(str::to_owned),
    })
}
