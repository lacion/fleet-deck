//! What the station reports about its disk, memory and CPU in
//! `station.info` (DL-193). Warnings only: the station never refuses work
//! because of these numbers.
//!
//! The gather runs on the blocking pool, under one time limit, and the
//! result is cached for 30 s.

use std::collections::BTreeSet;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use fleetdeck_proto::resources::{CpuLoad, DiskSpace, MemoryUse, StationResources};
use tokio::sync::Mutex;

use crate::manager::policy::Policy;

/// The load of each live session's process tree (R13 §10).
#[path = "resources_sessions.rs"]
pub mod resources_sessions;

/// How long a gather result stays valid.
const CACHE_TTL: Duration = Duration::from_secs(30);
/// The time limit of one gather.
const GATHER_TIMEOUT: Duration = Duration::from_secs(3);
/// The time limit of one macOS command.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(2);
/// The most disks in a report.
const MAX_DISKS: usize = 8;

static DATA_DIR: OnceLock<PathBuf> = OnceLock::new();
static CACHE: Cache = Cache::new();

/// Sets the station data directory that the disk report measures (DL-193).
/// The first call wins.
pub fn set_data_dir(dir: PathBuf) {
    if let Err(kept) = DATA_DIR.set(dir) {
        tracing::debug!(dir = %kept.display(), "the data directory is already set");
    }
}

/// The resources of this host, from the cache or a new gather. `None` when
/// nothing could be measured or the gather took too long.
pub async fn current(policy: &Policy) -> Option<StationResources> {
    let data = DATA_DIR.get().cloned();
    let roots = policy.allowed_roots().to_vec();
    let mut found = CACHE.get(CACHE_TTL, || gather_timed(data, roots)).await?;
    // The session list has its own task, so it is not as old as the cache
    // (R13 §10).
    found.sessions = resources_sessions::latest();
    Some(found)
}

/// One cached value. The lock stays held during a gather, so a second call
/// waits and gets the same result (single flight).
struct Cache {
    slot: Mutex<Option<(Instant, StationResources)>>,
}

impl Cache {
    const fn new() -> Self {
        Self {
            slot: Mutex::const_new(None),
        }
    }

    /// A failed gather (`None`) is not cached.
    async fn get<F, Fut>(&self, ttl: Duration, gather: F) -> Option<StationResources>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Option<StationResources>>,
    {
        let mut slot = self.slot.lock().await;
        if let Some((at, value)) = slot.as_ref()
            && at.elapsed() < ttl
        {
            return Some(value.clone());
        }
        let fresh = gather().await;
        if let Some(value) = &fresh {
            *slot = Some((Instant::now(), value.clone()));
        }
        fresh
    }
}

async fn gather_timed(data: Option<PathBuf>, roots: Vec<PathBuf>) -> Option<StationResources> {
    let task = tokio::task::spawn_blocking(move || gather(data.as_deref(), &roots));
    match tokio::time::timeout(GATHER_TIMEOUT, task).await {
        Ok(Ok(found)) => found,
        Ok(Err(e)) => {
            tracing::warn!(error = %e, "station resources task");
            None
        }
        Err(_) => {
            tracing::warn!("station resources took too long");
            None
        }
    }
}

fn gather(data: Option<&Path>, roots: &[PathBuf]) -> Option<StationResources> {
    let disks = disks(data, roots);
    let memory = memory();
    let cpu = cpu();
    if disks.is_empty() && memory.is_none() && cpu.is_none() {
        return None;
    }
    let at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_millis()).ok())
        .unwrap_or(0);
    Some(StationResources {
        at,
        disks,
        memory,
        cpu,
        sessions: Vec::new(),
    })
}

// --- disks ---

fn disks(data: Option<&Path>, roots: &[PathBuf]) -> Vec<DiskSpace> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    let data = data.map(|d| ("Data".to_owned(), d));
    let roots = roots
        .iter()
        .map(|r| (r.to_string_lossy().into_owned(), r.as_path()));
    for (label, path) in data.into_iter().chain(roots) {
        if out.len() >= MAX_DISKS {
            break;
        }
        let stat = match rustix::fs::statvfs(path) {
            Ok(stat) => stat,
            Err(e) => {
                tracing::debug!(error = %e, path = %path.display(), "statvfs");
                continue;
            }
        };
        if !seen.insert(stat.f_fsid) {
            continue;
        }
        out.push(DiskSpace {
            label,
            path: path.to_string_lossy().into_owned(),
            total_bytes: stat.f_blocks.saturating_mul(stat.f_frsize),
            free_bytes: stat.f_bavail.saturating_mul(stat.f_frsize),
        });
    }
    out
}

// --- memory and CPU ---

fn memory() -> Option<MemoryUse> {
    if cfg!(target_os = "macos") {
        let total = parse_memsize(&run("sysctl", &["-n", "hw.memsize"])?)?;
        let available = parse_vm_stat(&run("vm_stat", &[])?)?;
        return Some(MemoryUse {
            total_bytes: total,
            available_bytes: available,
        });
    }
    parse_meminfo(&std::fs::read_to_string("/proc/meminfo").ok()?)
}

fn cpu() -> Option<CpuLoad> {
    let cores = u32::try_from(std::thread::available_parallelism().ok()?.get()).ok()?;
    let (load1_x100, load5_x100) = if cfg!(target_os = "macos") {
        parse_vm_loadavg(&run("sysctl", &["-n", "vm.loadavg"])?)?
    } else {
        parse_loadavg(&std::fs::read_to_string("/proc/loadavg").ok()?)?
    };
    Some(CpuLoad {
        cores,
        load1_x100,
        load5_x100,
    })
}

/// Runs a command under `COMMAND_TIMEOUT`; `None` on any failure.
fn run(program: &str, args: &[&str]) -> Option<String> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => break,
            Ok(Some(_)) | Err(_) => return None,
            Ok(None) if start.elapsed() >= COMMAND_TIMEOUT => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
        }
    }
    let mut text = String::new();
    child.stdout.take()?.read_to_string(&mut text).ok()?;
    Some(text)
}

// --- parsers (compiled on every OS) ---

/// `MemTotal` and `MemAvailable` of `/proc/meminfo` (kB) in bytes.
fn parse_meminfo(text: &str) -> Option<MemoryUse> {
    let field = |name: &str| {
        text.lines().find_map(|line| {
            let rest = line.strip_prefix(name)?.strip_prefix(':')?;
            let kb: u64 = rest.trim().strip_suffix("kB")?.trim().parse().ok()?;
            kb.checked_mul(1024)
        })
    };
    Some(MemoryUse {
        total_bytes: field("MemTotal")?,
        available_bytes: field("MemAvailable")?,
    })
}

/// The first two numbers of `/proc/loadavg`, times 100.
fn parse_loadavg(text: &str) -> Option<(u32, u32)> {
    let mut it = text.split_whitespace();
    Some((load_x100(it.next()?)?, load_x100(it.next()?)?))
}

/// The first two numbers of `{ 1.23 1.10 0.98 }`, times 100.
fn parse_vm_loadavg(text: &str) -> Option<(u32, u32)> {
    let inner = text.trim().strip_prefix('{')?.strip_suffix('}')?;
    parse_loadavg(inner)
}

fn load_x100(word: &str) -> Option<u32> {
    let v: f64 = word.parse().ok()?;
    if !v.is_finite() || !(0.0..1e6).contains(&v) {
        return None;
    }
    Some((v * 100.0).round() as u32)
}

/// `sysctl -n hw.memsize`: the bytes.
fn parse_memsize(text: &str) -> Option<u64> {
    text.trim().parse().ok()
}

/// `vm_stat`: (free + inactive + speculative) pages times the page size of
/// the first line. A missing page size gives `None`; a missing count is 0.
fn parse_vm_stat(text: &str) -> Option<u64> {
    let first = text.lines().next()?;
    let after = first.split("page size of").nth(1)?;
    let page: u64 = after.split_whitespace().next()?.parse().ok()?;
    let count = |name: &str| -> u64 {
        text.lines()
            .find_map(|line| {
                let rest = line.strip_prefix(name)?.strip_prefix(':')?;
                rest.trim().trim_end_matches('.').parse().ok()
            })
            .unwrap_or(0)
    };
    let pages = count("Pages free")
        .saturating_add(count("Pages inactive"))
        .saturating_add(count("Pages speculative"));
    Some(pages.saturating_mul(page))
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU32, Ordering};

    use super::*;

    const MEMINFO: &str = "MemTotal:       16384000 kB\nMemFree:         1000000 kB\nMemAvailable:    8192000 kB\nBuffers:           10 kB\n";

    #[test]
    fn meminfo_gives_total_and_available_in_bytes() {
        let m = parse_meminfo(MEMINFO).unwrap();
        assert_eq!(m.total_bytes, 16_384_000 * 1024);
        assert_eq!(m.available_bytes, 8_192_000 * 1024);
    }

    #[test]
    fn meminfo_without_an_available_line_is_none() {
        assert_eq!(parse_meminfo("MemTotal: 10 kB\n"), None);
        assert_eq!(parse_meminfo("MemTotal: x kB\nMemAvailable: 1 kB\n"), None);
        assert_eq!(parse_meminfo(""), None);
    }

    #[test]
    fn loadavg_gives_the_first_two_numbers_times_100() {
        assert_eq!(
            parse_loadavg("0.52 0.58 0.59 1/467 12345\n"),
            Some((52, 58))
        );
        assert_eq!(parse_loadavg("12.345 0.005 0 1/2 3"), Some((1235, 1)));
        assert_eq!(parse_loadavg("0.52"), None);
        assert_eq!(parse_loadavg("a b c"), None);
        assert_eq!(parse_loadavg("-1.0 0.5 0.5"), None);
        assert_eq!(parse_loadavg("NaN 0.5 0.5"), None);
    }

    #[test]
    fn vm_loadavg_reads_the_braces() {
        assert_eq!(parse_vm_loadavg("{ 1.23 1.10 0.98 }\n"), Some((123, 110)));
        assert_eq!(parse_vm_loadavg("1.23 1.10 0.98"), None);
        assert_eq!(parse_vm_loadavg("{ }"), None);
    }

    const VM_STAT: &str = "Mach Virtual Memory Statistics: (page size of 16384 bytes)\n\
Pages free:                               1000.\n\
Pages active:                             5000.\n\
Pages inactive:                           2000.\n\
Pages speculative:                         500.\n\
Pages wired down:                         4000.\n";

    #[test]
    fn vm_stat_sums_free_inactive_and_speculative_pages() {
        assert_eq!(parse_vm_stat(VM_STAT), Some(3500 * 16384));
    }

    #[test]
    fn vm_stat_without_a_page_size_is_none_and_a_missing_count_is_zero() {
        assert_eq!(parse_vm_stat("Pages free: 10.\n"), None);
        assert_eq!(parse_vm_stat(""), None);
        let no_spec =
            "Mach Virtual Memory Statistics: (page size of 4096 bytes)\nPages free: 10.\n";
        assert_eq!(parse_vm_stat(no_spec), Some(10 * 4096));
    }

    #[test]
    fn memsize_is_the_number() {
        assert_eq!(parse_memsize("17179869184\n"), Some(17_179_869_184));
        assert_eq!(parse_memsize("x"), None);
    }

    #[test]
    fn two_paths_on_one_filesystem_give_one_disk() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        let found = disks(Some(dir.path()), &[sub.clone(), dir.path().to_owned()]);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].label, "Data");
        assert!(found[0].total_bytes >= found[0].free_bytes);
        assert!(found[0].total_bytes > 0);
    }

    #[test]
    fn a_path_that_fails_is_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing");
        let found = disks(Some(&missing), std::slice::from_ref(&dir.path().to_owned()));
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].label, dir.path().to_string_lossy());
    }

    fn sample() -> StationResources {
        StationResources {
            at: 1,
            disks: Vec::new(),
            memory: None,
            cpu: None,
            sessions: Vec::new(),
        }
    }

    #[tokio::test]
    async fn two_calls_within_the_ttl_gather_once() {
        let cache = Cache::new();
        let n = AtomicU32::new(0);
        let gather = || async {
            n.fetch_add(1, Ordering::SeqCst);
            Some(sample())
        };
        assert_eq!(cache.get(CACHE_TTL, gather).await, Some(sample()));
        assert_eq!(cache.get(CACHE_TTL, gather).await, Some(sample()));
        assert_eq!(n.load(Ordering::SeqCst), 1);
        // An expired value gathers again.
        assert!(cache.get(Duration::ZERO, gather).await.is_some());
        assert_eq!(n.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn a_failed_gather_is_not_cached() {
        let cache = Cache::new();
        let n = AtomicU32::new(0);
        let gather = || async {
            n.fetch_add(1, Ordering::SeqCst);
            None
        };
        assert_eq!(cache.get(CACHE_TTL, gather).await, None);
        assert_eq!(cache.get(CACHE_TTL, gather).await, None);
        assert_eq!(n.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn a_second_call_during_a_gather_waits_for_it() {
        let cache = Cache::new();
        let n = AtomicU32::new(0);
        let gather = || async {
            n.fetch_add(1, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(50)).await;
            Some(sample())
        };
        let (a, b) = tokio::join!(cache.get(CACHE_TTL, gather), cache.get(CACHE_TTL, gather));
        assert_eq!(a, b);
        assert!(a.is_some());
        assert_eq!(n.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_gather_on_this_host_finds_a_disk() {
        let dir = tempfile::tempdir().unwrap();
        let found = gather(Some(dir.path()), &[]).unwrap();
        assert_eq!(found.disks.len(), 1);
        assert!(found.at > 0);
    }
}
