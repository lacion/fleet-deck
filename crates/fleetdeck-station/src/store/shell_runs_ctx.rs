//! The reads and guarded writes that the shell execution and the model
//! context need on top of [`super`] (B8, DL-196): the attachable rows in a
//! decision, the start-up classification of the claimed rows, and a settle
//! that is guarded by the state that the caller read.

use fleetdeck_proto::SessionId;
use rusqlite::{Connection, params};

use super::{
    Decision, Result, ShellRow, ShellState, Store, StoreError, attachable, call, include, select,
};

/// The Claude rows that the previous station process may have started: a
/// `running` row, or a `queued` row whose effect was claimed (or ended as
/// `outcome_unknown`).
fn claimed(conn: &Connection) -> Result<Vec<ShellRow>> {
    select(
        conn,
        "provider = 'claude' AND (state = 'running' OR (state = 'queued' AND effect_id IN
           (SELECT effect_id FROM effects WHERE state IN ('claimed', 'outcome_unknown'))))",
        &[],
    )
}

/// Settles a row to the terminal `to`, only while its state is `from`.
fn settle_from(
    conn: &Connection,
    effect_id: &str,
    from: ShellState,
    to: ShellState,
    now: i64,
) -> Result<bool> {
    if !to.is_terminal() || from.is_terminal() {
        return Err(StoreError::Corrupt(format!(
            "a shell row settles from an open state to a terminal one, not {} to {}",
            from.as_str(),
            to.as_str()
        )));
    }
    let n = conn.execute(
        "UPDATE shell_runs SET state = ?3, advance_pending = 1, ended_at = ?4
         WHERE effect_id = ?1 AND state = ?2",
        params![effect_id, from.as_str(), to.as_str(), now],
    )?;
    Ok(n == 1)
}

impl Decision<'_> {
    /// The Claude rows of a session that a later input can carry (B8).
    ///
    /// # Errors
    /// A database error.
    pub fn shell_attachable(&self, session: &SessionId) -> Result<Vec<ShellRow>> {
        attachable(self.connection(), session)
    }

    /// Marks rows as carried by `input_id`; returns how many changed (B8).
    ///
    /// # Errors
    /// A database error.
    pub fn shell_include(&mut self, effect_ids: &[String], input_id: &str) -> Result<usize> {
        include(self.connection(), effect_ids, input_id)
    }
}

impl Store {
    /// The rows that the start-up settles `unknown` after the marked-process
    /// kill (see [`claimed`]).
    ///
    /// # Errors
    /// A database error.
    pub async fn shell_claimed_rows(&self) -> Result<Vec<ShellRow>> {
        call(&self.inner.db, move |conn| claimed(conn)).await
    }

    /// Settles a row to `to` in one guarded transaction: only while its state
    /// is still `from`. `false` when another path settled it first.
    ///
    /// # Errors
    /// A database error.
    pub async fn shell_settle_from(
        &self,
        effect_id: String,
        from: ShellState,
        to: ShellState,
    ) -> Result<bool> {
        let now = self.now();
        call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            let moved = settle_from(&tx, &effect_id, from, to, now)?;
            tx.commit()?;
            Ok(moved)
        })
        .await
    }
}
