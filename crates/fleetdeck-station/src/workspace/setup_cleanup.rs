//! The restart cleanup of setup commands (DL-148).
//!
//! A `setup_runs` row that exists at the station start belongs to a setup run
//! that the previous station process did not finish. A setup process may have
//! outlived that station. Every process of the setup run holds
//! `FLEETDECK_SETUP_ID=<effect id>` in its environment, so the cleanup finds
//! them by it and kills them. A process that cleared its own environment is
//! not found (a documented limit).

use std::time::Duration;

use fleetdeck_proto::event::Event;
use rustix::process::{Pid, Signal, kill_process, kill_process_group};

use super::setup::MARKER_VAR;
use crate::store::setup_runs::SetupRun;
use crate::store::{NewEvent, Store, StoreError};

/// How long the cleanup repeats its scan for one row.
const SCAN_WAIT: Duration = Duration::from_secs(5);
const SCAN_POLL: Duration = Duration::from_millis(100);
/// The `code` of the `session.error` event of a row that stays.
pub const CODE_PROCESS_LEFT: &str = "setup_process_left";

/// What one scan found for one setup run.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Found {
    /// The processes of the station's user that carry the marker.
    pub marked: Vec<i32>,
    /// The processes of the group, when the platform tells it (Linux).
    pub group: Vec<i32>,
}

/// Stops the processes of every stored setup run and deletes the rows of the
/// runs that have no process left. Returns the number of rows deleted. A run
/// with a process that stays keeps its row, is logged, and its session gets a
/// `session.error` event.
///
/// # Errors
/// A database error when the rows cannot be read.
pub async fn cleanup_stale(store: &Store) -> Result<usize, StoreError> {
    let mut deleted = 0;
    for run in store.setup_runs().await? {
        let left = stop_run(&run).await;
        if left {
            tracing::error!(
                effect = %run.effect_id,
                session = %run.session_id,
                "a setup process may still run"
            );
            report_left(store, &run).await;
            continue;
        }
        match store.setup_run_end(run.effect_id.clone()).await {
            Ok(()) => deleted += 1,
            Err(e) => tracing::warn!(error = %e, effect = %run.effect_id, "setup row not deleted"),
        }
    }
    Ok(deleted)
}

async fn report_left(store: &Store, run: &SetupRun) {
    let cwd = match store.summary(run.session_id.clone()).await {
        Ok(Some(s)) => s.cwd.unwrap_or_else(|| "its workspace".to_owned()),
        Ok(None) => "its workspace".to_owned(),
        Err(e) => {
            tracing::error!(error = %e, "the session of a setup run is unreadable");
            "its workspace".to_owned()
        }
    };
    let event = Event::Error {
        code: CODE_PROCESS_LEFT.into(),
        message: format!(
            "a setup process may still run in {cwd}; check it before you start a new session there"
        ),
        retry_at: None,
    };
    if let Err(e) = store
        .append(run.session_id.clone(), vec![NewEvent::from(event)])
        .await
    {
        tracing::error!(error = %e, session = %run.session_id, "the setup warning was not stored");
    }
}

/// Kills what the scan finds, until it finds nothing or the time is up.
/// Returns true when a process may be left (also when the scan cannot run).
async fn stop_run(run: &SetupRun) -> bool {
    let (effect, pgid) = (run.effect_id.clone(), run.pgid);
    match tokio::task::spawn_blocking(move || kill_marked_in(MARKER_VAR, &effect, pgid, SCAN_WAIT))
        .await
    {
        Ok(Ok(left)) => left > 0,
        Ok(Err(e)) => {
            tracing::error!(error = %e, "the setup processes cannot be listed");
            true
        }
        Err(e) => {
            tracing::error!(error = %e, "the setup scan stopped");
            true
        }
    }
}

/// SIGKILLs every process of the station's user that carries the marker
/// `var=id` in its environment, in any process group, and scans again until
/// none is left or `wait` has passed. Returns the number of processes that the last scan
/// still found. It blocks the thread: call it from a blocking context.
///
/// # Errors
/// The text of the error when the processes cannot be listed.
pub(crate) fn kill_marked(var: &str, id: &str, wait: Duration) -> Result<usize, String> {
    kill_marked_in(var, id, 0, wait)
}

/// [`kill_marked`] with the process group of the run, which is also killed
/// when all of its processes carry the marker.
pub(crate) fn kill_marked_in(
    var: &str,
    id: &str,
    pgid: i32,
    wait: Duration,
) -> Result<usize, String> {
    let deadline = std::time::Instant::now() + wait;
    loop {
        let found = scan(var, id, pgid).map_err(|e| e.to_string())?;
        if found.marked.is_empty() {
            return Ok(0);
        }
        if std::time::Instant::now() >= deadline {
            return Ok(found.marked.len());
        }
        kill_found(&found, pgid);
        std::thread::sleep(SCAN_POLL);
    }
}

/// SIGKILL to every marked process, and to the whole group when all of its
/// processes carry the marker.
fn kill_found(found: &Found, pgid: i32) {
    let group_is_ours =
        !found.group.is_empty() && found.group.iter().all(|p| found.marked.contains(p));
    if group_is_ours
        && let Some(pgid) = Pid::from_raw(pgid).filter(|p| p.as_raw_nonzero().get() > 1)
        && let Err(e) = kill_process_group(pgid, Signal::KILL)
        && e != rustix::io::Errno::SRCH
    {
        tracing::warn!(error = %e, "cannot kill the setup process group");
    }
    for pid in &found.marked {
        let Some(pid) = Pid::from_raw(*pid).filter(|p| p.as_raw_nonzero().get() > 1) else {
            continue;
        };
        if let Err(e) = kill_process(pid, Signal::KILL)
            && e != rustix::io::Errno::SRCH
        {
            tracing::warn!(error = %e, "cannot kill a setup process");
        }
    }
}

/// Lists the processes of the station's user that carry the marker of
/// `effect_id`, on Linux from `/proc`.
#[cfg(target_os = "linux")]
fn scan(var: &str, id: &str, pgid: i32) -> std::io::Result<Found> {
    use std::os::unix::fs::MetadataExt as _;

    let uid = rustix::process::getuid().as_raw();
    let marker = format!("{var}={id}");
    let own = i32::try_from(std::process::id()).unwrap_or(0);
    let mut found = Found::default();
    for entry in std::fs::read_dir("/proc")? {
        let Ok(entry) = entry else { continue };
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|n| n.parse::<i32>().ok())
        else {
            continue;
        };
        if pid == own {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        if meta.uid() != uid {
            continue;
        }
        if let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat"))
            && stat_pgrp(&stat) == Some(pgid)
        {
            found.group.push(pid);
        }
        let Ok(environ) = std::fs::read(format!("/proc/{pid}/environ")) else {
            continue;
        };
        if environ
            .split(|b| *b == 0)
            .any(|var| var == marker.as_bytes())
        {
            found.marked.push(pid);
        }
    }
    Ok(found)
}

/// The process group field of a `/proc/<pid>/stat` line: the command name
/// is in parentheses and can hold spaces, so the fields start after the last
/// `)`: state, ppid, pgrp.
#[cfg(any(target_os = "linux", test))]
pub(crate) fn stat_pgrp(stat: &str) -> Option<i32> {
    let rest = &stat[stat.rfind(')')? + 1..];
    rest.split_whitespace().nth(2)?.parse().ok()
}

/// Lists the processes of the station's user that carry the marker, on macOS
/// from `ps -E`. The process group is not checked on macOS.
#[cfg(target_os = "macos")]
fn scan(var: &str, id: &str, _pgid: i32) -> std::io::Result<Found> {
    let uid = rustix::process::getuid().as_raw().to_string();
    let out = std::process::Command::new("ps")
        .args(["-E", "-ww", "-o", "pid=,command=", "-U", &uid])
        .output()?;
    if !out.status.success() {
        return Err(std::io::Error::other("ps failed"));
    }
    let own = i32::try_from(std::process::id()).unwrap_or(0);
    let text = String::from_utf8_lossy(&out.stdout);
    let marked = text
        .lines()
        .filter_map(|line| ps_match(line, var, id))
        .filter(|pid| *pid != own)
        .collect();
    Ok(Found {
        marked,
        group: Vec::new(),
    })
}

/// A platform without a process listing cannot find the processes.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn scan(_var: &str, _id: &str, _pgid: i32) -> std::io::Result<Found> {
    Err(std::io::Error::other(
        "this platform cannot list the setup processes",
    ))
}

/// The pid of a `ps -E` line whose command line ends with the marker or has
/// it followed by a space.
#[cfg(any(target_os = "macos", test))]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn ps_match(line: &str, var: &str, id: &str) -> Option<i32> {
    let marker = format!(" {var}={id}");
    let found = line.match_indices(&marker).any(|(at, _)| {
        let after = &line[at + marker.len()..];
        after.is_empty() || after.starts_with(' ')
    });
    if !found {
        return None;
    }
    line.split_whitespace().next()?.parse().ok()
}

#[cfg(test)]
mod tests {
    use std::os::unix::process::ExitStatusExt as _;
    use std::process::Stdio;

    use fleetdeck_proto::SessionId;
    use fleetdeck_testkit::Det;
    use tokio::process::Command;

    use super::*;
    use crate::store::StoreConfig;

    async fn store() -> Store {
        let det = Det::new();
        Store::open_in_memory(det.clock(), det.ids(), StoreConfig::default())
            .await
            .unwrap()
    }

    /// `sleep 60` in its own process group, with the marker.
    fn marked_sleep(effect_id: &str, marked: bool) -> tokio::process::Child {
        let mut cmd = Command::new("/bin/sh");
        cmd.args(["-c", "exec sleep 60"])
            .process_group(0)
            .stdin(Stdio::null());
        if marked {
            cmd.env(MARKER_VAR, effect_id);
        }
        cmd.spawn().unwrap()
    }

    #[tokio::test]
    async fn the_cleanup_kills_a_marked_process_and_deletes_the_row() {
        let store = store().await;
        let mut child = marked_sleep("c1", true);
        let pid = i32::try_from(child.id().unwrap()).unwrap();
        store
            .setup_run_begin("c1".into(), SessionId::from("s1"), pid)
            .await
            .unwrap();
        let deleted = tokio::time::timeout(Duration::from_secs(20), cleanup_stale(&store))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(deleted, 1);
        assert!(store.setup_runs().await.unwrap().is_empty());
        let status = tokio::time::timeout(Duration::from_secs(5), child.wait())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(status.signal(), Some(9));
    }

    #[tokio::test]
    async fn the_cleanup_leaves_a_process_without_the_marker() {
        let store = store().await;
        let mut child = marked_sleep("c2", false);
        let pid = i32::try_from(child.id().unwrap()).unwrap();
        store
            .setup_run_begin("c2".into(), SessionId::from("s1"), pid)
            .await
            .unwrap();
        assert_eq!(cleanup_stale(&store).await.unwrap(), 1);
        assert!(store.setup_runs().await.unwrap().is_empty());
        assert!(child.try_wait().unwrap().is_none(), "still running");
        child.kill().await.unwrap();
    }

    #[tokio::test]
    async fn a_row_without_a_process_is_deleted() {
        let store = store().await;
        store
            .setup_run_begin("c3".into(), SessionId::from("s1"), 999_999)
            .await
            .unwrap();
        assert_eq!(cleanup_stale(&store).await.unwrap(), 1);
        assert!(store.setup_runs().await.unwrap().is_empty());
    }

    #[test]
    fn the_group_field_of_a_stat_line_survives_a_command_name_with_spaces() {
        assert_eq!(stat_pgrp("123 (sleep) S 1 456 456 0 -1"), Some(456));
        assert_eq!(stat_pgrp("123 (a b) c) S 1 789 789 0"), Some(789));
        assert_eq!(stat_pgrp("garbage"), None);
    }

    #[test]
    fn a_ps_line_matches_the_marker_only_as_a_whole_variable() {
        let line = " 4242 sleep 60 HOME=/h FLEETDECK_SETUP_ID=abc PATH=/bin";
        assert_eq!(ps_match(line, MARKER_VAR, "abc"), Some(4242));
        assert_eq!(
            ps_match(" 7 sh FLEETDECK_SETUP_ID=abc", MARKER_VAR, "abc"),
            Some(7)
        );
        assert_eq!(ps_match(line, MARKER_VAR, "ab"), None);
        assert_eq!(
            ps_match(" 7 sh XFLEETDECK_SETUP_ID=abc", MARKER_VAR, "abc"),
            None
        );
    }
}
