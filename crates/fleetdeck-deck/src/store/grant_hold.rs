//! B26: the grants of a station's sessions survive its update or restart
//! (design `grants-update.md` §3.2-§3.3).

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, PoisonError};

use fleetdeck_core::ids::IdSource;
use fleetdeck_proto::{IncarnationId, SessionId, StationId};
use rusqlite::{Connection, OptionalExtension, params};

use super::{active_grants, in_effect};
use crate::store::grant_audit::{
    DROPPED, KEPT, REASON_STATION_OFFLINE, REASON_STATION_RESET, REASON_STATION_UPDATED,
    active_of_station, audit_tx, live_of_station, station_name,
};
use crate::store::grants::owner_grants;
use crate::store::{DeckStore, Inner, Result, call, now_ms};

#[path = "grant_hold_tests.rs"]
#[cfg(test)]
mod tests;

/// How long after the last frame of an offline station the deck counts the
/// time as use of the bound grants of its sessions.
pub const GRANT_HOLD_MAX_MS: i64 = 60 * 60 * 1000;

/// A gap of at least this length gets a `grant_holds` row, so the return
/// writes a kept row; a shorter gap only touches (T12: a flapping link
/// writes no audit rows).
pub const HOLD_AUDIT_MIN_MS: i64 = 30 * 1000;

/// The stations with a live link now. The reachability lock is taken and
/// released here, before the caller's first statement.
fn online(inner: &Inner) -> BTreeSet<StationId> {
    inner
        .reachable
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .keys()
        .cloned()
        .collect()
}

/// The hold of §3.1 item 1 in the caller's transaction: every `active`,
/// bound grant whose station is not in `online` (and is `only`, when it is
/// given), whose station was last heard at most `GRANT_HOLD_MAX_MS` ago (and
/// not after `now`), and that was in effect at that last frame, gets
/// `last_used_at = now`, and a `grant_holds` row (kept when it exists) when
/// the gap `now - last_seen` is at least `HOLD_AUDIT_MIN_MS`. Then the holds
/// of grants that are not `active` go. Returns the number of touched grants.
pub(in crate::store) fn hold_tx(
    tx: &Connection,
    online: &BTreeSet<StationId>,
    only: Option<&StationId>,
    now: i64,
) -> Result<usize> {
    let mut gaps: BTreeMap<StationId, i64> = BTreeMap::new();
    {
        let mut stmt = tx.prepare(
            "SELECT station_id, last_seen FROM stations
             WHERE last_seen IS NOT NULL AND (?1 IS NULL OR station_id = ?1)",
        )?;
        let rows = stmt.query_map([only.map(StationId::as_str)], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
        })?;
        for row in rows {
            let (id, seen) = row?;
            let id = StationId::from(id);
            if !online.contains(&id) && seen <= now && now - seen <= GRANT_HOLD_MAX_MS {
                gaps.insert(id, seen);
            }
        }
    }
    let mut held = 0;
    for grant in active_grants(tx)? {
        let Some(&since) = gaps.get(&grant.station_id) else {
            continue;
        };
        if grant.binding.is_none() || now > grant.expires_at || !in_effect(&grant, since) {
            continue;
        }
        if now - since >= HOLD_AUDIT_MIN_MS {
            tx.execute(
                "INSERT INTO grant_holds (grant_id, station_id, since) VALUES (?1, ?2, ?3)
                 ON CONFLICT (grant_id) DO NOTHING",
                params![grant.grant_id, grant.station_id.as_str(), since],
            )?;
        }
        held += tx.execute(
            "UPDATE tokens SET last_used_at = MAX(COALESCE(last_used_at, ?2), ?2)
             WHERE grant_id = ?1 AND state = 'active'",
            params![grant.grant_id, now],
        )?;
    }
    tx.execute(
        "DELETE FROM grant_holds
         WHERE grant_id NOT IN (SELECT grant_id FROM tokens WHERE state = 'active')",
        [],
    )?;
    Ok(held)
}

/// The sweep step (touch point 5): [`hold_tx`] for every offline station.
pub(in crate::store) fn hold_offline(tx: &Connection, inner: &Inner, now: i64) -> Result<usize> {
    hold_tx(tx, &online(inner), None, now)
}

/// What one `station.info` answer settled.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Resolved {
    pub kept: usize,
    pub dropped: usize,
    /// This answer opened the return gate of its link (§3.3a): the link
    /// waited, and the check committed.
    pub settled: bool,
}

/// Ends one grant of `station` with `reason` (§3.3 item 6). Returns whether
/// the grant changed and the stations whose scopes changed.
fn end_grant_tx(
    tx: &Connection,
    ids: &dyn IdSource,
    station: &StationId,
    grant_id: &str,
    reason: &str,
    (now, delivery_ms): (i64, i64),
) -> Result<(bool, BTreeSet<StationId>)> {
    let (row, scopes) = owner_grants::revoke_with_row_tx(tx, ids, grant_id, reason, now)?;
    if row {
        // The grant was not revoked (it is in the drop set), so it changed.
        return Ok((true, scopes));
    }
    let (changed, scopes) = crate::store::worker_grant::revoke_tx(tx, grant_id, reason, now)?;
    if changed {
        let session: Option<String> = tx
            .query_row(
                "SELECT session_id FROM tokens WHERE grant_id = ?1",
                [grant_id],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(session) = session.filter(|s| !s.is_empty()) {
            crate::store::grants_lifecycle::queue_revoke(
                tx,
                station,
                &SessionId(session),
                grant_id,
                (ids, now, delivery_ms),
            )?;
        }
    }
    Ok((changed, scopes))
}

/// §3.3 in the caller's transaction. `answer` is the incarnation and the
/// version of the answer; `keep` is `keep_grants_on_update`.
pub(in crate::store) fn resolve_tx(
    tx: &Connection,
    ids: &dyn IdSource,
    station: &StationId,
    (incarnation, version): (&IncarnationId, Option<&str>),
    (keep, delivery_ms): (bool, i64),
    now: i64,
) -> Result<(Resolved, BTreeSet<StationId>)> {
    let mut resolved = Resolved::default();
    let mut changed = BTreeSet::new();
    let old: Option<(Option<String>, Option<String>)> = tx
        .query_row(
            "SELECT info_incarnation, info_version FROM stations WHERE station_id = ?1",
            [station.as_str()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((old_incarnation, old_version)) = old else {
        return Ok((resolved, changed));
    };
    tx.execute(
        "UPDATE stations SET info_incarnation = ?2, info_version = COALESCE(?3, info_version)
         WHERE station_id = ?1",
        params![station.as_str(), incarnation.as_str(), version],
    )?;
    let reset = old_incarnation
        .as_deref()
        .is_some_and(|o| o != incarnation.as_str());
    let versions = match (old_version.as_deref(), version) {
        (Some(a), Some(b)) if a != b => Some((a.to_owned(), b.to_owned())),
        _ => None,
    };
    let updated = versions.is_some();
    let (from, to) = versions.unwrap_or_default();
    let holds: BTreeMap<String, i64> = {
        let mut stmt =
            tx.prepare("SELECT grant_id, since FROM grant_holds WHERE station_id = ?1")?;
        stmt.query_map([station.as_str()], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
        })?
        .collect::<rusqlite::Result<BTreeMap<_, _>>>()?
    };
    tx.execute(
        "DELETE FROM grant_holds WHERE station_id = ?1",
        [station.as_str()],
    )?;
    let name = station_name(tx, station)?;
    if reset || (updated && !keep) {
        let reason = if reset {
            REASON_STATION_RESET
        } else {
            REASON_STATION_UPDATED
        };
        let detail = if reset {
            format!("reason={reason};station={name}")
        } else {
            format!("reason={reason};station={name};from={from};to={to}")
        };
        for id in live_of_station(tx, station)? {
            let (did, scopes) = end_grant_tx(tx, ids, station, &id, reason, (now, delivery_ms))?;
            changed.extend(scopes);
            if did {
                audit_tx(tx, now, DROPPED, &id, &detail)?;
                resolved.dropped += 1;
            }
        }
    } else {
        for id in active_of_station(tx, station)? {
            let since = holds.get(&id).copied();
            let detail = match (updated, since) {
                (true, Some(s)) => format!(
                    "reason={REASON_STATION_UPDATED};station={name};from={from};to={to};offline_ms={}",
                    now - s
                ),
                (true, None) => {
                    format!("reason={REASON_STATION_UPDATED};station={name};from={from};to={to}")
                }
                (false, Some(s)) => format!(
                    "reason={REASON_STATION_OFFLINE};station={name};offline_ms={}",
                    now - s
                ),
                (false, None) => continue,
            };
            audit_tx(tx, now, KEPT, &id, &detail)?;
            resolved.kept += 1;
        }
    }
    Ok((resolved, changed))
}

impl DeckStore {
    /// The link start of `station` (§3.2 item 5): settles the offline gap of
    /// `station` with [`hold_tx`] (only `station`; nothing when another link
    /// of it is live) and reads whether the deck has a stored `station.info`
    /// answer of it (`stations.info_incarnation IS NOT NULL`). `true` means
    /// a returning link: it waits for its return check (§3.3a). No row:
    /// `false`. One `call` closure, one transaction, `now_ms(&inner.clock)`.
    ///
    /// # Errors
    /// A database error.
    pub async fn hold_returning(&self, station: StationId) -> Result<bool> {
        let inner = Arc::clone(&self.inner);
        call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            let now = now_ms(&inner.clock);
            hold_tx(&tx, &online(&inner), Some(&station), now)?;
            let returning: Option<bool> = tx
                .query_row(
                    "SELECT info_incarnation IS NOT NULL FROM stations WHERE station_id = ?1",
                    [station.as_str()],
                    |r| r.get(0),
                )
                .optional()?;
            tx.commit()?;
            Ok(returning.unwrap_or(false))
        })
        .await
    }

    /// Settles the holds and the update rule of `station` with one
    /// `station.info` answer of live link `token`. Nothing is written when
    /// the link ended. After the commit, opens the return gate of `token`
    /// (`Inner::settle`, §3.3a). Notes the changed scopes; wakes the command
    /// router when it dropped a grant. An error leaves the gate closed.
    ///
    /// # Errors
    /// A database error.
    pub async fn resolve_grant_holds(
        &self,
        station: StationId,
        token: u64,
        incarnation: IncarnationId,
        version: Option<String>,
    ) -> Result<Resolved> {
        let inner = Arc::clone(&self.inner);
        let (resolved, changed) = call(&self.inner.db, move |conn| {
            if !inner.link_live(&station, token) {
                return Ok((Resolved::default(), BTreeSet::new()));
            }
            let knobs = inner.knobs();
            let tx = conn.transaction()?;
            let now = now_ms(&inner.clock);
            let (mut resolved, changed) = resolve_tx(
                &tx,
                inner.ids.as_ref(),
                &station,
                (&incarnation, version.as_deref()),
                (knobs.keep_grants_on_update, knobs.delivery_ms),
                now,
            )?;
            tx.commit()?;
            resolved.settled = inner.settle(&station, token);
            Ok((resolved, changed))
        })
        .await?;
        self.note_scope_changes(changed);
        if resolved.dropped > 0 {
            self.commands_changed();
        }
        Ok(resolved)
    }
}
