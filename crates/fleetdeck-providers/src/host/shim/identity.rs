//! Provider process identity (ADR-005 §3.1). A PID alone is never identity:
//! `{ pid, start_time, boot_id }`. A stored identity that differs from the
//! current one (PID reuse, reboot) means `lost { identity_mismatch }`.
//!
//! Linux reads `/proc/<pid>/stat` field 22 and
//! `/proc/sys/kernel/random/boot_id`. The macOS calls
//! (`proc_pidinfo(PROC_PIDTBSDINFO)`, `kern.bootsessionuuid`) are not in
//! this build: [`identity_of`] fails closed there, and the station keeps the
//! direct host on macOS (owner/platform validation).

use super::wire::ProviderIdentity;

#[derive(Debug, thiserror::Error)]
pub enum IdentityError {
    #[error("the process {0} does not exist or cannot be read")]
    NoProcess(u32),
    #[error("the process status is malformed")]
    Malformed,
    #[error("the boot id cannot be read: {0}")]
    BootId(std::io::Error),
    #[error("process identity is not supported on this platform")]
    Unsupported,
}

/// The start time from the text of `/proc/<pid>/stat`. The command name in
/// field 2 can hold spaces and `)`, so the fields after the last `)` count.
#[must_use]
pub fn start_time_of_stat(stat: &str) -> Option<u64> {
    let rest = &stat[stat.rfind(')')? + 1..];
    // After `)`: field 3 (state) is the first; field 22 is the 20th.
    rest.split_whitespace().nth(19)?.parse().ok()
}

/// The identity of a live process.
#[cfg(target_os = "linux")]
pub fn identity_of(pid: u32) -> Result<ProviderIdentity, IdentityError> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat"))
        .map_err(|_| IdentityError::NoProcess(pid))?;
    let start_time = start_time_of_stat(&stat).ok_or(IdentityError::Malformed)?;
    Ok(ProviderIdentity {
        pid,
        start_time,
        boot_id: boot_id()?,
    })
}

/// The boot id of this machine.
#[cfg(target_os = "linux")]
pub fn boot_id() -> Result<String, IdentityError> {
    std::fs::read_to_string("/proc/sys/kernel/random/boot_id")
        .map(|s| s.trim().to_owned())
        .map_err(IdentityError::BootId)
}

#[cfg(not(target_os = "linux"))]
pub fn identity_of(_pid: u32) -> Result<ProviderIdentity, IdentityError> {
    Err(IdentityError::Unsupported)
}

#[cfg(not(target_os = "linux"))]
pub fn boot_id() -> Result<String, IdentityError> {
    Err(IdentityError::Unsupported)
}

/// What an attach shows about the stored provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdentityCheck {
    /// The same process: reattach.
    Same,
    /// Another process or another boot: `lost { identity_mismatch }`.
    Mismatch,
    /// The shim reported no identity (valid only in `starting`).
    Missing,
}

/// Compares the stored identity with the one that the shim reports. Every
/// field must be equal.
#[must_use]
pub fn check_identity(
    stored: Option<&ProviderIdentity>,
    reported: Option<&ProviderIdentity>,
) -> IdentityCheck {
    match (stored, reported) {
        (_, None) => IdentityCheck::Missing,
        (None, Some(_)) => IdentityCheck::Mismatch,
        (Some(a), Some(b)) if a == b => IdentityCheck::Same,
        (Some(_), Some(_)) => IdentityCheck::Mismatch,
    }
}
