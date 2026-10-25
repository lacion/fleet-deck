//! The audit rows of grants that a station update, restart, reset, removal
//! or event keeps or drops (B26, design `grants-update.md` §3.5).

use std::collections::BTreeSet;

use fleetdeck_proto::StationId;
use rusqlite::{Connection, OptionalExtension, params};

use super::Result;

pub(crate) const KEPT: &str = "coordinator.grant_kept";
pub(crate) const DROPPED: &str = "coordinator.grant_dropped";
#[allow(dead_code)] // used by the hold and the return (T2)
pub(crate) const REASON_STATION_UPDATED: &str = "station_updated";
#[allow(dead_code)] // used by the hold and the return (T2)
pub(crate) const REASON_STATION_RESET: &str = "station_reset";
pub(crate) const REASON_STATION_REMOVED: &str = "station_removed";
#[allow(dead_code)] // used by the hold and the return (T2)
pub(crate) const REASON_STATION_OFFLINE: &str = "station_offline";
pub(crate) const REASON_KEY_ROTATED: &str = "key_rotated";

/// One audit row (actor `deck`, result `ok`, `command_id` NULL) in the
/// caller's transaction.
pub(crate) fn audit_tx(
    tx: &Connection,
    now: i64,
    action: &str,
    grant_id: &str,
    detail: &str,
) -> Result<()> {
    tx.execute(
        "INSERT INTO audit (at, actor, action, target, command_id, result, detail)
         VALUES (?1, 'deck', ?2, ?3, NULL, 'ok', ?4)",
        params![now, action, grant_id, detail],
    )?;
    Ok(())
}

/// The name of `station`, or `?` when it has no row.
pub(crate) fn station_name(tx: &Connection, station: &StationId) -> Result<String> {
    Ok(tx
        .query_row(
            "SELECT name FROM stations WHERE station_id = ?1",
            [station.as_str()],
            |r| r.get::<_, String>(0),
        )
        .optional()?
        .unwrap_or_else(|| "?".to_owned()))
}

fn grant_ids(tx: &Connection, sql: &str, station: &StationId) -> Result<BTreeSet<String>> {
    let mut stmt = tx.prepare(sql)?;
    let ids = stmt
        .query_map([station.as_str()], |r| r.get::<_, String>(0))?
        .collect::<std::result::Result<BTreeSet<_>, _>>()?;
    Ok(ids)
}

/// The ids of the `active` grants whose `station_id` is `station`.
pub(crate) fn active_of_station(tx: &Connection, station: &StationId) -> Result<BTreeSet<String>> {
    grant_ids(
        tx,
        "SELECT grant_id FROM tokens WHERE station_id = ?1 AND state = 'active'",
        station,
    )
}

/// The ids of the grants whose `station_id` is `station` and whose state is
/// not `revoked` (`active` or `expired`): the drop set of §3.3 item 5 and
/// §3.4 item 1.
pub(crate) fn live_of_station(tx: &Connection, station: &StationId) -> Result<BTreeSet<String>> {
    grant_ids(
        tx,
        "SELECT grant_id FROM tokens WHERE station_id = ?1 AND state != 'revoked'",
        station,
    )
}

/// One `coordinator.grant_dropped` row for each id of `before` that is no
/// longer `active`: `reason=<revoked_reason or state>;cause=<cause>;station=<name>`.
/// Returns the count.
pub(crate) fn audit_ends_tx(
    tx: &Connection,
    station: &StationId,
    before: &BTreeSet<String>,
    cause: &str,
    now: i64,
) -> Result<usize> {
    if before.is_empty() {
        return Ok(0);
    }
    let name = station_name(tx, station)?;
    let mut count = 0;
    for id in before {
        let ended: Option<(String, Option<String>)> = tx
            .query_row(
                "SELECT state, revoked_reason FROM tokens WHERE grant_id = ?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let Some((state, reason)) = ended else {
            continue;
        };
        if state == "active" {
            continue;
        }
        let reason = reason.unwrap_or(state);
        audit_tx(
            tx,
            now,
            DROPPED,
            id,
            &format!("reason={reason};cause={cause};station={name}"),
        )?;
        count += 1;
    }
    Ok(count)
}

/// §3.4 item 1: revokes every grant of `station` that is not `revoked`
/// (`station_removed`) and writes one `coordinator.grant_dropped` row for
/// each grant that changed. Returns the stations whose scopes changed.
pub(crate) fn drop_station_tx(
    tx: &Connection,
    station: &StationId,
    name: &str,
    now: i64,
) -> Result<BTreeSet<StationId>> {
    let mut changed = BTreeSet::new();
    for id in live_of_station(tx, station)? {
        let (did, scopes) = super::worker_grant::revoke_tx(tx, &id, REASON_STATION_REMOVED, now)?;
        changed.extend(scopes);
        if did {
            audit_tx(
                tx,
                now,
                DROPPED,
                &id,
                &format!("reason={REASON_STATION_REMOVED};station={name}"),
            )?;
        }
    }
    Ok(changed)
}

/// §3.4 item 2: one `coordinator.grant_kept` row for each `active` grant of
/// the station with the new key `new_key_hex`. Returns the count.
pub(crate) fn kept_on_rotation_tx(tx: &Connection, new_key_hex: &str, now: i64) -> Result<usize> {
    let found: Option<(String, String)> = tx
        .query_row(
            "SELECT station_id, name FROM stations WHERE key = ?1",
            [new_key_hex],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((station_id, name)) = found else {
        return Ok(0);
    };
    let ids = active_of_station(tx, &StationId::from(station_id))?;
    for id in &ids {
        audit_tx(
            tx,
            now,
            KEPT,
            id,
            &format!("reason={REASON_KEY_ROTATED};station={name}"),
        )?;
    }
    Ok(ids.len())
}

#[cfg(test)]
#[path = "grant_audit_tests.rs"]
mod tests;
