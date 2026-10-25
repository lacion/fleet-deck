//! The directive store of a display (R13 §5D items 3 to 5, R2).
//!
//! - [`DeckStore::display_directive_set`] validates a directive and stores it
//!   in one transaction (a monotonic generation, one row for each display);
//! - [`DeckStore::display_directive`] reads the stored row, checks it again
//!   (the sender authority and every reference against the current ceiling)
//!   and returns it, or `mission`. The read changes nothing;
//! - [`DeckStore::display_scope`] returns the ceiling narrowed by the valid
//!   directive filter (stations and repository match).
//!
//! The table `display_directives` is made by the V25 migration. The
//! invalidation helper ([`super::display_directive_schema`]) deletes a row
//! when the ceiling, the binding or the display changes.

use std::collections::BTreeSet;

use fleetdeck_proto::StationId;
use fleetdeck_proto::display_directive::{DirectiveBody, DirectiveView, SessionRef};
use rusqlite::{Connection, OptionalExtension, params};

use super::display_directives_coordinator::CoordinatorSender;
use super::displays::{DisplayRecord, load};
use super::{DeckStore, Result, call};

#[path = "display_directives_check.rs"]
mod check;
pub use check::{DirectiveError, MAX_ARTIFACT_ID};
pub(in crate::store) use check::{Stored, check_references, check_shape, still_valid};

#[cfg(test)]
#[path = "display_directives_tests.rs"]
mod tests;

/// Who sends a directive (R13 §5D item 2). The owner is granted. The station
/// sender (hotkeys, P4 task E) is granted through
/// [`DeckStore::display_directive_station`]; the coordinator sender (the
/// `desktop` class, P6) is granted only through
/// [`DeckStore::display_directive_coordinator`]: `display_directive_set` and
/// [`write_directive`] answer `not_granted` for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DirectiveSender {
    Owner,
    Station(StationId),
    Coordinator(CoordinatorSender),
}

/// What a display may read now: the stations of its ceiling, narrowed by the
/// filter of its directive (R13 §5D item 17).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectiveScope {
    pub stations: BTreeSet<StationId>,
    /// A repository basename; `None`: no repository filter.
    pub repo: Option<String>,
}

impl EffectiveScope {
    /// The ceiling, narrowed by the station list and the repository of the
    /// filter of `directive`.
    #[must_use]
    pub fn narrowed(ceiling: Vec<StationId>, directive: &DirectiveBody) -> Self {
        let mut stations: BTreeSet<StationId> = ceiling.into_iter().collect();
        let mut repo = None;
        if let Some(filter) = directive.filter() {
            if !filter.stations.is_empty() {
                stations.retain(|s| filter.stations.contains(s));
            }
            repo.clone_from(&filter.repo);
        }
        Self { stations, repo }
    }
}

/// The directive that holds now, or `mission`, with the generation. `None`
/// when the display does not exist or is revoked.
pub(super) fn live(
    conn: &Connection,
    display_id: &str,
) -> Result<Option<(u64, DirectiveBody, Vec<StationId>)>> {
    let Some(display) = load(conn, display_id)? else {
        return Ok(None);
    };
    let generation: i64 = conn.query_row(
        "SELECT directive_generation FROM displays WHERE display_id = ?1",
        params![display_id],
        |r| r.get(0),
    )?;
    let generation = u64::try_from(generation).unwrap_or(0);
    let row: Option<(String, String)> = conn
        .query_row(
            "SELECT sender_kind, body FROM display_directives WHERE display_id = ?1",
            params![display_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let directive = match row {
        // The owner and the station (hotkeys, R13 P4) senders are granted
        // (R13 §5D item 2). A coordinator row reads `mission` here: its
        // delivery needs the live checks of `coordinator_desktop::directive`,
        // so every synchronous reader fails closed.
        Some((kind, body)) if kind == "owner" || kind == "station" => {
            match serde_json::from_str::<Stored>(&body) {
                Ok(stored) if still_valid(conn, &display.ceiling, &stored.directive)? => {
                    stored.directive
                }
                _ => DirectiveBody::mission(),
            }
        }
        _ => DirectiveBody::mission(),
    };
    Ok(Some((generation, directive, display.ceiling)))
}

impl DeckStore {
    /// The directive that the display reads now and its generation. A display
    /// without a valid stored directive reads `mission`. `None` when the
    /// display does not exist or is revoked.
    ///
    /// # Errors
    /// A database error.
    pub async fn display_directive(&self, display_id: String) -> Result<Option<DirectiveView>> {
        call(&self.inner.db, move |conn| {
            Ok(
                live(conn, &display_id)?.map(|(generation, directive, _)| DirectiveView {
                    generation,
                    directive,
                }),
            )
        })
        .await
    }

    /// The effective scope of a display: the ceiling, narrowed by the station
    /// list and the repository of its valid directive. `None` when the display
    /// does not exist or is revoked.
    ///
    /// # Errors
    /// A database error.
    pub async fn display_scope(&self, display_id: String) -> Result<Option<EffectiveScope>> {
        call(&self.inner.db, move |conn| {
            Ok(live(conn, &display_id)?
                .map(|(_, directive, ceiling)| EffectiveScope::narrowed(ceiling, &directive)))
        })
        .await
    }

    /// Validates and stores a directive: one transaction that checks it,
    /// moves the generation up and replaces the row of the display.
    ///
    /// # Errors
    /// [`DirectiveError`]: the display is unknown, the body is invalid, a
    /// reference is outside the ceiling or does not exist, or the sender is
    /// not granted.
    pub async fn display_directive_set(
        &self,
        display_id: String,
        sender: DirectiveSender,
        body: DirectiveBody,
    ) -> std::result::Result<DirectiveView, DirectiveError> {
        self.display_directive_set_counted(display_id, sender, body)
            .await
            .map(|(view, _)| view)
    }

    /// As [`Self::display_directive_set`], and also the number of sessions
    /// that the directive expanded to (for the audit row).
    ///
    /// # Errors
    /// As [`Self::display_directive_set`].
    pub async fn display_directive_set_counted(
        &self,
        display_id: String,
        sender: DirectiveSender,
        body: DirectiveBody,
    ) -> std::result::Result<(DirectiveView, usize), DirectiveError> {
        let now = self.now_ms();
        call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            let Some(display) = load(&tx, &display_id)? else {
                return Ok(Err(DirectiveError::DisplayUnknown));
            };
            let out = write_directive(&tx, &display, &sender, body, now)?;
            if out.is_ok() {
                tx.commit()?;
            }
            Ok(out)
        })
        .await?
    }
}

/// The shared validated write: shape, references, the sender, the generation
/// and the stored row. The caller holds the transaction and commits it when
/// the answer is `Ok`.
///
/// `Coordinator` is refused here. `Station` is accepted: its only entry
/// point is [`DeckStore::display_directive_station`].
pub(super) fn write_directive(
    tx: &rusqlite::Transaction<'_>,
    display: &DisplayRecord,
    sender: &DirectiveSender,
    body: DirectiveBody,
    now: i64,
) -> Result<std::result::Result<(DirectiveView, usize), DirectiveError>> {
    if let Err(why) = check_shape(&body) {
        return Ok(Err(DirectiveError::Invalid(why)));
    }
    let refs = match check_references(tx, &display.ceiling, &body) {
        Ok(refs) => refs,
        Err(DirectiveError::Store(e)) => return Err(e),
        Err(e) => return Ok(Err(e)),
    };
    // A page directive is the owner's alone (B17 §3.2.5): a second barrier,
    // because the station and coordinator senders cannot build one today.
    if matches!(body, DirectiveBody::Page { .. }) && !matches!(sender, DirectiveSender::Owner) {
        return Ok(Err(DirectiveError::NotGranted));
    }
    let (kind, sender_id) = match sender {
        DirectiveSender::Owner => ("owner", String::new()),
        DirectiveSender::Station(id) => ("station", id.as_str().to_owned()),
        DirectiveSender::Coordinator(_) => {
            return Ok(Err(DirectiveError::NotGranted));
        }
    };
    store_row(tx, display, (kind, sender_id, None), body, refs, now).map(Ok)
}

/// The write of a coordinator row. Only
/// [`DeckStore::display_directive_coordinator`] calls it, after its own
/// checks of the display, the grant, the reference rules, the shape and the
/// ceiling; `refs` are the references that the ceiling check expanded.
pub(super) fn store_coordinator_row(
    tx: &rusqlite::Transaction<'_>,
    display: &DisplayRecord,
    sender: &CoordinatorSender,
    body: DirectiveBody,
    refs: Vec<SessionRef>,
    now: i64,
) -> Result<(DirectiveView, usize)> {
    let row = ("coordinator", sender.grant_id.clone(), Some(sender));
    store_row(tx, display, row, body, refs, now)
}

/// Moves the generation of the display up and replaces its directive row.
/// `row` is the `sender_kind`, the `sender_id` and the provenance of a
/// coordinator row.
fn store_row(
    tx: &rusqlite::Transaction<'_>,
    display: &DisplayRecord,
    row: (&'static str, String, Option<&CoordinatorSender>),
    body: DirectiveBody,
    refs: Vec<SessionRef>,
    now: i64,
) -> Result<(DirectiveView, usize)> {
    let display_id = display.display_id.as_str();
    let (kind, sender_id, coordinator) = row;
    tx.execute(
        "UPDATE displays SET directive_generation = directive_generation + 1
         WHERE display_id = ?1",
        params![display_id],
    )?;
    let generation: i64 = tx.query_row(
        "SELECT directive_generation FROM displays WHERE display_id = ?1",
        params![display_id],
        |r| r.get(0),
    )?;
    let members = refs.len();
    let stored = serde_json::to_string(&Stored {
        directive: body.clone(),
        refs,
    })?;
    // The provenance columns of V34: set for a coordinator row, else NULL.
    let sender_generation = coordinator.map(|c| c.generation);
    let sender_station = coordinator.map(|c| c.caller_station.as_str());
    let sender_session = coordinator.map(|c| c.caller_session.as_str());
    tx.execute(
        "INSERT INTO display_directives
             (display_id, generation, mode, sender_kind, sender_id, body, created_at,
              sender_generation, sender_station, sender_session)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
         ON CONFLICT (display_id) DO UPDATE SET
             generation = excluded.generation, mode = excluded.mode,
             sender_kind = excluded.sender_kind, sender_id = excluded.sender_id,
             body = excluded.body, created_at = excluded.created_at,
             sender_generation = excluded.sender_generation,
             sender_station = excluded.sender_station,
             sender_session = excluded.sender_session",
        params![
            display_id,
            generation,
            mode_name(&body),
            kind,
            sender_id,
            stored,
            now,
            sender_generation,
            sender_station,
            sender_session
        ],
    )?;
    Ok((
        DirectiveView {
            generation: u64::try_from(generation).unwrap_or(0),
            directive: body,
        },
        members,
    ))
}

/// The `mode` column text: the serde name of the mode.
pub fn mode_name(body: &DirectiveBody) -> &'static str {
    use fleetdeck_proto::display_directive::DirectiveMode;
    match body.mode() {
        DirectiveMode::Mission => "mission",
        DirectiveMode::Focus => "focus",
        DirectiveMode::Review => "review",
        DirectiveMode::Race => "race",
        DirectiveMode::Artifact => "artifact",
        DirectiveMode::Ambient => "ambient",
        DirectiveMode::Page => "page",
    }
}
