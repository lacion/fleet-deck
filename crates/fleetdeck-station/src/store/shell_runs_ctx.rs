//! The reads and guarded writes that the shell execution and the model
//! context need on top of [`super`] (B8, DL-196): the attachable rows in a
//! decision, the start-up classification of the claimed rows, and a settle
//! that is guarded by the state that the caller read.

use fleetdeck_proto::SessionId;
use fleetdeck_proto::event::{Event, ToolKind};
use rusqlite::{Connection, OptionalExtension, params};

use super::super::log;
use super::{
    Decision, Result, ShellRow, ShellState, Store, StoreError, attachable, call, include, select,
    set_awaiting_proof,
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

/// The Codex rows that the previous station process may have sent: a
/// `running` row, or a `queued` row whose effect was claimed (or ended as
/// `outcome_unknown`).
fn codex_sent(conn: &Connection) -> Result<Vec<ShellRow>> {
    select(
        conn,
        "provider = 'codex' AND (state = 'running' OR (state = 'queued' AND effect_id IN
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

/// Does the session have a `tool.started` of the node `shell-<effect id>`?
fn has_card(conn: &Connection, session: &SessionId, effect_id: &str) -> Result<bool> {
    Ok(conn
        .query_row(
            "SELECT 1 FROM events WHERE session_id = ?1 AND kind = 'tool.started'
               AND json_extract(payload, '$.node_id') = ?2 LIMIT 1",
            params![session.as_str(), format!("shell-{effect_id}")],
            |r| r.get::<_, i64>(0),
        )
        .optional()?
        .is_some())
}

/// The guarded move to `awaiting_proof` and, when the row moved and the
/// session has no card for the command yet, the card (B8 PRE-PR r1).
/// Returns whether the row moved and the sequence of the card, if any.
fn await_proof(
    conn: &Connection,
    effect_id: &str,
    from: ShellState,
    now: i64,
) -> Result<(bool, Option<u64>)> {
    if !set_awaiting_proof(conn, effect_id, from)? {
        return Ok((false, None));
    }
    let Some(row) = super::get(conn, effect_id)? else {
        return Ok((true, None));
    };
    if has_card(conn, &row.session_id, effect_id)? {
        return Ok((true, None));
    }
    let mut refs = serde_json::Map::new();
    refs.insert("station".to_owned(), "awaiting_proof".into());
    let card = Event::ToolStarted {
        node_id: format!("shell-{effect_id}").into(),
        parent_node_id: None,
        tool_kind: ToolKind::Shell,
        title: "Shell command".to_owned(),
        input_summary: row.command,
        files: Vec::new(),
        provider_refs: refs,
    };
    match log::append_one(conn, &row.session_id, card.into(), now) {
        Ok(envelope) => Ok((true, Some(envelope.station_seq))),
        // The session is gone: the row still awaits its proof.
        Err(StoreError::UnknownSession(_)) => {
            tracing::warn!(effect = %effect_id, "the session of a shell row is gone");
            Ok((true, None))
        }
        Err(e) => Err(e),
    }
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

    /// The Codex rows that the start-up reclassifies (see [`codex_sent`]).
    ///
    /// # Errors
    /// A database error.
    pub async fn shell_codex_sent_rows(&self) -> Result<Vec<ShellRow>> {
        call(&self.inner.db, move |conn| codex_sent(conn)).await
    }

    /// Moves a row from `from` to `awaiting_proof` and gives a command that
    /// has no card yet its card, in one transaction (B8 PRE-PR r1).
    /// `false` when the row was not in `from`.
    ///
    /// # Errors
    /// A database error.
    pub async fn shell_await_proof(&self, effect_id: String, from: ShellState) -> Result<bool> {
        let now = self.now();
        let (moved, seq) = call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            let out = await_proof(&tx, &effect_id, from, now)?;
            tx.commit()?;
            Ok(out)
        })
        .await?;
        if let Some(seq) = seq {
            self.publish(seq);
        }
        Ok(moved)
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
