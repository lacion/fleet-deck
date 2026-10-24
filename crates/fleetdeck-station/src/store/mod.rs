//! The station event log and command receipts (PLAN §6.4, §10; M2 P2-4).
//!
//! - **One transaction per write.** Appending events allocates `station_seq`
//!   (one counter per station, in `meta`) and `session_seq` (per session),
//!   stores the events and folds them into the session projection in one
//!   SQLite transaction. Subscribers see a new head only after the commit.
//! - **Command decisions** ([`Store::decide`]) commit the decision's events,
//!   projection updates, effects and the receipt in one transaction.
//! - **Effects** ([`effects`]) go through the outbox; the worker claims them
//!   with its fence token before the I/O. Provider evidence confirms them in
//!   the transaction that stores the evidence event.
//! - **Recovery** ([`Store::recover`]) runs at station start, before the
//!   store admits any command (A10).
//! - **Receipts** acknowledge acceptance, not completion: a duplicate
//!   `command_id` returns the stored receipt and does nothing else; a reused
//!   id with another method, payload or caller is rejected (`command_id_reused`)
//!   and the stored receipt stays.
//! - **Incarnation.** A new database gets a new `incarnation_id`; a deck that
//!   sees another incarnation drops its cursor (G3, G4).
//! - **Retention.** Events stay until every paired deck acknowledged them
//!   **and** the retention window passed; a cursor below the removed prefix
//!   gets [`ReadResult::SnapshotRequired`]. Receipts stay for
//!   [`RECEIPT_RETENTION_MS`]. Finished effects, requests, processes, shell
//!   effects, coordinator outbox rows and artifact suggestions stay for
//!   [`HISTORY_RETENTION_MS`].

pub mod analytics;
pub mod app_calls;
pub mod apps;
pub mod artifact_deletions;
pub mod artifacts;
pub mod artifacts_state;
pub mod assist;
pub mod auto_continue;
pub mod capture_ops;
pub mod coordinator_outbox;
pub mod desktop_ops;
pub mod effects;
mod effects_observe;
pub mod execution_stops;
pub mod git_cleanup;
pub mod git_state;
pub mod home_attempts;
pub mod land_fences;
pub mod land_publish;
pub mod land_runs;
pub mod land_temps;
mod launch_facts;
mod log;
mod migrations;
pub mod move_commit;
pub mod move_discard;
pub mod move_runs;
pub mod move_source;
pub mod move_staging;
pub mod native_browse;
mod outbox;
mod payload;
pub mod peers;
/// The starts that prepare their workspace in a task of their own.
pub mod provisioning;
mod receipts;
mod recovery;
pub mod requests;
pub mod resumes;
pub mod reverts;
pub mod schema;
mod schema_v26;
mod schema_v27;
mod schema_v28;
mod schema_v29;
mod schema_v30;
mod schema_v31;
mod schema_v32;
mod schema_v33;
mod schema_v34;
mod schema_v35;
mod schema_v36;
mod schema_v37;
mod schema_v38;
mod schema_v39;
mod schema_v40;
mod schema_v41;
mod schema_v42;
mod schema_v43;
mod schema_v44;
mod schema_v45;
mod schema_v46;
mod schema_v50;
pub mod secret_homes;
pub mod session_files;
pub mod session_grants;
pub mod session_holds;
pub mod setup_runs;
mod sharing;
pub mod shell_effects;
mod shell_observe;
pub mod shell_retire;
pub mod shell_runs;
pub mod shim_bindings;
mod shim_bindings_rewrite;
mod shim_bindings_start;
pub mod shim_driver;
pub mod shim_gaps;
pub mod shim_writes;
pub mod shims;
pub mod subagent_logs;
pub mod terminal;
pub mod transcripts;
mod turn_checkpoints;
pub mod usage_recount;
pub mod workflow_agent_logs;
pub mod workflow_pages;
mod workflow_state;
pub mod worktree_ops;

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use fleetdeck_core::db::{Db, DbError};
use fleetdeck_core::{SharedClock, SharedIds, new_id};
use fleetdeck_proto::command::RECEIPT_RETENTION_MS;
use fleetdeck_proto::event::{ProviderKind, RawRef};
use fleetdeck_proto::link::MAX_SNAPSHOT_SESSIONS;
use fleetdeck_proto::mutation::Mutation;
use fleetdeck_proto::{
    CommandEnvelope, Event, EventBody, EventEnvelope, IncarnationId, ProviderInstanceId, Receipt,
    SessionId, SessionSummary,
};
use fleetdeck_providers::Emitted;
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use tokio::sync::watch;

pub use effects::{
    Dispatch, Effect, EffectId, EffectPayload, EffectState, FIRST_INPUT_NOT_SENT,
    FIRST_INPUT_UNKNOWN, InputRecord, ProcessIncarnation, SyncPlan, WorkspacePlan, WorktreeAction,
};
pub use receipts::{Caller, DecideError, Decided, Decision};
pub use recovery::RecoveryReport;
pub use requests::{
    AnswerClaim, AnswerReceipt, REASON_REQUEST_EXPIRED, RequestState, RequestTime, RuntimeRequest,
};
pub use reverts::{NewRevert, RevertRow, RevertStep};
pub use sharing::overlaps;
pub use shim_bindings::IdentitySignal;
pub use shim_driver::{FrameState, ShimFrame, ShimTransition};
pub use shim_writes::ClaimedWrite;
pub use shims::{NewShim, OutputCommit, OutputMeta, RunningFacts, ShimRow, ShimState};
pub use turn_checkpoints::{
    AppTurnId, Finalized, NewTurnCheckpoint, RowSide, RowState, TurnCheckpointRow, Which,
};
pub use worktree_ops::{NewWorktreeOp, WorktreeOp};

/// The default event retention after the deck acknowledged the events.
pub const EVENT_RETENTION_MS: i64 = 7 * 24 * 60 * 60 * 1000;

/// The default retention of the finished recovery and history rows: effects,
/// runtime requests, dead processes, shell effects, coordinator outbox rows
/// and artifact suggestions (NV-02, NV-10, NV-26, NV-28).
pub const HISTORY_RETENTION_MS: i64 = 90 * 24 * 60 * 60 * 1000;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error(transparent)]
    Db(#[from] DbError),
    #[error("stored JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("unknown session {0}")]
    UnknownSession(SessionId),
    #[error("session {0} exists already")]
    SessionExists(SessionId),
    #[error("deck {0} is not paired")]
    UnknownDeck(String),
    #[error(
        "this station is already paired with deck {0}; run `fleetdeck station forget-deck` first"
    )]
    DeckPaired(String),
    #[error("cursor {cursor} is ahead of the head {head}")]
    CursorAhead { cursor: u64, head: u64 },
    #[error("corrupt store: {0}")]
    Corrupt(String),
    #[error("session {0} has no live provider process")]
    NoProcess(SessionId),
    #[error("effect worker {fence} was replaced by worker {current}")]
    Fenced { fence: u64, current: u64 },
    #[error("the store admits no command before start recovery ran")]
    NotRecovered,
}

impl From<rusqlite::Error> for StoreError {
    fn from(e: rusqlite::Error) -> Self {
        Self::Db(DbError::Sqlite(e))
    }
}

pub type Result<T, E = StoreError> = std::result::Result<T, E>;

/// Retention settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StoreConfig {
    pub event_retention_ms: i64,
    pub receipt_retention_ms: i64,
    /// How long a finished history row stays (NV-26, NV-28).
    pub history_retention_ms: i64,
    /// The most sessions in a snapshot batch (DL-051).
    pub snapshot_sessions: u64,
}

impl Default for StoreConfig {
    fn default() -> Self {
        Self {
            event_retention_ms: EVENT_RETENTION_MS,
            receipt_retention_ms: RECEIPT_RETENTION_MS,
            history_retention_ms: HISTORY_RETENTION_MS,
            snapshot_sessions: MAX_SNAPSHOT_SESSIONS,
        }
    }
}

/// What the station does with an open approval while the deck is away (§6.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(tag = "policy", rename_all = "snake_case")]
pub enum OfflineApprovalPolicy {
    #[default]
    Wait,
    DenyAfter {
        after_ms: u64,
    },
}

/// A new row of the `sessions` table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewSession {
    pub session_id: SessionId,
    pub provider: ProviderKind,
    pub provider_instance: ProviderInstanceId,
    pub cwd: String,
    pub worktree: Option<String>,
    /// The git branch of `cwd` at start (M2 P4-2); kept in the summary.
    pub branch: Option<String>,
    pub mode: Option<String>,
    pub title: Option<String>,
    pub offline_approval_policy: OfflineApprovalPolicy,
}

/// One event to append.
#[derive(Debug, Clone, PartialEq)]
pub struct NewEvent {
    pub event: EventBody,
    pub raw_ref: Option<RawRef>,
}

impl From<Event> for NewEvent {
    fn from(event: Event) -> Self {
        Self {
            event: EventBody::Known(event),
            raw_ref: None,
        }
    }
}

impl From<Emitted> for NewEvent {
    fn from(e: Emitted) -> Self {
        Self {
            event: EventBody::Known(e.event),
            raw_ref: e.raw_ref,
        }
    }
}

/// The answer to a resync cursor.
#[derive(Debug, Clone, PartialEq)]
pub enum ReadResult {
    /// The events after the cursor (at most the limit), in `station_seq` order.
    Events {
        events: Vec<EventEnvelope>,
        head: u64,
    },
    /// Retention removed events after the cursor: send a snapshot (G4).
    SnapshotRequired { head: u64 },
}

/// All session projections at one watermark (G4).
#[derive(Debug, Clone, PartialEq)]
pub struct Snapshot {
    pub incarnation: IncarnationId,
    pub watermark: u64,
    pub sessions: Vec<SessionSummary>,
}

/// What [`Store::prune`] removed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PruneReport {
    pub events: usize,
    pub receipts: usize,
    /// The finished history rows that the prune removed (NV-26).
    pub history: usize,
    pub pruned_through: u64,
}

struct Inner {
    db: Db,
    clock: SharedClock,
    ids: SharedIds,
    incarnation: IncarnationId,
    /// A fresh id at each open: the station process that opened the store
    /// (B15 app call recovery). Unlike `incarnation`, a reopen changes it.
    run: String,
    config: StoreConfig,
    head: watch::Sender<u64>,
    /// Wakes the effect worker after a commit that queued effects.
    outbox: watch::Sender<u64>,
    recovered: AtomicBool,
    /// The budget, the cache and the snapshot directory of the session
    /// history (DL-189).
    history: crate::history::History,
    /// The memory side of the shim bindings; none until the host sets it
    /// (DL-194).
    binding_sink: std::sync::RwLock<Option<Arc<dyn shim_bindings::BindingSink>>>,
    /// True while a deck link without `fleet_git.v1` is up: the Fleet Git
    /// loops pause (design §3.3).
    fleet_git_paused: Arc<AtomicBool>,
}

/// The station store. Clones share one database thread.
#[derive(Clone)]
pub struct Store {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for Store {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Store")
            .field("db", &self.inner.db)
            .field("incarnation", &self.inner.incarnation)
            .finish_non_exhaustive()
    }
}

impl Store {
    /// Opens (or creates) `station.db`. A new database gets a new incarnation.
    ///
    /// # Errors
    /// The database cannot open or migrate, or `meta` is corrupt.
    pub async fn open(
        path: &Path,
        clock: SharedClock,
        ids: SharedIds,
        config: StoreConfig,
    ) -> Result<Self> {
        let db = Db::open(path, schema::migrations())?;
        let store = Self::start(db, clock, ids, config, crate::history::History::default()).await?;
        // The snapshots of the session history live next to `station.db`.
        if let Some(dir) = path.parent() {
            store.inner.history.set_dir(dir.join("history"));
        }
        Ok(store)
    }

    /// An in-memory store (tests).
    ///
    /// # Errors
    /// See [`Self::open`].
    pub async fn open_in_memory(
        clock: SharedClock,
        ids: SharedIds,
        config: StoreConfig,
    ) -> Result<Self> {
        Self::open_in_memory_with_history(clock, ids, config, crate::history::History::default())
            .await
    }

    /// An in-memory store with the given session history state (tests).
    ///
    /// # Errors
    /// See [`Self::open`].
    pub async fn open_in_memory_with_history(
        clock: SharedClock,
        ids: SharedIds,
        config: StoreConfig,
        history: crate::history::History,
    ) -> Result<Self> {
        let db = Db::open_in_memory(schema::migrations())?;
        Self::start(db, clock, ids, config, history).await
    }

    async fn start(
        db: Db,
        clock: SharedClock,
        ids: SharedIds,
        config: StoreConfig,
        history: crate::history::History,
    ) -> Result<Self> {
        let fresh: IncarnationId = new_id(ids.as_ref());
        // Every open draws `fresh`; a kept database uses it only as the run id.
        let run = fresh.0.clone();
        let ids_kept = Arc::clone(&ids);
        let now = now_ms(&clock);
        let (incarnation, head) = call(&db, move |conn| {
            let tx = conn.transaction()?;
            let found = tx
                .query_row(
                    "SELECT incarnation_id, station_seq_next FROM meta WHERE id = 1",
                    [],
                    |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)),
                )
                .optional()?;
            let (inc, next) = match found {
                Some(row) => row,
                None => {
                    tx.execute(
                        "INSERT INTO meta (id, incarnation_id, station_seq_next, created_at)
                         VALUES (1, ?1, 1, ?2)",
                        rusqlite::params![fresh.as_str(), now],
                    )?;
                    (fresh.0.clone(), 1)
                }
            };
            tx.commit()?;
            Ok((IncarnationId::from(inc), to_u64(next)?.saturating_sub(1)))
        })
        .await?;
        let (head_tx, _) = watch::channel(head);
        let (outbox_tx, _) = watch::channel(0);
        Ok(Self {
            inner: Arc::new(Inner {
                db,
                clock,
                ids: ids_kept,
                incarnation,
                run,
                config,
                head: head_tx,
                outbox: outbox_tx,
                recovered: AtomicBool::new(false),
                history,
                binding_sink: std::sync::RwLock::new(None),
                fleet_git_paused: Arc::new(AtomicBool::new(false)),
            }),
        })
    }

    /// The station clock in Unix milliseconds (command envelopes of the
    /// local CLI use it for `retry_until`).
    #[must_use]
    pub fn now_ms(&self) -> i64 {
        now_ms(&self.inner.clock)
    }

    /// The flag that pauses the Fleet Git loops while a deck link without
    /// `fleet_git.v1` is up (design §3.3).
    #[must_use]
    pub fn fleet_git_paused(&self) -> &Arc<AtomicBool> {
        &self.inner.fleet_git_paused
    }

    /// The history state of this station: its budget, cache and directory.
    #[must_use]
    pub fn history(&self) -> &crate::history::History {
        &self.inner.history
    }

    #[must_use]
    pub fn incarnation(&self) -> &IncarnationId {
        &self.inner.incarnation
    }

    /// The id of this open of the store: new at each open, also of the same
    /// database. The app call rows name the station process with it (B15).
    #[must_use]
    pub fn run_id(&self) -> &str {
        &self.inner.run
    }

    /// The committed head `station_seq`; it changes only after a commit.
    #[must_use]
    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.inner.head.subscribe()
    }

    #[must_use]
    pub fn head(&self) -> u64 {
        *self.inner.head.borrow()
    }

    fn now(&self) -> i64 {
        now_ms(&self.inner.clock)
    }

    fn publish(&self, head: u64) {
        self.inner.head.send_if_modified(|h| {
            let moved = head > *h;
            if moved {
                *h = head;
            }
            moved
        });
    }

    /// Creates a session row outside a command (for example a session that
    /// the station adopts). Commands use [`Decision::create_session`].
    ///
    /// # Errors
    /// [`StoreError::SessionExists`] or a database error.
    pub async fn create_session(&self, session: NewSession) -> Result<()> {
        let now = self.now();
        call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            log::insert_session(&tx, &session, now)?;
            tx.commit()?;
            Ok(())
        })
        .await
    }

    /// Appends provider or station events of one session in one transaction.
    ///
    /// # Errors
    /// [`StoreError::UnknownSession`] or a database error; nothing is stored.
    pub async fn append(
        &self,
        session_id: SessionId,
        events: Vec<NewEvent>,
    ) -> Result<Vec<EventEnvelope>> {
        let now = self.now();
        let out = call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            let mut out = Vec::with_capacity(events.len());
            for event in events {
                out.push(log::append_one(&tx, &session_id, event, now)?);
            }
            tx.commit()?;
            Ok(out)
        })
        .await?;
        if let Some(last) = out.last() {
            self.publish(last.station_seq);
        }
        Ok(out)
    }

    /// The events after `cursor`, at most `limit`.
    ///
    /// # Errors
    /// [`StoreError::CursorAhead`] or a database error.
    pub async fn read_after(&self, cursor: u64, limit: usize) -> Result<ReadResult> {
        call(&self.inner.db, move |conn| {
            log::read_after(conn, cursor, limit)
        })
        .await
    }

    /// One session's projection.
    ///
    /// # Errors
    /// A database or JSON error.
    pub async fn summary(&self, session_id: SessionId) -> Result<Option<SessionSummary>> {
        call(&self.inner.db, move |conn| log::summary(conn, &session_id)).await
    }

    /// The session projections of a snapshot batch and the watermark in one
    /// read: every session that has not exited, then the newest exited
    /// ones, [`StoreConfig::snapshot_sessions`] at most (DL-051).
    ///
    /// # Errors
    /// A database or JSON error.
    pub async fn snapshot(&self) -> Result<Snapshot> {
        let incarnation = self.inner.incarnation.clone();
        let limit = self.inner.config.snapshot_sessions;
        call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            let watermark = log::head(&tx)?;
            let sessions = log::summaries(&tx, limit)?;
            tx.commit()?;
            Ok(Snapshot {
                incarnation,
                watermark,
                sessions,
            })
        })
        .await
    }

    /// Records a paired deck (Phase 3 pairing writes it).
    ///
    /// # Errors
    /// A database error.
    pub async fn pair_deck(&self, deck_key: String, deck_url: Option<String>) -> Result<()> {
        let now = self.now();
        call(&self.inner.db, move |conn| {
            conn.execute(
                "INSERT INTO pairing (deck_key, deck_url, paired_at) VALUES (?1, ?2, ?3)
                 ON CONFLICT (deck_key) DO UPDATE SET deck_url = excluded.deck_url",
                rusqlite::params![deck_key, deck_url, now],
            )?;
            Ok(())
        })
        .await
    }

    /// Stores a deck acknowledgement; it never moves back.
    ///
    /// # Errors
    /// [`StoreError::UnknownDeck`], [`StoreError::CursorAhead`] or a
    /// database error.
    pub async fn ack(&self, deck_key: String, station_seq: u64) -> Result<()> {
        let now = self.now();
        call(&self.inner.db, move |conn| {
            log::ack(conn, &deck_key, station_seq, now)
        })
        .await
    }

    /// Removes acknowledged events older than the retention window, receipts
    /// older than the receipt retention, and finished history rows older than
    /// the history retention (see [`log::prune`]).
    ///
    /// # Errors
    /// A database error.
    pub async fn prune(&self) -> Result<PruneReport> {
        let now = self.now();
        let config = self.inner.config;
        call(&self.inner.db, move |conn| log::prune(conn, now, config)).await
    }

    /// Decides one command (§6.4). See [`receipts`](self) for the rules.
    ///
    /// # Errors
    /// A database error: nothing is stored and the caller answers with an
    /// error, not with a receipt.
    /// Rejected with [`StoreError::NotRecovered`] before [`Self::recover`].
    pub async fn decide<F>(
        &self,
        envelope: CommandEnvelope,
        caller: Caller,
        decide: F,
    ) -> Result<Decided>
    where
        F: FnOnce(
                &mut Decision<'_>,
            ) -> Result<fleetdeck_proto::command::CommandResult, DecideError>
            + Send
            + 'static,
    {
        self.decide_mutation(Mutation::Session(envelope), caller, decide)
            .await
    }

    /// Decides any mutating envelope (session command, `link.rotate_key`,
    /// `station.configure`) with the same receipt rules as [`Self::decide`].
    ///
    /// # Errors
    /// See [`Self::decide`].
    pub async fn decide_mutation<F>(
        &self,
        mutation: Mutation,
        caller: Caller,
        decide: F,
    ) -> Result<Decided>
    where
        F: FnOnce(
                &mut Decision<'_>,
            ) -> Result<fleetdeck_proto::command::CommandResult, DecideError>
            + Send
            + 'static,
    {
        if !self.inner.recovered.load(Ordering::Acquire) {
            return Err(StoreError::NotRecovered);
        }
        let now = self.now();
        let incarnation = self.inner.incarnation.clone();
        let ids = Arc::clone(&self.inner.ids);
        let decided = call(&self.inner.db, move |conn| {
            receipts::decide(
                conn,
                ids.as_ref(),
                &incarnation,
                now,
                &mutation,
                &caller,
                decide,
            )
        })
        .await?;
        if let Some(last) = decided.events.last() {
            self.publish(last.station_seq);
        }
        if !decided.effects.is_empty() {
            self.wake_worker();
        }
        Ok(decided)
    }

    /// The stored receipt of a command.
    ///
    /// # Errors
    /// A database or JSON error.
    pub async fn receipt(&self, command_id: fleetdeck_proto::CommandId) -> Result<Option<Receipt>> {
        call(&self.inner.db, move |conn| {
            receipts::stored(conn, &command_id).map(|r| r.map(|r| r.receipt))
        })
        .await
    }
}

/// Runs `f` on the DB thread; a store error passes through the DB call.
async fn call<R: Send + 'static>(
    db: &Db,
    f: impl FnOnce(&mut Connection) -> Result<R> + Send + 'static,
) -> Result<R> {
    db.call(move |conn| Ok(f(conn))).await?
}

fn now_ms(clock: &SharedClock) -> i64 {
    i64::try_from(clock.now_ms()).unwrap_or(i64::MAX)
}

fn to_u64(v: i64) -> Result<u64> {
    u64::try_from(v).map_err(|_| StoreError::Corrupt(format!("negative sequence {v}")))
}

fn to_i64(v: u64) -> Result<i64> {
    i64::try_from(v).map_err(|_| StoreError::Corrupt(format!("sequence {v} too large")))
}

#[cfg(test)]
#[path = "analytics_tests.rs"]
mod analytics_tests;
#[cfg(test)]
mod effects_tests;
#[cfg(test)]
mod retention_tests;
#[cfg(test)]
mod tests;
#[cfg(test)]
#[path = "turn_checkpoints_tests.rs"]
mod turn_checkpoints_tests;
