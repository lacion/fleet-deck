//! The end of a worker grant by its session and the cap on its renewal (MCP P4a
//! PRE-PR R1): `session.ended` revokes the worker grants of that session, and a
//! renewal never makes more worker grants in effect than `grant_limits.worker_grants`.

use std::collections::BTreeSet;

use fleetdeck_core::config_grant_limits::GrantLimits;
use fleetdeck_proto::command::GrantKind;
use fleetdeck_proto::{CommandId, SessionId, StationId};
use rusqlite::{Connection, OptionalExtension, params};

use super::Result;
use super::grants::Grant;
use super::worker_grant::{over_worker_grants, revoke_tx};

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

/// Revokes the worker grant of start `command_id` (`session_ended`) when the
/// session that its accepted receipt names has already ended on the grant's
/// station (PRE-PR R2 BLOCKER): `session.ended` can be folded before the
/// receipt is stored, and then [`apply_session_ended`] finds no grant. A session
/// has ended when its summary state is `exited` or `lost`.
pub(super) fn revoke_if_ended(
    tx: &Connection,
    command_id: &CommandId,
    session: &SessionId,
    now: i64,
) -> Result<BTreeSet<StationId>> {
    let found: Option<String> = tx
        .query_row(
            "SELECT t.grant_id FROM tokens t
             JOIN sessions s ON s.station_id = t.station_id AND s.session_id = ?2
             WHERE t.start_command_id = ?1 AND t.kind = 'worker' AND t.state != 'revoked'
               AND json_extract(s.summary, '$.state') IN ('exited', 'lost')",
            params![command_id.as_str(), session.0],
            |r| r.get(0),
        )
        .optional()?;
    match found {
        Some(id) => Ok(revoke_tx(tx, &id, "session_ended", now)?.1),
        None => Ok(BTreeSet::new()),
    }
}

/// True when reactivating `grant` would pass the worker cap (PRE-PR R1 MAJOR 2): a worker grant and
/// `limits.worker_grants` worker grants already in effect.
pub(super) fn over_worker_cap(
    tx: &Connection,
    grant: &Grant,
    now: i64,
    limits: &GrantLimits,
) -> Result<bool> {
    Ok(grant.kind == GrantKind::Worker && over_worker_grants(tx, now, limits)?)
}
