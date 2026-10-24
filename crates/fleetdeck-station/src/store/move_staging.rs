//! The staging record of a move upload (FG-9, design §12.3 step 3, V32): the
//! folder `<station dir>/moves/<move_id>/` with its `dev` and `ino`, and the
//! manifest, written before the `move_in_runs` row exists. The `MoveIn`
//! decision copies the staging identity into its row; a discard and the
//! station start delete the folder by this identity.

use fleetdeck_proto::fleet_git::TranscriptManifest;
use rusqlite::{Connection, OptionalExtension, params};

use super::{Result, Store, StoreError, call};

/// The state of a `move_staging` row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StagingState {
    /// The upload runs.
    Uploading,
    /// Every file arrived and its SHA-256 matched.
    Ready,
    /// The folder is deleted (or was never complete).
    Deleted,
}

impl StagingState {
    /// The stored text.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Uploading => "uploading",
            Self::Ready => "ready",
            Self::Deleted => "deleted",
        }
    }

    fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "uploading" => Self::Uploading,
            "ready" => Self::Ready,
            "deleted" => Self::Deleted,
            _ => return None,
        })
    }
}

/// One row of `move_staging`.
#[derive(Debug, Clone, PartialEq)]
pub struct StagingRow {
    pub move_id: String,
    pub path: String,
    pub dev: u64,
    pub ino: u64,
    pub manifest: TranscriptManifest,
    pub state: StagingState,
    pub created_at: i64,
    pub updated_at: i64,
}

const COLUMNS: &str = "move_id, path, dev, ino, manifest, state, created_at, updated_at";

fn bad(column: usize, what: &str) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        column,
        rusqlite::types::Type::Text,
        what.to_owned().into(),
    )
}

fn row_of(r: &rusqlite::Row<'_>) -> rusqlite::Result<StagingRow> {
    let manifest: String = r.get(4)?;
    let state: String = r.get(5)?;
    Ok(StagingRow {
        move_id: r.get(0)?,
        path: r.get(1)?,
        dev: u64::try_from(r.get::<_, i64>(2)?).unwrap_or(0),
        ino: u64::try_from(r.get::<_, i64>(3)?).unwrap_or(0),
        manifest: serde_json::from_str(&manifest).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(4, rusqlite::types::Type::Text, Box::new(e))
        })?,
        state: StagingState::parse(&state).ok_or_else(|| bad(5, "unknown staging state"))?,
        created_at: r.get(6)?,
        updated_at: r.get(7)?,
    })
}

fn to_i64(v: u64) -> i64 {
    i64::try_from(v).unwrap_or(i64::MAX)
}

/// Inserts the record of a new upload (state `uploading`). A row in state
/// `deleted` is replaced (a new upload of the same move id); a row in another
/// state stays and the answer is `false`.
///
/// # Errors
/// A database error.
pub fn insert(
    conn: &Connection,
    move_id: &str,
    folder: (&str, u64, u64),
    manifest: &TranscriptManifest,
    now: i64,
) -> Result<bool> {
    let text = serde_json::to_string(manifest)
        .map_err(|e| StoreError::Corrupt(format!("move manifest JSON: {e}")))?;
    let changed = conn.execute(
        "INSERT INTO move_staging (move_id, path, dev, ino, manifest, state, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, 'uploading', ?6, ?6)
         ON CONFLICT (move_id) DO UPDATE SET
            path = excluded.path, dev = excluded.dev, ino = excluded.ino,
            manifest = excluded.manifest, state = 'uploading', created_at = excluded.created_at,
            updated_at = excluded.updated_at
         WHERE move_staging.state = 'deleted'",
        params![move_id, folder.0, to_i64(folder.1), to_i64(folder.2), text, now],
    )?;
    Ok(changed > 0)
}

/// The record of `move_id`.
///
/// # Errors
/// A database error.
pub fn get(conn: &Connection, move_id: &str) -> Result<Option<StagingRow>> {
    Ok(conn
        .query_row(
            &format!("SELECT {COLUMNS} FROM move_staging WHERE move_id = ?1"),
            params![move_id],
            row_of,
        )
        .optional()?)
}

/// The conditional transition of a record: `false` when it is not in `from`.
///
/// # Errors
/// A database error.
pub fn set_state(
    conn: &Connection,
    move_id: &str,
    from: StagingState,
    to: StagingState,
    now: i64,
) -> Result<bool> {
    let changed = conn.execute(
        "UPDATE move_staging SET state = ?3, updated_at = ?4 WHERE move_id = ?1 AND state = ?2",
        params![move_id, from.as_str(), to.as_str(), now],
    )?;
    Ok(changed > 0)
}

/// The records in `state`.
///
/// # Errors
/// A database error.
pub fn in_state(conn: &Connection, state: StagingState) -> Result<Vec<StagingRow>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLUMNS} FROM move_staging WHERE state = ?1 ORDER BY created_at"
    ))?;
    let rows = stmt
        .query_map(params![state.as_str()], row_of)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

impl Store {
    /// Inserts the record of a new upload (see [`insert`]).
    ///
    /// # Errors
    /// A database error.
    pub async fn move_staging_insert(
        &self,
        move_id: String,
        folder: (String, u64, u64),
        manifest: TranscriptManifest,
    ) -> Result<bool> {
        let now = self.now();
        call(&self.inner.db, move |conn| {
            insert(
                conn,
                &move_id,
                (&folder.0, folder.1, folder.2),
                &manifest,
                now,
            )
        })
        .await
    }

    /// The record of `move_id`.
    ///
    /// # Errors
    /// A database error.
    pub async fn move_staging(&self, move_id: String) -> Result<Option<StagingRow>> {
        call(&self.inner.db, move |conn| get(conn, &move_id)).await
    }

    /// The conditional transition of a record (see [`set_state`]).
    ///
    /// # Errors
    /// A database error.
    pub async fn move_staging_set(
        &self,
        move_id: String,
        from: StagingState,
        to: StagingState,
    ) -> Result<bool> {
        let now = self.now();
        call(&self.inner.db, move |conn| {
            set_state(conn, &move_id, from, to, now)
        })
        .await
    }

    /// The records in `state`.
    ///
    /// # Errors
    /// A database error.
    pub async fn move_staging_in(&self, state: StagingState) -> Result<Vec<StagingRow>> {
        call(&self.inner.db, move |conn| in_state(conn, state)).await
    }

    /// Appends one placed file to `move_in_runs.imported`, in a transaction of
    /// its own (design §12.3 step 4); `false` when the row does not exist.
    ///
    /// # Errors
    /// A database error.
    pub async fn move_imported_append(
        &self,
        move_id: String,
        file: super::move_runs::ImportedFile,
    ) -> Result<bool> {
        let now = self.now();
        call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            let done = super::move_runs::append_imported(&tx, &move_id, &file, now)?;
            tx.commit()?;
            Ok(done)
        })
        .await
    }
}
