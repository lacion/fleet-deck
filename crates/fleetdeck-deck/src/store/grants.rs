//! Coordinator grants (M3 6-5, ADR-006 §4; PLAN §10 table `tokens`).
//!
//! The deck is the authority for a coordinator session: its classes, its
//! target stations, its spawn cap with the durable `spawn_used` counter, its
//! mode ceilings per provider family, its idle TTL and its expiry. The
//! binding fields come from the station's `coordinator.bound` facts; a call
//! is valid only with the same binding (§6.2 step 3).

use std::collections::{BTreeMap, BTreeSet};

use fleetdeck_proto::mutation::CoordinatorBinding;
use fleetdeck_proto::{ProviderInstanceId, SessionId, StationId};
use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};

use super::{DeckError, DeckStore, Result, call, new_id};

/// The default spawn cap (ADR-006 §4).
pub const DEFAULT_SPAWN_CAP: u32 = 3;
/// The highest spawn cap.
pub const MAX_SPAWN_CAP: u32 = 50;
/// The default idle TTL: 30 min.
pub const DEFAULT_IDLE_TTL_MS: i64 = 30 * 60 * 1000;
/// The longest idle TTL: 24 h.
pub const MAX_IDLE_TTL_MS: i64 = 24 * 60 * 60 * 1000;
/// The default lifetime: 12 h.
pub const DEFAULT_TTL_MS: i64 = 12 * 60 * 60 * 1000;
/// The longest lifetime: 7 days.
pub const MAX_TTL_MS: i64 = 7 * 24 * 60 * 60 * 1000;

/// A class of coordinator operations (ADR-006 §6.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GrantClass {
    Read,
    Mail,
    Drive,
    Spawn,
}

/// The state of a grant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrantState {
    Active,
    Revoked,
    Expired,
}

/// An owner-authorized grant before it is stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewGrant {
    pub station_id: StationId,
    pub session_id: SessionId,
    pub classes: BTreeSet<GrantClass>,
    /// The target stations; the coordinator's own station by default.
    pub stations: BTreeSet<StationId>,
    pub spawn_cap: u32,
    /// Provider family (`claude`, `codex`) → the most permissive native
    /// mode of a spawned child. A family without an entry cannot spawn.
    pub max_modes: BTreeMap<String, String>,
    pub idle_ttl_ms: i64,
    /// The lifetime from creation.
    pub ttl_ms: i64,
    /// The owner's session start that creates the grant (DL-217): the
    /// session id stays empty until the station names the session, and a
    /// resend of the same start returns the same grant.
    pub start_command_id: Option<String>,
}

impl NewGrant {
    /// The default grant of a coordinator session: `read` and `mail` on
    /// its own station, no spawn ceiling, 30 min idle TTL, 12 h lifetime.
    #[must_use]
    pub fn new(station_id: StationId, session_id: SessionId) -> Self {
        Self {
            stations: BTreeSet::from([station_id.clone()]),
            station_id,
            session_id,
            classes: BTreeSet::from([GrantClass::Read, GrantClass::Mail]),
            spawn_cap: DEFAULT_SPAWN_CAP,
            max_modes: BTreeMap::new(),
            idle_ttl_ms: DEFAULT_IDLE_TTL_MS,
            ttl_ms: DEFAULT_TTL_MS,
            start_command_id: None,
        }
    }
}

/// A stored grant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Grant {
    pub grant_id: String,
    pub station_id: StationId,
    pub session_id: SessionId,
    pub classes: BTreeSet<GrantClass>,
    pub stations: BTreeSet<StationId>,
    pub spawn_cap: u32,
    pub spawn_used: u32,
    pub max_modes: BTreeMap<String, String>,
    pub idle_ttl_ms: i64,
    pub created_at: i64,
    pub expires_at: i64,
    pub state: GrantState,
    pub revoked_reason: Option<String>,
    /// `None` until the station reports the binding.
    pub binding: Option<CoordinatorBinding>,
    pub last_used_at: Option<i64>,
}

pub(crate) const GRANT_COLUMNS: &str = "grant_id, station_id, session_id, classes, stations, \
    spawn_cap, spawn_used, max_modes, idle_ttl_ms, created_at, expires_at, state, revoked_reason, \
    bound_generation, bound_native_id, bound_provider_instance, binding_id, last_used_at";

fn state_of(text: &str) -> GrantState {
    match text {
        "active" => GrantState::Active,
        "revoked" => GrantState::Revoked,
        // Unknown text fails closed.
        _ => GrantState::Expired,
    }
}

pub(crate) fn grant_row(r: &Row<'_>) -> rusqlite::Result<(Grant, String, String, String)> {
    let generation: Option<i64> = r.get(13)?;
    let native: Option<String> = r.get(14)?;
    let instance: Option<String> = r.get(15)?;
    let binding_id: Option<String> = r.get(16)?;
    let grant_id: String = r.get(0)?;
    let binding = match (generation, native, instance, binding_id) {
        (Some(g), Some(native_id), Some(instance), Some(binding_id)) => Some(CoordinatorBinding {
            binding_id,
            grant_id: grant_id.clone(),
            generation: u32::try_from(g).unwrap_or(u32::MAX),
            provider_instance: ProviderInstanceId(instance),
            native_id,
        }),
        _ => None,
    };
    let state: String = r.get(11)?;
    let cap: i64 = r.get(5)?;
    let used: i64 = r.get(6)?;
    Ok((
        Grant {
            grant_id,
            station_id: StationId(r.get(1)?),
            session_id: SessionId(r.get(2)?),
            classes: BTreeSet::new(),
            stations: BTreeSet::new(),
            spawn_cap: u32::try_from(cap).unwrap_or(0),
            spawn_used: u32::try_from(used).unwrap_or(u32::MAX),
            max_modes: BTreeMap::new(),
            idle_ttl_ms: r.get(8)?,
            created_at: r.get(9)?,
            expires_at: r.get(10)?,
            state: state_of(&state),
            revoked_reason: r.get(12)?,
            binding,
            last_used_at: r.get(17)?,
        },
        r.get(3)?,
        r.get(4)?,
        r.get(7)?,
    ))
}

/// Decodes the JSON columns. Unknown class names fail closed: the grant
/// loads with no classes.
pub(crate) fn finish(raw: (Grant, String, String, String)) -> Result<Grant> {
    let (mut grant, classes, stations, modes) = raw;
    grant.classes = serde_json::from_str(&classes).unwrap_or_default();
    grant.stations = serde_json::from_str(&stations)?;
    grant.max_modes = serde_json::from_str(&modes)?;
    Ok(grant)
}

pub(crate) fn get_grant(conn: &Connection, grant_id: &str) -> Result<Option<Grant>> {
    conn.query_row(
        &format!("SELECT {GRANT_COLUMNS} FROM tokens WHERE grant_id = ?1"),
        [grant_id],
        grant_row,
    )
    .optional()?
    .map(finish)
    .transpose()
}

/// The newest grant of a coordinator session.
pub(crate) fn caller_grant(
    conn: &Connection,
    station: &StationId,
    session: &SessionId,
) -> Result<Option<Grant>> {
    conn.query_row(
        &format!(
            "SELECT {GRANT_COLUMNS} FROM tokens WHERE station_id = ?1 AND session_id = ?2
             ORDER BY created_at DESC, grant_id DESC LIMIT 1"
        ),
        params![station.as_str(), session.0],
        grant_row,
    )
    .optional()?
    .map(finish)
    .transpose()
}

impl DeckStore {
    /// Stores an owner-authorized grant (ADR-006 §4). The limits are
    /// checked here: cap ≤ 50, idle TTL ≤ 24 h, lifetime ≤ 7 days.
    ///
    /// # Errors
    /// [`DeckError::Invalid`] for a value out of range, or a database error.
    pub async fn create_grant(&self, grant: NewGrant) -> Result<Grant> {
        if grant.spawn_cap > MAX_SPAWN_CAP
            || !(1..=MAX_IDLE_TTL_MS).contains(&grant.idle_ttl_ms)
            || !(1..=MAX_TTL_MS).contains(&grant.ttl_ms)
            || grant.stations.is_empty()
        {
            return Err(DeckError::Invalid(
                "a grant needs a cap of at most 50, an idle TTL of at most 24 h, a lifetime of \
                 at most 7 days and at least one station"
                    .to_owned(),
            ));
        }
        let now = self.now_ms();
        let grant_id: String = new_id(self.inner.ids.as_ref());
        call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            if let Some(start) = grant.start_command_id.as_deref() {
                let found: Option<String> = tx
                    .query_row(
                        "SELECT grant_id FROM tokens WHERE start_command_id = ?1",
                        [start],
                        |r| r.get(0),
                    )
                    .optional()?;
                if let Some(id) = found {
                    return get_grant(&tx, &id)?
                        .ok_or_else(|| DeckError::Corrupt("a found grant is gone".to_owned()));
                }
            }
            // The owner's grant lets the caller's station relay coordinator
            // calls: it gets the link scope `coordinate` in the same
            // transaction (DL-211). Pairing alone never grants it.
            let scopes: Option<String> = tx
                .query_row(
                    "SELECT scopes FROM stations WHERE station_id = ?1",
                    [grant.station_id.as_str()],
                    |r| r.get(0),
                )
                .optional()?;
            if let Some(text) = scopes {
                let mut set: BTreeSet<fleetdeck_proto::link::Scope> = serde_json::from_str(&text)?;
                if set.insert(fleetdeck_proto::link::Scope::Coordinate) {
                    tx.execute(
                        "UPDATE stations SET scopes = ?2 WHERE station_id = ?1",
                        params![grant.station_id.as_str(), serde_json::to_string(&set)?],
                    )?;
                }
            }
            tx.execute(
                "INSERT INTO tokens (grant_id, station_id, session_id, classes, stations,
                     spawn_cap, max_modes, idle_ttl_ms, created_at, expires_at, state,
                     start_command_id)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 'active', ?11)",
                params![
                    grant_id,
                    grant.station_id.as_str(),
                    grant.session_id.0,
                    serde_json::to_string(&grant.classes)?,
                    serde_json::to_string(&grant.stations)?,
                    grant.spawn_cap,
                    serde_json::to_string(&grant.max_modes)?,
                    grant.idle_ttl_ms,
                    now,
                    now.saturating_add(grant.ttl_ms),
                    grant.start_command_id
                ],
            )?;
            let stored = get_grant(&tx, &grant_id)?
                .ok_or_else(|| DeckError::Corrupt("an inserted grant is gone".to_owned()))?;
            tx.commit()?;
            Ok(stored)
        })
        .await
    }

    /// One grant.
    ///
    /// # Errors
    /// A database error.
    pub async fn grant(&self, grant_id: String) -> Result<Option<Grant>> {
        call(&self.inner.db, move |conn| get_grant(conn, &grant_id)).await
    }

    /// Applies the station's binding facts (`coordinator.bound`, §6.1) to
    /// an active grant. A generation lower than or equal to the stored one
    /// is refused (it never becomes valid again), except the same binding.
    /// Returns `false` when nothing changed.
    ///
    /// # Errors
    /// A database error.
    pub async fn bind_grant(&self, binding: CoordinatorBinding) -> Result<bool> {
        let now = self.now_ms();
        call(&self.inner.db, move |conn| {
            let Some(grant) = get_grant(conn, &binding.grant_id)? else {
                return Ok(false);
            };
            if grant.state != GrantState::Active {
                return Ok(false);
            }
            if let Some(old) = &grant.binding
                && (old.generation >= binding.generation
                    || renew_generation(conn, &binding.grant_id)?
                        != Some(i64::from(binding.generation)))
            {
                return Ok(false);
            }
            let n = conn.execute(
                "UPDATE tokens SET bound_generation = ?2, bound_native_id = ?3,
                     bound_provider_instance = ?4, binding_id = ?5, last_used_at = ?6
                 WHERE grant_id = ?1 AND state = 'active'",
                params![
                    binding.grant_id,
                    binding.generation,
                    binding.native_id,
                    binding.provider_instance.0,
                    binding.binding_id,
                    now
                ],
            )?;
            Ok(n == 1)
        })
        .await
    }

    /// The owner renews an idle-expired grant (ADR-006 §5.5, test 25b).
    /// Only a grant that is `expired` for `idle`, still inside its
    /// lifetime and bound once gets a pending renewal generation (the bound
    /// generation + 1). The grant stays expired until the station
    /// acknowledges that generation with `coordinator.bound`. Returns the
    /// pending generation, or `None` when the grant cannot be renewed. A
    /// repeated renewal returns the same generation.
    ///
    /// # Errors
    /// A database error.
    pub async fn renew_grant(&self, grant_id: String) -> Result<Option<u32>> {
        let now = self.now_ms();
        call(&self.inner.db, move |conn| {
            let Some(grant) = get_grant(conn, &grant_id)? else {
                return Ok(None);
            };
            let idle = grant.state == GrantState::Expired
                && grant.revoked_reason.as_deref() == Some("idle");
            let Some(binding) = grant.binding.filter(|_| idle && grant.expires_at > now) else {
                return Ok(None);
            };
            let next = binding.generation.saturating_add(1);
            conn.execute(
                "UPDATE tokens SET renew_generation = ?2
                 WHERE grant_id = ?1 AND state = 'expired'",
                params![grant_id, next],
            )?;
            Ok(Some(next))
        })
        .await
    }

    /// The newest grants, at most `limit`.
    ///
    /// # Errors
    /// A database error.
    pub async fn grants(&self, limit: u32) -> Result<Vec<Grant>> {
        call(&self.inner.db, move |conn| {
            let mut stmt = conn.prepare(&format!(
                "SELECT {GRANT_COLUMNS} FROM tokens ORDER BY created_at DESC, grant_id DESC LIMIT ?1"
            ))?;
            let rows = stmt
                .query_map([limit], grant_row)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows.into_iter().map(finish).collect()
        })
        .await
    }

    /// The owner revokes a grant. From this transaction on, every decision
    /// for it is `forbidden` (ADR-006 §6.7).
    ///
    /// # Errors
    /// A database error.
    pub async fn revoke_grant(&self, grant_id: String, reason: String) -> Result<bool> {
        call(&self.inner.db, move |conn| {
            let n = conn.execute(
                "UPDATE tokens SET state = 'revoked', revoked_reason = ?2
                 WHERE grant_id = ?1 AND state != 'revoked'",
                params![grant_id, reason],
            )?;
            Ok(n == 1)
        })
        .await
    }

    /// Records that `session` on `station` was started by `grant_id` (the
    /// lineage of `interrupt` and `stop`, §6.3).
    ///
    /// # Errors
    /// A database error.
    pub async fn add_lineage(
        &self,
        grant_id: String,
        station: StationId,
        session: SessionId,
        command_id: String,
    ) -> Result<()> {
        let now = self.now_ms();
        call(&self.inner.db, move |conn| {
            conn.execute(
                "INSERT INTO coordinator_lineage
                     (command_id, grant_id, station_id, session_id, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT (command_id) DO UPDATE SET session_id = excluded.session_id",
                params![command_id, grant_id, station.as_str(), session.0, now],
            )?;
            Ok(())
        })
        .await
    }

    /// Deletes coordinator receipts older than `before` (Unix ms; the 24 h
    /// retention of ADR-004 §9.3). Grants and their `spawn_used` stay.
    ///
    /// # Errors
    /// A database error.
    pub async fn prune_coordinator_receipts(&self, before: i64) -> Result<usize> {
        call(&self.inner.db, move |conn| {
            Ok(conn.execute("DELETE FROM coordinator_receipts WHERE at < ?1", [before])?)
        })
        .await
    }

    /// The number of start intents that grants wrote (lineage rows).
    ///
    /// # Errors
    /// A database error.
    pub async fn coordinator_starts(&self) -> Result<u64> {
        call(&self.inner.db, |conn| {
            let n: i64 = conn.query_row(
                "SELECT COUNT(*) FROM coordinator_lineage WHERE start_outbox_id IS NOT NULL",
                [],
                |r| r.get(0),
            )?;
            super::to_u64(n)
        })
        .await
    }
}

/// The pending renewal generation of a grant (V10), if any.
fn renew_generation(conn: &Connection, grant_id: &str) -> Result<Option<i64>> {
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
pub(super) fn record_lifecycle(
    tx: &Connection,
    station: &StationId,
    e: &fleetdeck_proto::EventEnvelope,
) -> Result<()> {
    use fleetdeck_proto::event::{Event, EventBody};
    let EventBody::Known(event) = &e.event else {
        return Ok(());
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
        ),
        Event::CoordinatorEnded {
            binding_id,
            generation,
            reason,
        } => apply_ended(tx, station, binding_id, *generation, reason),
        _ => Ok(()),
    }
}

/// The `coordinator.bound` rules of [`record_lifecycle`], also used by a
/// snapshot projection (ADR-006 §6.1, test 25e).
fn apply_bound(
    tx: &Connection,
    station: &StationId,
    session: &SessionId,
    at: i64,
    b: &CoordinatorBinding,
) -> Result<()> {
    let Some(grant) = get_grant(tx, &b.grant_id)? else {
        return Ok(());
    };
    let ours = &grant.station_id == station
        && (&grant.session_id == session
            || claims_start(tx, &b.grant_id, &grant.session_id, session)?);
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
) -> Result<()> {
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
            apply_bound(tx, station, &p.session_id, at, &binding)?;
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
    Ok(())
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
