//! The owner grant of a session on the deck (MCP P4b, design
//! `docs/v2/design/mcp-p4b.md` §3, §6.1 to §6.3, §9): the per-session
//! sequence in `session_owner_grant`, the one pending
//! `session.coordinator_set` of a session, and the give, change and revoke
//! transactions that queue it.
//!
//! - Every function takes the caller's transaction; a `DeckStore` wrapper at
//!   the end runs one, notes the changed scopes and tells the router.
//! - A grant is **terminal** when it is `revoked`, `expired` for a reason
//!   other than `idle`, or past its lifetime. Every other grant is **held**:
//!   in effect, `active` with its idle time passed, or `expired` for `idle`
//!   inside its lifetime (the owner can renew it).
//! - The router sends a queued set after the commit, once the link negotiated
//!   `coordinator_live.v1`.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use fleetdeck_core::ids::{IdSource, new_id};
use fleetdeck_proto::command::{
    CommandOrigin, CoordinatorSpec, DeckCommandState, GrantKind, RETRY_WINDOW_MS, ReceiptState,
    SessionCommand,
};
use fleetdeck_proto::version::feature;
use fleetdeck_proto::{CommandEnvelope, CommandId, Receipt, SessionId, StationId};
use rusqlite::{Connection, OptionalExtension, params};

use super::owner_spec::{SpecFeatures, station_spec};
use super::{
    DEFAULT_ANSWER_CAP, DEFAULT_SPAWN_CAP, GRANT_COLUMNS, Grant, GrantClass, GrantState,
    MAX_ANSWER_CAP, NewGrant, finish, get_grant, grant_row,
};
use crate::store::coordinator::grant_narrowing::{Removal, fail_removed_tx};
use crate::store::grant_scopes::{in_effect, sync_coordinate_scopes};
use crate::store::grant_spawn_caps::spawn_caps;
use crate::store::stations::station_by_id;
use crate::store::worker_grant::{create_grant_tx, prepare_grant, revoke_tx};
use crate::store::{
    DeckError, DeckStore, LinkFeature, NewCommand, Result, call, commands, to_i64, to_u64,
};

/// The method of the command that [`queue_set_tx`] stores.
const SET_METHOD: &str = "session.coordinator_set";

/// The classes whose membership changes only at a session start.
const START_ONLY: [GrantClass; 3] = [GrantClass::Desktop, GrantClass::Capture, GrantClass::Input];

/// One `session_owner_grant` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnerRow {
    pub grant_id: String,
    /// The session's sequence: the last value that the deck took.
    pub seq: u64,
    /// The highest sequence that the station acknowledged.
    pub station_seq: u64,
}

/// What the transaction functions need besides the transaction: the id source
/// and the features of the station's link that filter a station spec.
pub(crate) struct Ctx<'a> {
    pub ids: &'a dyn IdSource,
    pub features: SpecFeatures,
}

/// A give (design §6.1). `station` and `session` replace the same fields of
/// `grant`. The caller leaves `grant.start_command_id` empty.
#[derive(Debug, Clone)]
pub struct Give {
    pub station: StationId,
    pub session: SessionId,
    pub grant: NewGrant,
    pub now: i64,
}

/// Why a give does not happen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GiveError {
    /// The session has an owner grant that the owner can still renew (or that
    /// is in effect): renew or revoke it first.
    HasGrant,
}

/// A change of the classes of an owner grant (design §6.2).
#[derive(Debug, Clone)]
pub struct Change {
    pub classes: BTreeSet<GrantClass>,
    /// Only with a newly added `spawn`.
    pub spawn_cap: Option<u32>,
    /// Only with a newly added `spawn` or `manage`.
    pub max_modes: Option<BTreeMap<String, String>>,
    /// Only with a newly added `answer`.
    pub answer_cap: Option<u32>,
    /// The owner elevated this request: a widening is allowed.
    pub allow_widening: bool,
    /// The time of the change. [`DeckStore::change_access`] replaces it with
    /// the clock of the store.
    pub now: i64,
}

/// Why a change does not happen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeError {
    /// No such grant, or it is terminal.
    NotFound,
    /// A worker grant never changes its classes.
    Worker,
    /// The grant has no `session_owner_grant` row (a grant without a record).
    NoRow,
    /// The request breaks a rule; the text names it.
    Invalid(&'static str),
    /// The change widens the grant and the owner did not elevate.
    ElevationRequired,
}

/// True when `grant` is terminal at `now` (see the module text).
#[must_use]
pub fn is_terminal(grant: &Grant, now: i64) -> bool {
    let by_state = match grant.state {
        GrantState::Revoked => true,
        GrantState::Expired => grant.revoked_reason.as_deref() != Some("idle"),
        GrantState::Active => false,
    };
    by_state || now > grant.expires_at
}

/// The row of a session.
pub fn row(tx: &Connection, station: &StationId, session: &SessionId) -> Result<Option<OwnerRow>> {
    tx.query_row(
        "SELECT grant_id, seq, station_seq FROM session_owner_grant
         WHERE station_id = ?1 AND session_id = ?2",
        params![station.as_str(), session.0],
        |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?,
            ))
        },
    )
    .optional()?
    .map(|(grant_id, seq, station_seq)| {
        Ok(OwnerRow {
            grant_id,
            seq: to_u64(seq)?,
            station_seq: to_u64(station_seq)?,
        })
    })
    .transpose()
}

/// The row of a grant, with its station and session.
pub fn row_of_grant(
    tx: &Connection,
    grant_id: &str,
) -> Result<Option<(StationId, SessionId, OwnerRow)>> {
    tx.query_row(
        "SELECT station_id, session_id, seq, station_seq FROM session_owner_grant
         WHERE grant_id = ?1",
        [grant_id],
        |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, i64>(3)?,
            ))
        },
    )
    .optional()?
    .map(|(station, session, seq, station_seq)| {
        Ok((
            StationId::from(station),
            SessionId(session),
            OwnerRow {
                grant_id: grant_id.to_owned(),
                seq: to_u64(seq)?,
                station_seq: to_u64(station_seq)?,
            },
        ))
    })
    .transpose()
}

/// The next sequence of a session: the row's `seq` plus one, or 1.
pub fn next_seq(tx: &Connection, station: &StationId, session: &SessionId) -> Result<u64> {
    Ok(row(tx, station, session)?.map_or(1, |r| r.seq.saturating_add(1)))
}

/// Takes the next sequence of a session into its row and returns it.
pub fn bump_seq(tx: &Connection, station: &StationId, session: &SessionId) -> Result<u64> {
    let seq = next_seq(tx, station, session)?;
    tx.execute(
        "UPDATE session_owner_grant SET seq = ?3 WHERE station_id = ?1 AND session_id = ?2",
        params![station.as_str(), session.0, to_i64(seq)?],
    )?;
    Ok(seq)
}

/// Fails every pending `session.coordinator_set` of the session
/// (`superseded`), then stores the new one as `pending`. The router sends it
/// after the commit.
///
/// # Errors
/// [`DeckError::UnknownStation`], [`DeckError::NotSynced`], or a database
/// error.
#[allow(clippy::too_many_arguments)]
pub fn queue_set_tx(
    tx: &Connection,
    ids: &dyn IdSource,
    station: &StationId,
    session: &SessionId,
    seq: u64,
    grant_id: &str,
    spec: Option<CoordinatorSpec>,
    now: i64,
) -> Result<CommandId> {
    tx.execute(
        "UPDATE commands SET state = 'failed', reason = 'superseded', updated_at = ?3
         WHERE station_id = ?1 AND method = ?4 AND state = 'pending'
           AND json_extract(envelope, '$.command.session_id') = ?2",
        params![station.as_str(), session.0, now, SET_METHOD],
    )?;
    let rec =
        station_by_id(tx, station)?.ok_or_else(|| DeckError::UnknownStation(station.0.clone()))?;
    let target = rec
        .incarnation
        .ok_or_else(|| DeckError::NotSynced(rec.name.clone()))?;
    let command_id: CommandId = new_id(ids);
    let retry_until = now.saturating_add(RETRY_WINDOW_MS);
    let command = SessionCommand::CoordinatorSet {
        session_id: session.clone(),
        seq,
        grant_id: grant_id.to_owned(),
        spec: spec.map(Box::new),
    };
    let method = command.method().to_owned();
    let envelope = serde_json::to_value(CommandEnvelope {
        authority: None,
        command_id: command_id.clone(),
        target_incarnation: Some(target),
        retry_until,
        origin: CommandOrigin::Board,
        command,
    })?;
    commands::insert(
        tx,
        &NewCommand {
            command_id: command_id.clone(),
            station_id: station.clone(),
            method,
            origin: crate::link::router::origin_name(CommandOrigin::Board).to_owned(),
            envelope,
            retry_until,
            author: None,
        },
        now,
    )?;
    Ok(command_id)
}

/// The grants of a session by their `session_id`.
fn session_grants(tx: &Connection, station: &StationId, session: &SessionId) -> Result<Vec<Grant>> {
    let mut stmt = tx.prepare(&format!(
        "SELECT {GRANT_COLUMNS} FROM tokens WHERE station_id = ?1 AND session_id = ?2"
    ))?;
    let rows = stmt
        .query_map(params![station.as_str(), session.0], grant_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    rows.into_iter().map(finish).collect()
}

/// True when the session has a held owner grant: the grant of its row, or any
/// grant of kind `coordinator` that names the session (a start-time grant
/// from before P4b has no row).
fn has_held_grant(
    tx: &Connection,
    station: &StationId,
    session: &SessionId,
    now: i64,
) -> Result<bool> {
    if let Some(r) = row(tx, station, session)?
        && get_grant(tx, &r.grant_id)?.is_some_and(|g| !is_terminal(&g, now))
    {
        return Ok(true);
    }
    Ok(session_grants(tx, station, session)?
        .iter()
        .any(|g| g.kind == GrantKind::Coordinator && !is_terminal(g, now)))
}

/// Gives a grant to a session (design §6.1). Returns the grant and the
/// stations whose scopes changed.
///
/// # Errors
/// [`DeckError::Invalid`] when the grant breaks a creation rule, or a
/// database error. [`GiveError::HasGrant`] is the inner error.
pub(crate) fn give_tx(
    tx: &Connection,
    ctx: &Ctx<'_>,
    req: Give,
) -> Result<std::result::Result<(Grant, BTreeSet<StationId>), GiveError>> {
    let Give {
        station,
        session,
        mut grant,
        now,
    } = req;
    grant.station_id = station.clone();
    grant.session_id = session.clone();
    let grant = prepare_grant(grant)?;
    if has_held_grant(tx, &station, &session, now)? {
        return Ok(Err(GiveError::HasGrant));
    }
    let seq = next_seq(tx, &station, &session)?;
    let grant_id: String = new_id(ctx.ids);
    let (stored, mut scopes) = create_grant_tx(tx, &grant_id, &grant, now)?;
    tx.execute(
        "INSERT INTO session_owner_grant (station_id, session_id, grant_id, seq)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT (station_id, session_id)
         DO UPDATE SET grant_id = excluded.grant_id, seq = excluded.seq",
        params![station.as_str(), session.0, grant_id, to_i64(seq)?],
    )?;
    // The station ends a worker grant inside the set: no worker revoke.
    for worker in session_grants(tx, &station, &session)? {
        if worker.kind == GrantKind::Worker && in_effect(&worker, now) {
            scopes.extend(revoke_tx(tx, &worker.grant_id, "replaced", now)?.1);
        }
    }
    queue_set_tx(
        tx,
        ctx.ids,
        &station,
        &session,
        seq,
        &grant_id,
        Some(station_spec(&stored, ctx.features)),
        now,
    )?;
    Ok(Ok((stored, scopes)))
}

/// What a change writes besides the classes.
struct Plan {
    spawn_cap: u32,
    max_modes: BTreeMap<String, String>,
    answer_cap: Option<u32>,
}

/// The rules of a change that do not need the elevation (design §6.2) and the
/// values that it writes. Only the fields of newly added classes change.
fn plan(before: &Grant, ch: &Change) -> std::result::Result<Plan, &'static str> {
    if START_ONLY
        .iter()
        .any(|c| before.classes.contains(c) != ch.classes.contains(c))
    {
        return Err("desktop, screenshot and input access is given only at start");
    }
    if ch.classes.is_empty() {
        return Err("a coordinator grant needs a class");
    }
    let added = |c: GrantClass| ch.classes.contains(&c) && !before.classes.contains(&c);
    let (spawn, manage, answer) = (
        added(GrantClass::Spawn),
        added(GrantClass::Manage),
        added(GrantClass::Answer),
    );
    if (ch.spawn_cap.is_some() && !spawn)
        || (ch.max_modes.is_some() && !spawn && !manage)
        || (ch.answer_cap.is_some() && !answer)
    {
        return Err("a cap is changed with the caps route");
    }
    let mut out = Plan {
        spawn_cap: before.spawn_cap,
        max_modes: before.max_modes.clone(),
        answer_cap: before.answer_cap,
    };
    if spawn {
        out.spawn_cap = ch.spawn_cap.unwrap_or(DEFAULT_SPAWN_CAP);
        out.max_modes = ch.max_modes.clone().unwrap_or_default();
    } else if manage && let Some(modes) = &ch.max_modes {
        out.max_modes = modes.clone();
    }
    if spawn || (manage && ch.max_modes.is_some()) {
        // The creation rules, on the grant as it will be.
        let after = Grant {
            classes: ch.classes.clone(),
            spawn_cap: out.spawn_cap,
            max_modes: out.max_modes.clone(),
            ..before.clone()
        };
        spawn_caps(&after, spawn.then_some(out.spawn_cap), Some(&out.max_modes))?;
    }
    if answer {
        let cap = ch.answer_cap.unwrap_or(DEFAULT_ANSWER_CAP);
        if cap > MAX_ANSWER_CAP {
            return Err("answer_cap is 0 to 5000");
        }
        out.answer_cap = Some(cap);
    }
    Ok(out)
}

/// The grant before and after a change, and the stations whose scopes changed.
pub type Changed = ((Grant, Grant), BTreeSet<StationId>);

/// Changes the classes of an owner grant (design §6.2). Usage counters never
/// change.
///
/// # Errors
/// A database error. [`ChangeError`] is the inner error.
pub(crate) fn change_tx(
    tx: &Connection,
    ctx: &Ctx<'_>,
    grant_id: &str,
    ch: Change,
) -> Result<std::result::Result<Changed, ChangeError>> {
    let Some(before) = get_grant(tx, grant_id)? else {
        return Ok(Err(ChangeError::NotFound));
    };
    if before.kind == GrantKind::Worker {
        return Ok(Err(ChangeError::Worker));
    }
    if is_terminal(&before, ch.now) {
        return Ok(Err(ChangeError::NotFound));
    }
    let Some((station, session, _)) = row_of_grant(tx, grant_id)? else {
        return Ok(Err(ChangeError::NoRow));
    };
    let plan = match plan(&before, &ch) {
        Ok(plan) => plan,
        Err(text) => return Ok(Err(ChangeError::Invalid(text))),
    };
    // Every added class widens the grant.
    if !ch.allow_widening && !ch.classes.is_subset(&before.classes) {
        return Ok(Err(ChangeError::ElevationRequired));
    }
    tx.execute(
        "UPDATE tokens SET classes = ?2, spawn_cap = ?3, max_modes = ?4, answer_cap = ?5
         WHERE grant_id = ?1",
        params![
            grant_id,
            serde_json::to_string(&ch.classes)?,
            plan.spawn_cap,
            serde_json::to_string(&plan.max_modes)?,
            plan.answer_cap
        ],
    )?;
    let scopes = sync_coordinate_scopes(tx, ch.now)?;
    fail_removed_tx(tx, grant_id, &ch.classes, Removal::Narrowed, ch.now)?;
    let seq = bump_seq(tx, &station, &session)?;
    let after = get_grant(tx, grant_id)?
        .ok_or_else(|| DeckError::Corrupt("a changed grant is gone".to_owned()))?;
    queue_set_tx(
        tx,
        ctx.ids,
        &station,
        &session,
        seq,
        grant_id,
        Some(station_spec(&after, ctx.features)),
        ch.now,
    )?;
    Ok(Ok(((before, after), scopes)))
}

/// Revokes a grant that has a row (design §6.3): the grant is revoked, the
/// session takes the next sequence and `session.coordinator_set` with no spec
/// is queued. A grant that `revoke_tx` did not change (it was revoked
/// already) does nothing more. Returns `false` without a row: the caller uses
/// `session.coordinator_revoke`. Also returns the stations whose scopes
/// changed.
///
/// # Errors
/// A database error.
pub fn revoke_with_row_tx(
    tx: &Connection,
    ids: &dyn IdSource,
    grant_id: &str,
    reason: &str,
    now: i64,
) -> Result<(bool, BTreeSet<StationId>)> {
    let Some((station, session, _)) = row_of_grant(tx, grant_id)? else {
        return Ok((false, BTreeSet::new()));
    };
    let (changed, scopes) = revoke_tx(tx, grant_id, reason, now)?;
    if changed {
        let seq = bump_seq(tx, &station, &session)?;
        queue_set_tx(tx, ids, &station, &session, seq, grant_id, None, now)?;
    }
    Ok((true, scopes))
}

/// True when a `session.coordinator_set` of the session is pending.
fn pending_set(tx: &Connection, station: &StationId, session: &SessionId) -> Result<bool> {
    Ok(tx
        .query_row(
            "SELECT 1 FROM commands
             WHERE station_id = ?1 AND method = ?3 AND state = 'pending'
               AND json_extract(envelope, '$.command.session_id') = ?2
             LIMIT 1",
            params![station.as_str(), session.0, SET_METHOD],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

/// Queues the latest desired state of every session of `station` whose
/// `station_seq` is below its `seq` and that has no pending set (design §3).
/// Returns how many it queued.
///
/// # Errors
/// A database error.
pub(crate) fn reconcile_tx(
    tx: &Connection,
    ctx: &Ctx<'_>,
    station: &StationId,
    now: i64,
) -> Result<usize> {
    let mut stmt = tx.prepare(
        "SELECT session_id, grant_id, seq, station_seq FROM session_owner_grant
         WHERE station_id = ?1 AND station_seq < seq ORDER BY session_id",
    )?;
    let due = stmt
        .query_map([station.as_str()], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(stmt);
    let mut queued = 0;
    for (session, grant_id, seq) in due {
        let session = SessionId(session);
        if pending_set(tx, station, &session)? {
            continue;
        }
        let spec = get_grant(tx, &grant_id)?
            .filter(|g| !is_terminal(g, now))
            .map(|g| station_spec(&g, ctx.features));
        queue_set_tx(
            tx,
            ctx.ids,
            station,
            &session,
            to_u64(seq)?,
            &grant_id,
            spec,
            now,
        )?;
        queued += 1;
    }
    Ok(queued)
}

/// What the end of a command does to the owner grants (design §3, §6.1, §9),
/// in the transaction that ends the command. Returns the stations whose
/// scopes changed.
///
/// - An accepted `session.coordinator_set` moves the row's `station_seq`.
/// - A failed (not `superseded`) or rejected `Some` set of the row's grant,
///   when the station never acknowledged a set and the grant never bound,
///   revokes the grant (`attach_failed`). An `ambiguous` set revokes nothing.
/// - An accepted `session.start` whose receipt names `seq = 1` and a session,
///   for an owner grant, writes the row of the session if it has none.
///
/// # Errors
/// A database error.
pub fn settle_tx(
    tx: &Connection,
    id: &CommandId,
    state: DeckCommandState,
    receipt: Option<&Receipt>,
    reason: Option<&str>,
    now: i64,
) -> Result<BTreeSet<StationId>> {
    let found: Option<(String, String, String)> = tx
        .query_row(
            "SELECT method, station_id, envelope FROM commands WHERE command_id = ?1",
            [id.as_str()],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let Some((method, station, envelope)) = found else {
        return Ok(BTreeSet::new());
    };
    let station = StationId::from(station);
    let accepted = receipt
        .filter(|r| r.state == ReceiptState::Accepted)
        .and_then(|r| r.result.as_ref());
    if method == "session.start" {
        if let Some(result) = accepted
            && result.seq == Some(1)
            && let Some(session) = &result.session_id
        {
            tx.execute(
                "INSERT INTO session_owner_grant (station_id, session_id, grant_id, seq, station_seq)
                 SELECT station_id, ?2, grant_id, 1, 1 FROM tokens
                 WHERE start_command_id = ?1 AND kind = 'coordinator' AND station_id = ?3
                 ON CONFLICT (station_id, session_id) DO NOTHING",
                params![id.as_str(), session.0, station.as_str()],
            )?;
        }
        return Ok(BTreeSet::new());
    }
    if method != SET_METHOD {
        return Ok(BTreeSet::new());
    }
    let envelope: CommandEnvelope = serde_json::from_str(&envelope)?;
    let SessionCommand::CoordinatorSet {
        session_id,
        grant_id,
        spec,
        ..
    } = envelope.command
    else {
        return Ok(BTreeSet::new());
    };
    if let Some(seq) = accepted.and_then(|r| r.seq) {
        tx.execute(
            "UPDATE session_owner_grant SET station_seq = MAX(station_seq, ?3)
             WHERE station_id = ?1 AND session_id = ?2",
            params![station.as_str(), session_id.0, to_i64(seq)?],
        )?;
        return Ok(BTreeSet::new());
    }
    let rejected = receipt.is_some_and(|r| r.state == ReceiptState::Rejected);
    let failed = state == DeckCommandState::Failed && reason != Some("superseded");
    if !(rejected || failed) || spec.is_none() {
        return Ok(BTreeSet::new());
    }
    let never_acknowledged = row(tx, &station, &session_id)?
        .is_some_and(|r| r.grant_id == grant_id && r.station_seq == 0);
    let unbound = get_grant(tx, &grant_id)?.is_some_and(|g| g.binding.is_none());
    if never_acknowledged && unbound {
        return Ok(revoke_tx(tx, &grant_id, "attach_failed", now)?.1);
    }
    Ok(BTreeSet::new())
}

impl DeckStore {
    /// The features of the link of `station` that filter a station spec.
    fn spec_features(&self, station: &StationId) -> SpecFeatures {
        let on = |name: &str| self.link_feature(station, name) == LinkFeature::Negotiated;
        SpecFeatures {
            recording: on(feature::CAPTURE_RECORDING_MCP),
            input: on(feature::DESKTOP_INPUT_MCP),
            click: on(feature::DESKTOP_CLICK_MCP),
            input_any: on(feature::DESKTOP_INPUT_ANY),
        }
    }

    /// Gives `grant` to `session` in one transaction ([`give_tx`]). The
    /// router sends the queued set after the commit or on its tick.
    ///
    /// # Errors
    /// [`DeckError::Invalid`] for a value out of range, or a database error.
    pub async fn give(
        &self,
        station: StationId,
        session: SessionId,
        grant: NewGrant,
    ) -> Result<std::result::Result<Grant, GiveError>> {
        let features = self.spec_features(&station);
        let now = self.now_ms();
        let ids = Arc::clone(&self.inner.ids);
        let out = call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            let ctx = Ctx {
                ids: ids.as_ref(),
                features,
            };
            let out = give_tx(
                &tx,
                &ctx,
                Give {
                    station,
                    session,
                    grant,
                    now,
                },
            )?;
            if out.is_ok() {
                tx.commit()?;
            }
            Ok(out)
        })
        .await?;
        Ok(out.map(|(grant, scopes)| {
            self.note_scope_changes(scopes);
            self.commands_changed();
            grant
        }))
    }

    /// Changes the classes of an owner grant in one transaction
    /// ([`change_tx`]); the time is the clock of the store. Returns the grant
    /// before and after.
    ///
    /// # Errors
    /// A database error.
    pub async fn change_access(
        &self,
        grant_id: String,
        ch: Change,
    ) -> Result<std::result::Result<(Grant, Grant), ChangeError>> {
        let Some(current) = self.grant(grant_id.clone()).await? else {
            return Ok(Err(ChangeError::NotFound));
        };
        let features = self.spec_features(&current.station_id);
        let mut ch = ch;
        ch.now = self.now_ms();
        let ids = Arc::clone(&self.inner.ids);
        let out = call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            let ctx = Ctx {
                ids: ids.as_ref(),
                features,
            };
            let out = change_tx(&tx, &ctx, &grant_id, ch)?;
            if out.is_ok() {
                tx.commit()?;
            }
            Ok(out)
        })
        .await?;
        Ok(out.map(|(grants, scopes)| {
            self.note_scope_changes(scopes);
            self.commands_changed();
            grants
        }))
    }

    /// Revokes a grant that has a row ([`revoke_with_row_tx`]). Returns
    /// `false` for a grant without a row; nothing changed then.
    ///
    /// # Errors
    /// A database error.
    pub async fn revoke_with_row(&self, grant_id: String, reason: String) -> Result<bool> {
        let now = self.now_ms();
        let ids = Arc::clone(&self.inner.ids);
        let (has_row, scopes) = call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            let out = revoke_with_row_tx(&tx, ids.as_ref(), &grant_id, &reason, now)?;
            tx.commit()?;
            Ok(out)
        })
        .await?;
        self.note_scope_changes(scopes);
        if has_row {
            self.commands_changed();
        }
        Ok(has_row)
    }

    /// Queues the latest desired state of the sessions of `station` that the
    /// station did not acknowledge ([`reconcile_tx`]). Returns how many sets
    /// it queued.
    ///
    /// # Errors
    /// A database error.
    pub async fn reconcile_owner_grants(&self, station: StationId) -> Result<usize> {
        let features = self.spec_features(&station);
        let now = self.now_ms();
        let ids = Arc::clone(&self.inner.ids);
        let queued = call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            let ctx = Ctx {
                ids: ids.as_ref(),
                features,
            };
            let out = reconcile_tx(&tx, &ctx, &station, now)?;
            tx.commit()?;
            Ok(out)
        })
        .await?;
        if queued > 0 {
            self.commands_changed();
        }
        Ok(queued)
    }
    /// The ids of the grants that a `session_owner_grant` row names.
    pub async fn live_grant_ids(&self) -> Result<BTreeSet<String>> {
        call(&self.inner.db, |conn| {
            let mut stmt = conn.prepare("SELECT grant_id FROM session_owner_grant")?;
            let rows = stmt.query_map([], |r| r.get(0))?;
            Ok(rows.collect::<rusqlite::Result<_>>()?)
        })
        .await
    }
}

#[cfg(test)]
#[path = "owner_grants_tests.rs"]
mod tests;
