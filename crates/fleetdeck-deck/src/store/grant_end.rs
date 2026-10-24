//! Why a grant is not in effect, and the text that a refused call of its
//! session shows (G6, design `grant-idle.md` §3.5).

use std::collections::BTreeMap;

use fleetdeck_proto::{SessionId, StationId};
use rusqlite::{Connection, params};

use super::{GRANT_COLUMNS, Grant, GrantState, caller_grant, finish, grant_row};
use crate::store::grant_scopes::in_effect;
use crate::store::{DeckStore, Result, call};

/// Why a grant is not in effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrantEnd {
    /// Idle longer than its idle time: stored `expired`/`idle`, or still
    /// `active` and idle (the sweep has not run yet).
    Idle {
        idle_ttl_ms: i64,
    },
    /// Past `expires_at`, or stored `expired` for another reason.
    Lifetime,
    Revoked,
}

/// What the link cache keeps of the ended newest grant of a session
/// (§3.6, revision 2 R2-3): the end when the cache was filled and the
/// lifetime. The end is classified again at each refusal with [`Self::at`],
/// so a lifetime that passes later needs no refresh.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EndFacts {
    pub end: GrantEnd,
    pub expires_at: i64,
}

impl EndFacts {
    /// The end at `now`, in the order of [`grant_end`]: revoked stays
    /// revoked; past `expires_at` is the lifetime; else the cached end.
    #[must_use]
    pub fn at(self, now: i64) -> GrantEnd {
        match self.end {
            GrantEnd::Revoked => GrantEnd::Revoked,
            _ if now > self.expires_at => GrantEnd::Lifetime,
            end => end,
        }
    }
}

/// Why `grant` is not in effect at `now`; `None` while it is in effect.
/// The order: revoked, then the lifetime, then the idle time.
#[must_use]
pub fn grant_end(grant: &Grant, now: i64) -> Option<GrantEnd> {
    let idle = GrantEnd::Idle {
        idle_ttl_ms: grant.idle_ttl_ms,
    };
    match grant.state {
        GrantState::Revoked => Some(GrantEnd::Revoked),
        _ if now > grant.expires_at => Some(GrantEnd::Lifetime),
        GrantState::Expired if grant.revoked_reason.as_deref() == Some("idle") => Some(idle),
        GrantState::Expired => Some(GrantEnd::Lifetime),
        GrantState::Active if in_effect(grant, now) => None,
        GrantState::Active => Some(idle),
    }
}

/// The text of a refused call (exact strings; tests compare them).
#[must_use]
pub fn end_text(end: GrantEnd) -> String {
    match end {
        GrantEnd::Idle { idle_ttl_ms } => format!(
            "Your fleet grant expired (idle for {}). The owner can renew it on the board.",
            duration_text(idle_ttl_ms)
        ),
        GrantEnd::Lifetime => "Your fleet grant expired (its lifetime ended). \
            The owner can give a new grant on the board."
            .to_owned(),
        GrantEnd::Revoked => {
            "Your fleet grant was revoked. The owner can give a new grant on the board.".to_owned()
        }
    }
}

/// Whole minutes as minutes, else seconds rounded up (at least 1).
fn duration_text(ms: i64) -> String {
    let unit = |n: i64, one: &str| {
        if n == 1 {
            format!("1 {one}")
        } else {
            format!("{n} {one}s")
        }
    };
    if ms >= 60_000 && ms % 60_000 == 0 {
        unit(ms / 60_000, "minute")
    } else {
        unit(((ms.max(1) + 999) / 1000).max(1), "second")
    }
}

/// The text for a refused call of `session` on `station`: the end of its
/// newest grant (`caller_grant`), or `None` when it has no grant or the
/// grant is in effect.
pub(crate) fn grant_refusal(
    conn: &Connection,
    station: &StationId,
    session: &SessionId,
    now: i64,
) -> Result<Option<String>> {
    Ok(caller_grant(conn, station, session)?
        .and_then(|g| grant_end(&g, now))
        .map(end_text))
}

/// The end facts of the newest grant of each session of `station` whose
/// newest grant is not in effect (the order of `caller_grant`:
/// `created_at`, then `grant_id`).
pub(crate) fn station_grant_ends(
    conn: &Connection,
    station: &StationId,
    now: i64,
) -> Result<BTreeMap<SessionId, EndFacts>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {GRANT_COLUMNS} FROM tokens WHERE station_id = ?1
         ORDER BY created_at ASC, grant_id ASC"
    ))?;
    let rows = stmt
        .query_map(params![station.as_str()], grant_row)?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let mut newest: BTreeMap<SessionId, Grant> = BTreeMap::new();
    for raw in rows {
        let grant = finish(raw)?;
        newest.insert(grant.session_id.clone(), grant);
    }
    Ok(newest
        .into_iter()
        .filter_map(|(session, g)| {
            grant_end(&g, now).map(|end| {
                (
                    session,
                    EndFacts {
                        end,
                        expires_at: g.expires_at,
                    },
                )
            })
        })
        .collect())
}

impl DeckStore {
    /// [`station_grant_ends`] at the time read in the call (§3.6).
    ///
    /// # Errors
    /// A database error.
    pub(crate) async fn grant_ends(
        &self,
        station: StationId,
    ) -> Result<BTreeMap<SessionId, EndFacts>> {
        let clock = self.inner.clock.clone();
        call(&self.inner.db, move |conn| {
            station_grant_ends(conn, &station, crate::store::now_ms(&clock))
        })
        .await
    }
}

#[cfg(test)]
#[path = "grant_end_tests.rs"]
mod tests;
