//! Coordinator grant lifecycle: the `coordinator.bound` and
//! `coordinator.ended` rules and the snapshot projection (moved out of
//! [`super::grants`]; no behavior change).

use std::collections::BTreeSet;

use fleetdeck_core::ids::IdSource;
use fleetdeck_proto::mutation::CoordinatorBinding;
use fleetdeck_proto::{SessionId, StationId};
use rusqlite::{Connection, OptionalExtension, params};

use super::display_directives_coordinator::{
    invalidate_ended_binding, invalidate_stale_generation,
};
use super::grant_scopes::sync_coordinate_scopes;
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
        } => apply_ended(tx, station, binding_id, *generation, reason)?,
        _ => return Ok(BTreeSet::new()),
    }
    sync_coordinate_scopes(tx, now)
}

/// The `coordinator.bound` rules of [`record_lifecycle`], also used by a
/// snapshot projection (ADR-006 §6.1, test 25e).
///
/// A binding of a worker grant that is terminal and never bound (MCP P4a
/// §6.4) changes nothing, and `late` queues `session.coordinator_revoke` for
/// the session, so the station's binding ends too.
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
        let item = NewOutboxItem {
            station_id: station.clone(),
            origin: "board".to_owned(),
            payload: serde_json::to_value(CommandKind::Session {
                command: SessionCommand::CoordinatorRevoke {
                    session_id: session.clone(),
                    grant_id: b.grant_id.clone(),
                },
            })?,
            expires_at: now.saturating_add(delivery_ms),
        };
        let id: String = fleetdeck_core::ids::new_id(ids);
        insert_item(tx, &id, &item, now)?;
    }
    // A first binding needs no renewal; a higher generation of a bound
    // grant needs the owner's matching pending renewal (25b).
    let renewal = renew_generation(tx, &b.grant_id)? == Some(i64::from(b.generation));
    let first = grant.binding.is_none() && grant.state == GrantState::Active;
    let renewed = grant
        .binding
        .as_ref()
        .is_some_and(|old| old.generation < b.generation)
        && renewal
        && grant.state != GrantState::Revoked;
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

/// The `coordinator.ended` rules of [`record_lifecycle`]: `idle` expires
/// the bound binding, any other reason revokes it; a revoked grant stays
/// revoked.
fn apply_ended(
    tx: &Connection,
    station: &StationId,
    binding_id: &str,
    generation: u32,
    reason: &str,
) -> Result<()> {
    let state = if reason == "idle" {
        "expired"
    } else {
        "revoked"
    };
    tx.execute(
        "UPDATE tokens SET state = ?4, revoked_reason = ?5
         WHERE station_id = ?1 AND binding_id = ?2 AND bound_generation = ?3
           AND state != 'revoked'",
        params![station.as_str(), binding_id, generation, state, reason],
    )?;
    invalidate_ended_binding(tx, station, binding_id, generation)?;
    Ok(())
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
pub(super) fn apply_projection(
    tx: &Connection,
    station: &StationId,
    at: i64,
    projection: &[fleetdeck_proto::CoordinatorBindingState],
) -> Result<BTreeSet<StationId>> {
    use fleetdeck_proto::CoordinatorBindingPhase as Phase;
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
        apply_ended(tx, station, &p.binding_id, p.generation, reason)?;
        if p.state == Phase::Revoked {
            tx.execute(
                "UPDATE tokens SET state = 'revoked', revoked_reason = 'snapshot_revoked'
                 WHERE station_id = ?1 AND grant_id = ?2 AND binding_id IS NULL
                   AND state != 'revoked'",
                params![station.as_str(), p.grant_id],
            )?;
        }
    }
    // The projection can reactivate, end or revoke grants (design §2.3).
    sync_coordinate_scopes(tx, at)
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
