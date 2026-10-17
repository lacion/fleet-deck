//! The temporary worktrees of a land (FG-8, design §11.6, §3.5 V30): one row
//! for each `(land_id, attempt)`. The row is `reserved` before
//! `git worktree add`, `present` when the folder identity is recorded, and
//! `removed` or `kept` when the cleanup ended.

use rusqlite::{Connection, OptionalExtension, Row, params};

use super::{Result, Store, call};

/// The state of a temporary worktree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LandTempState {
    Reserved,
    Present,
    Removed,
    Kept,
}

impl LandTempState {
    /// The stored text.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Reserved => "reserved",
            Self::Present => "present",
            Self::Removed => "removed",
            Self::Kept => "kept",
        }
    }

    fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "reserved" => Self::Reserved,
            "present" => Self::Present,
            "removed" => Self::Removed,
            "kept" => Self::Kept,
            _ => return None,
        })
    }
}

/// One row of `land_temps`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LandTempRow {
    pub land_id: String,
    pub attempt: u32,
    /// The canonical allowed root that holds the folder.
    pub root: String,
    pub path: String,
    pub dev: Option<u64>,
    pub ino: Option<u64>,
    pub admin_dir: Option<String>,
    pub state: LandTempState,
    pub code: Option<String>,
}

const COLUMNS: &str = "land_id, attempt, root, path, dev, ino, admin_dir, state, code";

fn row_of(r: &Row<'_>) -> rusqlite::Result<LandTempRow> {
    let state: String = r.get(7)?;
    let state = LandTempState::parse(&state).ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            7,
            rusqlite::types::Type::Text,
            format!("unknown land_temps state `{state}`").into(),
        )
    })?;
    Ok(LandTempRow {
        land_id: r.get(0)?,
        attempt: r.get(1)?,
        root: r.get(2)?,
        path: r.get(3)?,
        dev: r
            .get::<_, Option<i64>>(4)?
            .and_then(|v| u64::try_from(v).ok()),
        ino: r
            .get::<_, Option<i64>>(5)?
            .and_then(|v| u64::try_from(v).ok()),
        admin_dir: r.get(6)?,
        state,
        code: r.get(8)?,
    })
}

/// Stores the row `reserved`, before `git worktree add` (design §11.6).
///
/// # Errors
/// A database error (a constraint error for a `(land_id, attempt)` that has
/// a row).
pub fn reserve(
    conn: &Connection,
    land_id: &str,
    attempt: u32,
    root: &str,
    path: &str,
    now: i64,
) -> Result<()> {
    conn.execute(
        "INSERT INTO land_temps (land_id, attempt, root, path, state, updated_at)
         VALUES (?1, ?2, ?3, ?4, 'reserved', ?5)",
        params![land_id, attempt, root, path, now],
    )?;
    Ok(())
}

/// Records the folder identity and the Git admin entry after the add:
/// `reserved` becomes `present`. Returns whether a `reserved` row changed.
///
/// # Errors
/// A database error.
pub fn set_present(
    conn: &Connection,
    land_id: &str,
    attempt: u32,
    identity: (u64, u64),
    admin_dir: &str,
    now: i64,
) -> Result<bool> {
    let (dev, ino) = (
        i64::try_from(identity.0).unwrap_or(i64::MAX),
        i64::try_from(identity.1).unwrap_or(i64::MAX),
    );
    Ok(conn.execute(
        "UPDATE land_temps SET state = 'present', dev = ?3, ino = ?4, admin_dir = ?5,
                updated_at = ?6
         WHERE land_id = ?1 AND attempt = ?2 AND state = 'reserved'",
        params![land_id, attempt, dev, ino, admin_dir, now],
    )? > 0)
}

/// Ends the cleanup of a row: `removed`, or `kept` with the reason `code`
/// (`temp_changed`, `temp_unknown`). Returns whether a row changed.
///
/// # Errors
/// A database error.
pub fn finish(
    conn: &Connection,
    land_id: &str,
    attempt: u32,
    state: LandTempState,
    code: Option<&str>,
    now: i64,
) -> Result<bool> {
    Ok(conn.execute(
        "UPDATE land_temps SET state = ?3, code = ?4, updated_at = ?5
         WHERE land_id = ?1 AND attempt = ?2 AND state IN ('reserved', 'present')",
        params![land_id, attempt, state.as_str(), code, now],
    )? > 0)
}

/// The row of `(land_id, attempt)`.
///
/// # Errors
/// A database error.
pub fn get(conn: &Connection, land_id: &str, attempt: u32) -> Result<Option<LandTempRow>> {
    Ok(conn
        .query_row(
            &format!("SELECT {COLUMNS} FROM land_temps WHERE land_id = ?1 AND attempt = ?2"),
            params![land_id, attempt],
            row_of,
        )
        .optional()?)
}

/// Every row in `reserved` or `present`, oldest first: the rows that the
/// cleanup looks at.
///
/// # Errors
/// A database error.
pub fn open(conn: &Connection) -> Result<Vec<LandTempRow>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLUMNS} FROM land_temps WHERE state IN ('reserved', 'present')
         ORDER BY updated_at, land_id, attempt"
    ))?;
    let rows = stmt.query_map([], row_of)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Every row in state `kept`, oldest first (for `fleetdeck doctor`).
///
/// # Errors
/// A database error.
pub fn kept(conn: &Connection) -> Result<Vec<LandTempRow>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLUMNS} FROM land_temps WHERE state = 'kept'
         ORDER BY updated_at, land_id, attempt"
    ))?;
    let rows = stmt.query_map([], row_of)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

impl Store {
    /// Stores the row `reserved`, before `git worktree add`.
    ///
    /// # Errors
    /// A database error.
    pub async fn land_temp_reserve(
        &self,
        land_id: String,
        attempt: u32,
        root: String,
        path: String,
    ) -> Result<()> {
        let now = self.now();
        call(&self.inner.db, move |conn| {
            reserve(conn, &land_id, attempt, &root, &path, now)
        })
        .await
    }

    /// Records the identity and the admin entry: `reserved` becomes `present`.
    ///
    /// # Errors
    /// A database error.
    pub async fn land_temp_present(
        &self,
        land_id: String,
        attempt: u32,
        identity: (u64, u64),
        admin_dir: String,
    ) -> Result<bool> {
        let now = self.now();
        call(&self.inner.db, move |conn| {
            set_present(conn, &land_id, attempt, identity, &admin_dir, now)
        })
        .await
    }

    /// Ends the cleanup of a row: `removed`, or `kept` with a code.
    ///
    /// # Errors
    /// A database error.
    pub async fn land_temp_finish(
        &self,
        land_id: String,
        attempt: u32,
        state: LandTempState,
        code: Option<String>,
    ) -> Result<bool> {
        let now = self.now();
        call(&self.inner.db, move |conn| {
            finish(conn, &land_id, attempt, state, code.as_deref(), now)
        })
        .await
    }

    /// The row of `(land_id, attempt)`.
    ///
    /// # Errors
    /// A database error.
    pub async fn land_temp(&self, land_id: String, attempt: u32) -> Result<Option<LandTempRow>> {
        call(&self.inner.db, move |conn| get(conn, &land_id, attempt)).await
    }

    /// The rows that the cleanup looks at (`reserved` or `present`).
    ///
    /// # Errors
    /// A database error.
    pub async fn land_temps_open(&self) -> Result<Vec<LandTempRow>> {
        call(&self.inner.db, move |conn| open(conn)).await
    }

    /// The rows that the cleanup kept.
    ///
    /// # Errors
    /// A database error.
    pub async fn land_temps_kept(&self) -> Result<Vec<LandTempRow>> {
        call(&self.inner.db, move |conn| kept(conn)).await
    }
}

#[cfg(test)]
#[path = "land_temps_tests.rs"]
mod tests;
