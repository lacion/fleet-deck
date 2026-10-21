//! Station names in coordinator calls and fleet views: a tool may name a
//! station by its name (for example `dev1`), as `fleet_view` shows it, as
//! well as by its id.
//!
//! The rule of [`resolve_station`]: a value that is a `station_id` stays; else
//! the one station whose `name` equals the value; else the one station whose
//! name equals the value when ASCII case is ignored; else the value stays
//! and the checks that exist refuse it as before (also when two stations
//! match).

use fleetdeck_proto::StationId;
use fleetdeck_proto::mutation::CoordinatorOp;
use rusqlite::{Connection, params};

use super::Result;

/// The `station_id` of the one station that `value` names, or `None` when
/// the value stays as it is: it is a station id, it names no station, or it
/// names more than one.
///
/// # Errors
/// A database error.
pub(in crate::store) fn resolve_station(conn: &Connection, value: &str) -> Result<Option<String>> {
    let is_id: bool = conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM stations WHERE station_id = ?1)",
        params![value],
        |r| r.get(0),
    )?;
    if is_id {
        return Ok(None);
    }
    for sql in [
        "SELECT station_id FROM stations WHERE name = ?1 LIMIT 2",
        "SELECT station_id FROM stations WHERE lower(name) = lower(?1) LIMIT 2",
    ] {
        let mut stmt = conn.prepare(sql)?;
        let ids = stmt
            .query_map(params![value], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        if let [id] = ids.as_slice() {
            return Ok(Some(id.clone()));
        }
    }
    Ok(None)
}

/// A copy of `op` in which every station that is a name is its station id
/// (see [`resolve_station`]). The call keeps the op as received for its
/// receipt and audit; only the decision reads the copy.
///
/// # Errors
/// A database error.
pub(super) fn resolve_station_names(
    conn: &Connection,
    op: &CoordinatorOp,
) -> Result<CoordinatorOp> {
    let mut resolved = op.clone();
    for station in resolved.stations_mut() {
        if let Some(id) = resolve_station(conn, station.as_str())? {
            *station = StationId::from(id);
        }
    }
    Ok(resolved)
}
