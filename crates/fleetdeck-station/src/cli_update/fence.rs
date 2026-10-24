//! The fence of an update attempt (design §3.6.3).
//!
//! Every child of an attempt carries `FLEETDECK_CLI_UPDATE_ID=<marker>` and
//! runs in its own process group. [`fence`] stops the group and every other
//! process that carries the marker. It reuses the setup cleanup (DL-148): a
//! group is signalled only when every process in it carries the marker, so a
//! process group id that the system reused after a reboot is never signalled.

use std::time::{Duration, Instant};

use fleetdeck_proto::cli_update::reason;
use rustix::io::Errno;
use rustix::process::{Pid, Signal, kill_process_group, test_kill_process_group};

use crate::workspace::setup_cleanup::kill_marked_in;

/// The marker of every child of an attempt.
pub const MARKER_VAR: &str = "FLEETDECK_CLI_UPDATE_ID";

/// The wait between SIGTERM and SIGKILL, and the limit of the marker scan.
const WAIT: Duration = Duration::from_secs(5);
const POLL: Duration = Duration::from_millis(50);

/// Stops every process of `marker`: SIGTERM to the group `pgid`, a wait of
/// at most 5 s, SIGKILL to the group, then the marker scan of the setup
/// cleanup (`kill_marked_in`, 5 s). `Ok` only when the group is gone
/// (`test_kill_process_group` answers `ESRCH`) and the scan finds no
/// process. Blocking: call it in `spawn_blocking`.
///
/// # Errors
/// `process_left`.
pub fn fence(marker: &str, pgid: Option<i32>) -> Result<(), &'static str> {
    fence_with(marker, pgid, WAIT)
}

/// [`fence`] with the wait between SIGTERM and SIGKILL (tests use a short one).
pub(crate) fn fence_with(
    marker: &str,
    pgid: Option<i32>,
    term_wait: Duration,
) -> Result<(), &'static str> {
    let group = pgid
        .and_then(Pid::from_raw)
        .filter(|p| p.as_raw_nonzero().get() > 1);
    if let Some(group) = group {
        if group_is_ours(marker, group.as_raw_nonzero().get()) {
            signal(group, Signal::TERM);
            wait_gone(group, term_wait);
        }
        if group_is_ours(marker, group.as_raw_nonzero().get()) {
            signal(group, Signal::KILL);
        }
    }
    match kill_marked_in(MARKER_VAR, marker, pgid.unwrap_or(0), WAIT) {
        Ok(0) => {}
        Ok(_) => return Err(reason::PROCESS_LEFT),
        Err(e) => {
            tracing::error!(error = %e, "the processes of an update cannot be listed");
            return Err(reason::PROCESS_LEFT);
        }
    }
    match group {
        Some(group) => {
            wait_gone(group, term_wait);
            if gone(group) {
                Ok(())
            } else {
                Err(reason::PROCESS_LEFT)
            }
        }
        None => Ok(()),
    }
}

fn signal(group: Pid, sig: Signal) {
    if let Err(e) = kill_process_group(group, sig)
        && e != Errno::SRCH
    {
        tracing::warn!(error = %e, "cannot signal an update process group");
    }
}

/// True when no process is in the group.
fn gone(group: Pid) -> bool {
    test_kill_process_group(group) == Err(Errno::SRCH)
}

fn wait_gone(group: Pid, wait: Duration) {
    let deadline = Instant::now() + wait;
    while !gone(group) && Instant::now() < deadline {
        std::thread::sleep(POLL);
    }
}

/// True when the group has members and every one of them carries the marker
/// (or is a zombie, which has no environment left to read). Without a
/// process listing (not Linux) a group is never signalled here; the marker
/// scan then finds the processes.
#[cfg(target_os = "linux")]
fn group_is_ours(marker: &str, pgid: i32) -> bool {
    use crate::workspace::setup_cleanup::stat_pgrp;

    let want = format!("{MARKER_VAR}={marker}");
    let Ok(dir) = std::fs::read_dir("/proc") else {
        return false;
    };
    let mut members = 0;
    for entry in dir.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|n| n.parse::<i32>().ok())
        else {
            continue;
        };
        let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
            continue;
        };
        if stat_pgrp(&stat) != Some(pgid) {
            continue;
        }
        members += 1;
        let zombie = stat
            .rfind(')')
            .and_then(|i| stat[i + 1..].split_whitespace().next())
            == Some("Z");
        let marked = std::fs::read(format!("/proc/{pid}/environ"))
            .is_ok_and(|e| e.split(|b| *b == 0).any(|v| v == want.as_bytes()));
        if !(marked || zombie) {
            return false;
        }
    }
    members > 0
}

#[cfg(not(target_os = "linux"))]
fn group_is_ours(_marker: &str, _pgid: i32) -> bool {
    false
}

#[cfg(all(test, target_os = "linux"))]
#[path = "fence_tests.rs"]
mod tests;
