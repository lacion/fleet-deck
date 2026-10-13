//! The durable record of a running setup command (DL-148).
//!
//! [`Store::setup_run_begin`] commits the row **before** the command gets its
//! go signal. The row ends after the process group is gone
//! ([`Store::setup_run_end`]). A row that is still there at the next station
//! start belongs to a setup process that may have outlived the station: the
//! restart cleanup ([`crate::workspace::setup_cleanup`]) looks for it.

use fleetdeck_proto::SessionId;
use rusqlite::params;

use super::{Result, Store, call};

/// One stored setup run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetupRun {
    pub effect_id: String,
    pub session_id: SessionId,
    pub pgid: i32,
    pub started_at: i64,
}

impl Store {
    /// Stores the row of a setup run before its command can run.
    ///
    /// # Errors
    /// A database error; no row is stored.
    pub async fn setup_run_begin(
        &self,
        effect_id: String,
        session_id: SessionId,
        pgid: i32,
    ) -> Result<()> {
        let now = self.now();
        call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            tx.execute(
                "INSERT INTO setup_runs (effect_id, session_id, pgid, started_at)
                 VALUES (?1, ?2, ?3, ?4)",
                params![effect_id, session_id.as_str(), pgid, now],
            )?;
            tx.commit()?;
            Ok(())
        })
        .await
    }

    /// Deletes the row of a setup run whose process group is gone.
    ///
    /// # Errors
    /// A database error.
    pub async fn setup_run_end(&self, effect_id: String) -> Result<()> {
        call(&self.inner.db, move |conn| {
            conn.execute("DELETE FROM setup_runs WHERE effect_id = ?1", [effect_id])?;
            Ok(())
        })
        .await
    }

    /// Every stored setup run, oldest first.
    ///
    /// # Errors
    /// A database error.
    pub async fn setup_runs(&self) -> Result<Vec<SetupRun>> {
        call(&self.inner.db, move |conn| {
            let mut stmt = conn.prepare(
                "SELECT effect_id, session_id, pgid, started_at FROM setup_runs
                 ORDER BY started_at, effect_id",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok(SetupRun {
                    effect_id: r.get(0)?,
                    session_id: SessionId::from(r.get::<_, String>(1)?),
                    pgid: r.get(2)?,
                    started_at: r.get(3)?,
                })
            })?;
            Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
        })
        .await
    }
}
