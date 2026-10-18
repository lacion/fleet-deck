//! The Omarchy lock (`omarchy-admin.md` §3.5): one exclusive advisory lock,
//! `<config home>/fleetdeck/omarchy.lock`, that every writer of the install
//! record, of the screensaver file or of its backup holds for its whole run.

use std::os::fd::OwnedFd;
use std::path::Path;
use std::time::{Duration, Instant};

use rustix::fs::{FlockOperation, flock};
use rustix::io::Errno;

use super::fsx::Home;

/// The lock file, relative to the config home.
pub const LOCK_PATH: &str = "fleetdeck/omarchy.lock";

/// The time between two tries.
const STEP: Duration = Duration::from_millis(100);
/// How long a command waits for the lock.
pub const WAIT: Duration = Duration::from_secs(10);
/// The line when another change holds the lock.
pub const BUSY: &str = "Another Omarchy change is running.";

/// The held lock. It is released when the guard is dropped.
#[derive(Debug)]
pub struct LockGuard {
    _fd: OwnedFd,
}

/// The result of [`take`].
#[derive(Debug)]
pub enum Take {
    /// The lock is held.
    Held(LockGuard),
    /// Another holder kept it for the whole wait.
    Busy,
    /// The folder or the lock file is missing, and `create` was false.
    NoLockFile,
}

/// Takes the lock, trying every 100 ms until `wait` has passed. With
/// `create`, the folder (0755) and the lock file (0600) are made.
///
/// # Errors
/// A link or a foreign owner on the path, or an I/O error.
pub fn take(home: &Home, wait: Duration, create: bool) -> anyhow::Result<Take> {
    let Some(fd) = home.open_lock(Path::new(LOCK_PATH), create)? else {
        return Ok(Take::NoLockFile);
    };
    let deadline = Instant::now() + wait;
    loop {
        match flock(&fd, FlockOperation::NonBlockingLockExclusive) {
            Ok(()) => return Ok(Take::Held(LockGuard { _fd: fd })),
            Err(Errno::WOULDBLOCK) => {
                let left = deadline.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    return Ok(Take::Busy);
                }
                std::thread::sleep(STEP.min(left));
            }
            Err(Errno::INTR) => {}
            Err(e) => return Err(e.into()),
        }
    }
}

#[cfg(test)]
#[path = "lock_tests.rs"]
mod tests;
