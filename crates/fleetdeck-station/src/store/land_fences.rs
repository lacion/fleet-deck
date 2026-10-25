//! The land fences of the station store (FG-8, design §11.2 M3, §11.5, V30):
//! a fence stops every later write of one land id, also an id that the
//! station does not know. A fence is never removed.

use rusqlite::{Connection, params};

use super::{Result, Store, call};

/// Records the fence of `land_id` (idempotent: the first `fenced_at` stays).
///
/// # Errors
/// A database error.
pub fn insert(conn: &Connection, land_id: &str, now: i64) -> Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO land_fences (land_id, fenced_at) VALUES (?1, ?2)",
        params![land_id, now],
    )?;
    Ok(())
}

/// The time of the fence of `land_id`, if it is fenced.
///
/// # Errors
/// A database error.
pub fn fenced_at(conn: &Connection, land_id: &str) -> Result<Option<i64>> {
    use rusqlite::OptionalExtension;
    Ok(conn
        .query_row(
            "SELECT fenced_at FROM land_fences WHERE land_id = ?1",
            params![land_id],
            |r| r.get(0),
        )
        .optional()?)
}

/// Is `land_id` fenced?
///
/// # Errors
/// A database error.
pub fn is_fenced(conn: &Connection, land_id: &str) -> Result<bool> {
    Ok(fenced_at(conn, land_id)?.is_some())
}

impl Store {
    /// Is `land_id` fenced?
    ///
    /// # Errors
    /// A database error.
    pub async fn land_fenced(&self, land_id: String) -> Result<bool> {
        call(&self.inner.db, move |conn| is_fenced(conn, &land_id)).await
    }
}

#[cfg(test)]
#[path = "land_fences_tests.rs"]
mod tests;
