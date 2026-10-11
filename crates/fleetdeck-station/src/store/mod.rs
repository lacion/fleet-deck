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
//!   [`RECEIPT_RETENTION_MS`].

pub mod effects;
mod log;
mod outbox;
pub mod peers;
mod receipts;
mod recovery;
pub mod requests;
pub mod schema;

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
    FIRST_INPUT_UNKNOWN, InputRecord, ProcessIncarnation,
};
pub use receipts::{Caller, DecideError, Decided, Decision};
pub use recovery::RecoveryReport;
pub use requests::{AnswerClaim, AnswerReceipt, RequestState, RuntimeRequest};

/// The default event retention after the deck acknowledged the events.
pub const EVENT_RETENTION_MS: i64 = 7 * 24 * 60 * 60 * 1000;

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
    /// The most sessions in a snapshot batch (DL-051).
    pub snapshot_sessions: u64,
}

impl Default for StoreConfig {
    fn default() -> Self {
        Self {
            event_retention_ms: EVENT_RETENTION_MS,
            receipt_retention_ms: RECEIPT_RETENTION_MS,
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
    pub pruned_through: u64,
}

struct Inner {
    db: Db,
    clock: SharedClock,
    ids: SharedIds,
    incarnation: IncarnationId,
    config: StoreConfig,
    head: watch::Sender<u64>,
    /// Wakes the effect worker after a commit that queued effects.
    outbox: watch::Sender<u64>,
    recovered: AtomicBool,
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
        Self::start(db, clock, ids, config).await
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
        let db = Db::open_in_memory(schema::migrations())?;
        Self::start(db, clock, ids, config).await
    }

    async fn start(
        db: Db,
        clock: SharedClock,
        ids: SharedIds,
        config: StoreConfig,
    ) -> Result<Self> {
        let fresh: IncarnationId = new_id(ids.as_ref());
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
                config,
                head: head_tx,
                outbox: outbox_tx,
                recovered: AtomicBool::new(false),
            }),
        })
    }

    /// The station clock in Unix milliseconds (command envelopes of the
    /// local CLI use it for `retry_until`).
    #[must_use]
    pub fn now_ms(&self) -> i64 {
        now_ms(&self.inner.clock)
    }

    #[must_use]
    pub fn incarnation(&self) -> &IncarnationId {
        &self.inner.incarnation
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

    /// Removes acknowledged events older than the retention window, and
    /// receipts older than the receipt retention.
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
mod effects_tests;
#[cfg(test)]
mod tests;
