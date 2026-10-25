//! The journal and the quarantine of provider CLI updates (B28, design
//! `cli-update.md` §3.7, §3.7.2, §3.8.1 and §3.11).
//!
//! The journal row is written with the attempt row before any change and is
//! deleted only in the transaction that records the final result. Every
//! method that the design calls "one transaction" is one SQLite transaction.

use std::path::{Path, PathBuf};

use fleetdeck_proto::cli_update::{InstallMethod, UpdateOutcome};
use fleetdeck_proto::event::ProviderKind;
use rusqlite::{OptionalExtension, Row, Transaction, params};
use serde::{Deserialize, Serialize};

use super::cli_updates::{AttemptEnd, NewAttempt, end_in, enum_from, enum_text, insert_in};
use super::{Result, Store, StoreError, call};

/// The phase of a journal row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JournalPhase {
    /// Nothing of the install changed yet.
    Staging,
    /// The activation started; the install may have changed.
    Activating,
}

/// The journal row of a new attempt (phase `staging`, marker = attempt id).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewJournal {
    pub provider: ProviderKind,
    pub method: InstallMethod,
    pub from_version: String,
    pub target_version: String,
    pub launcher: PathBuf,
    pub dest: PathBuf,
    pub stage_root: PathBuf,
    pub rollback_sha256: Option<String>,
}

/// One entry of the npm or Bun folders that the activation moved.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MovedEntry {
    pub name: String,
    pub digest: Option<String>,
}

/// What the rollback copy step writes before the activation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Activation {
    pub rollback_path: PathBuf,
    pub rollback_sha256: Option<String>,
    pub moved: Vec<MovedEntry>,
}

/// One journal row, for the run and the recovery (internal; never on the wire).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryRecord {
    pub provider: ProviderKind,
    pub attempt_id: String,
    pub marker: String,
    pub method: InstallMethod,
    pub phase: JournalPhase,
    pub from_version: String,
    pub target_version: String,
    pub launcher: PathBuf,
    pub dest: PathBuf,
    pub stage_root: PathBuf,
    pub rollback_path: Option<PathBuf>,
    pub rollback_sha256: Option<String>,
    pub moved: Vec<MovedEntry>,
    pub installer_pgid: Option<i32>,
}

/// What `cli_update_finish` does with the journal row of the attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JournalAction {
    Delete,
    Keep,
}

/// What `cli_update_finish` does with the quarantine row of the provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QuarantineAction {
    Clear,
    Set { reason: &'static str, text: String },
    Keep,
}

/// One quarantine row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Quarantine {
    pub provider: ProviderKind,
    pub attempt_id: String,
    pub reason: String,
    pub text: String,
    pub since: i64,
}

fn path_text(p: &Path) -> Result<&str> {
    p.to_str()
        .ok_or_else(|| StoreError::Corrupt(format!("path {} is not UTF-8", p.display())))
}

const JOURNAL_COLUMNS: &str = "provider, attempt_id, marker, method, phase, from_version, \
     target_version, launcher, dest, stage_root, rollback_path, rollback_sha256, moved, \
     installer_pgid";

struct RawRow {
    provider: String,
    attempt_id: String,
    marker: String,
    method: String,
    phase: String,
    from_version: String,
    target_version: String,
    launcher: String,
    dest: String,
    stage_root: String,
    rollback_path: Option<String>,
    rollback_sha256: Option<String>,
    moved: Option<String>,
    installer_pgid: Option<i32>,
}

fn read(r: &Row<'_>) -> rusqlite::Result<RawRow> {
    Ok(RawRow {
        provider: r.get(0)?,
        attempt_id: r.get(1)?,
        marker: r.get(2)?,
        method: r.get(3)?,
        phase: r.get(4)?,
        from_version: r.get(5)?,
        target_version: r.get(6)?,
        launcher: r.get(7)?,
        dest: r.get(8)?,
        stage_root: r.get(9)?,
        rollback_path: r.get(10)?,
        rollback_sha256: r.get(11)?,
        moved: r.get(12)?,
        installer_pgid: r.get(13)?,
    })
}

fn record(raw: RawRow) -> Result<RecoveryRecord> {
    let moved = match raw.moved.as_deref() {
        None | Some("") => Vec::new(),
        Some(text) => serde_json::from_str(text)?,
    };
    Ok(RecoveryRecord {
        provider: enum_from(raw.provider)?,
        attempt_id: raw.attempt_id,
        marker: raw.marker,
        method: enum_from(raw.method)?,
        phase: enum_from(raw.phase)?,
        from_version: raw.from_version,
        target_version: raw.target_version,
        launcher: PathBuf::from(raw.launcher),
        dest: PathBuf::from(raw.dest),
        stage_root: PathBuf::from(raw.stage_root),
        rollback_path: raw.rollback_path.map(PathBuf::from),
        rollback_sha256: raw.rollback_sha256,
        moved,
        installer_pgid: raw.installer_pgid,
    })
}

fn journal_of(tx: &Transaction<'_>, provider: &str) -> Result<Option<RecoveryRecord>> {
    let raw = tx
        .query_row(
            &format!("SELECT {JOURNAL_COLUMNS} FROM cli_update_journal WHERE provider = ?1"),
            [provider],
            read,
        )
        .optional()?;
    raw.map(record).transpose()
}

fn upsert_quarantine(
    tx: &Transaction<'_>,
    provider: &str,
    attempt_id: &str,
    reason: &str,
    text: &str,
    now: i64,
) -> Result<()> {
    tx.execute(
        "INSERT OR REPLACE INTO cli_update_quarantine (provider, attempt_id, reason, text, since)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![provider, attempt_id, reason, text, now],
    )?;
    Ok(())
}

fn no_journal(provider: &str) -> StoreError {
    StoreError::Corrupt(format!("no update journal row for {provider}"))
}

impl Store {
    /// §3.7 step 10: the attempt row and the journal row (phase `staging`,
    /// marker = the attempt id) in one transaction. A journal row of the
    /// provider exists: an error, and nothing is written.
    ///
    /// # Errors
    /// A database error, also the unique key of the journal.
    pub async fn cli_update_begin(&self, a: NewAttempt, j: NewJournal) -> Result<u64> {
        let now = self.now();
        call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            let seq = insert_in(&tx, &a, None, now)?;
            tx.execute(
                "INSERT INTO cli_update_journal
                   (provider, attempt_id, marker, method, phase, from_version, target_version,
                    launcher, dest, stage_root, rollback_sha256, updated_at)
                 VALUES (?1, ?2, ?2, ?3, 'staging', ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    enum_text(&j.provider)?,
                    a.attempt_id,
                    enum_text(&j.method)?,
                    j.from_version,
                    j.target_version,
                    path_text(&j.launcher)?,
                    path_text(&j.dest)?,
                    path_text(&j.stage_root)?,
                    j.rollback_sha256,
                    now
                ],
            )?;
            tx.commit()?;
            Ok(seq)
        })
        .await
    }

    /// Stores the process group of the installer child.
    ///
    /// # Errors
    /// A database error, or no journal row for the provider.
    pub async fn cli_update_journal_pgid(&self, provider: ProviderKind, pgid: i32) -> Result<()> {
        let now = self.now();
        let provider = enum_text(&provider)?;
        call(&self.inner.db, move |conn| {
            let n = conn.execute(
                "UPDATE cli_update_journal SET installer_pgid = ?2, updated_at = ?3
                  WHERE provider = ?1",
                params![provider, pgid, now],
            )?;
            if n == 1 {
                Ok(())
            } else {
                Err(no_journal(&provider))
            }
        })
        .await
    }

    /// §3.6 A1: phase `activating` and the rollback data, committed before A2.
    ///
    /// # Errors
    /// A database error, or no journal row for the provider.
    pub async fn cli_update_journal_activate(
        &self,
        provider: ProviderKind,
        act: Activation,
    ) -> Result<()> {
        let now = self.now();
        let provider = enum_text(&provider)?;
        call(&self.inner.db, move |conn| {
            let moved = serde_json::to_string(&act.moved)?;
            let n = conn.execute(
                "UPDATE cli_update_journal
                    SET phase = 'activating', rollback_path = ?2, rollback_sha256 = ?3,
                        moved = ?4, updated_at = ?5
                  WHERE provider = ?1",
                params![
                    provider,
                    path_text(&act.rollback_path)?,
                    act.rollback_sha256,
                    moved,
                    now
                ],
            )?;
            if n == 1 {
                Ok(())
            } else {
                Err(no_journal(&provider))
            }
        })
        .await
    }

    /// Every journal row, Claude first, then Codex.
    ///
    /// # Errors
    /// A database error.
    pub async fn cli_update_journals(&self) -> Result<Vec<RecoveryRecord>> {
        call(&self.inner.db, move |conn| {
            let mut stmt = conn.prepare(&format!(
                "SELECT {JOURNAL_COLUMNS} FROM cli_update_journal ORDER BY provider"
            ))?;
            let raws = stmt
                .query_map([], read)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            raws.into_iter().map(record).collect()
        })
        .await
    }

    /// §3.7.2 c, one transaction: ends the journal's attempt `interrupted`
    /// when it is running, inserts `successor`, points the journal row at it
    /// (the marker stays), and upserts the quarantine row `recovery_pending`
    /// with `text`. Returns the updated record.
    ///
    /// # Errors
    /// A database error, or no journal row for the provider. Nothing is written.
    pub async fn cli_update_recover_begin(
        &self,
        provider: ProviderKind,
        successor: NewAttempt,
        text: String,
    ) -> Result<RecoveryRecord> {
        let now = self.now();
        let provider = enum_text(&provider)?;
        call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            let old = journal_of(&tx, &provider)?.ok_or_else(|| no_journal(&provider))?;
            tx.execute(
                "UPDATE cli_update_attempts SET outcome = 'interrupted', ended_at = ?2
                  WHERE attempt_id = ?1 AND outcome = 'running'",
                params![old.attempt_id, now],
            )?;
            insert_in(&tx, &successor, None, now)?;
            tx.execute(
                "UPDATE cli_update_journal SET attempt_id = ?2, updated_at = ?3
                  WHERE provider = ?1",
                params![provider, successor.attempt_id, now],
            )?;
            upsert_quarantine(
                &tx,
                &provider,
                &successor.attempt_id,
                "recovery_pending",
                &text,
                now,
            )?;
            let updated = journal_of(&tx, &provider)?.ok_or_else(|| no_journal(&provider))?;
            tx.commit()?;
            Ok(updated)
        })
        .await
    }

    /// One transaction: ends `attempt_id`, then applies `journal` and
    /// `quarantine` to the provider of that attempt.
    ///
    /// # Errors
    /// A database error, or an unknown attempt id. Nothing is written.
    pub async fn cli_update_finish(
        &self,
        attempt_id: String,
        end: AttemptEnd,
        journal: JournalAction,
        quarantine: QuarantineAction,
    ) -> Result<()> {
        let now = self.now();
        call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            let provider = end_in(&tx, &attempt_id, &end, now)?;
            if journal == JournalAction::Delete {
                tx.execute(
                    "DELETE FROM cli_update_journal WHERE attempt_id = ?1",
                    [&attempt_id],
                )?;
            }
            match quarantine {
                QuarantineAction::Keep => {}
                QuarantineAction::Clear => {
                    tx.execute(
                        "DELETE FROM cli_update_quarantine WHERE provider = ?1",
                        [&provider],
                    )?;
                }
                QuarantineAction::Set { reason, text } => {
                    upsert_quarantine(&tx, &provider, &attempt_id, reason, &text, now)?;
                }
            }
            tx.commit()?;
            Ok(())
        })
        .await
    }

    /// §3.8.1 step 5, one transaction: the `verified` row (already ended) and
    /// the delete of the quarantine row of its provider. Returns `seq`.
    ///
    /// # Errors
    /// A database error. Nothing is written.
    pub async fn cli_update_verified(&self, a: NewAttempt, mut end: AttemptEnd) -> Result<u64> {
        let now = self.now();
        end.outcome = UpdateOutcome::Verified;
        call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            let seq = insert_in(&tx, &a, Some(&end), now)?;
            tx.execute(
                "DELETE FROM cli_update_quarantine WHERE provider = ?1",
                [enum_text(&a.provider)?],
            )?;
            tx.commit()?;
            Ok(seq)
        })
        .await
    }

    /// Every quarantine row.
    ///
    /// # Errors
    /// A database error.
    pub async fn cli_update_quarantines(&self) -> Result<Vec<Quarantine>> {
        call(&self.inner.db, move |conn| {
            let mut stmt = conn.prepare(
                "SELECT provider, attempt_id, reason, text, since
                   FROM cli_update_quarantine ORDER BY provider",
            )?;
            let rows = stmt
                .query_map([], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, i64>(4)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows.into_iter()
                .map(|(provider, attempt_id, reason, text, since)| {
                    Ok(Quarantine {
                        provider: enum_from(provider)?,
                        attempt_id,
                        reason,
                        text,
                        since,
                    })
                })
                .collect()
        })
        .await
    }
}

#[cfg(test)]
#[path = "cli_update_journal_tests.rs"]
mod cli_update_journal_tests;
