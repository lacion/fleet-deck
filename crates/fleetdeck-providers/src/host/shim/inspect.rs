//! Process inspection for the termination proof (DL-182, ADR-005 §4.6).
//!
//! The shim counts the members of the provider's process group or session
//! when the provider exits ([`group_members`]), and the station runs the
//! same inspection for the owner's execution check ([`inspect`]). An
//! inspection is complete only when `/proc` was listed to the end and every
//! entry was read or ended in the meantime (`ENOENT`, `ESRCH`). Any other
//! failure makes the inspection incomplete: it never counts as "no
//! members" (fail closed). Only Linux has a reader; other platforms are
//! always incomplete.

use std::path::Path;

use super::wire::ProviderIdentity;

/// The longest process name that a check reports.
pub const COMMAND_CHARS: usize = 120;
/// The longest parent chain that the escaped-descendant search follows.
const CHAIN_LIMIT: usize = 64;

/// The fields of `/proc/<pid>/stat` that the inspection reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcStat {
    pub pid: u32,
    /// Field 3: `R`, `S`, `Z`, ...
    pub state: char,
    pub ppid: u32,
    pub pgrp: u32,
    pub session: u32,
    /// Field 22.
    pub start_time: u64,
}

impl ProcStat {
    /// A zombie has ended; it is not a live member.
    #[must_use]
    pub fn is_zombie(&self) -> bool {
        self.state == 'Z'
    }
}

/// The fields of one stat line. The command name in field 2 can hold
/// spaces and `)`, so the fields after the last `)` count. `None` for a
/// malformed line.
#[must_use]
pub fn parse_stat(pid: u32, stat: &str) -> Option<ProcStat> {
    let rest = &stat[stat.rfind(')')? + 1..];
    let mut fields = rest.split_whitespace();
    let state = fields.next()?.chars().next()?;
    let ppid = fields.next()?.parse().ok()?;
    let pgrp = fields.next()?.parse().ok()?;
    let session = fields.next()?.parse().ok()?;
    // After `)`: field 3 (state) is the first; field 22 is the 20th.
    let start_time = rest.split_whitespace().nth(19)?.parse().ok()?;
    Some(ProcStat {
        pid,
        state,
        ppid,
        pgrp,
        session,
        start_time,
    })
}

fn gone(e: &std::io::Error) -> bool {
    e.kind() == std::io::ErrorKind::NotFound
        || e.raw_os_error() == Some(rustix::io::Errno::SRCH.raw_os_error())
}

/// Reads every process under `root` (a `/proc` mount). `Err` with the
/// reason when the listing or any entry cannot be read completely.
///
/// # Errors
/// The reason text of the first failure.
pub fn scan(root: &Path) -> Result<Vec<ProcStat>, String> {
    let entries =
        std::fs::read_dir(root).map_err(|e| format!("cannot list {}: {e}", root.display()))?;
    let mut out = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| format!("cannot list {}: {e}", root.display()))?;
        let name = entry.file_name();
        let Some(pid) = name.to_str().and_then(|n| n.parse::<u32>().ok()) else {
            continue;
        };
        let path = root.join(pid.to_string()).join("stat");
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if gone(&e) => continue,
            Err(e) => return Err(format!("cannot read {}: {e}", path.display())),
        };
        let stat = parse_stat(pid, &text)
            .ok_or_else(|| format!("the status of process {pid} is malformed"))?;
        out.push(stat);
    }
    Ok(out)
}

/// The mount that the inspection reads.
const PROC: &str = "/proc";

/// The live members of the provider's process group or session, the
/// leader excluded. `None` when the inspection is incomplete or the
/// platform has no reader.
#[must_use]
pub fn group_members(leader: u32) -> Option<u32> {
    group_members_in(Path::new(PROC), leader)
}

/// [`group_members`] over another `/proc` root (tests).
#[must_use]
pub fn group_members_in(root: &Path, leader: u32) -> Option<u32> {
    if !cfg!(target_os = "linux") {
        return None;
    }
    let all = scan(root).ok()?;
    let count = all
        .iter()
        .filter(|p| p.pid != leader && !p.is_zombie() && (p.pgrp == leader || p.session == leader))
        .count();
    Some(u32::try_from(count).unwrap_or(u32::MAX))
}

/// One process in an inspection answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Proc {
    pub pid: u32,
    /// The first [`COMMAND_CHARS`] characters of `comm`; `?` when
    /// unreadable. Never the arguments.
    pub command: String,
}

/// The result of [`inspect`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Inspection {
    /// A live process has the stored identity (pid and start time).
    pub identity_alive: bool,
    /// Live members of the provider's process group.
    pub group: Vec<Proc>,
    /// Live members of the provider's session.
    pub session: Vec<Proc>,
    /// Live processes outside both whose parent is the shim, or whose
    /// parent chain passes the provider's former pid. Informational.
    pub escaped: Vec<Proc>,
    /// False when any read failed or the platform has no reader.
    pub complete: bool,
    pub error: Option<String>,
}

fn command_of(root: &Path, pid: u32) -> String {
    std::fs::read_to_string(root.join(pid.to_string()).join("comm"))
        .map(|text| {
            text.trim_end_matches('\n')
                .chars()
                .take(COMMAND_CHARS)
                .collect()
        })
        .unwrap_or_else(|_| "?".to_owned())
}

/// Inspects the processes of a provider execution on the same boot. The
/// caller has checked that the boot id is the stored one.
#[must_use]
pub fn inspect(identity: &ProviderIdentity, shim_pid: Option<u32>) -> Inspection {
    inspect_in(Path::new(PROC), identity, shim_pid)
}

/// [`inspect`] over another `/proc` root (tests).
#[must_use]
pub fn inspect_in(root: &Path, identity: &ProviderIdentity, shim_pid: Option<u32>) -> Inspection {
    if !cfg!(target_os = "linux") {
        return Inspection {
            error: Some("process inspection is not available on this platform".to_owned()),
            ..Inspection::default()
        };
    }
    let all = match scan(root) {
        Ok(all) => all,
        Err(error) => {
            return Inspection {
                error: Some(error),
                ..Inspection::default()
            };
        }
    };
    let leader = identity.pid;
    let identity_alive = all
        .iter()
        .any(|p| p.pid == leader && p.start_time == identity.start_time && !p.is_zombie());
    let live = |p: &&ProcStat| !p.is_zombie() && p.pid != leader;
    let proc_of = |p: &ProcStat| Proc {
        pid: p.pid,
        command: command_of(root, p.pid),
    };
    let group: Vec<Proc> = all
        .iter()
        .filter(live)
        .filter(|p| p.pgrp == leader)
        .map(proc_of)
        .collect();
    let session: Vec<Proc> = all
        .iter()
        .filter(live)
        .filter(|p| p.session == leader)
        .map(proc_of)
        .collect();
    let parent_of = |pid: u32| all.iter().find(|p| p.pid == pid).map(|p| p.ppid);
    let escaped: Vec<Proc> = all
        .iter()
        .filter(live)
        .filter(|p| p.pgrp != leader && p.session != leader)
        .filter(|p| Some(p.pid) != shim_pid)
        .filter(|p| {
            if shim_pid.is_some() && Some(p.ppid) == shim_pid {
                return true;
            }
            let mut at = p.ppid;
            for _ in 0..CHAIN_LIMIT {
                if at == leader {
                    return true;
                }
                match parent_of(at) {
                    Some(next) if next != at && next != 0 => at = next,
                    _ => return false,
                }
            }
            false
        })
        .map(proc_of)
        .collect();
    Inspection {
        identity_alive,
        group,
        session,
        escaped,
        complete: true,
        error: None,
    }
}

/// True when a live process has `identity` (pid and start time; the boot id
/// is compared by the caller). `None` when the check cannot be made.
#[must_use]
pub fn identity_alive(identity: &ProviderIdentity) -> Option<bool> {
    identity_alive_in(Path::new(PROC), identity)
}

/// [`identity_alive`] over another `/proc` root (tests).
#[must_use]
pub fn identity_alive_in(root: &Path, identity: &ProviderIdentity) -> Option<bool> {
    if !cfg!(target_os = "linux") {
        return None;
    }
    let path = root.join(identity.pid.to_string()).join("stat");
    match std::fs::read_to_string(&path) {
        Ok(text) => {
            let stat = parse_stat(identity.pid, &text)?;
            Some(stat.start_time == identity.start_time && !stat.is_zombie())
        }
        Err(e) if gone(&e) => Some(false),
        Err(_) => None,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn stat_line(pid: u32, state: char, ppid: u32, pgrp: u32, session: u32, start: u64) -> String {
        // Fields 1..=22; the command name holds a space and a `)`.
        format!(
            "{pid} (a b) c) {state} {ppid} {pgrp} {session} 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 {start} 0"
        )
    }

    fn put(root: &Path, pid: u32, stat: &str) {
        let dir = root.join(pid.to_string());
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("stat"), stat).unwrap();
        std::fs::write(dir.join("comm"), "worker\n").unwrap();
    }

    #[test]
    fn the_stat_parser_reads_a_name_with_spaces_and_a_bracket() {
        let s = parse_stat(7, &stat_line(7, 'S', 3, 5, 6, 99)).unwrap();
        assert_eq!((s.ppid, s.pgrp, s.session, s.start_time), (3, 5, 6, 99));
        assert!(parse_stat(7, "7 (x) S 1").is_none());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_complete_listing_counts_members_and_skips_zombies() {
        let dir = tempfile::tempdir().unwrap();
        put(dir.path(), 100, &stat_line(100, 'S', 1, 100, 100, 5));
        put(dir.path(), 101, &stat_line(101, 'S', 100, 100, 100, 6));
        put(dir.path(), 102, &stat_line(102, 'Z', 100, 100, 100, 7));
        put(dir.path(), 103, &stat_line(103, 'S', 1, 103, 100, 8));
        put(dir.path(), 200, &stat_line(200, 'S', 1, 200, 200, 9));
        std::fs::create_dir_all(dir.path().join("self")).unwrap();
        assert_eq!(group_members_in(dir.path(), 100), Some(2));
        let empty = tempfile::tempdir().unwrap();
        assert_eq!(group_members_in(empty.path(), 100), Some(0));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn an_unlistable_proc_is_not_zero_members() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(group_members_in(&dir.path().join("missing"), 100), None);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn an_unreadable_entry_is_not_zero_members() {
        let dir = tempfile::tempdir().unwrap();
        // `stat` is a directory: reading it fails with EISDIR, not ENOENT.
        std::fs::create_dir_all(dir.path().join("100").join("stat")).unwrap();
        assert_eq!(group_members_in(dir.path(), 999), None);
        // An entry without a `stat` file ended between listing and reading.
        let ended = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(ended.path().join("100")).unwrap();
        assert_eq!(group_members_in(ended.path(), 999), Some(0));
        // A malformed line is incomplete too.
        let bad = tempfile::tempdir().unwrap();
        put(bad.path(), 100, "garbage");
        assert_eq!(group_members_in(bad.path(), 999), None);
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn a_platform_without_a_reader_is_never_zero_members() {
        assert_eq!(group_members(1), None);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn the_inspection_lists_group_session_and_escaped_processes() {
        let dir = tempfile::tempdir().unwrap();
        // The provider 100 is gone; its group member 101 runs, 103 left the
        // group but not the session, 300 left both (child of the shim 50).
        put(dir.path(), 101, &stat_line(101, 'S', 1, 100, 100, 6));
        put(dir.path(), 103, &stat_line(103, 'S', 1, 103, 100, 8));
        put(dir.path(), 300, &stat_line(300, 'S', 50, 300, 300, 9));
        // A grandchild of the shim, not through the provider: not listed.
        put(dir.path(), 301, &stat_line(301, 'S', 300, 301, 301, 9));
        // A child of the provider's former pid and its own child: listed.
        put(dir.path(), 500, &stat_line(500, 'S', 100, 500, 500, 9));
        put(dir.path(), 501, &stat_line(501, 'S', 500, 501, 501, 9));
        put(dir.path(), 400, &stat_line(400, 'S', 1, 400, 400, 9));
        let id = ProviderIdentity {
            pid: 100,
            start_time: 5,
            boot_id: "b".to_owned(),
        };
        let got = inspect_in(dir.path(), &id, Some(50));
        assert!(got.complete && got.error.is_none());
        assert!(!got.identity_alive);
        assert_eq!(got.group.iter().map(|p| p.pid).collect::<Vec<_>>(), [101]);
        assert_eq!(
            got.session.iter().map(|p| p.pid).collect::<Vec<_>>(),
            [101, 103]
        );
        let mut escaped: Vec<u32> = got.escaped.iter().map(|p| p.pid).collect();
        escaped.sort_unstable();
        assert_eq!(escaped, [300, 500, 501]);
        assert_eq!(got.group[0].command, "worker");
        put(dir.path(), 100, &stat_line(100, 'S', 50, 100, 100, 5));
        assert!(inspect_in(dir.path(), &id, Some(50)).identity_alive);
        let reused = ProviderIdentity {
            start_time: 6,
            ..id.clone()
        };
        assert!(!inspect_in(dir.path(), &reused, Some(50)).identity_alive);
        assert_eq!(identity_alive_in(dir.path(), &id), Some(true));
        assert_eq!(identity_alive_in(dir.path(), &reused), Some(false));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn an_incomplete_inspection_says_why() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("100").join("stat")).unwrap();
        let id = ProviderIdentity {
            pid: 1,
            start_time: 1,
            boot_id: "b".to_owned(),
        };
        let got = inspect_in(dir.path(), &id, None);
        assert!(!got.complete);
        assert!(got.error.unwrap().contains("cannot read"));
    }
}
