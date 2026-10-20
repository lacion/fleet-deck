//! The derived `coordinate` scope of a station (grant target stations design,
//! ADR-006 §"Creation", DL-230 item 7).
//!
//! - A grant is **in effect** ([`in_effect`]) when it is `active`, not past
//!   `expires_at` and not idle-expired. This is the only definition.
//! - `coordinate` in `stations.scopes` is derived only from grants: a station
//!   has it exactly when at least one grant in effect names it
//!   ([`sync_coordinate_scopes`]). The stored and cached scope is a coarse
//!   admission filter that can lag the clock; every `coordinate` admission
//!   also checks [`station_covered`] in its own transaction.
//! - Every admission (a call, a query, each pass of an authority check,
//!   [`DeckStore::station_covered_now`]) reads the shared clock inside its own
//!   database transaction, immediately before [`station_covered`], and uses
//!   that time for its remaining expiry checks. A time taken before an await
//!   (a session read, `policy_now`, the queue of the database) can be late
//!   (grant stations PRE-PR round 1, GS2).
//! - [`DeckStore::sweep_grants`] ends every grant that is `active` but past
//!   its lifetime or idle, and rebuilds the scopes. It runs at deck start
//!   and every 30 s.
//! - Every transaction that changes a scope notes the changed stations
//!   ([`DeckStore::note_scope_changes`]); the link cache (`DeckPeers`) takes
//!   the notes and refreshes each station from the store.

use std::collections::BTreeSet;

use fleetdeck_proto::StationId;
use fleetdeck_proto::link::Scope;
use rusqlite::{Connection, Transaction, params};

use super::display_directives_coordinator::invalidate_coordinator_directives;
use super::grants::{GRANT_COLUMNS, Grant, GrantState, finish, grant_row};
use super::{AuditResult, DeckStore, Inner, NewAudit, Result, call};

/// The refusal text of a `coordinate` admission when no grant in effect
/// names the station.
pub const NOT_COVERED: &str = "No coordinator grant covers this station now.";

/// True when `grant` is `active`, `now` is not past `expires_at`, and the
/// grant is not idle-expired (`last_used_at` is unset or at most
/// `idle_ttl_ms` ago).
#[must_use]
pub fn in_effect(grant: &Grant, now: i64) -> bool {
    grant.state == GrantState::Active
        && now <= grant.expires_at
        && grant
            .idle_since()
            .is_none_or(|t| now.saturating_sub(t) <= grant.idle_ttl_ms)
}

/// Every grant whose state is `active` (some may be past a deadline).
pub(super) fn active_grants(tx: &Connection) -> Result<Vec<Grant>> {
    let mut stmt = tx.prepare(&format!(
        "SELECT {GRANT_COLUMNS} FROM tokens WHERE state = 'active'"
    ))?;
    let rows = stmt
        .query_map([], grant_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    rows.into_iter().map(finish).collect()
}

/// True when at least one grant in effect names `station`. The only
/// implementation of the per-call admission rule (design §2.3a).
pub(crate) fn station_covered(tx: &Connection, station: &StationId, now: i64) -> Result<bool> {
    Ok(active_grants(tx)?
        .iter()
        .any(|g| in_effect(g, now) && g.stations.contains(station)))
}

/// Recomputes the derived `coordinate` scope of every station in the
/// caller's transaction and returns the stations whose scopes changed. Every
/// other scope of a station stays as it is.
pub(crate) fn sync_coordinate_scopes(tx: &Connection, now: i64) -> Result<BTreeSet<StationId>> {
    let covered: BTreeSet<StationId> = active_grants(tx)?
        .into_iter()
        .filter(|g| in_effect(g, now))
        .flat_map(|g| g.stations)
        .collect();
    let mut stmt = tx.prepare("SELECT station_id, scopes FROM stations")?;
    let rows = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(stmt);
    let mut changed = BTreeSet::new();
    for (id, text) in rows {
        let mut set: BTreeSet<Scope> = serde_json::from_str(&text)?;
        let station = StationId::from(id);
        let moved = if covered.contains(&station) {
            set.insert(Scope::Coordinate)
        } else {
            set.remove(&Scope::Coordinate)
        };
        if moved {
            tx.execute(
                "UPDATE stations SET scopes = ?2 WHERE station_id = ?1",
                params![station.as_str(), serde_json::to_string(&set)?],
            )?;
            changed.insert(station);
        }
    }
    Ok(changed)
}

/// Marks every grant that is `active` but past `expires_at` (`expired`) or
/// idle (`idle`) as `expired`; returns each ended grant with its reason.
fn end_stale(tx: &Connection, now: i64) -> Result<Vec<(String, &'static str)>> {
    let mut ended = Vec::new();
    for g in active_grants(tx)? {
        let reason = if now > g.expires_at {
            "expired"
        } else if g
            .idle_since()
            .is_some_and(|t| now.saturating_sub(t) > g.idle_ttl_ms)
        {
            "idle"
        } else {
            continue;
        };
        tx.execute(
            "UPDATE tokens SET state = 'expired', revoked_reason = ?2
             WHERE grant_id = ?1 AND state = 'active'",
            params![g.grant_id, reason],
        )?;
        invalidate_coordinator_directives(tx, &g.grant_id)?;
        ended.push((g.grant_id, reason));
    }
    Ok(ended)
}

/// The lazy idle expiry of a call or a query: expires the grant, rebuilds the
/// scopes, commits, and notes the changed stations.
pub(super) fn expire_idle(
    tx: Transaction<'_>,
    inner: &Inner,
    grant_id: &str,
    now: i64,
) -> Result<()> {
    tx.execute(
        "UPDATE tokens SET state = 'expired', revoked_reason = 'idle'
         WHERE grant_id = ?1 AND state = 'active'",
        [grant_id],
    )?;
    invalidate_coordinator_directives(&tx, grant_id)?;
    let changed = sync_coordinate_scopes(&tx, now)?;
    tx.commit()?;
    inner.note_scope_changes(changed);
    Ok(())
}

impl Inner {
    /// Notes stations whose link cache entry must follow the store.
    pub(super) fn note_scope_changes(&self, stations: BTreeSet<StationId>) {
        if stations.is_empty() {
            return;
        }
        self.scope_dirty
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .extend(stations);
        self.scope_changes.send_modify(|n| *n += 1);
    }
}

impl DeckStore {
    /// Notes stations whose derived scope changed (see the module doc).
    pub fn note_scope_changes(&self, stations: BTreeSet<StationId>) {
        self.inner.note_scope_changes(stations);
    }

    /// Takes the noted stations; the link cache refreshes each of them.
    #[must_use]
    pub fn take_scope_changes(&self) -> BTreeSet<StationId> {
        std::mem::take(
            &mut *self
                .inner
                .scope_dirty
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        )
    }

    /// Puts stations back into the noted set without waking the follower (a
    /// refresh failed; the next refresh tries again).
    pub fn restore_scope_changes(&self, stations: BTreeSet<StationId>) {
        self.inner
            .scope_dirty
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .extend(stations);
    }

    /// A counter that grows with each note of changed scopes.
    #[must_use]
    pub fn subscribe_scope_changes(&self) -> tokio::sync::watch::Receiver<u64> {
        self.inner.scope_changes.subscribe()
    }

    /// True when a grant in effect names `station` now (design §2.3a).
    ///
    /// # Errors
    /// A database error.
    pub async fn station_covered_now(&self, station: &StationId) -> Result<bool> {
        let (station, clock) = (station.clone(), self.inner.clock.clone());
        call(&self.inner.db, move |conn| {
            station_covered(conn, &station, super::now_ms(&clock))
        })
        .await
    }

    /// Ends every grant that is `active` but past its lifetime or idle,
    /// rebuilds the derived scopes in the same transaction, and writes one
    /// `coordinator.grant_expired` audit row for each ended grant (actor
    /// `deck`). Returns the stations whose scopes changed; they are noted.
    ///
    /// # Errors
    /// A database error.
    pub async fn sweep_grants(&self) -> Result<BTreeSet<StationId>> {
        let now = self.now_ms();
        let (ended, changed) = call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            let ended = end_stale(&tx, now)?;
            super::worker_grant::prune(&tx, now)?;
            let changed = sync_coordinate_scopes(&tx, now)?;
            tx.commit()?;
            Ok((ended, changed))
        })
        .await?;
        for (grant_id, reason) in ended {
            self.audit_or_log(
                NewAudit::new("deck", "coordinator.grant_expired", AuditResult::Ok)
                    .target(grant_id)
                    .detail(reason),
            )
            .await;
        }
        self.inner.note_scope_changes(changed.clone());
        Ok(changed)
    }
}

#[cfg(test)]
#[path = "grant_scopes_tests.rs"]
mod tests;
