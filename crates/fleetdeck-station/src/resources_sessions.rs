//! The CPU, memory and process count of each live session's process tree,
//! read from `/proc` (R13 §10). Linux only: other systems report an empty
//! list.
//!
//! [`sample_tree`] is a pure function over a `/proc` directory. [`run`] is
//! the task that samples every [`SESSION_LOAD_EVERY`] and stores the latest
//! list for `resources::current`.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use fleetdeck_proto::SessionId;
use fleetdeck_proto::resources::SessionLoad;
use tokio::sync::watch;

use crate::manager::SessionManager;

/// The time between two samples (R13 §10).
pub const SESSION_LOAD_EVERY: Duration = Duration::from_secs(10);

/// Clock ticks per second in `/proc/<pid>/stat` (`USER_HZ` is 100 on Linux).
const USER_HZ: f64 = 100.0;

static LATEST: Mutex<Vec<SessionLoad>> = Mutex::new(Vec::new());

/// The list of the last sample; empty until the first one.
#[must_use]
pub fn latest() -> Vec<SessionLoad> {
    LATEST
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
}

fn store(loads: Vec<SessionLoad>) {
    *LATEST
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = loads;
}

/// A process is keyed by its pid and start time, so a reused pid is new.
type Key = (u32, u64);

/// The CPU ticks of each process of the last sample.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Prev {
    ticks: HashMap<Key, u64>,
}

struct Proc {
    ppid: u32,
    start: u64,
    ticks: u64,
}

/// Parses `/proc/<pid>/stat`. The comm field can hold spaces and
/// parentheses, so the fields start after the last `)`.
fn parse_stat(text: &str) -> Option<Proc> {
    let rest = &text[text.rfind(')')? + 1..];
    let f: Vec<&str> = rest.split_whitespace().collect();
    // Field n of the file is `f[n - 3]`: state is field 3.
    let num = |n: usize| f.get(n - 3)?.parse::<u64>().ok();
    Some(Proc {
        ppid: u32::try_from(num(4)?).ok()?,
        ticks: num(14)?.saturating_add(num(15)?),
        start: num(22)?,
    })
}

/// `VmRSS` of `/proc/<pid>/status` in bytes; 0 when the file or the line is
/// absent (a kernel thread or a zombie).
fn rss_bytes(proc_root: &Path, pid: u32) -> u64 {
    let Ok(text) = std::fs::read_to_string(proc_root.join(pid.to_string()).join("status")) else {
        return 0;
    };
    text.lines()
        .find_map(|l| l.strip_prefix("VmRSS:"))
        .and_then(|v| v.split_whitespace().next()?.parse::<u64>().ok())
        .map_or(0, |kb| kb.saturating_mul(1024))
}

/// Every process of `proc_root`. A process that ends during the read is
/// skipped.
fn read_procs(proc_root: &Path) -> HashMap<u32, Proc> {
    let mut out = HashMap::new();
    let Ok(dir) = std::fs::read_dir(proc_root) else {
        tracing::debug!(root = %proc_root.display(), "the process directory is unreadable");
        return out;
    };
    for entry in dir.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|n| n.parse::<u32>().ok())
        else {
            continue;
        };
        match std::fs::read_to_string(entry.path().join("stat")) {
            Ok(text) => {
                if let Some(p) = parse_stat(&text) {
                    out.insert(pid, p);
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => tracing::debug!(error = %e, pid, "the process stat is unreadable"),
        }
    }
    out
}

/// Sorts by `cpu_pct` descending, `None` last, then by session id.
fn sort_loads(loads: &mut [SessionLoad]) {
    loads.sort_by(|a, b| {
        let by_cpu = match (a.cpu_pct, b.cpu_pct) {
            (Some(x), Some(y)) => y.cmp(&x),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => std::cmp::Ordering::Equal,
        };
        by_cpu.then_with(|| a.session_id.cmp(&b.session_id))
    });
}

/// The load of each session's process tree and the state for the next call.
/// `roots` are the session ids with their provider pids; `elapsed` is the
/// time since the sample that made `prev`.
#[must_use]
pub fn sample_tree(
    proc_root: &Path,
    roots: &[(SessionId, u32)],
    prev: &Prev,
    elapsed: Duration,
) -> (Vec<SessionLoad>, Prev) {
    let procs = read_procs(proc_root);
    let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
    for (pid, p) in &procs {
        children.entry(p.ppid).or_default().push(*pid);
    }
    let secs = elapsed.as_secs_f64();
    let mut next = Prev::default();
    let mut loads = Vec::with_capacity(roots.len());
    for (session_id, root) in roots {
        let Some(root_proc) = procs.get(root) else {
            loads.push(SessionLoad {
                session_id: session_id.clone(),
                cpu_pct: None,
                rss_bytes: None,
                procs: 0,
            });
            continue;
        };
        let mut tree = BTreeSet::from([*root]);
        let mut todo = vec![*root];
        while let Some(pid) = todo.pop() {
            for child in children.get(&pid).into_iter().flatten() {
                if tree.insert(*child) {
                    todo.push(*child);
                }
            }
        }
        let (mut delta, mut rss) = (0_u64, 0_u64);
        for pid in &tree {
            let p = &procs[pid];
            let key = (*pid, p.start);
            delta += p.ticks - prev.ticks.get(&key).copied().unwrap_or(0).min(p.ticks);
            rss = rss.saturating_add(rss_bytes(proc_root, *pid));
            next.ticks.insert(key, p.ticks);
        }
        let seen = prev.ticks.contains_key(&(*root, root_proc.start));
        let cpu_pct = (seen && secs > 0.0).then(|| {
            // Whole percent of one core: ticks / USER_HZ is CPU seconds.
            let pct = (delta as f64 / USER_HZ / secs * 100.0).round();
            pct.min(f64::from(u32::MAX)) as u32
        });
        loads.push(SessionLoad {
            session_id: session_id.clone(),
            cpu_pct,
            rss_bytes: Some(rss),
            procs: u32::try_from(tree.len()).unwrap_or(u32::MAX),
        });
    }
    sort_loads(&mut loads);
    (loads, next)
}

/// Adds the pids that the manager knows now to `known`, drops the sessions
/// that are no longer live, and returns the roots to sample. A session whose
/// pid is not known now keeps its last pid; one never seen is left out.
fn merge_roots(
    known: &mut BTreeMap<SessionId, u32>,
    live: Vec<(SessionId, Option<u32>)>,
) -> Vec<(SessionId, u32)> {
    let ids: BTreeSet<&SessionId> = live.iter().map(|(id, _)| id).collect();
    known.retain(|id, _| ids.contains(id));
    for (id, pid) in &live {
        if let Some(pid) = pid {
            known.insert(id.clone(), *pid);
        }
    }
    known.iter().map(|(id, pid)| (id.clone(), *pid)).collect()
}

/// Samples the live sessions every `every` until `shutdown` is set.
pub async fn run(
    manager: Arc<SessionManager>,
    every: Duration,
    mut shutdown: watch::Receiver<bool>,
) {
    if !cfg!(target_os = "linux") {
        return;
    }
    let mut tick = tokio::time::interval(every);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut known = BTreeMap::new();
    let mut prev = Prev::default();
    let mut last: Option<Instant> = None;
    loop {
        tokio::select! {
            _ = tick.tick() => {
                let roots = merge_roots(&mut known, manager.live_pids());
                let now = Instant::now();
                let elapsed = last.map_or(Duration::ZERO, |t| now.duration_since(t));
                let before = std::mem::take(&mut prev);
                let task = tokio::task::spawn_blocking(move || {
                    sample_tree(Path::new("/proc"), &roots, &before, elapsed)
                });
                match task.await {
                    Ok((loads, next)) => {
                        prev = next;
                        last = Some(now);
                        store(loads);
                    }
                    Err(e) => tracing::warn!(error = %e, "the session load sample failed"),
                }
            }
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    return;
                }
            }
        }
    }
}

#[cfg(test)]
#[path = "resources_sessions_tests.rs"]
mod tests;
