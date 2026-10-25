//! The coordinator sender of a directive (R13 P6 §4.2, OD-7, decision O12-4
//! and the T5 brief): an agent with the `desktop` class shows one of its own
//! sessions or artifacts on an owner-paired wall.
//!
//! - [`DeckStore::display_directive_coordinator`] is the only entry point that
//!   writes a coordinator row. One transaction checks the display, the grant,
//!   the reference rules ([`coordinator_refs_ok`]) and the shared shape and
//!   ceiling checks, then writes the row with the provenance columns
//!   `sender_generation`, `sender_station` and `sender_session`.
//! - [`CoordinatorSender`] has a private seal field: only
//!   [`CoordinatorSender::new`] builds one, so a caller outside this module
//!   cannot claim a coordinator authority by a struct literal.
//! - The delivery reads ([`DeckStore::display_coordinator_row`],
//!   [`DeckStore::display_coordinator_valid`],
//!   [`DeckStore::display_coordinator_invalidate`]) serve the live delivery
//!   check of `coordinator_desktop::directive`.
//! - The invalidation helpers delete the directives of a grant when the grant
//!   ends, is revoked or changes its binding generation.

use std::collections::BTreeSet;

use fleetdeck_proto::coordinator_desktop_text::code;
use fleetdeck_proto::coordinator_desktop_wire::CoordinatorContext;
use fleetdeck_proto::display_directive::{DirectiveBody, DirectiveFilter, DirectiveView};
use fleetdeck_proto::{SessionId, StationId};
use rusqlite::{Connection, OptionalExtension, params};

use super::coordinator_ops::in_lineage;
use super::display_directive_schema::invalidate_directive;
use super::display_directives::{
    DirectiveError, Stored, check_references, check_shape, still_valid, store_coordinator_row,
};
use super::displays::{DisplayRecord, load};
use super::grant_scopes::in_effect;
use super::grants::{Grant, get_grant};
use super::{DeckStore, Result, call};

#[cfg(test)]
#[path = "display_directives_coordinator_tests.rs"]
mod tests;

/// The grant and the caller that a coordinator directive carries: the
/// provenance of the row. Built only by [`CoordinatorSender::new`].
// The private `seal` field keeps a struct literal out of the whole crate;
// `#[non_exhaustive]` would only do that for other crates.
#[allow(clippy::manual_non_exhaustive)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoordinatorSender {
    pub grant_id: String,
    /// The binding generation of the grant at the write.
    pub generation: u32,
    pub caller_station: StationId,
    pub caller_session: SessionId,
    /// Keeps the struct from being built outside this module.
    seal: (),
}

impl CoordinatorSender {
    /// The coordinator sender of a grant, its binding generation and its
    /// caller.
    #[must_use]
    pub(crate) fn new(
        grant_id: String,
        generation: u32,
        caller_station: StationId,
        caller_session: SessionId,
    ) -> Self {
        Self {
            grant_id,
            generation,
            caller_station,
            caller_session,
            seal: (),
        }
    }
}

/// What the entry point needs besides the body: the station that the display
/// must be bound to (the target station of the call).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoordinatorRefs {
    pub target: StationId,
}

/// A stored coordinator directive as a delivery check reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoordinatorRow {
    /// The generation that the display reads (`directive_generation`).
    pub generation: u64,
    pub directive: DirectiveBody,
    pub sender: CoordinatorSender,
    pub ceiling: Vec<StationId>,
    /// The station that the display is bound to; `None` when it is unbound.
    pub bound_station: Option<StationId>,
}

/// The station that a body names, with the filter that the deck builds: a
/// `focus` or an `artifact` directive whose filter is exactly that station.
/// `None` for any other body.
fn named_station(body: &DirectiveBody) -> Option<&StationId> {
    let (station, filter) = match body {
        DirectiveBody::Focus {
            session, filter, ..
        } => (&session.station_id, filter),
        DirectiveBody::Artifact {
            artifact, filter, ..
        } => (&artifact.station_id, filter),
        _ => return None,
    };
    let want = DirectiveFilter {
        stations: vec![station.clone()],
        repo: None,
    };
    (filter.as_ref() == Some(&want)).then_some(station)
}

/// True when `session` is the caller, or a session with this grant in its
/// spawn lineage. The lineage rule applies whatever the scope of the grant
/// (OD-7).
fn session_allowed(
    conn: &Connection,
    grant: &Grant,
    sender: &CoordinatorSender,
    station: &StationId,
    session: &SessionId,
) -> Result<bool> {
    if *station == sender.caller_station && *session == sender.caller_session {
        return Ok(true);
    }
    in_lineage(conn, grant, station, session)
}

/// True when the sessionless capture artifact `(station, artifact_id)` has a
/// deck capture record with a coordinator context of this grant, caller
/// station and caller session. An owner capture has no context.
fn capture_of_caller(
    conn: &Connection,
    sender: &CoordinatorSender,
    station: &StationId,
    artifact_id: &str,
) -> Result<bool> {
    let context: Option<String> = conn
        .query_row(
            "SELECT c.coordinator FROM desktop_captures c
             WHERE c.station_id = ?1 AND c.coordinator IS NOT NULL
               AND json_extract(c.published, '$.artifact.artifact_id') = ?2
             ORDER BY c.created_at, c.capture_id LIMIT 1",
            params![station.as_str(), artifact_id],
            |r| r.get(0),
        )
        .optional()?;
    let parsed = context.and_then(|c| serde_json::from_str::<CoordinatorContext>(&c).ok());
    Ok(parsed.is_some_and(|c| {
        c.grant_id == sender.grant_id
            && c.caller_station == sender.caller_station
            && c.caller_session == sender.caller_session
    }))
}

/// The reference rules of a coordinator directive (OD-7; they apply under
/// `scope = fleet` too):
///
/// - the body is a `focus` or an `artifact` directive with the filter that the
///   deck builds, and the referenced station is a grant station;
/// - a session reference is the caller or a session of the lineage of this
///   grant;
/// - an artifact reference has an explicit version that exists and is not
///   tombstoned, and either belongs to an allowed session, or is a sessionless
///   capture artifact of this grant and caller.
///
/// The display ceiling is checked by the shared checks, not here.
///
/// # Errors
/// A database error.
pub(in crate::store) fn coordinator_refs_ok(
    conn: &Connection,
    grant: &Grant,
    sender: &CoordinatorSender,
    body: &DirectiveBody,
) -> Result<bool> {
    let Some(station) = named_station(body) else {
        return Ok(false);
    };
    if !grant.stations.contains(station) {
        return Ok(false);
    }
    match body {
        DirectiveBody::Focus { session, .. } => session_allowed(
            conn,
            grant,
            sender,
            &session.station_id,
            &session.session_id,
        ),
        DirectiveBody::Artifact { artifact, .. } => {
            if artifact.version == 0
                || crate::store::artifact_tombstones::tombstoned(
                    conn,
                    artifact.station_id.as_str(),
                    &artifact.artifact_id,
                )?
            {
                return Ok(false);
            }
            let row: Option<(Option<String>, i64)> = conn
                .query_row(
                    "SELECT session_id, latest_version FROM artifacts
                     WHERE station_id = ?1 AND artifact_id = ?2",
                    params![artifact.station_id.as_str(), artifact.artifact_id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            let Some((session, latest)) = row else {
                return Ok(false);
            };
            if i64::from(artifact.version) > latest {
                return Ok(false);
            }
            match session {
                Some(session) => session_allowed(
                    conn,
                    grant,
                    sender,
                    &artifact.station_id,
                    &SessionId(session),
                ),
                None => {
                    capture_of_caller(conn, sender, &artifact.station_id, &artifact.artifact_id)
                }
            }
        }
        _ => Ok(false),
    }
}

/// The grant of the sender as it holds now: active, in effect (not past its
/// lifetime and not idle), the same binding generation, the same caller.
fn grant_holds(conn: &Connection, sender: &CoordinatorSender, now: i64) -> Result<Option<Grant>> {
    let Some(grant) = get_grant(conn, &sender.grant_id)? else {
        return Ok(None);
    };
    let bound = grant
        .binding
        .as_ref()
        .is_some_and(|b| b.generation == sender.generation);
    Ok((in_effect(&grant, now)
        && bound
        && grant.station_id == sender.caller_station
        && grant.session_id == sender.caller_session)
        .then_some(grant))
}

/// Step 1 of the entry point: the display exists, is not revoked and is bound
/// to `target`.
fn bound_display(
    conn: &Connection,
    display_id: &str,
    target: &StationId,
) -> Result<Option<DisplayRecord>> {
    Ok(load(conn, display_id)?
        .filter(|d| d.binding.as_ref().map(|b| &b.station_id) == Some(target)))
}

/// Steps 1 to 4 of the entry point, without the write: the code of the first
/// refusal, else the display and the expanded references.
fn judge(
    conn: &Connection,
    display_id: &str,
    (sender, refs): (&CoordinatorSender, &CoordinatorRefs),
    body: &DirectiveBody,
    now: i64,
) -> Result<
    std::result::Result<
        (
            DisplayRecord,
            Vec<fleetdeck_proto::display_directive::SessionRef>,
        ),
        &'static str,
    >,
> {
    let Some(display) = bound_display(conn, display_id, &refs.target)? else {
        return Ok(Err(code::DESKTOP_DISPLAY_NOT_FOUND));
    };
    let Some(grant) = grant_holds(conn, sender, now)? else {
        return Ok(Err(code::AUTHORITY_LOST));
    };
    if !coordinator_refs_ok(conn, &grant, sender, body)? {
        return Ok(Err(code::DESKTOP_REFERENCE_NOT_GRANTED));
    }
    if check_shape(body).is_err() {
        return Ok(Err(code::DESKTOP_REFERENCE_NOT_GRANTED));
    }
    match check_references(conn, &display.ceiling, body) {
        Ok(found) => Ok(Ok((display, found))),
        Err(DirectiveError::Store(e)) => Err(e),
        Err(_) => Ok(Err(code::DESKTOP_REFERENCE_NOT_GRANTED)),
    }
}

/// The columns of a stored coordinator row: the grant, the body, the
/// generation, the station and the session of the sender.
type RawRow = (String, String, Option<i64>, Option<String>, Option<String>);

/// The stored coordinator row of a display, with the current display state.
fn coordinator_row(conn: &Connection, display_id: &str) -> Result<Option<CoordinatorRow>> {
    let Some(display) = load(conn, display_id)? else {
        return Ok(None);
    };
    let row: Option<RawRow> = conn
        .query_row(
            "SELECT sender_id, body, sender_generation, sender_station, sender_session
             FROM display_directives WHERE display_id = ?1 AND sender_kind = 'coordinator'",
            params![display_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .optional()?;
    let Some((grant_id, body, generation, station, session)) = row else {
        return Ok(None);
    };
    // A row without its provenance, or that does not parse, is not delivered.
    let (Some(generation), Some(station), Some(session), Ok(stored)) = (
        generation.and_then(|g| u32::try_from(g).ok()),
        station,
        session,
        serde_json::from_str::<Stored>(&body),
    ) else {
        return Ok(None);
    };
    let shown: i64 = conn.query_row(
        "SELECT directive_generation FROM displays WHERE display_id = ?1",
        params![display_id],
        |r| r.get(0),
    )?;
    Ok(Some(CoordinatorRow {
        generation: u64::try_from(shown).unwrap_or(0),
        directive: stored.directive,
        sender: CoordinatorSender::new(
            grant_id,
            generation,
            StationId::from(station),
            SessionId(session),
        ),
        bound_station: display.binding.as_ref().map(|b| b.station_id.clone()),
        ceiling: display.ceiling,
    }))
}

/// Deletes the directive of every display that holds one of the grant's
/// coordinator rows selected by `ids`.
fn invalidate_displays(conn: &Connection, ids: Vec<String>) -> Result<usize> {
    let mut n = 0;
    for id in ids {
        if invalidate_directive(conn, &id)? {
            n += 1;
        }
    }
    Ok(n)
}

/// Invalidates every coordinator directive of `grant_id` (the grant is
/// revoked or ended). The caller holds the transaction. Returns the number of
/// directives that were deleted.
///
/// # Errors
/// A database error.
pub(in crate::store) fn invalidate_coordinator_directives(
    conn: &Connection,
    grant_id: &str,
) -> Result<usize> {
    let mut stmt = conn.prepare(
        "SELECT display_id FROM display_directives
         WHERE sender_kind = 'coordinator' AND sender_id = ?1",
    )?;
    let ids = stmt
        .query_map(params![grant_id], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(stmt);
    invalidate_displays(conn, ids)
}

/// Invalidates the coordinator directives of `grant_id` whose
/// `sender_generation` is not `generation` (the binding moved to a new
/// generation). The caller holds the transaction.
///
/// # Errors
/// A database error.
pub(in crate::store) fn invalidate_stale_generation(
    conn: &Connection,
    grant_id: &str,
    generation: u32,
) -> Result<usize> {
    let mut stmt = conn.prepare(
        "SELECT display_id FROM display_directives
         WHERE sender_kind = 'coordinator' AND sender_id = ?1
           AND (sender_generation IS NULL OR sender_generation != ?2)",
    )?;
    let ids = stmt
        .query_map(params![grant_id, generation], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(stmt);
    invalidate_displays(conn, ids)
}

/// Invalidates the coordinator directives of the grants that hold the ended
/// binding `(station, binding_id, generation)`. The caller holds the
/// transaction.
///
/// # Errors
/// A database error.
pub(in crate::store) fn invalidate_ended_binding(
    conn: &Connection,
    station: &StationId,
    binding_id: &str,
    generation: u32,
) -> Result<usize> {
    let mut stmt = conn.prepare(
        "SELECT grant_id FROM tokens
         WHERE station_id = ?1 AND binding_id = ?2 AND bound_generation = ?3",
    )?;
    let grants = stmt
        .query_map(params![station.as_str(), binding_id, generation], |r| {
            r.get::<_, String>(0)
        })?
        .collect::<rusqlite::Result<BTreeSet<_>>>()?;
    drop(stmt);
    let mut n = 0;
    for grant_id in grants {
        n += invalidate_coordinator_directives(conn, &grant_id)?;
    }
    Ok(n)
}

impl DeckStore {
    /// Writes a coordinator directive in one transaction.
    ///
    /// The refusals are the codes of §11.1, in this order: the display does
    /// not exist, is revoked or is not bound to `refs.target`
    /// (`desktop_display_not_found`); the grant is not active, is expired or
    /// idle, has another binding generation than `sender.generation` or
    /// another caller (`authority_lost`); a reference rule of
    /// [`coordinator_refs_ok`], the shape or the display ceiling fails
    /// (`desktop_reference_not_granted`).
    ///
    /// # Errors
    /// A database error. A refusal is `Ok(Err(code))` and stores nothing.
    pub async fn display_directive_coordinator(
        &self,
        display_id: String,
        sender: CoordinatorSender,
        body: DirectiveBody,
        refs: CoordinatorRefs,
        now: i64,
    ) -> Result<std::result::Result<DirectiveView, &'static str>> {
        call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            let (display, found) = match judge(&tx, &display_id, (&sender, &refs), &body, now)? {
                Ok(ok) => ok,
                Err(refusal) => return Ok(Err(refusal)),
            };
            let (view, _) = store_coordinator_row(&tx, &display, &sender, body, found, now)?;
            tx.commit()?;
            Ok(Ok(view))
        })
        .await
    }

    /// The checks of [`Self::display_directive_coordinator`] as a read: the
    /// same refusals in the same order, and no write.
    ///
    /// # Errors
    /// A database error. A refusal is `Ok(Err(code))`.
    pub async fn display_coordinator_precheck(
        &self,
        display_id: String,
        sender: CoordinatorSender,
        body: DirectiveBody,
        refs: CoordinatorRefs,
        now: i64,
    ) -> Result<std::result::Result<(), &'static str>> {
        call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            Ok(judge(&tx, &display_id, (&sender, &refs), &body, now)?.map(|_| ()))
        })
        .await
    }

    /// The stored coordinator row of a display. `None` when the display does
    /// not exist, is revoked, or holds no coordinator directive.
    ///
    /// # Errors
    /// A database error.
    pub async fn display_coordinator_row(
        &self,
        display_id: String,
    ) -> Result<Option<CoordinatorRow>> {
        call(&self.inner.db, move |conn| {
            coordinator_row(conn, &display_id)
        })
        .await
    }

    /// The local pass of a delivery, in one read transaction: the display
    /// exists and is bound to a grant station, the row is still the
    /// coordinator row of `generation`, the display ceiling and every
    /// reference still hold ([`still_valid`]), the grant holds, and the
    /// reference rules and the tombstones hold. Returns the grant as it is
    /// now, or `None`.
    ///
    /// # Errors
    /// A database error.
    pub async fn display_coordinator_valid(
        &self,
        display_id: String,
        generation: u64,
        now: i64,
    ) -> Result<Option<Grant>> {
        call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            let Some(row) = coordinator_row(&tx, &display_id)? else {
                return Ok(None);
            };
            if row.generation != generation {
                return Ok(None);
            }
            let Some(grant) = grant_holds(&tx, &row.sender, now)? else {
                return Ok(None);
            };
            let bound = row
                .bound_station
                .as_ref()
                .is_some_and(|s| grant.stations.contains(s));
            let holds = bound
                && still_valid(&tx, &row.ceiling, &row.directive)?
                && coordinator_refs_ok(&tx, &grant, &row.sender, &row.directive)?;
            Ok(holds.then_some(grant))
        })
        .await
    }

    /// Invalidates the coordinator directive of a display, only when the
    /// stored row is still the coordinator row of `generation`: a newer
    /// directive (any sender) stays. Returns true when a row was deleted.
    ///
    /// # Errors
    /// A database error.
    pub async fn display_coordinator_invalidate(
        &self,
        display_id: String,
        generation: u64,
    ) -> Result<bool> {
        call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            let same =
                coordinator_row(&tx, &display_id)?.is_some_and(|row| row.generation == generation);
            let deleted = same && invalidate_directive(&tx, &display_id)?;
            tx.commit()?;
            Ok(deleted)
        })
        .await
    }
}
