//! The setup reports of unpaired keys and their retry permissions (BG-3,
//! design `docs/v2/design/board-gaps.md` sections 4.2, 4.4 and 4.6).
//!
//! One latest report for each authenticated key, at most [`MAX_REPORTS`]
//! rows, rows kept for [`RETENTION_MS`] after the last contact. A report
//! never creates a station row, a pin, a grant or an invitation: the rows
//! hold only the validated fields. A permission to retry is bound to the
//! exact `terminal_id` and `state_digest` of the report and is delivered
//! once. Every call is one transaction.
//!
//! Forward only. To reverse the migration (V39), drop `provision_retries`
//! and then `provision_reports`; the rows are diagnostics that expire by
//! themselves, so nothing else refers to them.

use fleetdeck_link::report_accept::ValidReport;
use rusqlite::{OptionalExtension, Transaction, params};

use super::{DeckStore, Result, call};

/// The migration (V39).
pub(super) const SQL: &str = "
CREATE TABLE provision_reports (          -- BG-3: the latest setup report of each unpaired key
    fingerprint  TEXT    PRIMARY KEY,     -- the full fingerprint of the authenticated key
    terminal_id  TEXT    NOT NULL,
    state_digest TEXT    NOT NULL,
    code         TEXT    NOT NULL,
    phase        TEXT    NOT NULL,
    name         TEXT    NOT NULL,        -- self-reported
    received_at  INTEGER NOT NULL,
    last_contact INTEGER NOT NULL,
    updated_at   INTEGER NOT NULL
) STRICT;
CREATE TABLE provision_retries (          -- BG-3: one retry permission for each key
    fingerprint  TEXT    PRIMARY KEY,
    retry_id     TEXT    NOT NULL,
    terminal_id  TEXT    NOT NULL,
    state_digest TEXT    NOT NULL,
    request_id   TEXT    NOT NULL,
    created_at   INTEGER NOT NULL,
    expires_at   INTEGER NOT NULL,
    delivered_at INTEGER
) STRICT;
";

/// The most reports that the deck keeps.
pub const MAX_REPORTS: i64 = 64;
/// A report row is kept this long after the last contact (24 hours).
pub const RETENTION_MS: i64 = 24 * 60 * 60 * 1000;
/// One accepted update of a key in this time (5 minutes).
pub const RATE_MS: i64 = 5 * 60 * 1000;
/// A retry permission lasts this long (20 minutes).
pub const RETRY_TTL_MS: i64 = 20 * 60 * 1000;

/// What the store did with a report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The report is stored (a new key, or a rewrite).
    Stored,
    /// The key reported less than five minutes ago; this many seconds
    /// remain (at least 1).
    RateLimited(u64),
    /// The deck holds [`MAX_REPORTS`] rows and the key is new.
    Capacity,
    /// The owner allowed a retry for this exact report; it is now
    /// delivered.
    Retry { retry_id: String },
}

/// The result of [`DeckStore::apply_provision_report`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Applied {
    pub outcome: Outcome,
    /// True when the call inserted or rewrote the row (it counts toward the
    /// global limit).
    pub counted: bool,
}

/// The state of a retry permission that is not expired.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryState {
    pub delivered: bool,
    pub expires_at: i64,
}

/// One stored report, with its live retry permission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredReport {
    pub fingerprint: String,
    pub terminal_id: String,
    pub state_digest: String,
    pub code: String,
    pub phase: String,
    pub name: String,
    pub received_at: i64,
    pub last_contact: i64,
    pub retry: Option<RetryState>,
}

/// A request to allow one retry.
#[derive(Debug, Clone)]
pub struct AllowRetry {
    pub fingerprint: String,
    pub terminal_id: String,
    pub state_digest: String,
    pub request_id: String,
    /// The id of a new permission (32 hex digits).
    pub retry_id: String,
    pub now: i64,
    /// True for a code that a retry can repair.
    pub code_allowed: fn(&str) -> bool,
}

/// The result of [`DeckStore::allow_provision_retry`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Allowed {
    /// The key has no report.
    NoReport,
    /// The binding of the request is not the binding of the report.
    Changed,
    /// The code of the report cannot be repaired by a retry.
    NotAllowed,
    /// A permission with another request id is live.
    Queued,
    /// The permission of this request: new, or the one that this request
    /// id made before.
    Granted(RetryState),
}

/// The result of [`DeckStore::cancel_provision_retry`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cancelled {
    NoReport,
    NoRetry,
    /// The station got the permission already.
    Delivered,
    Done,
}

/// Deletes the rows that expired and the permissions of a missing report.
fn prune(tx: &Transaction<'_>, now: i64) -> Result<()> {
    tx.execute(
        "DELETE FROM provision_reports WHERE last_contact < ?1",
        [now - RETENTION_MS],
    )?;
    tx.execute(
        "DELETE FROM provision_retries
         WHERE expires_at <= ?1
            OR fingerprint NOT IN (SELECT fingerprint FROM provision_reports)",
        [now],
    )?;
    Ok(())
}

/// The live permission of `fingerprint`, when it is not expired.
fn live_retry(tx: &Transaction<'_>, fingerprint: &str, now: i64) -> Result<Option<RetryState>> {
    Ok(tx
        .query_row(
            "SELECT delivered_at IS NOT NULL, expires_at FROM provision_retries
             WHERE fingerprint = ?1 AND expires_at > ?2",
            params![fingerprint, now],
            |r| {
                Ok(RetryState {
                    delivered: r.get(0)?,
                    expires_at: r.get(1)?,
                })
            },
        )
        .optional()?)
}

/// Delivers the permission of `fingerprint` when it is live, not delivered
/// and bound to `report`. Returns its id.
fn deliver(
    tx: &Transaction<'_>,
    fingerprint: &str,
    report: &ValidReport,
    now: i64,
) -> Result<Option<String>> {
    let delivered = tx.execute(
        "UPDATE provision_retries SET delivered_at = ?1
         WHERE fingerprint = ?2 AND delivered_at IS NULL AND expires_at > ?1
           AND terminal_id = ?3 AND state_digest = ?4",
        params![now, fingerprint, report.terminal_id, report.state_digest],
    )?;
    if delivered == 0 {
        return Ok(None);
    }
    Ok(Some(tx.query_row(
        "SELECT retry_id FROM provision_retries WHERE fingerprint = ?1",
        [fingerprint],
        |r| r.get(0),
    )?))
}

fn remaining_seconds(received_at: i64, now: i64) -> u64 {
    let left_ms = u64::try_from((received_at + RATE_MS - now).max(0)).unwrap_or(0);
    left_ms.div_ceil(1000).max(1)
}

impl DeckStore {
    /// Stores one report of `fingerprint` (the authenticated key), or says
    /// why not. The order of the checks is: a key that reported less than
    /// five minutes ago (only `last_contact` changes), then the capacity
    /// for a new key. A changed `terminal_id` or `state_digest` replaces
    /// the report and deletes the retry permission of the key. A live
    /// permission that is bound to the report is delivered once (`Retry`).
    ///
    /// # Errors
    /// A database error; nothing is written then.
    pub async fn apply_provision_report(
        &self,
        fingerprint: String,
        report: ValidReport,
        now: i64,
    ) -> Result<Applied> {
        call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            prune(&tx, now)?;
            let known: Option<(String, String, i64)> = tx
                .query_row(
                    "SELECT terminal_id, state_digest, received_at FROM provision_reports
                     WHERE fingerprint = ?1",
                    [&fingerprint],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .optional()?;
            let applied = match known {
                Some((_, _, received_at)) if now - received_at < RATE_MS => {
                    tx.execute(
                        "UPDATE provision_reports SET last_contact = ?1, updated_at = ?1
                         WHERE fingerprint = ?2",
                        params![now, fingerprint],
                    )?;
                    let outcome = match deliver(&tx, &fingerprint, &report, now)? {
                        Some(retry_id) => Outcome::Retry { retry_id },
                        None => Outcome::RateLimited(remaining_seconds(received_at, now)),
                    };
                    Applied {
                        outcome,
                        counted: false,
                    }
                }
                Some((terminal_id, state_digest, _)) => {
                    let changed =
                        terminal_id != report.terminal_id || state_digest != report.state_digest;
                    if changed {
                        tx.execute(
                            "DELETE FROM provision_retries WHERE fingerprint = ?1",
                            [&fingerprint],
                        )?;
                    }
                    tx.execute(
                        "UPDATE provision_reports
                         SET terminal_id = ?2, state_digest = ?3, code = ?4, phase = ?5,
                             name = ?6, received_at = ?1, last_contact = ?1, updated_at = ?1
                         WHERE fingerprint = ?7",
                        params![
                            now,
                            report.terminal_id,
                            report.state_digest,
                            report.code,
                            report.phase,
                            report.name,
                            fingerprint,
                        ],
                    )?;
                    let outcome = match deliver(&tx, &fingerprint, &report, now)? {
                        Some(retry_id) => Outcome::Retry { retry_id },
                        None => Outcome::Stored,
                    };
                    Applied {
                        outcome,
                        counted: true,
                    }
                }
                None => {
                    let rows: i64 =
                        tx.query_row("SELECT COUNT(*) FROM provision_reports", [], |r| r.get(0))?;
                    if rows >= MAX_REPORTS {
                        Applied {
                            outcome: Outcome::Capacity,
                            counted: false,
                        }
                    } else {
                        tx.execute(
                            "INSERT INTO provision_reports (fingerprint, terminal_id,
                                 state_digest, code, phase, name, received_at, last_contact,
                                 updated_at)
                             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7, ?7)",
                            params![
                                fingerprint,
                                report.terminal_id,
                                report.state_digest,
                                report.code,
                                report.phase,
                                report.name,
                                now,
                            ],
                        )?;
                        Applied {
                            outcome: Outcome::Stored,
                            counted: true,
                        }
                    }
                }
            };
            tx.commit()?;
            Ok(applied)
        })
        .await
    }

    /// The reports, newest receipt first, with their live permissions.
    /// Rows past the retention are deleted first.
    ///
    /// # Errors
    /// A database error.
    pub async fn provision_reports_list(&self, now: i64) -> Result<Vec<StoredReport>> {
        call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            prune(&tx, now)?;
            let rows = {
                let mut stmt = tx.prepare(
                    "SELECT p.fingerprint, p.terminal_id, p.state_digest, p.code, p.phase,
                            p.name, p.received_at, p.last_contact,
                            r.fingerprint IS NOT NULL, r.delivered_at IS NOT NULL, r.expires_at
                     FROM provision_reports p
                     LEFT JOIN provision_retries r ON r.fingerprint = p.fingerprint
                     ORDER BY p.received_at DESC, p.fingerprint",
                )?;
                stmt.query_map([], |r| {
                    let retry = r
                        .get::<_, bool>(8)?
                        .then(|| -> rusqlite::Result<RetryState> {
                            Ok(RetryState {
                                delivered: r.get(9)?,
                                expires_at: r.get(10)?,
                            })
                        });
                    Ok(StoredReport {
                        fingerprint: r.get(0)?,
                        terminal_id: r.get(1)?,
                        state_digest: r.get(2)?,
                        code: r.get(3)?,
                        phase: r.get(4)?,
                        name: r.get(5)?,
                        received_at: r.get(6)?,
                        last_contact: r.get(7)?,
                        retry: retry.transpose()?,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?
            };
            tx.commit()?;
            Ok(rows)
        })
        .await
    }

    /// Writes the retry permission of a report for the exact binding that
    /// the owner saw, or says why not.
    ///
    /// # Errors
    /// A database error.
    pub async fn allow_provision_retry(&self, a: AllowRetry) -> Result<Allowed> {
        call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            prune(&tx, a.now)?;
            let stored: Option<(String, String, String)> = tx
                .query_row(
                    "SELECT terminal_id, state_digest, code FROM provision_reports
                     WHERE fingerprint = ?1",
                    [&a.fingerprint],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .optional()?;
            let Some((terminal_id, state_digest, code)) = stored else {
                return Ok(Allowed::NoReport);
            };
            if terminal_id != a.terminal_id || state_digest != a.state_digest {
                return Ok(Allowed::Changed);
            }
            if !(a.code_allowed)(&code) {
                return Ok(Allowed::NotAllowed);
            }
            // `prune` deleted an expired permission: every row here is live.
            let existing: Option<(String, RetryState)> = tx
                .query_row(
                    "SELECT request_id, delivered_at IS NOT NULL, expires_at
                     FROM provision_retries WHERE fingerprint = ?1",
                    [&a.fingerprint],
                    |r| {
                        Ok((
                            r.get(0)?,
                            RetryState {
                                delivered: r.get(1)?,
                                expires_at: r.get(2)?,
                            },
                        ))
                    },
                )
                .optional()?;
            if let Some((request_id, state)) = existing {
                return Ok(if request_id == a.request_id {
                    Allowed::Granted(state)
                } else {
                    Allowed::Queued
                });
            }
            let expires_at = a.now + RETRY_TTL_MS;
            tx.execute(
                "INSERT INTO provision_retries (fingerprint, retry_id, terminal_id,
                     state_digest, request_id, created_at, expires_at, delivered_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, NULL)",
                params![
                    a.fingerprint,
                    a.retry_id,
                    a.terminal_id,
                    a.state_digest,
                    a.request_id,
                    a.now,
                    expires_at,
                ],
            )?;
            tx.commit()?;
            Ok(Allowed::Granted(RetryState {
                delivered: false,
                expires_at,
            }))
        })
        .await
    }

    /// Cancels a queued retry permission (not a delivered one).
    ///
    /// # Errors
    /// A database error.
    pub async fn cancel_provision_retry(&self, fingerprint: String, now: i64) -> Result<Cancelled> {
        call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            prune(&tx, now)?;
            let report: Option<i64> = tx
                .query_row(
                    "SELECT 1 FROM provision_reports WHERE fingerprint = ?1",
                    [&fingerprint],
                    |r| r.get(0),
                )
                .optional()?;
            if report.is_none() {
                return Ok(Cancelled::NoReport);
            }
            let result = match live_retry(&tx, &fingerprint, now)? {
                None => Cancelled::NoRetry,
                Some(state) if state.delivered => Cancelled::Delivered,
                Some(_) => {
                    tx.execute(
                        "DELETE FROM provision_retries WHERE fingerprint = ?1",
                        [&fingerprint],
                    )?;
                    Cancelled::Done
                }
            };
            tx.commit()?;
            Ok(result)
        })
        .await
    }

    /// Dismisses a report and its permission. `false`: there was no report.
    ///
    /// # Errors
    /// A database error.
    pub async fn dismiss_provision_report(&self, fingerprint: String) -> Result<bool> {
        call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            tx.execute(
                "DELETE FROM provision_retries WHERE fingerprint = ?1",
                [&fingerprint],
            )?;
            let removed = tx.execute(
                "DELETE FROM provision_reports WHERE fingerprint = ?1",
                [&fingerprint],
            )?;
            tx.commit()?;
            Ok(removed > 0)
        })
        .await
    }
}
