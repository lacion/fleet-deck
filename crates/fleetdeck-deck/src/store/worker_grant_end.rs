//! The end of a worker grant by its session and the cap on its renewal (MCP P4a
//! PRE-PR R1): `session.ended` revokes the worker grants of that session, and a
//! renewal never makes more than [`WORKER_GRANT_CAP`] worker grants in effect.

use std::collections::BTreeSet;

use fleetdeck_proto::command::GrantKind;
use fleetdeck_proto::{SessionId, StationId};
use rusqlite::{Connection, params};

use super::Result;
use super::grants::Grant;
use super::worker_grant::{WORKER_GRANT_CAP, revoke_tx, worker_grants_in_effect};

/// Folds `session.ended` of `station` (MCP P4a §6.4, PRE-PR R1 MAJOR 1): every worker grant of
/// that station and session that is not revoked is revoked with the reason `session_ended`. A grant
/// names its session by `session_id`, or, before its first binding, by the receipt of its start command.
pub(super) fn apply_session_ended(
    tx: &Connection,
    station: &StationId,
    session: &SessionId,
    now: i64,
) -> Result<BTreeSet<StationId>> {
    let ids = {
        let mut stmt = tx.prepare(
            "SELECT t.grant_id FROM tokens t
             LEFT JOIN commands c ON c.command_id = t.start_command_id
             WHERE t.kind = 'worker' AND t.station_id = ?1 AND t.state != 'revoked'
               AND (t.session_id = ?2
                    OR (t.session_id = '' AND json_extract(c.receipt, '$.result.session_id') = ?2))",
        )?;
        stmt.query_map(params![station.as_str(), session.0], |r| {
            r.get::<_, String>(0)
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?
    };
    let mut changed = BTreeSet::new();
    for id in ids {
        changed.extend(revoke_tx(tx, &id, "session_ended", now)?.1);
    }
    Ok(changed)
}

/// True when reactivating `grant` would pass the worker cap (PRE-PR R1 MAJOR 2): a worker grant and
/// `WORKER_GRANT_CAP` worker grants already in effect.
pub(super) fn over_worker_cap(tx: &Connection, grant: &Grant, now: i64) -> Result<bool> {
    Ok(grant.kind == GrantKind::Worker && worker_grants_in_effect(tx, now)? >= WORKER_GRANT_CAP)
}
