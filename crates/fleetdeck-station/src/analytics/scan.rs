//! The transcript scan (B29, design section 3.5): the homes and their
//! trust, the walk, the Codex ancestry, the candidates, the read of each
//! file and the schedule. The counters of the Fleet Deck events (section
//! 3.6) are driven from here first: no pass starts before the event cursor
//! is caught up.

use std::collections::HashMap;
use std::fs::File;
use std::future::Future;
use std::os::fd::OwnedFd;
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::{FileExt as _, MetadataExt as _};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use fleetdeck_proto::analytics::SourceState;
use fleetdeck_proto::event::ProviderKind;
use fleetdeck_proto::native::HomeState;
use ring::digest;
use rustix::fs::Mode;
use tokio::sync::watch;

use super::codex::CodexState;
use super::walk::{Found, Walked, walk_home};
use super::{HomeStatus, Parsed, ScanStatus, UsageRecord, claude, codex};
use crate::history::browse::{HomePlan, Resolved, plan_homes, resolve_plan};
use crate::history::lines::{Line, Lines, ReadError};
use crate::history::trust::Mounts;
use crate::manager::SessionManager;
use crate::store::analytics::{FileCommit, FileRow};
use crate::store::{Store, StoreError};
use crate::workspace::files::open_root;
use crate::workspace::files::{descend, file_flags};

/// The most bytes that one pass reads.
pub const PASS_BYTES: u64 = 2 * 1024 * 1024 * 1024;
/// The longest time of one pass.
pub const PASS_TIME: Duration = Duration::from_secs(5 * 60);
/// The wait between two passes.
pub const PASS_EVERY: Duration = Duration::from_secs(10 * 60);
/// The wait after a pass that stopped with bytes left.
pub const PASS_MORE: Duration = Duration::from_secs(30);
/// The wait after a failed step of the event cursor.
pub const CATCH_UP_RETRY: Duration = Duration::from_secs(30);
/// The events that one step of the event cursor counts.
const CATCH_UP_LIMIT: u32 = 2_000;
/// The longest line that is read; a longer one is malformed.
const LINE_MAX: usize = 16 * 1024 * 1024;
/// The bytes before an offset that the resume guard hashes.
const GUARD_BYTES: u64 = 64;
/// `cleanupPeriodDays` when the home does not set it.
const KEEPS_DEFAULT: u32 = 30;
/// The largest `settings.json` that is read.
const SETTINGS_MAX: u64 = 1024 * 1024;

/// The resolver of a home plan (the browse's `resolve_plan`; a test stub).
pub(crate) type Resolver = Arc<dyn Fn(&HomePlan) -> Resolved + Send + Sync>;

/// The limits and the knob of one pass.
#[derive(Debug, Clone)]
pub struct PassConfig {
    /// `analytics.transcripts`; false: nothing is read.
    pub enabled: bool,
    pub bytes: u64,
    pub time: Duration,
}

impl Default for PassConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            bytes: PASS_BYTES,
            time: PASS_TIME,
        }
    }
}

/// What a pass left to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PassOutcome {
    /// The pass stopped at its budget with bytes left.
    pub more: bool,
}

/// Why a pass or a step failed. The text names the kind, never a path.
#[derive(Debug, thiserror::Error)]
pub enum ScanError {
    #[error("store")]
    Store(#[from] StoreError),
    #[error("task")]
    Task,
}

impl ScanError {
    fn kind(&self) -> &'static str {
        match self {
            Self::Store(StoreError::Db(_)) => "database",
            Self::Store(StoreError::Json(_)) => "json",
            Self::Store(_) => "store",
            Self::Task => "task",
        }
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

fn bits(v: u64) -> i64 {
    i64::from_ne_bytes(v.to_ne_bytes())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// `AnalyticsSource.key`: 16 hex characters of the SHA-256 of the machine
/// id, a NUL and the canonical home path.
#[must_use]
pub fn source_key(machine_id: &str, canon: &Path) -> String {
    let mut ctx = digest::Context::new(&digest::SHA256);
    ctx.update(machine_id.as_bytes());
    ctx.update(&[0]);
    ctx.update(canon.as_os_str().as_bytes());
    hex(&ctx.finish().as_ref()[..8])
}

pub(crate) fn machine_id() -> String {
    std::fs::read_to_string("/etc/machine-id")
        .map(|s| s.trim().to_owned())
        .unwrap_or_default()
}

// ------------------------------------------------------------------ homes

/// One home after the trust policy and the walk.
struct Prep {
    provider: ProviderKind,
    key: String,
    state: SourceState,
    verified: bool,
    truncated: bool,
    keeps_days: Option<u32>,
    /// Set when the home is read.
    read: Option<(Arc<OwnedFd>, Walked)>,
}

/// `cleanupPeriodDays` of `<home>/settings.json`: an integer 1 to 36,500,
/// else 30.
fn keeps_days(root: &OwnedFd) -> u32 {
    let Ok(fd) = rustix::fs::openat(root, "settings.json", file_flags(), Mode::empty()) else {
        return KEEPS_DEFAULT;
    };
    let file = File::from(fd);
    let Ok(meta) = file.metadata() else {
        return KEEPS_DEFAULT;
    };
    if !meta.is_file() || meta.len() > SETTINGS_MAX {
        return KEEPS_DEFAULT;
    }
    let mut bytes = vec![0_u8; usize::try_from(meta.len()).unwrap_or(0)];
    if file.read_exact_at(&mut bytes, 0).is_err() {
        return KEEPS_DEFAULT;
    }
    serde_json::from_slice::<serde_json::Value>(&bytes)
        .ok()
        .and_then(|v| {
            v.get("cleanupPeriodDays")
                .and_then(serde_json::Value::as_u64)
        })
        .filter(|days| (1..=36_500).contains(days))
        .and_then(|days| u32::try_from(days).ok())
        .unwrap_or(KEEPS_DEFAULT)
}

/// Resolves the homes with the browse's trust policy and walks the ones
/// that are read (design section 3.5 steps 1 and 2).
fn prepare(plans: &[HomePlan], resolve: &Resolver, enabled: bool) -> Vec<Prep> {
    let machine = machine_id();
    let mut out: Vec<Prep> = Vec::new();
    for plan in plans {
        let Resolved {
            canon,
            state,
            exists,
            verified,
        } = resolve(plan);
        let Some(canon) = canon else {
            continue;
        };
        let key = source_key(&machine, &canon);
        if out.iter().any(|p| p.provider == plan.kind && p.key == key) {
            continue;
        }
        let mut prep = Prep {
            provider: plan.kind,
            key,
            state: SourceState::Missing,
            verified,
            truncated: false,
            keeps_days: None,
            read: None,
        };
        match state {
            _ if !enabled => prep.state = SourceState::Off,
            HomeState::Unavailable { reason } => prep.state = SourceState::Unavailable { reason },
            HomeState::Ok if !exists => {}
            HomeState::Ok => match open_root(&canon) {
                Err(rustix::io::Errno::NOENT) => {}
                Err(_) => {
                    prep.state = SourceState::Unavailable {
                        reason: "not_readable".to_owned(),
                    };
                }
                Ok(root) => {
                    prep.state = SourceState::Ok;
                    if plan.kind == ProviderKind::Claude {
                        prep.keeps_days = Some(keeps_days(&root));
                    }
                    let walked = walk_home(&root, plan.kind);
                    if walked.truncated {
                        tracing::warn!("analytics walk truncated");
                        prep.truncated = true;
                        prep.state = SourceState::Unavailable {
                            reason: "too_many_files".to_owned(),
                        };
                    }
                    prep.read = Some((Arc::new(root), walked));
                }
            },
        }
        out.push(prep);
    }
    out
}

// ------------------------------------------------------------------ files

/// Opens a transcript below the home root without following a link: a
/// regular file of the station user, else `None`.
fn open_file(root: &OwnedFd, rel: &str, uid: u32) -> Option<(File, std::fs::Metadata)> {
    let parts: Vec<&str> = rel.split('/').collect();
    let (name, dirs) = parts.split_last()?;
    let dir = descend(root, dirs).ok()?;
    let fd = rustix::fs::openat(&dir, *name, file_flags(), Mode::empty()).ok()?;
    let file = File::from(fd);
    let meta = file.metadata().ok()?;
    (meta.is_file() && meta.uid() == uid).then_some((file, meta))
}

fn mtime_ns(meta: &std::fs::Metadata) -> i64 {
    meta.mtime()
        .saturating_mul(1_000_000_000)
        .saturating_add(meta.mtime_nsec())
}

/// A file is a candidate when it has no row, when its `dev`, `ino`, size
/// or mtime differs from the row, or when an earlier pass left bytes of it.
fn is_candidate(found: &Found, row: Option<&FileRow>) -> bool {
    let Some(row) = row else {
        return true;
    };
    row.dev != bits(found.dev)
        || row.ino != bits(found.ino)
        || row.seen_size != found.size
        || row.mtime_ns != found.mtime_ns
        || row.offset < found.size
}

/// The SHA-256 (hex) of the up to 64 bytes before `offset`; empty at 0.
fn guard_of(file: &File, offset: u64) -> Option<String> {
    if offset == 0 {
        return Some(String::new());
    }
    let from = offset.saturating_sub(GUARD_BYTES);
    let mut bytes = vec![0_u8; usize::try_from(offset - from).ok()?];
    file.read_exact_at(&mut bytes, from).ok()?;
    Some(hex(digest::digest(&digest::SHA256, &bytes).as_ref()))
}

fn ends_with_newline(file: &File, end: u64) -> bool {
    let mut last = [0_u8; 1];
    end > 0 && file.read_exact_at(&mut last, end - 1).is_ok() && last[0] == b'\n'
}

/// The thread of a Codex rollout from its file name (`rollout-<stamp>-<id>`).
fn thread_of(rel: &str) -> Option<&str> {
    let name = rel.rsplit('/').next()?;
    let rest = name.strip_prefix("rollout-")?.strip_suffix(".jsonl")?;
    rest.get(20..).filter(|id| !id.is_empty())
}

/// The first record of a rollout (one `Lines` call).
fn first_record(root: &OwnedFd, rel: &str, uid: u32) -> Option<Vec<u8>> {
    let (file, meta) = open_file(root, rel, uid)?;
    let mut lines = Lines::new(&file, 0, meta.len(), LINE_MAX);
    match lines.next_line().ok()?? {
        (_, Line::Record(bytes)) => Some(bytes.to_vec()),
        (_, Line::TooLong) => None,
    }
}

/// What one file read made.
struct FileRead {
    commit: FileCommit,
    /// Bytes left on disk after the new offset.
    pending: u64,
    read_bytes: u64,
    /// The read stopped at the byte budget before the end of the file.
    clipped: bool,
    /// The deadline of the pass ended the read.
    timed_out: bool,
}

/// Reads the new whole lines of one candidate (design section 3.5 step 4).
/// `None`: the file cannot be used (no row is written).
#[allow(clippy::too_many_arguments)]
fn read_file(
    root: &OwnedFd,
    home: &str,
    provider: ProviderKind,
    found: &Found,
    row: Option<&FileRow>,
    uid: u32,
    budget: u64,
    deadline: Instant,
) -> Option<FileRead> {
    let (file, meta) = open_file(root, &found.rel, uid)?;
    let (size, mtime) = (meta.len(), mtime_ns(&meta));
    let (dev, ino) = (bits(meta.dev()), bits(meta.ino()));
    let resume = row.filter(|r| {
        r.dev == dev
            && r.ino == ino
            && size >= r.offset
            && !(r.mtime_ns != mtime && size == r.seen_size)
            && guard_of(&file, r.offset).as_deref() == Some(r.guard.as_str())
    });
    let (start, mut state, mut malformed) = match resume {
        Some(r) => match serde_json::from_str::<CodexState>(&r.state) {
            Ok(state) => (r.offset, state, r.malformed),
            Err(_) => (0, CodexState::default(), 0),
        },
        None => (0, CodexState::default(), 0),
    };
    let end = start + (size - start).min(budget);
    let mut lines = Lines::new(&file, start, end, LINE_MAX).with_deadline(deadline);
    let mut offset = start;
    let mut records: Vec<UsageRecord> = Vec::new();
    let mut timed_out = false;
    loop {
        match lines.next_line() {
            Ok(None) => break,
            Ok(Some((span, line))) => {
                let after = span.offset + span.len;
                let whole = match &line {
                    Line::Record(bytes) => span.len > bytes.len() as u64,
                    Line::TooLong => ends_with_newline(&file, after),
                };
                if !whole {
                    break;
                }
                let parsed = match line {
                    Line::TooLong => Parsed::Malformed,
                    Line::Record(bytes) if provider == ProviderKind::Codex => {
                        codex::parse_line(bytes, &mut state)
                    }
                    Line::Record(bytes) => claude::parse_line(bytes),
                };
                match parsed {
                    Parsed::Record(record) => records.push(record),
                    Parsed::Malformed => malformed += 1,
                    Parsed::Nothing => {}
                }
                offset = after;
            }
            Err(ReadError::Deadline) => {
                timed_out = true;
                break;
            }
            Err(ReadError::Io(_)) => return None,
        }
    }
    let state_text = if provider == ProviderKind::Codex {
        serde_json::to_string(&state).ok()?
    } else {
        "{}".to_owned()
    };
    Some(FileRead {
        commit: FileCommit {
            file: FileRow {
                home: home.to_owned(),
                rel_path: found.rel.clone(),
                provider,
                dev,
                ino,
                mtime_ns: mtime,
                offset,
                guard: guard_of(&file, offset)?,
                state: state_text,
                seen_size: size,
                malformed,
                scanned_at: now_ms(),
            },
            records,
        },
        pending: size.saturating_sub(offset),
        read_bytes: offset - start,
        clipped: end < size,
        timed_out,
    })
}

// ------------------------------------------------------------------- pass

/// One pass: the homes, the walk, the Codex ancestry, then the read of each
/// candidate (design section 3.5). The status is replaced at its end.
///
/// # Errors
/// A store error or a failed blocking task.
pub(crate) async fn pass(
    store: &Store,
    plans: Vec<HomePlan>,
    resolve: Resolver,
    uid: u32,
    config: &PassConfig,
    status: &ScanStatus,
    stop: &watch::Receiver<bool>,
) -> Result<PassOutcome, ScanError> {
    let enabled = config.enabled;
    let preps = tokio::task::spawn_blocking(move || prepare(&plans, &resolve, enabled))
        .await
        .map_err(|_| ScanError::Task)?;
    if !enabled {
        status.set(preps.iter().map(|p| status_of(p, 0)).collect());
        return Ok(PassOutcome { more: false });
    }
    // Candidates of every home, and the Codex ancestry before any usage.
    let mut todo: Vec<Vec<(Found, Option<FileRow>)>> = Vec::new();
    let mut pending: Vec<u64> = Vec::new();
    for prep in &preps {
        let Some((root, walked)) = &prep.read else {
            todo.push(Vec::new());
            pending.push(0);
            continue;
        };
        let rows: HashMap<String, FileRow> = store
            .analytics_files(prep.key.clone())
            .await?
            .into_iter()
            .map(|r| (r.rel_path.clone(), r))
            .collect();
        let mut candidates = Vec::new();
        let mut idle = 0_u64;
        for found in &walked.files {
            let row = rows.get(&found.rel);
            if is_candidate(found, row) {
                candidates.push((found.clone(), row.cloned()));
            } else {
                idle += found.size.saturating_sub(row.map_or(0, |r| r.offset));
            }
        }
        if prep.provider == ProviderKind::Codex {
            ancestry(store, prep, root, &candidates, uid).await?;
        }
        todo.push(candidates);
        pending.push(idle);
    }
    let started = Instant::now();
    let deadline = started + config.time;
    let mut remaining = config.bytes;
    let mut limited = false;
    'homes: for (i, prep) in preps.iter().enumerate() {
        let Some((root, _)) = &prep.read else {
            continue;
        };
        let mut left = std::mem::take(&mut todo[i]).into_iter();
        while let Some((found, row)) = left.next() {
            if *stop.borrow() || remaining == 0 || Instant::now() >= deadline {
                limited = !*stop.borrow();
                pending[i] += unread(&found, row.as_ref());
                for (f, r) in left {
                    pending[i] += unread(&f, r.as_ref());
                }
                break 'homes;
            }
            let (root, home, provider) = (Arc::clone(root), prep.key.clone(), prep.provider);
            let (job_found, job_row, budget) = (found.clone(), row.clone(), remaining);
            let read = tokio::task::spawn_blocking(move || {
                read_file(
                    &root,
                    &home,
                    provider,
                    &job_found,
                    job_row.as_ref(),
                    uid,
                    budget,
                    deadline,
                )
            })
            .await
            .map_err(|_| ScanError::Task)?;
            let Some(read) = read else {
                continue;
            };
            pending[i] += read.pending;
            remaining = if read.clipped {
                0
            } else {
                remaining.saturating_sub(read.read_bytes)
            };
            let report = store.analytics_commit_file(read.commit).await?;
            if report.ancestry_broken {
                tracing::warn!("analytics thread ancestry broken");
            }
            if read.timed_out {
                limited = true;
                for (f, r) in left {
                    pending[i] += unread(&f, r.as_ref());
                }
                break 'homes;
            }
        }
    }
    if limited {
        // The homes after the stop have candidates that no read touched.
        for (i, rest) in todo.iter().enumerate() {
            for (f, r) in rest {
                pending[i] += unread(f, r.as_ref());
            }
        }
    }
    let total: u64 = pending.iter().sum();
    status.set(
        preps
            .iter()
            .zip(&pending)
            .map(|(p, bytes)| status_of(p, *bytes))
            .collect(),
    );
    Ok(PassOutcome {
        more: limited && total > 0,
    })
}

/// The bytes of a candidate that no read touched.
fn unread(found: &Found, row: Option<&FileRow>) -> u64 {
    found.size.saturating_sub(row.map_or(0, |r| r.offset))
}

fn status_of(prep: &Prep, pending: u64) -> HomeStatus {
    let complete = match prep.state {
        SourceState::Ok | SourceState::Missing => pending == 0 && !prep.truncated,
        SourceState::Unavailable { .. } | SourceState::Off => false,
    };
    HomeStatus {
        provider: prep.provider,
        key: prep.key.clone(),
        state: prep.state.clone(),
        verified: prep.verified,
        complete,
        pending_bytes: pending,
        scanned_at: Some(now_ms()),
        keeps_days: prep.keeps_days,
    }
}

/// Fills `analytics_threads` from the first record of each Codex candidate
/// whose thread has no row, before any usage is read (design section 3.5
/// step 3).
async fn ancestry(
    store: &Store,
    prep: &Prep,
    root: &Arc<OwnedFd>,
    candidates: &[(Found, Option<FileRow>)],
    uid: u32,
) -> Result<(), ScanError> {
    let mut known = store.analytics_known_threads(prep.key.clone()).await?;
    for (found, _) in candidates {
        if thread_of(&found.rel).is_some_and(|id| known.contains(id)) {
            continue;
        }
        let (root, rel) = (Arc::clone(root), found.rel.clone());
        let first = tokio::task::spawn_blocking(move || first_record(&root, &rel, uid))
            .await
            .map_err(|_| ScanError::Task)?;
        let Some(meta) = first.and_then(|line| codex::parse_meta(&line)) else {
            continue;
        };
        store
            .analytics_put_thread(prep.key.clone(), meta.thread.clone(), meta.parent)
            .await?;
        known.insert(meta.thread);
    }
    Ok(())
}

// --------------------------------------------------------------- schedule

/// The waits of the schedule (a test shortens them).
#[derive(Debug, Clone, Copy)]
pub struct Timing {
    pub every: Duration,
    pub more: Duration,
    pub retry: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Self {
            every: PASS_EVERY,
            more: PASS_MORE,
            retry: CATCH_UP_RETRY,
        }
    }
}

/// Waits `time`; true when the station stops first.
async fn sleep_or_stop(shutdown: &mut watch::Receiver<bool>, time: Duration) -> bool {
    tokio::select! {
        () = tokio::time::sleep(time) => *shutdown.borrow(),
        changed = shutdown.changed() => changed.is_err() || *shutdown.borrow(),
    }
}

/// Runs `step` until it returns `false` (the event cursor is caught up). A
/// failed step logs and is retried after `retry`. False: the station
/// stopped first.
pub(crate) async fn catch_up<F>(
    mut step: impl FnMut() -> F,
    shutdown: &mut watch::Receiver<bool>,
    retry: Duration,
) -> bool
where
    F: Future<Output = Result<bool, ScanError>>,
{
    loop {
        if *shutdown.borrow() {
            return false;
        }
        match step().await {
            Ok(true) => {}
            Ok(false) => return true,
            Err(e) => {
                tracing::warn!(kind = e.kind(), "analytics catch-up failed");
                if sleep_or_stop(shutdown, retry).await {
                    return false;
                }
            }
        }
    }
}

/// The task of the scanner with its inputs injected.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_loop(
    store: Store,
    plans: impl Fn() -> Vec<HomePlan>,
    resolver: impl Fn() -> Resolver,
    uid: u32,
    config: PassConfig,
    status: Arc<ScanStatus>,
    mut shutdown: watch::Receiver<bool>,
    timing: Timing,
) {
    let step_store = store.clone();
    let caught = catch_up(
        || {
            let store = step_store.clone();
            async move {
                store
                    .analytics_catch_up_step(CATCH_UP_LIMIT)
                    .await
                    .map_err(ScanError::from)
            }
        },
        &mut shutdown,
        timing.retry,
    )
    .await;
    if !caught {
        return;
    }
    if let Err(e) = store.analytics_close_ended_runs().await {
        tracing::warn!(
            kind = ScanError::from(e).kind(),
            "analytics close of ended runs failed"
        );
    }
    if !config.enabled {
        // Every source says `off`; there is nothing to read.
        if let Err(e) = pass(
            &store,
            plans(),
            resolver(),
            uid,
            &config,
            &status,
            &shutdown,
        )
        .await
        {
            tracing::warn!(kind = e.kind(), "analytics scan failed");
        }
        while !*shutdown.borrow() && shutdown.changed().await.is_ok() {}
        return;
    }
    loop {
        let wait = match pass(
            &store,
            plans(),
            resolver(),
            uid,
            &config,
            &status,
            &shutdown,
        )
        .await
        {
            Ok(outcome) if outcome.more => timing.more,
            Ok(_) => timing.every,
            Err(e) => {
                tracing::warn!(kind = e.kind(), "analytics scan failed");
                timing.every
            }
        };
        if sleep_or_stop(&mut shutdown, wait).await {
            return;
        }
    }
}

/// The task of the scanner (spawned in `serve.rs`): the event cursor first,
/// then one pass at once and one every 10 minutes (30 seconds while bytes
/// are left). `enabled` is the knob `analytics.transcripts`.
pub async fn run(manager: Arc<SessionManager>, enabled: bool, shutdown: watch::Receiver<bool>) {
    let store = manager.store().clone();
    let uid = store.history().uid();
    let policy_manager = Arc::clone(&manager);
    let config = PassConfig {
        enabled,
        ..PassConfig::default()
    };
    run_loop(
        store,
        move || plan_homes(&policy_manager.policy()),
        move || {
            let mounts = Mounts::load();
            Arc::new(move |plan: &HomePlan| resolve_plan(plan, uid, &mounts))
        },
        uid,
        config,
        ScanStatus::shared(),
        shutdown,
        Timing::default(),
    )
    .await;
}
