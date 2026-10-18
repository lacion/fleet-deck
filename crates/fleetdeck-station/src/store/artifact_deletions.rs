//! The permanent tombstones of deleted artifacts (R13 P5a, A4). Each
//! [`Store`] method runs one transaction on the one station connection, so
//! a delete never interleaves with the move and the commit of a publish
//! (`artifacts::commit`).

use rusqlite::{Connection, OptionalExtension, params};

use super::{Result, Store, StoreError, call};

/// The state of the tombstone after [`Store::artifact_delete_begin`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeleteBegin {
    /// The files are still to be removed.
    Pending,
    /// The deletion is done.
    Complete,
}

/// Whether the artifact has a tombstone (`pending` or `complete`).
pub(crate) fn tombstoned(conn: &Connection, artifact_id: &str) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM artifact_deletions WHERE artifact_id = ?1)",
        [artifact_id],
        |r| r.get(0),
    )
}

impl Store {
    /// Starts the deletion of an artifact in one transaction. A tombstone
    /// that exists answers with its state and changes nothing. An artifact
    /// row gets a `pending` tombstone. An identity that the station does
    /// not know gets a `complete` tombstone.
    ///
    /// # Errors
    /// A database error.
    pub async fn artifact_delete_begin(&self, artifact_id: String) -> Result<DeleteBegin> {
        let now = self.now();
        call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            let state: Option<String> = tx
                .query_row(
                    "SELECT state FROM artifact_deletions WHERE artifact_id = ?1",
                    [&artifact_id],
                    |r| r.get(0),
                )
                .optional()?;
            let out = match state.as_deref() {
                Some("pending") => DeleteBegin::Pending,
                Some("complete") => DeleteBegin::Complete,
                Some(other) => {
                    return Err(StoreError::Corrupt(format!(
                        "artifact deletion state {other}"
                    )));
                }
                None => {
                    let known: bool = tx.query_row(
                        "SELECT EXISTS (SELECT 1 FROM artifacts WHERE artifact_id = ?1)",
                        [&artifact_id],
                        |r| r.get(0),
                    )?;
                    let (state, out) = if known {
                        ("pending", DeleteBegin::Pending)
                    } else {
                        ("complete", DeleteBegin::Complete)
                    };
                    tx.execute(
                        "INSERT INTO artifact_deletions (artifact_id, state, created_at, updated_at)
                         VALUES (?1, ?2, ?3, ?3)",
                        params![artifact_id, state, now],
                    )?;
                    out
                }
            };
            tx.commit()?;
            Ok(out)
        })
        .await
    }

    /// Finishes the deletion after the files are gone: removes the version
    /// rows and the artifact row and sets the tombstone to `complete`, in
    /// one transaction. It is safe to repeat.
    ///
    /// # Errors
    /// A database error.
    pub async fn artifact_delete_finish(&self, artifact_id: String) -> Result<()> {
        let now = self.now();
        call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            tx.execute(
                "DELETE FROM artifact_versions WHERE artifact_id = ?1",
                [&artifact_id],
            )?;
            tx.execute(
                "DELETE FROM artifacts WHERE artifact_id = ?1",
                [&artifact_id],
            )?;
            tx.execute(
                "UPDATE artifact_deletions SET state = 'complete', updated_at = ?2
                 WHERE artifact_id = ?1",
                params![artifact_id, now],
            )?;
            tx.commit()?;
            Ok(())
        })
        .await
    }

    /// The artifacts with a `pending` tombstone, oldest first.
    ///
    /// # Errors
    /// A database error.
    pub async fn artifact_deletions_pending(&self) -> Result<Vec<String>> {
        call(&self.inner.db, move |conn| {
            let mut stmt = conn.prepare(
                "SELECT artifact_id FROM artifact_deletions WHERE state = 'pending'
                 ORDER BY created_at, artifact_id",
            )?;
            let rows = stmt
                .query_map([], |r| r.get(0))?
                .collect::<rusqlite::Result<Vec<String>>>()?;
            Ok(rows)
        })
        .await
    }
}
