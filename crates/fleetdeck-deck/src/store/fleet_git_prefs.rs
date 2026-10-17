//! The Fleet Git preferences of the deck (design §8.2, §14.3): today only
//! the drift reference station (`fleet_git_prefs.drift_reference`, FG-11).

use rusqlite::{OptionalExtension, params};

use super::{DeckStore, Result, call};

const DRIFT_REFERENCE: &str = "drift_reference";

impl DeckStore {
    /// The stored drift reference station id, whether or not the station
    /// still exists.
    ///
    /// # Errors
    /// A database error.
    pub async fn fleet_git_drift_reference(&self) -> Result<Option<String>> {
        call(&self.inner.db, |conn| {
            Ok(conn
                .query_row(
                    "SELECT value FROM fleet_git_prefs WHERE key = ?1",
                    [DRIFT_REFERENCE],
                    |r| r.get(0),
                )
                .optional()?)
        })
        .await
    }

    /// Stores the drift reference station id; `None` clears it.
    ///
    /// # Errors
    /// A database error.
    pub async fn set_fleet_git_drift_reference(&self, station: Option<String>) -> Result<()> {
        call(&self.inner.db, move |conn| {
            match station {
                Some(id) => conn.execute(
                    "INSERT INTO fleet_git_prefs (key, value) VALUES (?1, ?2)
                     ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                    params![DRIFT_REFERENCE, id],
                )?,
                None => conn.execute(
                    "DELETE FROM fleet_git_prefs WHERE key = ?1",
                    [DRIFT_REFERENCE],
                )?,
            };
            Ok(())
        })
        .await
    }
}
