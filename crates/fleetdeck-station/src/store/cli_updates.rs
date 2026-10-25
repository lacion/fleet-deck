//! The attempts of provider CLI updates (B28, design `cli-update.md` §3.11).
//!
//! One row per attempt in `cli_update_attempts`. A row is inserted `running`
//! or already ended. The deck imports only finished rows, and only those
//! before the first `running` row, so a `seq` that the deck saw never gets
//! a late neighbour below it.

use fleetdeck_proto::cli_update::{CliUpdateAttempt, InstallMethod, UpdateOutcome, UpdateTrigger};
use fleetdeck_proto::event::ProviderKind;
use rusqlite::{Connection, Row, Transaction, params};
use serde::Serialize;
use serde::de::DeserializeOwned;

use super::{Result, Store, StoreError, call, to_u64};

/// The fields of a new attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewAttempt {
    pub attempt_id: String,
    pub provider: ProviderKind,
    pub trigger: UpdateTrigger,
    pub method: InstallMethod,
    /// `stable`, `latest` or `pin`.
    pub channel: String,
    pub from_version: Option<String>,
    pub target_version: Option<String>,
}

/// How an attempt ends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttemptEnd {
    pub outcome: UpdateOutcome,
    pub reason: Option<String>,
    pub to_version: Option<String>,
    /// `Some` replaces the target of the row; `None` keeps it.
    pub target_version: Option<String>,
}

/// The snake_case text of a wire enum, as the columns keep it.
pub(super) fn enum_text<T: Serialize>(value: &T) -> Result<String> {
    match serde_json::to_value(value)? {
        serde_json::Value::String(s) => Ok(s),
        other => Err(StoreError::Corrupt(format!("enum value {other}"))),
    }
}

/// The wire enum of a stored text.
pub(super) fn enum_from<T: DeserializeOwned>(text: String) -> Result<T> {
    Ok(serde_json::from_value(serde_json::Value::String(text))?)
}

const COLUMNS: &str = "seq, attempt_id, provider, trigger, method, channel, from_version, \
     target_version, to_version, outcome, reason, started_at, ended_at";

fn read(r: &Row<'_>) -> rusqlite::Result<RawAttempt> {
    Ok(RawAttempt {
        seq: r.get(0)?,
        attempt_id: r.get(1)?,
        provider: r.get(2)?,
        trigger: r.get(3)?,
        method: r.get(4)?,
        channel: r.get(5)?,
        from_version: r.get(6)?,
        target_version: r.get(7)?,
        to_version: r.get(8)?,
        outcome: r.get(9)?,
        reason: r.get(10)?,
        started_at: r.get(11)?,
        ended_at: r.get(12)?,
    })
}

struct RawAttempt {
    seq: i64,
    attempt_id: String,
    provider: String,
    trigger: String,
    method: String,
    channel: String,
    from_version: Option<String>,
    target_version: Option<String>,
    to_version: Option<String>,
    outcome: String,
    reason: Option<String>,
    started_at: i64,
    ended_at: Option<i64>,
}

impl RawAttempt {
    fn into_attempt(self) -> Result<CliUpdateAttempt> {
        Ok(CliUpdateAttempt {
            seq: to_u64(self.seq)?,
            attempt_id: self.attempt_id,
            provider: enum_from(self.provider)?,
            trigger: enum_from(self.trigger)?,
            method: enum_from(self.method)?,
            channel: self.channel,
            from_version: self.from_version,
            target_version: self.target_version,
            to_version: self.to_version,
            outcome: enum_from(self.outcome)?,
            reason: self.reason,
            started_at: self.started_at,
            ended_at: self.ended_at,
        })
    }
}

/// Inserts a row inside a transaction; returns `seq`.
pub(super) fn insert_in(
    tx: &Transaction<'_>,
    a: &NewAttempt,
    end: Option<&AttemptEnd>,
    now: i64,
) -> Result<u64> {
    let (outcome, reason, to_version, target, ended_at) = match end {
        Some(e) => (
            enum_text(&e.outcome)?,
            e.reason.clone(),
            e.to_version.clone(),
            e.target_version
                .clone()
                .or_else(|| a.target_version.clone()),
            Some(now),
        ),
        None => (
            enum_text(&UpdateOutcome::Running)?,
            None,
            None,
            a.target_version.clone(),
            None,
        ),
    };
    tx.execute(
        "INSERT INTO cli_update_attempts
           (attempt_id, provider, trigger, method, channel, from_version, target_version,
            to_version, outcome, reason, started_at, ended_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        params![
            a.attempt_id,
            enum_text(&a.provider)?,
            enum_text(&a.trigger)?,
            enum_text(&a.method)?,
            a.channel,
            a.from_version,
            target,
            to_version,
            outcome,
            reason,
            now,
            ended_at
        ],
    )?;
    to_u64(tx.last_insert_rowid())
}

/// Ends the row `attempt_id`; returns its provider text. An unknown id is an error.
pub(super) fn end_in(
    tx: &Transaction<'_>,
    attempt_id: &str,
    end: &AttemptEnd,
    now: i64,
) -> Result<String> {
    let changed = tx.execute(
        "UPDATE cli_update_attempts
            SET outcome = ?2, reason = ?3, to_version = ?4,
                target_version = COALESCE(?5, target_version), ended_at = ?6
          WHERE attempt_id = ?1",
        params![
            attempt_id,
            enum_text(&end.outcome)?,
            end.reason,
            end.to_version,
            end.target_version,
            now
        ],
    )?;
    if changed != 1 {
        return Err(StoreError::Corrupt(format!(
            "no update attempt {attempt_id}"
        )));
    }
    Ok(tx.query_row(
        "SELECT provider FROM cli_update_attempts WHERE attempt_id = ?1",
        [attempt_id],
        |r| r.get(0),
    )?)
}

fn page(conn: &Connection, after_seq: i64, limit: u32) -> Result<Vec<CliUpdateAttempt>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLUMNS} FROM cli_update_attempts
          WHERE seq > ?1
            AND seq < COALESCE((SELECT MIN(seq) FROM cli_update_attempts
                                 WHERE outcome = 'running'), 9223372036854775807)
          ORDER BY seq LIMIT ?2"
    ))?;
    let rows = stmt
        .query_map(params![after_seq, i64::from(limit)], read)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    rows.into_iter().map(RawAttempt::into_attempt).collect()
}

impl Store {
    /// Inserts a row (outcome `running`, or the final outcome when `end` is
    /// given); returns `seq`.
    ///
    /// # Errors
    /// A database error (a duplicate attempt id, or a provider that is not Claude or Codex).
    pub async fn cli_update_insert(&self, a: NewAttempt, end: Option<AttemptEnd>) -> Result<u64> {
        let now = self.now();
        call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            let seq = insert_in(&tx, &a, end.as_ref(), now)?;
            tx.commit()?;
            Ok(seq)
        })
        .await
    }

    /// Finished rows after `after_seq`, oldest first, at most `limit`,
    /// stopping before the first `running` row.
    ///
    /// # Errors
    /// A database error.
    pub async fn cli_update_attempts(
        &self,
        after_seq: u64,
        limit: u32,
    ) -> Result<Vec<CliUpdateAttempt>> {
        let after = i64::try_from(after_seq).unwrap_or(i64::MAX);
        call(&self.inner.db, move |conn| page(conn, after, limit)).await
    }

    /// The newest row of a provider, `running` or not.
    ///
    /// # Errors
    /// A database error.
    pub async fn cli_update_last(
        &self,
        provider: ProviderKind,
    ) -> Result<Option<CliUpdateAttempt>> {
        let provider = enum_text(&provider)?;
        call(&self.inner.db, move |conn| {
            let mut stmt = conn.prepare(&format!(
                "SELECT {COLUMNS} FROM cli_update_attempts
                  WHERE provider = ?1 ORDER BY seq DESC LIMIT 1"
            ))?;
            let raw = stmt
                .query_map([provider], read)?
                .collect::<rusqlite::Result<Vec<_>>>()?
                .pop();
            raw.map(RawAttempt::into_attempt).transpose()
        })
        .await
    }

    /// The highest `seq` of a finished row; 0 when there is none.
    ///
    /// # Errors
    /// A database error.
    pub async fn cli_update_last_seq(&self) -> Result<u64> {
        call(&self.inner.db, move |conn| {
            let seq: i64 = conn.query_row(
                "SELECT COALESCE(MAX(seq), 0) FROM cli_update_attempts WHERE outcome != 'running'",
                [],
                |r| r.get(0),
            )?;
            to_u64(seq)
        })
        .await
    }

    /// Marks every `running` row without a journal row `interrupted`;
    /// returns the count.
    ///
    /// # Errors
    /// A database error.
    pub async fn cli_update_interrupt_orphans(&self) -> Result<usize> {
        let now = self.now();
        call(&self.inner.db, move |conn| {
            Ok(conn.execute(
                "UPDATE cli_update_attempts
                    SET outcome = 'interrupted', ended_at = ?1
                  WHERE outcome = 'running'
                    AND attempt_id NOT IN (SELECT attempt_id FROM cli_update_journal)",
                [now],
            )?)
        })
        .await
    }

    /// Keeps the newest `keep` rows, and never a row that a journal row or a
    /// quarantine row names; returns the count deleted.
    ///
    /// # Errors
    /// A database error.
    pub async fn cli_update_prune(&self, keep: u32) -> Result<usize> {
        call(&self.inner.db, move |conn| {
            Ok(conn.execute(
                "DELETE FROM cli_update_attempts
                  WHERE seq NOT IN (SELECT seq FROM cli_update_attempts
                                     ORDER BY seq DESC LIMIT ?1)
                    AND attempt_id NOT IN (SELECT attempt_id FROM cli_update_journal)
                    AND attempt_id NOT IN (SELECT attempt_id FROM cli_update_quarantine)",
                [i64::from(keep)],
            )?)
        })
        .await
    }
}

#[cfg(test)]
#[path = "cli_updates_tests.rs"]
pub(in crate::store) mod cli_updates_tests;
