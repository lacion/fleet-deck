//! SQLite on a dedicated DB thread (PLAN §12): `rusqlite` (bundled, WAL) and
//! numbered, transactional migrations with `rusqlite_migration`
//! (`PRAGMA user_version`).
//!
//! One thread owns the connection. Async code sends a closure with
//! [`Db::call`]; synchronous code (the local CLI) uses [`Db::call_blocking`].
//! The closures run one after another, so one closure is one consistent unit
//! of work. Use a transaction inside the closure for multi-statement writes.
//!
//! Durability: `synchronous = FULL`. The event log and the effect journal
//! (§6.4) rely on a commit that survives a power loss before the station
//! acknowledges or dispatches anything.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::JoinHandle;

use rusqlite::Connection;
use rusqlite_migration::Migrations;

use crate::paths::ensure_private_dir;

/// How long a statement waits for a lock held by another process.
pub const BUSY_TIMEOUT_MS: u32 = 5_000;

#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("migration: {0}")]
    Migration(#[from] rusqlite_migration::Error),
    #[error(
        "the database {path} has schema version {found}, newer than this build knows; use a newer fleetdeck"
    )]
    TooNew { path: PathBuf, found: i64 },
    #[error("cannot create the database directory: {0}")]
    Io(#[from] std::io::Error),
    #[error("the database thread has stopped")]
    Closed,
    #[error("a database job panicked: {0}")]
    Panicked(String),
}

type Job = Box<dyn FnOnce(&mut Connection) + Send>;

struct Inner {
    tx: Mutex<Option<mpsc::Sender<Job>>>,
    thread: Mutex<Option<JoinHandle<()>>>,
    path: Option<PathBuf>,
}

impl Drop for Inner {
    fn drop(&mut self) {
        // Close the queue, then wait until the thread has run every queued
        // job and closed the connection (the WAL is checkpointed on close).
        if let Ok(mut tx) = self.tx.lock() {
            tx.take();
        }
        if let Ok(mut thread) = self.thread.lock()
            && let Some(handle) = thread.take()
            && handle.thread().id() != std::thread::current().id()
        {
            let _ = handle.join();
        }
    }
}

/// A handle to one database. Clones share the DB thread; the thread stops
/// when the last clone is dropped.
#[derive(Clone)]
pub struct Db {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for Db {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Db")
            .field("path", &self.inner.path)
            .finish()
    }
}

impl Db {
    /// Opens (or creates) the database file, sets the pragmas and runs the
    /// migrations. The parent directory is created owner-only.
    pub fn open(path: &Path, migrations: Migrations<'static>) -> Result<Self, DbError> {
        if let Some(parent) = path.parent() {
            ensure_private_dir(parent)?;
        }
        let owned = path.to_path_buf();
        Self::start(Some(owned.clone()), migrations, move || {
            let conn = Connection::open(&owned)?;
            conn.pragma_update(None, "journal_mode", "WAL")?;
            Ok(conn)
        })
    }

    /// An in-memory database (tests).
    pub fn open_in_memory(migrations: Migrations<'static>) -> Result<Self, DbError> {
        Self::start(None, migrations, || Ok(Connection::open_in_memory()?))
    }

    fn start(
        path: Option<PathBuf>,
        migrations: Migrations<'static>,
        connect: impl FnOnce() -> Result<Connection, DbError> + Send + 'static,
    ) -> Result<Self, DbError> {
        let (tx, rx) = mpsc::channel::<Job>();
        let (ready_tx, ready_rx) = mpsc::channel::<Result<(), DbError>>();
        let label = path.clone();
        let name = format!(
            "db:{}",
            path.as_deref()
                .and_then(Path::file_name)
                .map_or("memory".into(), |n| n.to_string_lossy())
        );
        let thread = std::thread::Builder::new().name(name).spawn(move || {
            let mut conn = match connect().and_then(|c| prepare(c, label, migrations)) {
                Ok(conn) => {
                    let _ = ready_tx.send(Ok(()));
                    conn
                }
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                    return;
                }
            };
            while let Ok(job) = rx.recv() {
                job(&mut conn);
            }
        })?;
        ready_rx.recv().map_err(|_| DbError::Closed)??;
        Ok(Self {
            inner: Arc::new(Inner {
                tx: Mutex::new(Some(tx)),
                thread: Mutex::new(Some(thread)),
                path,
            }),
        })
    }

    pub fn path(&self) -> Option<&Path> {
        self.inner.path.as_deref()
    }

    fn submit<R: Send + 'static>(
        &self,
        f: impl FnOnce(&mut Connection) -> Result<R, DbError> + Send + 'static,
        reply: impl FnOnce(Result<R, DbError>) + Send + 'static,
    ) -> Result<(), DbError> {
        let job: Job = Box::new(move |conn| {
            let result = catch_unwind(AssertUnwindSafe(|| f(conn)))
                .unwrap_or_else(|p| Err(DbError::Panicked(panic_text(&*p))));
            reply(result);
        });
        let tx = self.inner.tx.lock().map_err(|_| DbError::Closed)?;
        tx.as_ref()
            .ok_or(DbError::Closed)?
            .send(job)
            .map_err(|_| DbError::Closed)
    }

    /// Runs `f` on the DB thread and waits for the result without blocking
    /// the async runtime.
    pub async fn call<R: Send + 'static>(
        &self,
        f: impl FnOnce(&mut Connection) -> Result<R, DbError> + Send + 'static,
    ) -> Result<R, DbError> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.submit(f, move |r| {
            let _ = tx.send(r);
        })?;
        rx.await.map_err(|_| DbError::Closed)?
    }

    /// Runs `f` on the DB thread and blocks the current thread until it is
    /// done. Do not call it on an async runtime thread or on the DB thread.
    pub fn call_blocking<R: Send + 'static>(
        &self,
        f: impl FnOnce(&mut Connection) -> Result<R, DbError> + Send + 'static,
    ) -> Result<R, DbError> {
        let (tx, rx) = mpsc::sync_channel(1);
        self.submit(f, move |r| {
            let _ = tx.send(r);
        })?;
        rx.recv().map_err(|_| DbError::Closed)?
    }
}

fn prepare(
    mut conn: Connection,
    path: Option<PathBuf>,
    migrations: Migrations<'static>,
) -> Result<Connection, DbError> {
    conn.busy_timeout(std::time::Duration::from_millis(BUSY_TIMEOUT_MS.into()))?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.pragma_update(None, "synchronous", "FULL")?;
    migrations.validate()?;
    let found: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
    match migrations.to_latest(&mut conn) {
        Ok(()) => Ok(conn),
        // A downgrade would run old code on a newer schema.
        Err(rusqlite_migration::Error::MigrationDefinition(
            rusqlite_migration::MigrationDefinitionError::DatabaseTooFarAhead,
        )) => Err(DbError::TooNew {
            path: path.unwrap_or_default(),
            found,
        }),
        Err(e) => Err(e.into()),
    }
}

fn panic_text(p: &(dyn std::any::Any + Send)) -> String {
    p.downcast_ref::<&str>()
        .map(|s| (*s).to_owned())
        .or_else(|| p.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "unknown panic".to_owned())
}

#[cfg(test)]
#[path = "db_tests.rs"]
mod tests;
