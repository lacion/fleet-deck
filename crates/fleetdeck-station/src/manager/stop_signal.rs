//! The pidfd calls of the stop of a lost run (BG-4, design
//! `docs/v2/design/board-gaps.md` §5.1 and §5.3 rules 8 and 11). A pidfd is
//! a stable reference to one process: a signal through it never reaches a
//! process that reused the PID. Nothing here signals a numeric PID or a
//! group.

use std::os::fd::OwnedFd;

/// Why a pidfd could not be opened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpenError {
    /// The process ended before the pidfd was opened (`ESRCH`).
    Gone,
    /// This platform or kernel has no pidfd (`ENOSYS`).
    Unsupported,
    Other(String),
}

/// The result of one signal through a pidfd.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Sent {
    Delivered,
    /// The process ended (`ESRCH`).
    Gone,
    /// The station user may not signal it (`EPERM`).
    Denied,
    Failed(String),
}

/// The signals of the stop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Which {
    Term,
    Kill,
}

/// Opens a pidfd for `pid`.
///
/// # Errors
/// [`OpenError`].
#[cfg(target_os = "linux")]
pub fn open(pid: u32) -> Result<OwnedFd, OpenError> {
    use rustix::io::Errno;
    use rustix::process::{Pid, PidfdFlags, pidfd_open};
    let Some(raw) = i32::try_from(pid).ok().and_then(Pid::from_raw) else {
        return Err(OpenError::Other(format!("{pid} is not a process id")));
    };
    pidfd_open(raw, PidfdFlags::empty()).map_err(|e| match e {
        Errno::SRCH => OpenError::Gone,
        Errno::NOSYS => OpenError::Unsupported,
        other => OpenError::Other(format!("pidfd_open: {other}")),
    })
}

/// Opens a pidfd for `pid`.
///
/// # Errors
/// Always [`OpenError::Unsupported`] on this platform.
#[cfg(not(target_os = "linux"))]
pub fn open(_pid: u32) -> Result<OwnedFd, OpenError> {
    Err(OpenError::Unsupported)
}

/// Sends `which` to the process of `fd`.
#[cfg(target_os = "linux")]
#[must_use]
pub fn send(fd: &OwnedFd, which: Which) -> Sent {
    use rustix::io::Errno;
    use rustix::process::{Signal, pidfd_send_signal};
    let signal = match which {
        Which::Term => Signal::TERM,
        Which::Kill => Signal::KILL,
    };
    match pidfd_send_signal(fd, signal) {
        Ok(()) => Sent::Delivered,
        Err(Errno::SRCH) => Sent::Gone,
        Err(Errno::PERM) => Sent::Denied,
        Err(other) => Sent::Failed(format!("pidfd_send_signal: {other}")),
    }
}

/// Sends `which` to the process of `fd`.
#[cfg(not(target_os = "linux"))]
#[must_use]
pub fn send(_fd: &OwnedFd, _which: Which) -> Sent {
    Sent::Failed("pidfd is not available on this platform".to_owned())
}
