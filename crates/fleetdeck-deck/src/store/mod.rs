//! The deck store: `deck.db` (PLAN §7, §10).
//!
//! - [`stations`]: the station registry with pins, grants and the
//!   revocation generation (G5, one transaction per change).
//! - [`read_model`]: the session read model and the condensed timeline,
//!   applied from the station event streams with the resync cursor, the
//!   snapshot batch and the incarnation rules (G4).
//!
//! - [`commands`]: the command router's stored envelopes and states (G3).
//! - [`outbox`]: deck-owned intent delivered as routed commands (A5).
//! - [`coordinator`]: the receipts of `coordinator.call` (G7).
//! - [`auth`]: the owner, login sessions, login links and API tokens
//!   (A13, L15); [`audit`]: the audit log of every mutation (§3 rule 7).
//!
//! Station reachability is link state, not stored state: the deck keeps it
//! in memory and the board status projection reads it (§7.2).

pub mod audit;
pub mod auth;
pub mod commands;
pub mod coordinator;
pub mod outbox;
pub mod read_model;
pub mod schema;
pub mod stations;

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};

use fleetdeck_core::clock::SharedClock;
use fleetdeck_core::db::{Db, DbError};
use fleetdeck_core::ids::{SharedIds, new_id};
use fleetdeck_proto::{IncarnationId, StationId};
use rusqlite::{Connection, OptionalExtension};
use tokio::sync::watch;

pub use audit::{AuditEntry, AuditResult, NewAudit};
pub use auth::{ApiToken, LoginMethod, LoginSession, NewLogin, Principal};
pub use commands::{DeckCommand, NewCommand, Settle};
pub use outbox::{NewOutboxItem, OutboxItem, OutboxState};
pub use read_model::{DeckSession, TIMELINE_CAP};
pub use stations::{Direction, StationRecord};

#[derive(Debug, thiserror::Error)]
pub enum DeckError {
    #[error(transparent)]
    Db(#[from] DbError),
    #[error("stored JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("unknown station {0}")]
    UnknownStation(String),
    #[error("a station named `{0}` exists already")]
    NameTaken(String),
    #[error("a deck pairs only with stations")]
    NotAStation,
    #[error(
        "station {station} sent events of incarnation {got}, the deck holds {expected:?}; a snapshot is required"
    )]
    IncarnationMismatch {
        station: StationId,
        expected: Option<IncarnationId>,
        got: IncarnationId,
    },
    #[error("the deck has not synced with station {0} yet; try again when it is reachable")]
    NotSynced(String),
    #[error("unknown command {0}")]
    UnknownCommand(String),
    #[error("{0}")]
    Invalid(String),
    #[error("password hashing: {0}")]
    Hash(String),
    #[error("corrupt store: {0}")]
    Corrupt(String),
}

impl From<rusqlite::Error> for DeckError {
    fn from(e: rusqlite::Error) -> Self {
        Self::Db(DbError::Sqlite(e))
    }
}

pub type Result<T, E = DeckError> = std::result::Result<T, E>;

struct Inner {
    db: Db,
    clock: SharedClock,
    ids: SharedIds,
    incarnation: IncarnationId,
    /// Counts command state changes (the router waits on it).
    commands: watch::Sender<u64>,
    /// The stations with a live link (§7.2 `unreachable`).
    reachable: Mutex<BTreeSet<StationId>>,
}

/// The deck store. Clones share one database thread.
#[derive(Clone)]
pub struct DeckStore {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for DeckStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeckStore")
            .field("db", &self.inner.db)
            .field("incarnation", &self.inner.incarnation)
            .finish_non_exhaustive()
    }
}

impl DeckStore {
    /// Opens (or creates) `deck.db`. A new database gets a new incarnation.
    ///
    /// # Errors
    /// The database cannot open or migrate, or `meta` is corrupt.
    pub async fn open(path: &Path, clock: SharedClock, ids: SharedIds) -> Result<Self> {
        let db = Db::open(path, schema::migrations())?;
        Self::start(db, clock, ids).await
    }

    /// An in-memory store (tests).
    ///
    /// # Errors
    /// See [`Self::open`].
    pub async fn open_in_memory(clock: SharedClock, ids: SharedIds) -> Result<Self> {
        let db = Db::open_in_memory(schema::migrations())?;
        Self::start(db, clock, ids).await
    }

    async fn start(db: Db, clock: SharedClock, ids: SharedIds) -> Result<Self> {
        let fresh: IncarnationId = new_id(ids.as_ref());
        let owner: String = new_id(ids.as_ref());
        let now = now_ms(&clock);
        let incarnation = call(&db, move |conn| {
            let tx = conn.transaction()?;
            let found: Option<String> = tx
                .query_row("SELECT incarnation_id FROM meta WHERE id = 1", [], |r| {
                    r.get(0)
                })
                .optional()?;
            let inc = match found {
                Some(inc) => inc,
                None => {
                    tx.execute(
                        "INSERT INTO meta (id, incarnation_id, created_at) VALUES (1, ?1, ?2)",
                        rusqlite::params![fresh.as_str(), now],
                    )?;
                    // D6: one owner now; the id keeps room for teams.
                    tx.execute(
                        "INSERT OR IGNORE INTO owner (id, owner_id, created_at) VALUES (1, ?1, ?2)",
                        rusqlite::params![owner.as_str(), now],
                    )?;
                    fresh.0.clone()
                }
            };
            tx.commit()?;
            Ok(IncarnationId::from(inc))
        })
        .await?;
        Ok(Self {
            inner: Arc::new(Inner {
                db,
                clock,
                ids,
                incarnation,
                commands: watch::channel(0).0,
                reachable: Mutex::default(),
            }),
        })
    }

    /// The deck incarnation (`coordinator.call` targets it, ADR-004 §9.3).
    #[must_use]
    pub fn incarnation(&self) -> &IncarnationId {
        &self.inner.incarnation
    }

    /// The database (the link epochs share it, G1).
    #[must_use]
    pub fn db(&self) -> Db {
        self.inner.db.clone()
    }

    /// The deck clock in Unix milliseconds.
    #[must_use]
    pub fn now_ms(&self) -> i64 {
        now_ms(&self.inner.clock)
    }

    /// Records whether the link to `station` is up.
    pub fn set_reachable(&self, station: &StationId, up: bool) {
        let mut set = self.reachable_set();
        if up {
            set.insert(station.clone());
        } else {
            set.remove(station);
        }
    }

    /// True while the link to `station` is up.
    #[must_use]
    pub fn is_reachable(&self, station: &StationId) -> bool {
        self.reachable_set().contains(station)
    }

    fn reachable_set(&self) -> std::sync::MutexGuard<'_, BTreeSet<StationId>> {
        self.inner
            .reachable
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// The persistent revocation generation (G5).
    ///
    /// # Errors
    /// A database error.
    pub async fn revocation_generation(&self) -> Result<u64> {
        call(&self.inner.db, |conn| {
            let n: i64 = conn.query_row(
                "SELECT revocation_generation FROM meta WHERE id = 1",
                [],
                |r| r.get(0),
            )?;
            to_u64(n)
        })
        .await
    }
}

pub(crate) fn bump_generation(conn: &Connection) -> Result<u64> {
    let n: i64 = conn.query_row(
        "UPDATE meta SET revocation_generation = revocation_generation + 1 WHERE id = 1
         RETURNING revocation_generation",
        [],
        |r| r.get(0),
    )?;
    to_u64(n)
}

pub(crate) async fn call<R: Send + 'static>(
    db: &Db,
    f: impl FnOnce(&mut Connection) -> Result<R> + Send + 'static,
) -> Result<R> {
    db.call(move |conn| Ok(f(conn))).await?
}

fn now_ms(clock: &SharedClock) -> i64 {
    i64::try_from(clock.now_ms()).unwrap_or(i64::MAX)
}

pub(crate) fn to_u64(v: i64) -> Result<u64> {
    u64::try_from(v).map_err(|_| DeckError::Corrupt(format!("negative sequence {v}")))
}

pub(crate) fn to_i64(v: u64) -> Result<i64> {
    i64::try_from(v).map_err(|_| DeckError::Corrupt(format!("sequence {v} too large")))
}

#[cfg(test)]
mod tests;
