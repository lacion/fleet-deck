//! The last known Taildrive machine name of each station (PRE-PR round 1,
//! H16, design §15.5). The deck keeps `station.info` in memory only, so a
//! restart, or a report without a machine (a failed `tailscale status`),
//! would hide the machine of a paired station. This table keeps the last
//! non-empty `taildrive.machine` of each station. A report without one never
//! clears it. The row is deleted with its station (`ON DELETE CASCADE`).

use std::collections::BTreeMap;

use fleetdeck_proto::StationId;
use fleetdeck_proto::link::StationInfo;
use rusqlite::params;

use super::{DeckStore, Result, call};

impl DeckStore {
    /// Keeps the `station.info` of `station` for the board and stores its
    /// machine name ([`Self::remember_machine`]).
    pub async fn record_station_info(&self, station: &StationId, info: StationInfo) {
        self.remember_machine(station, &info).await;
        self.set_station_info(station, info);
    }

    /// Stores the machine name of `info.taildrive` for `station`, when it is
    /// not empty. A database error is logged: the next report tries again.
    pub async fn remember_machine(&self, station: &StationId, info: &StationInfo) {
        let Some(machine) = info
            .taildrive
            .as_ref()
            .and_then(|t| t.machine.as_deref())
            .map(str::trim)
            .filter(|m| !m.is_empty())
            .map(str::to_owned)
        else {
            return;
        };
        let (id, now) = (station.as_str().to_owned(), self.now_ms());
        let stored = call(&self.inner.db, move |conn| {
            conn.execute(
                "INSERT INTO station_machines (station_id, machine, updated_at)
                 VALUES (?1, ?2, ?3)
                 ON CONFLICT(station_id) DO UPDATE
                 SET machine = excluded.machine, updated_at = excluded.updated_at",
                params![id, machine, now],
            )?;
            Ok(())
        })
        .await;
        if let Err(e) = stored {
            tracing::warn!(station = %station, error = %e, "the machine name of a station was not stored");
        }
    }

    /// The stored machine name of every station that has one.
    ///
    /// # Errors
    /// A database error.
    pub async fn station_machines(&self) -> Result<BTreeMap<StationId, String>> {
        call(&self.inner.db, |conn| {
            let mut stmt = conn.prepare("SELECT station_id, machine FROM station_machines")?;
            let rows = stmt.query_map([], |r| {
                Ok((
                    StationId::from(r.get::<_, String>(0)?),
                    r.get::<_, String>(1)?,
                ))
            })?;
            Ok(rows.collect::<std::result::Result<BTreeMap<_, _>, _>>()?)
        })
        .await
    }
}
