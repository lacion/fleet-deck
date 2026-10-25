//! Reads of an interval of a cached artifact file (R13 P5b E1): the deck
//! answers a byte range of a cached video from the SQLite blob, one slice at
//! a time, and never loads the whole blob into the process.

use fleetdeck_proto::StationId;
use rusqlite::{OptionalExtension, params};

use crate::store::{DeckError, DeckStore, Result, call, to_i64};

impl DeckStore {
    /// The length of the cached file, or `None` when the cache has no row
    /// for it (or its artifact has a tombstone).
    ///
    /// # Errors
    /// A database error.
    pub async fn cached_artifact_len(
        &self,
        station: StationId,
        artifact_id: String,
        version: u32,
        path: String,
    ) -> Result<Option<u64>> {
        call(&self.inner.db, move |conn| {
            let len: Option<i64> = conn
                .query_row(
                    concat!(
                        "SELECT length(bytes) FROM artifact_cache c
                         WHERE station_id = ?1 AND artifact_id = ?2 AND version = ?3
                           AND path = ?4 AND ",
                        not_tombstoned!("c")
                    ),
                    params![station.as_str(), artifact_id, version, path],
                    |r| r.get(0),
                )
                .optional()?;
            len.map(|n| {
                u64::try_from(n).map_err(|_| DeckError::Corrupt(format!("cache length {n}")))
            })
            .transpose()
        })
        .await
    }

    /// At most `len` bytes of the cached file from `offset` (fewer at the end
    /// of the file), or `None` when the cache has no row for it (or its
    /// artifact has a tombstone). SQLite cuts the slice (`substr`), so only
    /// the slice reaches the deck's memory.
    ///
    /// # Errors
    /// A database error.
    pub async fn cached_artifact_slice(
        &self,
        station: StationId,
        artifact_id: String,
        version: u32,
        path: String,
        offset: u64,
        len: u64,
    ) -> Result<Option<Vec<u8>>> {
        let now = self.now_ms();
        // `substr` counts from 1.
        let first = to_i64(offset.saturating_add(1))?;
        let len = to_i64(len)?;
        call(&self.inner.db, move |conn| {
            let slice: Option<Vec<u8>> = conn
                .query_row(
                    concat!(
                        "SELECT substr(bytes, ?5, ?6) FROM artifact_cache c
                         WHERE station_id = ?1 AND artifact_id = ?2 AND version = ?3
                           AND path = ?4 AND ",
                        not_tombstoned!("c")
                    ),
                    params![station.as_str(), artifact_id, version, path, first, len],
                    |r| r.get(0),
                )
                .optional()?;
            if slice.is_some() {
                // The capture cache evicts by this time (R13 P5a).
                conn.execute(
                    "UPDATE artifact_cache SET last_used_at = ?5
                     WHERE station_id = ?1 AND artifact_id = ?2 AND version = ?3 AND path = ?4",
                    params![station.as_str(), artifact_id, version, path, now],
                )?;
            }
            Ok(slice)
        })
        .await
    }
}
