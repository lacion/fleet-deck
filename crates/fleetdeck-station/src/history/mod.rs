//! The session history (DL-189, station part).
//!
//! The station keeps its own snapshot of the provider transcript of an
//! imported session and answers `session.history` from it:
//!
//! - [`locate`] finds the provider file and checks that it is trusted;
//! - [`capture`] copies a prefix of it into the station's data directory;
//! - [`parse`] reads one record; [`index`] scans a snapshot once and reads
//!   pages from it;
//! - this module owns the budget (one worker at a time, one deadline for a
//!   request), the cache of indexes, and the two entry points: [`at_import`]
//!   (called before an import resumes the conversation) and [`answer`].
//!
//! A history failure never fails an import. No transcript text is logged.

pub mod capture;
pub mod index;
pub mod lines;
pub mod locate;
pub mod parse;
pub mod subagent;

#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_capture;
#[cfg(test)]
mod tests_locate;
#[cfg(test)]
mod tests_page;
#[cfg(test)]
mod tests_subagent;
#[cfg(test)]
mod tests_support;

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use fleetdeck_core::ChildEnv;
use fleetdeck_proto::command::check_native_session_id;
use fleetdeck_proto::event::ProviderKind;
use fleetdeck_proto::history::{
    DEFAULT_LIMIT, HistoryState, HistoryUnavailable, MAX_LIMIT, SessionHistory,
    SessionHistoryQuery, code,
};
use fleetdeck_proto::{ProviderInstanceId, SessionId};
use tokio::sync::Semaphore;

use crate::manager::Policy;
use crate::store::Store;
use crate::store::transcripts::{Capture, FirstEffect, HistoryFacts, TranscriptRow};
use crate::workspace::PrivatePaths;

/// The limits of the history work.
#[derive(Debug, Clone)]
pub struct Limits {
    /// The longest record line that is read; a longer one is skipped.
    pub line_max: usize,
    /// The largest provider transcript.
    pub file_max: u64,
    /// The most items in an index; the newest stay.
    pub index_items: usize,
    /// The most indexes in memory.
    pub cache_entries: usize,
    /// The most memory of all indexes.
    pub cache_bytes: usize,
    /// The most directory entries that a lookup visits.
    pub dir_entries: usize,
    /// The most record bytes that one page reads.
    pub page_read_max: usize,
    /// The largest serialized answer.
    pub answer_max: usize,
    /// The time for one request, including the wait for the worker.
    pub deadline: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            line_max: 16 * 1024 * 1024,
            file_max: 256 * 1024 * 1024,
            index_items: 200_000,
            cache_entries: 4,
            cache_bytes: 32 * 1024 * 1024,
            dir_entries: 5_000,
            page_read_max: 1024 * 1024,
            answer_max: 512 * 1024,
            deadline: Duration::from_secs(10),
        }
    }
}

/// Why a piece of history work stopped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fail {
    /// The history is not available, for this reason.
    Unavailable(HistoryUnavailable),
    /// The deadline of the request passed.
    Busy,
    /// A file operation failed.
    Io,
    /// The provider wrote while the snapshot was copied.
    Changed,
    /// The station has no directory for snapshots.
    NoDir,
    /// A param is wrong.
    Invalid(&'static str),
}

impl Fail {
    /// The reason that a failed capture records.
    #[must_use]
    pub fn reason(&self) -> HistoryUnavailable {
        match self {
            Self::Unavailable(r) => *r,
            Self::Busy => HistoryUnavailable::Busy,
            Self::Io | Self::Changed | Self::NoDir | Self::Invalid(_) => {
                HistoryUnavailable::NotCaptured
            }
        }
    }
}

/// A wire error of `session.history`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryError {
    pub code: &'static str,
    pub message: String,
}

impl HistoryError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

/// Test seams. They stay empty in a running station.
#[doc(hidden)]
#[derive(Default)]
pub struct Seams {
    /// A pause at the start of every scan.
    pub scan_delay: Mutex<Option<Duration>>,
    /// Runs after each copy pass of a capture, with the attempt number.
    #[allow(clippy::type_complexity)]
    pub after_copy: Mutex<Option<Arc<dyn Fn(u32) + Send + Sync>>>,
    /// The user id that must own a transcript.
    pub uid: Mutex<Option<u32>>,
    /// The scans that ran.
    pub scans: AtomicUsize,
}

struct Shared {
    limits: Limits,
    budget: Arc<Semaphore>,
    /// One lazy capture at a time (DL-189): see [`lazy_row`].
    lazy: tokio::sync::Mutex<()>,
    dir: OnceLock<PathBuf>,
    cache: Mutex<index::Cache>,
    seams: Seams,
}

/// The shared state of the history work of one station: the budget, the
/// cache of indexes and the snapshot directory.
#[derive(Clone)]
pub struct History {
    shared: Arc<Shared>,
}

impl std::fmt::Debug for History {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("History").finish_non_exhaustive()
    }
}

impl Default for History {
    fn default() -> Self {
        Self::with_limits(Limits::default())
    }
}

impl History {
    #[must_use]
    pub fn with_limits(limits: Limits) -> Self {
        Self {
            shared: Arc::new(Shared {
                limits,
                budget: Arc::new(Semaphore::new(1)),
                lazy: tokio::sync::Mutex::new(()),
                dir: OnceLock::new(),
                cache: Mutex::new(index::Cache::default()),
                seams: Seams::default(),
            }),
        }
    }

    /// Sets the directory of the snapshots. The first call counts.
    pub fn set_dir(&self, dir: PathBuf) {
        let _ = self.shared.dir.set(dir);
    }

    /// The directory of the snapshots, if the station has one.
    #[must_use]
    pub fn dir(&self) -> Option<&PathBuf> {
        self.shared.dir.get()
    }

    #[must_use]
    pub fn limits(&self) -> &Limits {
        &self.shared.limits
    }

    #[doc(hidden)]
    #[must_use]
    pub fn seams(&self) -> &Seams {
        &self.shared.seams
    }

    /// The indexes in memory.
    #[must_use]
    pub fn cached(&self) -> usize {
        self.shared.cache.lock().map_or(0, |c| c.len())
    }

    /// The user id that must own a transcript.
    fn uid(&self) -> u32 {
        self.shared
            .seams
            .uid
            .lock()
            .ok()
            .and_then(|u| *u)
            .unwrap_or_else(|| rustix::process::getuid().as_raw())
    }

    /// Runs `work` on the blocking pool under the budget. The permit moves
    /// into the worker, so it is released only when the worker returns, also
    /// after the deadline answered `busy`.
    async fn run<T: Send + 'static>(
        &self,
        deadline: Instant,
        work: impl FnOnce(Instant) -> Result<T, Fail> + Send + 'static,
    ) -> Result<T, Fail> {
        let until = tokio::time::Instant::from_std(deadline);
        let permit =
            match tokio::time::timeout_at(until, Arc::clone(&self.shared.budget).acquire_owned())
                .await
            {
                Ok(Ok(permit)) => permit,
                _ => return Err(Fail::Busy),
            };
        let task = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            work(deadline)
        });
        match tokio::time::timeout_at(until, task).await {
            Ok(Ok(result)) => result,
            Ok(Err(e)) => {
                tracing::warn!(error = %e, "history worker");
                Err(Fail::Io)
            }
            Err(_) => Err(Fail::Busy),
        }
    }
}

/// The launch environment of a provider instance: the one that the policy
/// holds for its adapter, else the station's scrubbed environment.
#[must_use]
pub fn env_for(policy: Option<&Policy>, instance: &ProviderInstanceId) -> ChildEnv {
    policy
        .and_then(|p| p.setup_env(instance))
        .cloned()
        .unwrap_or_else(ChildEnv::from_process)
}

/// The wire name of a reason, as a failed capture stores it.
#[must_use]
pub fn reason_name(reason: HistoryUnavailable) -> String {
    serde_json::to_value(reason)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_else(|| "not_captured".to_owned())
}

fn reason_of(name: Option<&str>) -> HistoryUnavailable {
    name.and_then(|n| serde_json::from_value(serde_json::Value::String(n.to_owned())).ok())
        .unwrap_or(HistoryUnavailable::NotCaptured)
}

/// What a capture needs.
struct Job {
    session_id: SessionId,
    kind: ProviderKind,
    cwd: String,
    native: String,
    home: PathBuf,
    /// `Some(first process start in ms)` for a lazy capture.
    cutoff: Option<i64>,
    private: Arc<PrivatePaths>,
    dir: PathBuf,
}

/// What a capture stored.
struct Captured {
    home: PathBuf,
    path: PathBuf,
    snapshot: capture::Snapshot,
}

impl History {
    fn capture_job(&self, job: Job, deadline: Instant) -> Result<Captured, Fail> {
        let limits = self.shared.limits.clone();
        let uid = self.uid();
        let hook = self
            .shared
            .seams
            .after_copy
            .lock()
            .ok()
            .and_then(|h| h.clone());
        capture_blocking(&job, &limits, uid, deadline, hook)
    }

    /// Captures a transcript under the budget.
    async fn capture(&self, job: Job, deadline: Instant) -> Result<Captured, Fail> {
        let this = self.clone();
        self.run(deadline, move |deadline| this.capture_job(job, deadline))
            .await
    }
}

fn capture_blocking(
    job: &Job,
    limits: &Limits,
    uid: u32,
    deadline: Instant,
    hook: Option<Arc<dyn Fn(u32) + Send + Sync>>,
) -> Result<Captured, Fail> {
    let name = job.session_id.as_str();
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
    {
        return Err(Fail::Io);
    }
    let trust = locate::Trust {
        private: &job.private,
        uid,
        limits,
        deadline,
    };
    let opened = locate::locate(&job.home, job.kind, &job.cwd, &job.native, &trust)?;
    let mut prefix = capture::prefix_end(&opened.file, opened.size, deadline)?;
    if let Some(cutoff) = job.cutoff {
        prefix = capture::lazy_boundary(&opened.file, prefix, job.kind, cutoff, limits, deadline)?;
    }
    let snapshot = capture::copy_prefix(
        &opened.file,
        prefix,
        &job.dir,
        name,
        deadline,
        hook.as_deref(),
    )?;
    Ok(Captured {
        home: opened.home,
        path: opened.path,
        snapshot,
    })
}

fn row_of(
    session_id: &SessionId,
    capture: Capture,
    captured: Option<&Captured>,
    reason: Option<HistoryUnavailable>,
    now: i64,
) -> TranscriptRow {
    TranscriptRow {
        session_id: session_id.clone(),
        home: captured.map(|c| c.home.to_string_lossy().into_owned()),
        path: captured.map(|c| c.path.to_string_lossy().into_owned()),
        capture,
        reason: reason.map(reason_name),
        prefix_size: captured.map(|c| c.snapshot.size),
        prefix_sha256: captured.map(|c| c.snapshot.sha256.clone()),
        snapshot: captured.map(|c| c.snapshot.path.to_string_lossy().into_owned()),
        item_count: None,
        captured_at: now,
    }
}

/// The capture of an import, run before the resume starts the provider. It
/// does nothing when the session has a row. A failure is recorded as
/// `capture = failed`; it never fails the import.
pub async fn at_import(
    store: &Store,
    policy: Option<&Policy>,
    private: &Arc<PrivatePaths>,
    session_id: &SessionId,
    native: &str,
) {
    let facts = match store.history_facts(session_id.clone()).await {
        Ok(Some(facts)) => facts,
        Ok(None) => return,
        Err(e) => {
            tracing::warn!(error = %e, session = %session_id, "history capture: session facts");
            return;
        }
    };
    match store.transcript(session_id.clone()).await {
        Ok(None) => {}
        Ok(Some(_)) => return,
        Err(e) => {
            tracing::warn!(error = %e, session = %session_id, "history capture: row");
            return;
        }
    }
    let history = store.history();
    let deadline = Instant::now() + history.limits().deadline;
    let outcome = capture_for(
        history, policy, private, session_id, &facts, native, None, deadline,
    )
    .await;
    let now = store.now_ms();
    let row = match &outcome {
        Ok(captured) => row_of(session_id, Capture::Import, Some(captured), None, now),
        Err(fail) => {
            tracing::info!(session = %session_id, reason = %reason_name(fail.reason()), "history capture failed");
            row_of(session_id, Capture::Failed, None, Some(fail.reason()), now)
        }
    };
    if let Err(e) = store.put_transcript(row).await {
        tracing::warn!(error = %e, session = %session_id, "history capture: row not stored");
    }
}

#[allow(clippy::too_many_arguments)]
async fn capture_for(
    history: &History,
    policy: Option<&Policy>,
    private: &Arc<PrivatePaths>,
    session_id: &SessionId,
    facts: &HistoryFacts,
    native: &str,
    cutoff: Option<i64>,
    deadline: Instant,
) -> Result<Captured, Fail> {
    let dir = history.dir().cloned().ok_or(Fail::NoDir)?;
    let env = env_for(policy, &facts.provider_instance);
    let home = locate::resolve_home(&env, facts.provider)
        .filter(|h| h.is_absolute())
        .ok_or(Fail::Unavailable(HistoryUnavailable::NotFound))?;
    let job = Job {
        session_id: session_id.clone(),
        kind: facts.provider,
        cwd: facts.cwd.clone(),
        native: native.to_owned(),
        home,
        cutoff,
        private: Arc::clone(private),
        dir,
    };
    history.capture(job, deadline).await
}

fn unavailable(reason: HistoryUnavailable) -> SessionHistory {
    SessionHistory {
        items: Vec::new(),
        before: None,
        state: HistoryState::Unavailable { reason },
    }
}

/// A cursor: the first 16 hex digits of the snapshot hash and the number
/// of the item from the start of the snapshot.
fn parse_cursor(text: &str) -> Option<(String, u64)> {
    let (hash, number) = text.split_once(':')?;
    if hash.len() != 16 || !hash.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
        return None;
    }
    if number.is_empty() || !number.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some((hash.to_owned(), number.parse().ok()?))
}

fn busy() -> HistoryError {
    HistoryError::new(code::BUSY, "the history took too long")
}

fn internal(e: impl std::fmt::Display) -> HistoryError {
    HistoryError::new("internal", e.to_string())
}

/// `session.history`.
///
/// # Errors
/// [`HistoryError`] with `invalid_params`, `unknown_session`,
/// `stale_cursor`, `busy` or `internal`. `unavailable`, `empty` and
/// `partial` are answers.
pub async fn answer(
    store: &Store,
    policy: &Policy,
    private: &Arc<PrivatePaths>,
    query: SessionHistoryQuery,
) -> Result<SessionHistory, HistoryError> {
    let limit = query.limit.unwrap_or(DEFAULT_LIMIT);
    if !(1..=MAX_LIMIT).contains(&limit) {
        return Err(HistoryError::new(
            code::INVALID_PARAMS,
            format!("limit must be 1 to {MAX_LIMIT}"),
        ));
    }
    let cursor = match query.before.as_deref() {
        None => None,
        Some(text) => Some(parse_cursor(text).ok_or_else(|| {
            HistoryError::new(code::INVALID_PARAMS, "the cursor has a bad syntax")
        })?),
    };
    let session_id = query.session_id;
    let facts = store
        .history_facts(session_id.clone())
        .await
        .map_err(internal)?
        .ok_or_else(|| HistoryError::new(code::UNKNOWN_SESSION, "no such session"))?;
    let history = store.history();
    let deadline = Instant::now() + history.limits().deadline;
    let row = store
        .transcript(session_id.clone())
        .await
        .map_err(internal)?;
    let row = match row {
        Some(row) => row,
        None => {
            match lazy_row(
                store,
                history,
                policy,
                private,
                &session_id,
                &facts,
                query.member,
                deadline,
            )
            .await?
            {
                Ok(row) => row,
                Err(answer) => return Ok(answer),
            }
        }
    };
    let reason = match row.capture {
        Capture::Failed if query.member => HistoryUnavailable::NotCaptured,
        Capture::Failed => reason_of(row.reason.as_deref()),
        Capture::Lazy if query.member => HistoryUnavailable::NotCaptured,
        Capture::Import | Capture::Lazy => {
            return read_row(
                store,
                history,
                &row,
                &facts,
                cursor,
                limit,
                query.member,
                deadline,
            )
            .await;
        }
    };
    Ok(unavailable(reason))
}

/// The row of a session without one: the empty answer for a started
/// session, a lazy capture for an older import, or the reason why not.
#[allow(clippy::too_many_arguments)]
async fn lazy_row(
    store: &Store,
    history: &History,
    policy: &Policy,
    private: &Arc<PrivatePaths>,
    session_id: &SessionId,
    facts: &HistoryFacts,
    member: bool,
    deadline: Instant,
) -> Result<Result<TranscriptRow, SessionHistory>, HistoryError> {
    let answer = |state| {
        Ok(Err(SessionHistory {
            items: Vec::new(),
            before: None,
            state,
        }))
    };
    if facts.first_effect == FirstEffect::Start {
        return answer(HistoryState::Empty);
    }
    let Some(native) = facts.native.as_deref() else {
        return answer(HistoryState::Unavailable {
            reason: HistoryUnavailable::NoNativeId,
        });
    };
    if check_native_session_id(native).is_err() {
        return answer(HistoryState::Unavailable {
            reason: HistoryUnavailable::NoNativeId,
        });
    }
    let not_captured = HistoryState::Unavailable {
        reason: HistoryUnavailable::NotCaptured,
    };
    let Some(cutoff) = facts.first_process_at.filter(|_| !member) else {
        return answer(not_captured);
    };
    // The row is checked again under the lock, so a second capture never
    // replaces the snapshot of a committed row.
    let _lazy = history.shared.lazy.lock().await;
    if let Some(row) = store
        .transcript(session_id.clone())
        .await
        .map_err(internal)?
    {
        return Ok(Ok(row));
    }
    let captured = capture_for(
        history,
        Some(policy),
        private,
        session_id,
        facts,
        native,
        Some(cutoff),
        deadline,
    )
    .await;
    let captured = match captured {
        Ok(captured) => captured,
        Err(Fail::Busy) => return Err(busy()),
        Err(fail) => {
            return answer(HistoryState::Unavailable {
                reason: fail.reason(),
            });
        }
    };
    let row = row_of(
        session_id,
        Capture::Lazy,
        Some(&captured),
        None,
        store.now_ms(),
    );
    if !store.put_transcript(row.clone()).await.map_err(internal)? {
        // Another request stored a row first; use that one.
        let stored = store
            .transcript(session_id.clone())
            .await
            .map_err(internal)?;
        return match stored {
            Some(row) => Ok(Ok(row)),
            None => answer(not_captured),
        };
    }
    Ok(Ok(row))
}

/// Reads one page from the snapshot of an `import` or `lazy` row.
#[allow(clippy::too_many_arguments)]
async fn read_row(
    store: &Store,
    history: &History,
    row: &TranscriptRow,
    facts: &HistoryFacts,
    cursor: Option<(String, u64)>,
    limit: u32,
    member: bool,
    deadline: Instant,
) -> Result<SessionHistory, HistoryError> {
    let (Some(size), Some(sha), Some(path)) = (
        row.prefix_size,
        row.prefix_sha256.clone(),
        row.snapshot.clone(),
    ) else {
        return Ok(unavailable(HistoryUnavailable::NotCaptured));
    };
    let hash16 = sha.get(..16).unwrap_or_default().to_owned();
    let before = match cursor {
        None => None,
        Some((hash, number)) if hash == hash16 => Some(number),
        Some(_) => {
            return Err(HistoryError::new(
                code::STALE_CURSOR,
                "the cursor belongs to another snapshot",
            ));
        }
    };
    let this = history.clone();
    let session = row.session_id.to_string();
    let kind = facts.provider;
    let worked = history
        .run(deadline, move |deadline| {
            this.read_page(
                &session,
                &PathBuf::from(path),
                kind,
                size,
                &sha,
                &hash16,
                before,
                limit,
                member,
                deadline,
            )
        })
        .await;
    match worked {
        Ok((answer, total)) => {
            if row.item_count.is_none()
                && let Err(e) = store
                    .set_transcript_items(row.session_id.clone(), total)
                    .await
            {
                tracing::debug!(error = %e, "history item count");
            }
            Ok(answer)
        }
        Err(Fail::Busy) => Err(busy()),
        Err(Fail::Invalid(why)) => Err(HistoryError::new(code::INVALID_PARAMS, why)),
        Err(fail) => Ok(unavailable(fail.reason())),
    }
}

impl History {
    /// The blocking part of a read: the index (cached or scanned) and the
    /// page.
    #[allow(clippy::too_many_arguments)]
    fn read_page(
        &self,
        session: &str,
        path: &std::path::Path,
        kind: ProviderKind,
        size: u64,
        sha: &str,
        hash16: &str,
        before: Option<u64>,
        limit: u32,
        member: bool,
        deadline: Instant,
    ) -> Result<(SessionHistory, u64), Fail> {
        let limits = &self.shared.limits;
        let file = match std::fs::File::open(path) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(Fail::Unavailable(HistoryUnavailable::NotFound));
            }
            Err(_) => return Err(Fail::Io),
        };
        let key = (session.to_owned(), sha.to_owned());
        let cached = self.shared.cache.lock().map_err(|_| Fail::Io)?.get(&key);
        let index = if let Some(index) = cached {
            index
        } else {
            let delay = self.shared.seams.scan_delay.lock().ok().and_then(|d| *d);
            self.shared.seams.scans.fetch_add(1, Ordering::SeqCst);
            let built = Arc::new(index::build(
                &file, kind, size, sha, limits, deadline, delay,
            )?);
            self.shared
                .cache
                .lock()
                .map_err(|_| Fail::Io)?
                .put(key, Arc::clone(&built), limits);
            built
        };
        let query = index::PageQuery {
            kind,
            hash16,
            before,
            limit: limit as usize,
            member,
        };
        let page = index::page(&file, &index, &query, limits)?;
        Ok((page, index.total()))
    }
}
