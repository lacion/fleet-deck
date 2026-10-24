//! Coordinator grant lifecycle: the `coordinator.bound` and
//! `coordinator.ended` rules and the snapshot projection (moved out of
//! [`super::grants`]; no behavior change).

use std::collections::BTreeSet;

use fleetdeck_core::ids::IdSource;
use fleetdeck_proto::mutation::CoordinatorBinding;
use fleetdeck_proto::{SessionId, StationId};
use rusqlite::{Connection, OptionalExtension, params};

use super::coordinator::grant_narrowing::{self, Removal};
use super::display_directives_coordinator::{
    invalidate_coordinator_directives, invalidate_ended_binding, invalidate_stale_generation,
};
use super::grant_audit;
use super::grant_scopes::{in_effect, sync_coordinate_scopes};
use super::grants::owner_grants;
use super::grants::{GrantState, get_grant};
use super::outbox::insert_item;
use super::{NewOutboxItem, Result};
use crate::link::CommandKind;
use fleetdeck_proto::command::{GrantKind, SessionCommand};

/// The pending renewal generation of a grant (V10), if any.
pub(super) fn renew_generation(conn: &Connection, grant_id: &str) -> Result<Option<i64>> {
    Ok(conn
        .query_row(
            "SELECT renew_generation FROM tokens WHERE grant_id = ?1",
            [grant_id],
            |r| r.get::<_, Option<i64>>(0),
        )
        .optional()?
        .flatten())
}

/// Applies a coordinator lifecycle event of `station` to its grant in the
/// read-model transaction that folds the event (ADR-006 §6.1 watermark):
///
/// - `coordinator.bound` binds an active grant of the same station and
///   session once, or moves it to a higher generation. It never revives an
///   expired or revoked grant (a renewal needs the owner's renew command)
///   and never lowers a generation; a duplicate is a no-op.
/// - `coordinator.ended` of the bound binding and generation sets the grant
///   `expired` (`idle`) or `revoked` (any other reason); a revoked grant
///   stays revoked.
///
/// - `coordinator.worker_outcome` (MCP P4a §6.4) revokes the worker grant that
///   the event names, for the same station and start command only.
/// - `session.ended` revokes the worker grants of that session (MCP P4a §6.4).
///
/// These events change which grants are in effect, so the derived `coordinate`
/// scopes are rebuilt in the same transaction; the stations whose scopes
/// changed are returned (design §2.3 items 4 and 5).
///
/// `late` is the id source and the delivery time of a `session.coordinator_revoke`
/// that a late `coordinator.bound` of a terminal worker grant queues.
pub(super) fn record_lifecycle(
    tx: &Connection,
    station: &StationId,
    e: &fleetdeck_proto::EventEnvelope,
    now: i64,
    late: (&dyn IdSource, i64),
) -> Result<BTreeSet<StationId>> {
    use fleetdeck_proto::event::{Event, EventBody};
    let EventBody::Known(event) = &e.event else {
        return Ok(BTreeSet::new());
    };
    let cause = match event {
        Event::CoordinatorBound { .. } => "bound",
        Event::CoordinatorEnded { reason, .. } | Event::CoordinatorWorkerOutcome { reason, .. } => {
            reason.as_str()
        }
        Event::SessionEnded { .. } => "session_ended",
        _ => return apply_lifecycle(tx, station, e, now, late),
    };
    let before = grant_audit::active_of_station(tx, station)?;
    let changed = apply_lifecycle(tx, station, e, now, late)?;
    grant_audit::audit_ends_tx(tx, station, &before, cause, now)?;
    Ok(changed)
}

/// The body of [`record_lifecycle`], without the audit rows.
fn apply_lifecycle(
    tx: &Connection,
    station: &StationId,
    e: &fleetdeck_proto::EventEnvelope,
    now: i64,
    late: (&dyn IdSource, i64),
) -> Result<BTreeSet<StationId>> {
    use fleetdeck_proto::event::{Event, EventBody};
    let EventBody::Known(event) = &e.event else {
        return Ok(BTreeSet::new());
    };
    let mut changed = BTreeSet::new();
    match event {
        Event::CoordinatorBound {
            binding_id,
            grant_id,
            generation,
            provider_instance,
            native_id,
        } => apply_bound(
            tx,
            station,
            &e.session_id,
            e.at,
            &CoordinatorBinding {
                binding_id: binding_id.clone(),
                grant_id: grant_id.clone(),
                generation: *generation,
                provider_instance: provider_instance.clone().into(),
                native_id: native_id.clone(),
            },
            Some((late.0, now, late.1)),
        )?,
        Event::CoordinatorWorkerOutcome {
            grant_id,
            start_command_id,
            ..
        } => {
            return super::worker_grant::apply_outcome(
                tx,
                station,
                (grant_id, start_command_id),
                now,
            );
        }
        Event::CoordinatorEnded {
            binding_id,
            generation,
            reason,
        } => changed = apply_ended(tx, station, (binding_id, *generation), reason, now)?,
        Event::SessionEnded { .. } => {
            return super::worker_grant_end::apply_session_ended(tx, station, &e.session_id, now);
        }
        _ => return Ok(BTreeSet::new()),
    }
    changed.extend(sync_coordinate_scopes(tx, now)?);
    Ok(changed)
}

/// The `coordinator.bound` rules of [`record_lifecycle`], also used by a
/// snapshot projection (ADR-006 §6.1, test 25e).
///
/// A binding of a worker grant that is terminal and never bound (MCP P4a
/// §6.4) changes nothing, and `late` queues `session.coordinator_revoke` for
/// the session, so the station's binding ends too. A renewal that would pass
/// the worker cap is not applied: the grant is revoked (`worker_cap`) and `late`
/// queues the same `session.coordinator_revoke`.
///
/// A grant with a `session_owner_grant` row (MCP P4b §5, §6.4) also: rebinds
/// to a higher generation when it is in effect, keeping its idle clock and
/// counters; accepts a renewal with a generation of at least the pending one;
/// and, when it is terminal, queues a set with no spec (`late` only) instead
/// of a revoke.
fn apply_bound(
    tx: &Connection,
    station: &StationId,
    session: &SessionId,
    at: i64,
    b: &CoordinatorBinding,
    late: Option<(&dyn IdSource, i64, i64)>,
) -> Result<()> {
    let Some(grant) = get_grant(tx, &b.grant_id)? else {
        return Ok(());
    };
    let ours = &grant.station_id == station
        && (&grant.session_id == session
            || claims_start(tx, &b.grant_id, &grant.session_id, session)?);
    if ours
        && grant.kind == GrantKind::Worker
        && grant.binding.is_none()
        && grant.state != GrantState::Active
        && let Some((ids, now, delivery_ms)) = late
    {
        queue_revoke(tx, station, session, &b.grant_id, (ids, now, delivery_ms))?;
    }
    // The row of this grant, when it names this station and session.
    let owner = if ours {
        owner_grants::row_of_grant(tx, &b.grant_id)?.filter(|(row_station, row_session, _)| {
            row_station == station && row_session == session
        })
    } else {
        None
    };
    if let Some((row_station, row_session, _)) = &owner {
        if let Some((ids, now, _)) = late
            && owner_grants::is_terminal(&grant, now)
        {
            let key = (row_station, row_session);
            queue_end(tx, ids, key, &b.grant_id, now)?;
        }
        let now = late.map_or(at, |(_, now, _)| now);
        if grant
            .binding
            .as_ref()
            .is_some_and(|old| old.generation < b.generation)
            && in_effect(&grant, now)
        {
            return rebind(tx, b);
        }
    }
    // A first binding needs no renewal; a higher generation of a bound
    // grant needs the owner's matching pending renewal (25b). A grant with a
    // row accepts a generation of at least the pending one (P4b §5).
    let pending = renew_generation(tx, &b.grant_id)?;
    let renewal = if owner.is_some() {
        pending.is_some_and(|p| i64::from(b.generation) >= p)
    } else {
        pending == Some(i64::from(b.generation))
    };
    let first = grant.binding.is_none() && grant.state == GrantState::Active;
    let renewed = grant
        .binding
        .as_ref()
        .is_some_and(|old| old.generation < b.generation)
        && renewal
        && grant.state != GrantState::Revoked;
    if ours && renewed {
        let now = late.map_or(at, |(_, now, _)| now);
        if super::worker_grant_end::over_worker_cap(tx, &grant, now)? {
            super::worker_grant::revoke_tx(tx, &b.grant_id, "worker_cap", now)?;
            if let Some(queue) = late {
                queue_revoke(tx, station, session, &b.grant_id, queue)?;
            }
            return Ok(());
        }
    }
    if ours && (first || renewed) {
        tx.execute(
            "UPDATE tokens SET bound_generation = ?2, bound_native_id = ?3,
                 bound_provider_instance = ?4, binding_id = ?5, last_used_at = ?6,
                 state = 'active', revoked_reason = NULL, renew_generation = NULL,
                 session_id = ?7
             WHERE grant_id = ?1 AND state != 'revoked'",
            params![
                b.grant_id,
                b.generation,
                b.native_id,
                b.provider_instance.0,
                b.binding_id,
                at,
                session.0
            ],
        )?;
        invalidate_stale_generation(tx, &b.grant_id, b.generation)?;
    }
    Ok(())
}

/// Queues `session.coordinator_revoke` of `grant_id` for `session` on `station`
/// (`ids`: the id source, the time and the delivery window).
pub(super) fn queue_revoke(
    tx: &Connection,
    station: &StationId,
    session: &SessionId,
    grant_id: &str,
    (ids, now, delivery_ms): (&dyn IdSource, i64, i64),
) -> Result<()> {
    let item = NewOutboxItem {
        station_id: station.clone(),
        origin: "board".to_owned(),
        payload: serde_json::to_value(CommandKind::Session {
            command: SessionCommand::CoordinatorRevoke {
                session_id: session.clone(),
                grant_id: grant_id.to_owned(),
            },
        })?,
        expires_at: now.saturating_add(delivery_ms),
    };
    let id: String = fleetdeck_core::ids::new_id(ids);
    insert_item(tx, &id, &item, now)?;
    Ok(())
}

/// The `coordinator.ended` rules of [`record_lifecycle`]: `idle` expires
/// the bound binding, any other reason revokes it; a revoked grant stays
/// revoked. A grant with a row (MCP P4b §6.4) ends by the rule of
/// [`owner_ended`] first. Returns the stations whose scopes
/// changed.
fn apply_ended(
    tx: &Connection,
    station: &StationId,
    (binding_id, generation): (&str, u32),
    reason: &str,
    now: i64,
) -> Result<BTreeSet<StationId>> {
    if let Some(changed) = owner_ended(tx, station, (binding_id, generation), reason, now)? {
        return Ok(changed);
    }
    let state = if reason == "idle" {
        "expired"
    } else {
        "revoked"
    };
    let n = tx.execute(
        "UPDATE tokens SET state = ?4, revoked_reason = ?5
         WHERE station_id = ?1 AND binding_id = ?2 AND bound_generation = ?3
           AND state != 'revoked'",
        params![station.as_str(), binding_id, generation, state, reason],
    )?;
    invalidate_ended_binding(tx, station, binding_id, generation)?;
    // The grant's own station: the link cache reads its end texts again
    // (G6 §3.6).
    Ok(if n > 0 {
        BTreeSet::from([station.clone()])
    } else {
        BTreeSet::new()
    })
}

/// The station of `grant_id` when a write changed that grant (`n == 1`):
/// the caller notes it so that the link cache reads the end texts again
/// (G6 §3.6).
pub(super) fn ended_station(
    tx: &Connection,
    grant_id: &str,
    n: usize,
) -> Result<Option<StationId>> {
    if n != 1 {
        return Ok(None);
    }
    Ok(tx
        .query_row(
            "SELECT station_id FROM tokens WHERE grant_id = ?1",
            [grant_id],
            |r| r.get::<_, String>(0),
        )
        .optional()?
        .map(StationId::from))
}

/// The late-bound rule (design §6.4): a `coordinator.bound` for a terminal
/// grant with a row ends the grant on the station too. Takes the next
/// sequence of the session and queues a set with no spec.
fn queue_end(
    tx: &Connection,
    ids: &dyn IdSource,
    (station, session): (&StationId, &SessionId),
    grant_id: &str,
    now: i64,
) -> Result<()> {
    let seq = owner_grants::bump_seq(tx, station, session)?;
    owner_grants::queue_set_tx(tx, ids, station, session, seq, grant_id, None, now)?;
    Ok(())
}

/// The rebind of a grant in effect by a higher generation (design §5): the
/// binding moves to the new generation; the idle clock, the usage counters
/// and the session stay.
fn rebind(tx: &Connection, b: &CoordinatorBinding) -> Result<()> {
    tx.execute(
        "UPDATE tokens SET bound_generation = ?2, bound_native_id = ?3,
             bound_provider_instance = ?4, binding_id = ?5
         WHERE grant_id = ?1 AND state = 'active'",
        params![
            b.grant_id,
            b.generation,
            b.native_id,
            b.provider_instance.0,
            b.binding_id
        ],
    )?;
    invalidate_stale_generation(tx, &b.grant_id, b.generation)?;
    Ok(())
}

/// The terminal rule of a `coordinator.ended` for a grant with a row (design
/// §6.4): the event names the binding `"{session}:{grant}"` of the row, a
/// terminal reason, and a generation that is not below the bound one. The
/// grant becomes terminal (`revoked` with the reason, or `expired` for
/// `expired`) and its pending fleet items fail. The row stays. Returns the
/// stations whose scopes changed, or `None` when the rule does not match
/// (today's exact-generation rule then applies).
fn owner_ended(
    tx: &Connection,
    station: &StationId,
    (binding_id, generation): (&str, u32),
    reason: &str,
    now: i64,
) -> Result<Option<BTreeSet<StationId>>> {
    if !matches!(
        reason,
        "identity_changed" | "expired" | "revoked" | "replaced"
    ) {
        return Ok(None);
    }
    let found: Option<(String, Option<i64>)> = tx
        .query_row(
            "SELECT t.grant_id, t.bound_generation
             FROM session_owner_grant o JOIN tokens t ON t.grant_id = o.grant_id
             WHERE o.station_id = ?1 AND t.station_id = ?1
               AND ?2 = o.session_id || ':' || o.grant_id",
            params![station.as_str(), binding_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((grant_id, bound)) = found else {
        return Ok(None);
    };
    if bound.is_some_and(|bound| i64::from(generation) < bound) {
        return Ok(None);
    }
    if reason != "expired" {
        return Ok(Some(
            super::worker_grant::revoke_tx(tx, &grant_id, reason, now)?.1,
        ));
    }
    let n = tx.execute(
        "UPDATE tokens SET state = 'expired', revoked_reason = 'expired'
         WHERE grant_id = ?1 AND state != 'revoked'",
        [&grant_id],
    )?;
    if n == 1 {
        let none = BTreeSet::new();
        grant_narrowing::fail_removed_tx(tx, &grant_id, &none, Removal::Revoked, now)?;
    }
    invalidate_coordinator_directives(tx, &grant_id)?;
    let mut changed = sync_coordinate_scopes(tx, now)?;
    if n == 1 {
        changed.insert(station.clone());
    }
    Ok(Some(changed))
}

/// Applies the coordinator binding projection of a coordinator-capable
/// snapshot in the snapshot transaction (ADR-006 §6.1, test 25e), with the
/// admission rules of the lifecycle events: a pending binding changes
/// nothing; an active one binds like `coordinator.bound` (generation 1
/// only for a grant awaiting its initial binding, a higher generation
/// only for the recorded renewal); an expired or revoked one first binds
/// like that and then ends like `coordinator.ended`. A grant of the
/// station that never bound and whose binding the station revoked is
/// revoked (fail closed). Nothing revives a revoked grant.
/// A synthetic `Revoked` or `Expired` entry (no `native_id`) of a grant with
/// a row ends it by the rule of [`owner_ended`] (MCP P4b §5).
pub(super) fn apply_projection(
    tx: &Connection,
    station: &StationId,
    at: i64,
    projection: &[fleetdeck_proto::CoordinatorBindingState],
) -> Result<BTreeSet<StationId>> {
    let before = grant_audit::active_of_station(tx, station)?;
    let changed = apply_projection_rows(tx, station, at, projection)?;
    grant_audit::audit_ends_tx(tx, station, &before, "snapshot", at)?;
    Ok(changed)
}

/// The body of [`apply_projection`], without the audit rows.
fn apply_projection_rows(
    tx: &Connection,
    station: &StationId,
    at: i64,
    projection: &[fleetdeck_proto::CoordinatorBindingState],
) -> Result<BTreeSet<StationId>> {
    use fleetdeck_proto::CoordinatorBindingPhase as Phase;
    let mut changed = BTreeSet::new();
    for p in projection {
        if p.state == Phase::Pending {
            continue;
        }
        if let Some(native_id) = &p.native_id {
            let binding = CoordinatorBinding {
                binding_id: p.binding_id.clone(),
                grant_id: p.grant_id.clone(),
                generation: p.generation,
                provider_instance: p.provider_instance.clone().into(),
                native_id: native_id.clone(),
            };
            apply_bound(tx, station, &p.session_id, at, &binding, None)?;
        }
        let reason = match p.state {
            Phase::Pending | Phase::Active => continue,
            Phase::Expired => "idle",
            Phase::Revoked => "snapshot_revoked",
        };
        if p.native_id.is_none() {
            let owner_reason = if p.state == Phase::Revoked {
                "revoked"
            } else {
                "expired"
            };
            let key = (p.binding_id.as_str(), p.generation);
            if let Some(ended) = owner_ended(tx, station, key, owner_reason, at)? {
                changed.extend(ended);
                continue;
            }
        }
        changed.extend(apply_ended(
            tx,
            station,
            (&p.binding_id, p.generation),
            reason,
            at,
        )?);
        if p.state == Phase::Revoked {
            let n = tx.execute(
                "UPDATE tokens SET state = 'revoked', revoked_reason = 'snapshot_revoked'
                 WHERE station_id = ?1 AND grant_id = ?2 AND binding_id IS NULL
                   AND state != 'revoked'",
                params![station.as_str(), p.grant_id],
            )?;
            if n > 0 {
                changed.insert(station.clone());
            }
        }
    }
    // The projection can reactivate, end or revoke grants (design §2.3).
    changed.extend(sync_coordinate_scopes(tx, at)?);
    Ok(changed)
}

/// True if the first `coordinator.bound` of an owner-start grant may name
/// its session (DL-217): the grant has no session yet, and the deck's
/// receipt of its start command names no other session. The station
/// binds only the grant id that the deck put into the start command.
fn claims_start(
    tx: &Connection,
    grant_id: &str,
    grant_session: &SessionId,
    session: &SessionId,
) -> Result<bool> {
    if !grant_session.0.is_empty() {
        return Ok(false);
    }
    let receipt: Option<Option<String>> = tx
        .query_row(
            "SELECT json_extract(c.receipt, '$.result.session_id')
             FROM tokens t LEFT JOIN commands c ON c.command_id = t.start_command_id
             WHERE t.grant_id = ?1 AND t.start_command_id IS NOT NULL",
            [grant_id],
            |r| r.get(0),
        )
        .optional()?;
    Ok(match receipt {
        None => false,
        Some(named) => named.is_none_or(|s| s == session.0),
    })
}

#[cfg(test)]
#[path = "grants_lifecycle_notes_tests.rs"]
mod notes_tests;
