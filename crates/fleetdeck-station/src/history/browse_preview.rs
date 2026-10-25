//! The preview of one native session (B7, design §2.3), and the walk, the
//! budget and the one-permit runner that the list shares with it.
//!
//! The walk opens directories at a fixed depth with no link followed and
//! lists the transcript files of one provider home. The budget (directories,
//! entries, bytes, the deadline and the stop flag) is counted by [`Gauge`].

use std::fs::File;
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::fs::{FileExt as _, MetadataExt as _};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use fleetdeck_proto::command::check_native_session_id;
use fleetdeck_proto::event::ProviderKind;
use fleetdeck_proto::history::{HistoryItem, HistoryState, HistoryUnavailable, code};
use fleetdeck_proto::native::{NativePreview, NativePreviewQuery};
use fleetdeck_proto::subagent::MAX_ANSWER_BYTES;
use rustix::fs::{AtFlags, Dir, FileType, Mode};
use rustix::io::Errno;

use super::browse::HEAD_BYTES;
use super::lines::{Line, Lines, ReadError};
use super::locate::{self, Trust, resolve_home};
use super::parse::{self, Parsed};
use super::subagent::{Ring, state_of, trim};
use super::trust::{Identity, Mounts, PathCheck, rollout_id, trusted_home, trusted_path};
use super::{History, HistoryError, Limits, env_for};
use crate::manager::Policy;
use crate::store::Store;
use crate::workspace::PrivatePaths;
use crate::workspace::files::{descend, file_flags, open_root};

/// The time that the outer wait gives the worker after its deadline.
const GRACE: Duration = Duration::from_secs(2);

/// The budget of one request (design §2.2).
#[derive(Debug, Clone)]
pub struct Budget {
    pub dirs: usize,
    pub entries: usize,
    pub bytes: u64,
    pub deadline: Duration,
    /// How long a request waits for the shared permit.
    pub wait: Duration,
}

impl Default for Budget {
    fn default() -> Self {
        Self {
            dirs: 2_000,
            entries: 20_000,
            bytes: 64 * 1024 * 1024,
            deadline: Duration::from_secs(10),
            wait: Duration::from_secs(2),
        }
    }
}

/// Why a walk or a read stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Halt {
    /// The deadline passed, or the stop flag is set.
    Timed,
    /// A budget (directories, entries, bytes) is used up.
    Truncated,
    /// A directory that must be read cannot be opened or read.
    Unreadable,
}

/// The counters of one request.
pub(super) struct Gauge<'a> {
    budget: &'a Budget,
    deadline: Instant,
    stop: &'a AtomicBool,
    dirs: usize,
    entries: usize,
    bytes: u64,
}

impl<'a> Gauge<'a> {
    pub(super) fn new(budget: &'a Budget, deadline: Instant, stop: &'a AtomicBool) -> Self {
        Self {
            budget,
            deadline,
            stop,
            dirs: 0,
            entries: 0,
            bytes: 0,
        }
    }

    fn check(&self) -> Result<(), Halt> {
        if self.stop.load(Ordering::Relaxed) || Instant::now() >= self.deadline {
            Err(Halt::Timed)
        } else {
            Ok(())
        }
    }

    pub(super) fn dir(&mut self) -> Result<(), Halt> {
        self.check()?;
        self.dirs += 1;
        if self.dirs > self.budget.dirs {
            return Err(Halt::Truncated);
        }
        Ok(())
    }

    fn entry(&mut self) -> Result<(), Halt> {
        self.check()?;
        self.entries += 1;
        if self.entries > self.budget.entries {
            return Err(Halt::Truncated);
        }
        Ok(())
    }

    /// Charges `n` bytes that are about to be read.
    pub(super) fn bytes(&mut self, n: u64) -> Result<(), Halt> {
        self.check()?;
        if self.bytes.saturating_add(n) > self.budget.bytes {
            return Err(Halt::Truncated);
        }
        self.bytes += n;
        Ok(())
    }
}

// ----------------------------------------------------------------- walk

/// A transcript file found by the walk.
pub(super) struct Cand {
    pub home: usize,
    pub kind: ProviderKind,
    /// The directories below the home.
    pub dirs: Vec<String>,
    pub name: String,
    pub id: String,
    pub dev: u64,
    pub ino: u64,
    pub size: u64,
    pub mtime_ms: i64,
}

/// What the walk looks for.
pub(super) struct Walk<'a> {
    pub kind: ProviderKind,
    /// Only this native id (the preview).
    pub only: Option<&'a str>,
}

struct Listing {
    items: Vec<(String, FileType)>,
    halt: Option<Halt>,
}

/// The entries of a directory with their kinds, counted against the budget.
/// A stop in the middle keeps the entries read so far.
fn list_dir(dir: &OwnedFd, gauge: &mut Gauge<'_>) -> Result<Listing, Halt> {
    let reader = Dir::read_from(dir.as_fd()).map_err(|_| Halt::Unreadable)?;
    let mut items = Vec::new();
    for entry in reader {
        let entry = entry.map_err(|_| Halt::Unreadable)?;
        let name = entry.file_name();
        if name == c"." || name == c".." {
            continue;
        }
        if let Err(halt) = gauge.entry() {
            return Ok(Listing {
                items,
                halt: Some(halt),
            });
        }
        if let Ok(name) = name.to_str() {
            items.push((name.to_owned(), entry.file_type()));
        }
    }
    Ok(Listing { items, halt: None })
}

#[allow(clippy::unnecessary_cast)]
fn ms_of(sec: i64, nsec: u64) -> i64 {
    sec.saturating_mul(1000) + (nsec / 1_000_000) as i64
}

/// The metadata of a regular file by name, without following a link.
#[allow(clippy::unnecessary_cast)]
fn stat_regular(dir: &OwnedFd, name: &str) -> Option<(u64, u64, u64, i64)> {
    let st = rustix::fs::statat(dir, name, AtFlags::SYMLINK_NOFOLLOW).ok()?;
    if FileType::from_raw_mode(st.st_mode) != FileType::RegularFile {
        return None;
    }
    Some((
        st.st_dev as u64,
        st.st_ino as u64,
        u64::try_from(st.st_size).unwrap_or(0),
        ms_of(st.st_mtime as i64, st.st_mtime_nsec as u64),
    ))
}

fn file_id(kind: ProviderKind, name: &str) -> Option<String> {
    match kind {
        ProviderKind::Claude => name
            .strip_suffix(".jsonl")
            .filter(|id| check_native_session_id(id).is_ok())
            .map(str::to_owned),
        _ => rollout_id(name).map(str::to_owned),
    }
}

/// Walks `depth` levels of directories below `dir`, then reads the
/// transcript files of the last level. Only directories and regular files
/// are followed; a link is never opened.
pub(super) fn walk_level(
    dir: &OwnedFd,
    trail: &mut Vec<String>,
    depth: u8,
    gauge: &mut Gauge<'_>,
    walk: &Walk<'_>,
    home: usize,
    out: &mut Vec<Cand>,
) -> Result<(), Halt> {
    let listing = list_dir(dir, gauge)?;
    for (name, file_type) in &listing.items {
        if depth > 0 {
            let kind = if *file_type == FileType::Unknown {
                rustix::fs::statat(dir, name.as_str(), AtFlags::SYMLINK_NOFOLLOW)
                    .map_or(FileType::Unknown, |st| FileType::from_raw_mode(st.st_mode))
            } else {
                *file_type
            };
            if kind != FileType::Directory {
                continue;
            }
            let Ok(child) = descend(dir, &[name.as_str()]) else {
                tracing::debug!("native browse: a directory cannot be opened");
                continue;
            };
            gauge.dir()?;
            trail.push(name.clone());
            let result = walk_level(&child, trail, depth - 1, gauge, walk, home, out);
            trail.pop();
            match result {
                Ok(()) => {}
                Err(Halt::Unreadable) => {
                    tracing::debug!("native browse: a directory is unreadable")
                }
                Err(halt) => return Err(halt),
            }
        } else {
            let Some(id) = file_id(walk.kind, name) else {
                continue;
            };
            if walk.only.is_some_and(|only| only != id) {
                continue;
            }
            let Some((dev, ino, size, mtime_ms)) = stat_regular(dir, name) else {
                continue;
            };
            out.push(Cand {
                home,
                kind: walk.kind,
                dirs: trail.clone(),
                name: name.clone(),
                id,
                dev,
                ino,
                size,
                mtime_ms,
            });
        }
    }
    listing.halt.map_or(Ok(()), Err)
}

/// Walks one home: Claude `projects/*/*.jsonl`, Codex
/// `sessions/*/*/*/rollout-*.jsonl` and `archived_sessions/rollout-*.jsonl`.
/// A missing top directory is nothing to walk.
pub(super) fn walk_home(
    root: &OwnedFd,
    gauge: &mut Gauge<'_>,
    walk: &Walk<'_>,
    home: usize,
    out: &mut Vec<Cand>,
) -> Result<(), Halt> {
    let parts: &[(&str, u8)] = match walk.kind {
        ProviderKind::Claude => &[("projects", 1)],
        ProviderKind::Codex => &[("sessions", 3), ("archived_sessions", 0)],
        ProviderKind::Unknown => &[],
    };
    for (top, depth) in parts {
        let dir = match descend(root, &[top]) {
            Ok(dir) => dir,
            Err(Errno::NOENT) => continue,
            Err(_) => return Err(Halt::Unreadable),
        };
        gauge.dir()?;
        walk_level(
            &dir,
            &mut vec![(*top).to_owned()],
            *depth,
            gauge,
            walk,
            home,
            out,
        )?;
    }
    Ok(())
}

// --------------------------------------------------------------- windows

/// Reads the records of `[start, end)` of `file`, from the first whole line.
/// A line without its newline is not a record yet and is left out. `on`
/// gets the offset and the line, and returns false to stop.
pub(super) fn scan(
    file: &File,
    start: u64,
    end: u64,
    limits: &Limits,
    deadline: Instant,
    mut on: impl FnMut(u64, Line<'_>) -> bool,
) -> Result<(), ReadError> {
    let mut skip = false;
    if start > 0 {
        let mut previous = [0_u8; 1];
        file.read_exact_at(&mut previous, start - 1)?;
        skip = previous[0] != b'\n';
    }
    let mut lines = Lines::new(file, start, end, limits.line_max).with_deadline(deadline);
    while let Some((span, line)) = lines.next_line()? {
        if std::mem::take(&mut skip) {
            continue;
        }
        let line = match line {
            Line::Record(bytes) if span.len <= bytes.len() as u64 => continue,
            other => other,
        };
        if !on(span.offset, line) {
            break;
        }
    }
    Ok(())
}

pub(super) fn open_cand(root: &OwnedFd, cand: &Cand) -> Result<File, Errno> {
    let dirs: Vec<&str> = cand.dirs.iter().map(String::as_str).collect();
    let dir = descend(root, &dirs)?;
    let fd = rustix::fs::openat(&dir, cand.name.as_str(), file_flags(), Mode::empty())?;
    Ok(File::from(fd))
}

pub(super) fn mtime_ms(meta: &std::fs::Metadata) -> i64 {
    ms_of(meta.mtime(), u64::try_from(meta.mtime_nsec()).unwrap_or(0))
}

/// Runs `work` on the blocking pool under the shared permit (a second
/// request waits `budget.wait`, then `busy`). The deadline starts now. The
/// permit is released when the worker returns.
pub(super) async fn run_job<T: Send + 'static>(
    history: &History,
    budget: &Budget,
    work: impl FnOnce(Instant, &AtomicBool) -> T + Send + 'static,
) -> Result<T, HistoryError> {
    let deadline = Instant::now() + budget.deadline;
    let permit = match tokio::time::timeout(
        budget.wait,
        Arc::clone(&history.shared.budget).acquire_owned(),
    )
    .await
    {
        Ok(Ok(permit)) => permit,
        _ => return Err(busy("another history request is running")),
    };
    let stop = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&stop);
    let task = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        work(deadline, &flag)
    });
    let until = tokio::time::Instant::from_std(deadline + GRACE);
    match tokio::time::timeout_at(until, task).await {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(e)) => {
            tracing::warn!(error = %e, "native browse worker");
            Err(HistoryError::new(
                "internal",
                "the native browse worker failed",
            ))
        }
        Err(_) => {
            stop.store(true, Ordering::Relaxed);
            Err(busy("the native browse took too long"))
        }
    }
}

pub(super) fn busy(why: &str) -> HistoryError {
    HistoryError::new(code::BUSY, why)
}

pub(super) fn invalid(why: &str) -> HistoryError {
    HistoryError::new(code::INVALID_PARAMS, why)
}

// -------------------------------------------------------------- preview

/// The tail window of a preview.
const PREVIEW_TAIL: u64 = 4 * 1024 * 1024;
/// The room of the preview answer around its items, without the `cwd`.
const ENVELOPE: usize = 128;

/// Why a preview has no items.
struct Gone {
    reason: HistoryUnavailable,
    /// `(size, modified_at)` of the file, when one was opened.
    opened: Option<(u64, i64)>,
}

enum Stop {
    Gone(Gone),
    Halt(Halt),
}

fn gone(reason: HistoryUnavailable) -> Stop {
    Stop::Gone(Gone {
        reason,
        opened: None,
    })
}

struct PreviewJob {
    query: NativePreviewQuery,
    kind: ProviderKind,
    home: PathBuf,
    policy_private: Arc<PrivatePaths>,
    uid: u32,
    limits: Limits,
    budget: Budget,
    mounts: Option<Mounts>,
}

/// `station.native_preview`: the newest items of the pinned transcript,
/// without importing it.
///
/// # Errors
/// [`HistoryError`] with `invalid_params`, `busy` or `internal`. A transcript
/// that cannot be shown is an `unavailable` answer.
pub async fn native_preview(
    store: &Store,
    policy: &Arc<Policy>,
    private: &Arc<PrivatePaths>,
    query: NativePreviewQuery,
    budget: Budget,
) -> Result<NativePreview, HistoryError> {
    run_preview(store, policy, private, query, budget, None).await
}

/// [`native_preview`] with a mount table of the caller (tests).
pub(super) async fn run_preview(
    store: &Store,
    policy: &Arc<Policy>,
    private: &Arc<PrivatePaths>,
    query: NativePreviewQuery,
    budget: Budget,
    mounts: Option<Mounts>,
) -> Result<NativePreview, HistoryError> {
    check_native_session_id(&query.native_id).map_err(invalid)?;
    let kind = policy
        .instance(&query.provider_instance)
        .filter(|k| *k != ProviderKind::Unknown)
        .ok_or_else(|| invalid("unknown provider instance"))?;
    let env = env_for(Some(policy), &query.provider_instance);
    let Some(home) = resolve_home(&env, kind).filter(|h| h.is_absolute()) else {
        return Ok(answer_gone(&Gone {
            reason: HistoryUnavailable::NotFound,
            opened: None,
        }));
    };
    let history = store.history();
    let job = PreviewJob {
        query,
        kind,
        home,
        policy_private: Arc::clone(private),
        uid: history.uid(),
        limits: history.limits().clone(),
        budget: budget.clone(),
        mounts,
    };
    let worked = run_job(history, &budget, move |deadline, stop| {
        preview_blocking(&job, deadline, stop)
    })
    .await?;
    match worked {
        Ok(preview) => Ok(preview),
        Err(Stop::Gone(gone)) => Ok(answer_gone(&gone)),
        Err(Stop::Halt(halt)) => {
            tracing::debug!(?halt, "native preview stopped");
            Err(busy("the preview took too long"))
        }
    }
}

fn answer_gone(gone: &Gone) -> NativePreview {
    let (size, modified_at) = gone.opened.unwrap_or((0, 0));
    NativePreview {
        items: Vec::new(),
        state: HistoryState::Unavailable {
            reason: gone.reason,
        },
        cwd: String::new(),
        size,
        modified_at,
        earlier_omitted: false,
    }
}

fn preview_blocking(
    job: &PreviewJob,
    deadline: Instant,
    stop: &AtomicBool,
) -> Result<NativePreview, Stop> {
    use HistoryUnavailable::{NotFound, NotTrusted, TooLarge};
    let mounts = job.mounts.clone().unwrap_or_else(Mounts::load);
    let pin = &job.query.pin;
    // The home resolves through links of the station user or root only
    // (B7 PRE-PR r1). `home_not_found`, `not_readable`: the home is gone.
    let (home, checked) = match trusted_home(&job.home, job.uid, &mounts) {
        Ok(home) => (home, true),
        Err(PathCheck::Untrusted("missing" | "io")) => return Err(gone(NotFound)),
        // `home_not_trusted`.
        Err(PathCheck::Untrusted(_)) => return Err(gone(NotTrusted)),
        Err(PathCheck::Unverifiable(_)) => (
            std::fs::canonicalize(&job.home).map_err(|_| gone(NotFound))?,
            false,
        ),
    };
    // `home_changed`.
    if home.to_string_lossy() != pin.home.as_str() {
        return Err(gone(NotFound));
    }
    if !checked {
        match trusted_path(&home, job.uid, &mounts) {
            Ok(()) | Err(PathCheck::Unverifiable(_)) => {}
            // `home_not_trusted`.
            Err(PathCheck::Untrusted(_)) => return Err(gone(NotTrusted)),
        }
    }
    let root = open_root(&home).map_err(|_| gone(NotFound))?;
    let mut gauge = Gauge::new(&job.budget, deadline, stop);
    let mut cands: Vec<Cand> = Vec::new();
    let walk = Walk {
        kind: job.kind,
        only: Some(&job.query.native_id),
    };
    match walk_home(&root, &mut gauge, &walk, 0, &mut cands) {
        Ok(()) => {}
        Err(Halt::Unreadable) => return Err(gone(NotFound)),
        Err(halt) => return Err(Stop::Halt(halt)),
    }
    // A Codex id in two files is a contradiction.
    if job.kind == ProviderKind::Codex && cands.len() > 1 {
        return Err(gone(NotTrusted));
    }
    // The file whose `(dev, ino)` is the pin: `transcript_changed` otherwise.
    let mut found = None;
    for cand in cands {
        let Ok(file) = open_cand(&root, &cand) else {
            continue;
        };
        let Ok(meta) = file.metadata() else { continue };
        if meta.dev() == pin.dev && meta.ino() == pin.ino {
            found = Some((cand, file, meta));
            break;
        }
    }
    let (cand, file, meta) = found.ok_or_else(|| gone(NotFound))?;
    let (size, modified_at) = (meta.len(), mtime_ms(&meta));
    let opened = Some((size, modified_at));
    let fail = |reason| Stop::Gone(Gone { reason, opened });
    let trust = Trust {
        private: &job.policy_private,
        uid: job.uid,
        limits: &job.limits,
        deadline,
    };
    if locate::check_meta(&file, &trust, u64::MAX).is_err() {
        return Err(fail(NotTrusted));
    }
    if size > job.limits.file_max {
        return Err(fail(TooLarge));
    }
    let path = home
        .join(cand.dirs.iter().collect::<PathBuf>())
        .join(&cand.name);
    if let Err(PathCheck::Untrusted(_)) = trusted_path(&path, job.uid, &mounts) {
        return Err(fail(NotTrusted));
    }
    let head_end = size.min(HEAD_BYTES);
    let tail_start = size.saturating_sub(PREVIEW_TAIL);
    gauge
        .bytes(head_end + (size - tail_start))
        .map_err(Stop::Halt)?;
    let read = |e: ReadError| match e {
        ReadError::Deadline => Stop::Halt(Halt::Timed),
        ReadError::Io(_) => fail(NotFound),
    };
    let dir_name = (cand.kind == ProviderKind::Claude)
        .then(|| cand.dirs.get(1).map(String::as_str))
        .flatten();
    let mut identity = Identity::new(cand.kind, &cand.id, dir_name);
    let mut bad = false;
    scan(&file, 0, head_end, &job.limits, deadline, |offset, line| {
        match line {
            Line::TooLong => bad = offset == 0 && cand.kind == ProviderKind::Codex,
            Line::Record(bytes) => bad = identity.record(bytes).is_err(),
        }
        !bad
    })
    .map_err(read)?;
    let mut ring = Ring::default();
    if !bad {
        scan(
            &file,
            tail_start,
            size,
            &job.limits,
            deadline,
            |offset, line| {
                match line {
                    Line::TooLong => ring.skipped += 1,
                    Line::Record(bytes) => {
                        if identity.record(bytes).is_err() {
                            bad = true;
                            return false;
                        }
                        match parse::parse_line(cand.kind, bytes) {
                            Parsed::Ignored => {}
                            Parsed::Unknown => ring.skipped += 1,
                            Parsed::Known(record) => ring.record(record, offset),
                        }
                    }
                }
                true
            },
        )
        .map_err(read)?;
    }
    // A contradiction discards everything that was collected.
    let found = match identity.finish() {
        Ok(found) if !bad => found,
        _ => return Err(fail(NotTrusted)),
    };
    let make = |items: Vec<HistoryItem>, state, earlier_omitted| NativePreview {
        items,
        state,
        cwd: found.cwd.clone(),
        size,
        modified_at,
        earlier_omitted,
    };
    let trimmed = trim(
        ring,
        MAX_ANSWER_BYTES,
        ENVELOPE + found.cwd.len(),
        |items| {
            let all = make(items.to_vec(), HistoryState::Complete, true);
            serde_json::to_vec(&all).map_or(0, |v| v.len())
        },
    );
    let state = state_of(
        trimmed.items.is_empty(),
        trimmed.dropped,
        trimmed.skipped,
        trimmed.recognized,
        size - tail_start,
    );
    let earlier_omitted = tail_start > 0 || trimmed.dropped > 0;
    Ok(make(trimmed.items, state, earlier_omitted))
}
