//! The durable claim of a coordinator desktop mutation on the target (R13
//! P6 §7.3, decision O12-4 item 5): one `desktop_ops` row per `(deck
//! fingerprint, operation id)`, kept for ever. The claim is written before
//! the user-visible effect, so a claimed operation never runs again after a
//! restart or a reconnect.
//!
//! - [`Store::desktop_op_claim`] inserts a `claimed` row, or says what the
//!   existing row means.
//! - [`Store::desktop_op_finish`] moves a `claimed` row to `done` or
//!   `failed`; a terminal row never changes.
//! - [`Store::desktop_op_get`] reads one row for the `action_status` read.

use fleetdeck_proto::coordinator_desktop_wire::CoordinatorContext;
use rusqlite::{OptionalExtension, params};

use super::{Result, Store, StoreError, call};

/// What [`Store::desktop_op_claim`] decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Claim {
    /// A new `claimed` row: the caller runs the effect.
    New,
    /// The same operation, hash and kind finished before: the stored result.
    Done(Option<serde_json::Value>),
    /// Any other existing row: the effect may have run; it never runs again.
    Seen,
}

/// One `desktop_ops` row, as the `action_status` read needs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopOpRow {
    /// `claimed`, `done` or `failed`.
    pub state: String,
    pub result: Option<serde_json::Value>,
    pub code: Option<String>,
    /// `notify`, `preview_open` or `preview_close`.
    pub kind: String,
    pub grant_id: String,
    pub caller_station: String,
    pub caller_session: String,
}

impl Store {
    /// Claims one operation in one transaction. A new key inserts a
    /// `claimed` row ([`Claim::New`]). An existing row gives
    /// [`Claim::Done`] when its state is `done` and its action hash and
    /// kind equal the given ones, and [`Claim::Seen`] in every other case.
    ///
    /// # Errors
    /// A database error.
    pub async fn desktop_op_claim(
        &self,
        deck_fp: &str,
        operation_id: &str,
        action_hash: &str,
        kind: &str,
        ctx: &CoordinatorContext,
        now: i64,
    ) -> Result<Claim> {
        let (deck_fp, operation_id) = (deck_fp.to_owned(), operation_id.to_owned());
        let (action_hash, kind) = (action_hash.to_owned(), kind.to_owned());
        let (grant_id, station, session) = (
            ctx.grant_id.clone(),
            ctx.caller_station.to_string(),
            ctx.caller_session.to_string(),
        );
        call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            let existing: Option<(String, String, String, Option<String>)> = tx
                .query_row(
                    "SELECT state, action_hash, kind, result FROM desktop_ops
                     WHERE deck_fp = ?1 AND operation_id = ?2",
                    params![deck_fp, operation_id],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                )
                .optional()?;
            if let Some((state, hash, stored_kind, result)) = existing {
                if state == "done" && hash == action_hash && stored_kind == kind {
                    let result = result.map(|text| serde_json::from_str(&text)).transpose()?;
                    return Ok(Claim::Done(result));
                }
                return Ok(Claim::Seen);
            }
            tx.execute(
                "INSERT INTO desktop_ops
                     (deck_fp, operation_id, action_hash, kind, grant_id, caller_station,
                      caller_session, state, claimed_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'claimed', ?8, ?8)",
                params![
                    deck_fp,
                    operation_id,
                    action_hash,
                    kind,
                    grant_id,
                    station,
                    session,
                    now
                ],
            )?;
            tx.commit()?;
            Ok(Claim::New)
        })
        .await
    }

    /// Ends a `claimed` operation: `done` with its result, or `failed` with
    /// its code. A row in another state does not change.
    ///
    /// # Errors
    /// A database error.
    pub async fn desktop_op_finish(
        &self,
        deck_fp: &str,
        operation_id: &str,
        outcome: std::result::Result<Option<serde_json::Value>, &str>,
        now: i64,
    ) -> Result<()> {
        let (deck_fp, operation_id) = (deck_fp.to_owned(), operation_id.to_owned());
        let (state, result, code) = match outcome {
            Ok(result) => (
                "done",
                result.map(|v| serde_json::to_string(&v)).transpose()?,
                None,
            ),
            Err(code) => ("failed", None, Some(code.to_owned())),
        };
        call(&self.inner.db, move |conn| {
            conn.execute(
                "UPDATE desktop_ops SET state = ?3, result = ?4, code = ?5, updated_at = ?6
                 WHERE deck_fp = ?1 AND operation_id = ?2 AND state = 'claimed'",
                params![deck_fp, operation_id, state, result, code, now],
            )?;
            Ok(())
        })
        .await
    }

    /// One row.
    ///
    /// # Errors
    /// A database error; a stored result that is not JSON.
    pub async fn desktop_op_get(
        &self,
        deck_fp: &str,
        operation_id: &str,
    ) -> Result<Option<DesktopOpRow>> {
        let (deck_fp, operation_id) = (deck_fp.to_owned(), operation_id.to_owned());
        call(&self.inner.db, move |conn| {
            let raw = conn
                .query_row(
                    "SELECT state, result, code, kind, grant_id, caller_station, caller_session
                     FROM desktop_ops WHERE deck_fp = ?1 AND operation_id = ?2",
                    params![deck_fp, operation_id],
                    |r| {
                        Ok((
                            r.get::<_, String>(0)?,
                            r.get::<_, Option<String>>(1)?,
                            r.get::<_, Option<String>>(2)?,
                            r.get::<_, String>(3)?,
                            r.get::<_, String>(4)?,
                            r.get::<_, String>(5)?,
                            r.get::<_, String>(6)?,
                        ))
                    },
                )
                .optional()?;
            raw.map(
                |(state, result, code, kind, grant_id, caller_station, caller_session)| {
                    Ok::<_, StoreError>(DesktopOpRow {
                        state,
                        result: result.map(|t| serde_json::from_str(&t)).transpose()?,
                        code,
                        kind,
                        grant_id,
                        caller_station,
                        caller_session,
                    })
                },
            )
            .transpose()
        })
        .await
    }
}

#[cfg(test)]
#[path = "desktop_ops_tests.rs"]
mod tests;
