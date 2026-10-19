//! The inventory for the stop plan of a lost run (BG-4, design
//! `docs/v2/design/board-gaps.md` §5.3 rules 3 and 6).
//!
//! The read-side census of [`super::inspect`] settles at the first live
//! member: it is not a termination inventory. This module reads `/proc`
//! completely, twice, and gives the live members of the domain of a leader
//! PID: every process whose process group or session equals that number,
//! the leader included. It reads the start time, the state and the real UID
//! of each member. The two passes must give the same members (PID and
//! start time); otherwise the answer is [`StopInventoryError::Incomplete`].
//! Only Linux has a reader; other platforms answer
//! [`StopInventoryError::Unsupported`].
//!
//! The inventory proves nothing about who owns a member: an equal PGID or
//! SID alone is never a witness (rule 5). The station decides that.

use std::path::Path;

use super::inspect::{self, COMMAND_CHARS, ProcStat};

/// The mount that the inventory reads.
const PROC: &str = "/proc";
/// The longest parent chain that [`Inventory::ancestors`] follows.
const CHAIN_LIMIT: usize = 64;

/// Why no inventory exists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StopInventoryError {
    /// The platform has no `/proc` reader.
    Unsupported,
    /// A read failed or the two passes differ: the inventory proves nothing.
    Incomplete(String),
}

impl std::fmt::Display for StopInventoryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unsupported => f.write_str("process inspection is not available here"),
            Self::Incomplete(why) => f.write_str(why),
        }
    }
}

/// One live process of the domain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Member {
    pub pid: u32,
    /// Field 22 of `stat`.
    pub start_time: u64,
    /// The real UID (first field of `Uid:` in `status`).
    pub uid: u32,
    pub pgrp: u32,
    pub session: u32,
}

/// A complete, stable view of the domain of `leader`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Inventory {
    pub leader: u32,
    /// The process that has the PID `leader` now, in any state (a zombie
    /// too); `None` when no such process exists.
    pub leader_stat: Option<ProcStat>,
    /// The live (not zombie) processes whose group or session is `leader`,
    /// the leader included, sorted by PID.
    pub domain: Vec<Member>,
    /// Every process of the second pass, for the parent chains.
    pub all: Vec<ProcStat>,
}

impl Inventory {
    /// The PID chain of `pid` up to PID 1: the parents, the process itself
    /// excluded. A chain that is longer than 64 or broken ends there.
    #[must_use]
    pub fn ancestors(&self, pid: u32) -> Vec<u32> {
        let mut out = Vec::new();
        let mut at = pid;
        for _ in 0..CHAIN_LIMIT {
            let Some(parent) = self.all.iter().find(|p| p.pid == at).map(|p| p.ppid) else {
                break;
            };
            if parent == 0 || parent == at || out.contains(&parent) {
                break;
            }
            out.push(parent);
            at = parent;
        }
        out
    }

    /// The `(pid, start_time)` pairs of the domain.
    fn keys(&self) -> Vec<(u32, u64)> {
        self.domain.iter().map(|m| (m.pid, m.start_time)).collect()
    }
}

/// The inventory of `leader` over the real `/proc`. It blocks: call it from
/// a blocking thread.
///
/// # Errors
/// [`StopInventoryError`].
pub fn inventory(leader: u32) -> Result<Inventory, StopInventoryError> {
    inventory_in(Path::new(PROC), leader)
}

/// [`inventory`] over another `/proc` root (tests).
///
/// # Errors
/// [`StopInventoryError`].
pub fn inventory_in(root: &Path, leader: u32) -> Result<Inventory, StopInventoryError> {
    inventory_with(root, leader, &mut || {})
}

/// [`inventory_in`] with `between`, which runs once between the two passes
/// (tests change the fake `/proc` there).
///
/// # Errors
/// [`StopInventoryError`].
pub fn inventory_with(
    root: &Path,
    leader: u32,
    between: &mut dyn FnMut(),
) -> Result<Inventory, StopInventoryError> {
    if !cfg!(target_os = "linux") {
        return Err(StopInventoryError::Unsupported);
    }
    let first = pass(root, leader)?;
    between();
    let second = pass(root, leader)?;
    if first.keys() != second.keys() {
        return Err(StopInventoryError::Incomplete(
            "the processes changed during the safety check".to_owned(),
        ));
    }
    Ok(second)
}

fn incomplete(why: impl Into<String>) -> StopInventoryError {
    StopInventoryError::Incomplete(why.into())
}

/// One full pass over `root`.
fn pass(root: &Path, leader: u32) -> Result<Inventory, StopInventoryError> {
    let scan = inspect::scan(root).map_err(incomplete)?;
    let mut domain = Vec::new();
    for p in &scan.procs {
        if p.is_zombie() || (p.pgrp != leader && p.session != leader) {
            continue;
        }
        match uid_of(root, p.pid) {
            Ok(Some(uid)) => domain.push(Member {
                pid: p.pid,
                start_time: p.start_time,
                uid,
                pgrp: p.pgrp,
                session: p.session,
            }),
            // It ended between the `stat` and the `status` read.
            Ok(None) => {}
            Err(why) => return Err(incomplete(why)),
        }
    }
    domain.sort_by_key(|m| m.pid);
    let leader_stat = scan.procs.iter().find(|p| p.pid == leader).cloned();
    Ok(Inventory {
        leader,
        leader_stat,
        domain,
        all: scan.procs,
    })
}

/// The real UID of `pid`; `Ok(None)` when the process ended.
fn uid_of(root: &Path, pid: u32) -> Result<Option<u32>, String> {
    let path = root.join(pid.to_string()).join("status");
    let text = match std::fs::read(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) if e.raw_os_error() == Some(rustix::io::Errno::SRCH.raw_os_error()) => {
            return Ok(None);
        }
        Err(e) => return Err(format!("cannot read {}: {e}", path.display())),
    };
    text.split(|&b| b == b'\n')
        .find_map(|line| line.strip_prefix(b"Uid:"))
        .and_then(|rest| {
            let rest = String::from_utf8_lossy(rest);
            rest.split_whitespace().next()?.parse().ok()
        })
        .map(Some)
        .ok_or_else(|| format!("the status of process {pid} has no readable UID"))
}

/// The state of one PID now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StatNow {
    /// No such process.
    Gone,
    Alive(ProcStat),
    /// The read failed for another reason.
    Unreadable(String),
}

/// Reads `stat` of `pid` under the real `/proc`.
#[must_use]
pub fn stat_now(pid: u32) -> StatNow {
    stat_now_in(Path::new(PROC), pid)
}

/// [`stat_now`] over another `/proc` root (tests).
#[must_use]
pub fn stat_now_in(root: &Path, pid: u32) -> StatNow {
    let path = root.join(pid.to_string()).join("stat");
    match std::fs::read(&path) {
        Ok(text) => match inspect::parse_stat(pid, &text) {
            Some(stat) => StatNow::Alive(stat),
            None => StatNow::Unreadable(format!("the status of process {pid} is malformed")),
        },
        Err(e)
            if e.kind() == std::io::ErrorKind::NotFound
                || e.raw_os_error() == Some(rustix::io::Errno::SRCH.raw_os_error()) =>
        {
            StatNow::Gone
        }
        Err(e) => StatNow::Unreadable(format!("cannot read {}: {e}", path.display())),
    }
}

/// The first 120 characters of `comm` of `pid` under the real `/proc`;
/// `?` when unreadable. Never the arguments.
#[must_use]
pub fn comm_of(pid: u32) -> String {
    comm_in(Path::new(PROC), pid)
}

/// [`comm_of`] over another `/proc` root (tests).
#[must_use]
pub fn comm_in(root: &Path, pid: u32) -> String {
    std::fs::read_to_string(root.join(pid.to_string()).join("comm"))
        .map(|text| {
            text.trim_end_matches('\n')
                .chars()
                .take(COMMAND_CHARS)
                .collect()
        })
        .unwrap_or_else(|_| "?".to_owned())
}

#[cfg(test)]
#[path = "stop_inventory_tests.rs"]
mod tests;
