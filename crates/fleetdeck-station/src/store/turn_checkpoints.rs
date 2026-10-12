//! Durable turn checkpoint rows (PLAN §6.6, L13; DL-078, DL-082).
//!
//! One row per **app turn**: a station-minted ULID that is the turn part of
//! the hidden ref name. Provider turn ids (`turn-1`, …) restart in each
//! provider process, so they cannot key a ref.
//!
//! - **Row before the ref.** The station stores the row with the side
//!   `capturing` *before* it publishes a ref, and records the side's result
//!   after the publication. After a crash, recovery finds every side that is
//!   still `capturing` or `open` and settles it from the refs alone; it never
//!   captures again (the workspace changed since).
//! - **A side is decided once.** `capturing`/`open` → `available {commit}` or
//!   `unavailable {reason}`; a decided side never changes again, so a late
//!   capture result cannot overwrite an invalidation (DL-079). The one
//!   exception is [`withdraw`]: before finalization, recovery turns an
//!   `available {commit}` side whose ref is gone or names another commit into
//!   `unavailable {reason}`, so `turn.finalized` never names a checkpoint that
//!   does not exist (DL-083).
//! - **`turn.finalized` after the refs, once.** [`finalize`] needs both sides
//!   decided (so every ref that the event names exists) and appends
//!   `turn.finalized` in the transaction that marks the row `finalized`. A
//!   repeat returns the stored `station_seq` and appends nothing.
//! - **No turn, no event.** A row whose input never started a provider turn
//!   (not sent, discarded, session ended) is `abandoned` without an event.

use fleetdeck_proto::event::{CheckpointSide, DiffSummary, TurnCheckpoint};
use fleetdeck_proto::{Event, EventEnvelope, InputId, SessionId, TurnId};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use super::effects::ProcessIncarnation;
use super::{Result, StoreError, log, to_u64};

/// The station-minted id of one app turn (ULID); the ref key.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AppTurnId(pub String);

impl AppTurnId {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<String> for AppTurnId {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&str> for AppTurnId {
    fn from(s: &str) -> Self {
        Self(s.to_owned())
    }
}

impl std::fmt::Display for AppTurnId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// One stored checkpoint side.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RowSide {
    /// Completion side of a turn that has not completed yet.
    Open,
    /// A capture runs; its ref may or may not exist yet.
    Capturing,
    Available(String),
    Unavailable(String),
}

impl RowSide {
    fn decided(&self) -> bool {
        matches!(self, Self::Available(_) | Self::Unavailable(_))
    }

    fn to_columns(&self) -> (&'static str, Option<&str>) {
        match self {
            Self::Open => ("open", None),
            Self::Capturing => ("capturing", None),
            Self::Available(commit) => ("available", Some(commit)),
            Self::Unavailable(reason) => ("unavailable", Some(reason)),
        }
    }

    fn from_columns(state: &str, value: Option<String>) -> Result<Self> {
        let need = |v: Option<String>| {
            v.ok_or_else(|| StoreError::Corrupt(format!("checkpoint side {state} without value")))
        };
        Ok(match state {
            "open" => Self::Open,
            "capturing" => Self::Capturing,
            "available" => Self::Available(need(value)?),
            "unavailable" => Self::Unavailable(need(value)?),
            other => return Err(StoreError::Corrupt(format!("checkpoint side {other}"))),
        })
    }

    fn wire(&self) -> Option<CheckpointSide> {
        match self {
            Self::Available(commit) => Some(CheckpointSide::Available {
                commit: commit.clone(),
            }),
            Self::Unavailable(reason) => Some(CheckpointSide::Unavailable {
                reason: reason.clone(),
            }),
            Self::Open | Self::Capturing => None,
        }
    }
}

/// Which side of a turn checkpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Which {
    Start,
    End,
}

impl Which {
    fn columns(self) -> (&'static str, &'static str) {
        match self {
            Self::Start => ("start_state", "start_value"),
            Self::End => ("end_state", "end_value"),
        }
    }
}

/// The lifecycle of a row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowState {
    Open,
    Finalized,
    Abandoned,
}

/// A new row; [`plan`] stores it.
#[derive(Debug, Clone)]
pub struct NewTurnCheckpoint {
    pub app_turn: AppTurnId,
    pub session_id: SessionId,
    pub process: Option<ProcessIncarnation>,
    /// The input whose start side this is; `None` for a provider-initiated
    /// turn.
    pub input_id: Option<InputId>,
    /// The provider turn, when it is already known (provider-initiated).
    pub turn_id: Option<TurnId>,
    pub worktree_id: Option<String>,
    pub worktree_root: Option<String>,
    pub scope: String,
    pub start: RowSide,
}

/// One stored row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnCheckpointRow {
    pub app_turn: AppTurnId,
    pub session_id: SessionId,
    pub process: Option<ProcessIncarnation>,
    pub input_id: Option<InputId>,
    pub turn_id: Option<TurnId>,
    pub worktree_id: Option<String>,
    pub worktree_root: Option<String>,
    pub scope: String,
    pub start: RowSide,
    pub end: RowSide,
    pub state: RowState,
    /// The `station_seq` of `turn.finalized`.
    pub finalized_seq: Option<u64>,
}

/// The result of [`finalize`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Finalized {
    pub station_seq: u64,
    /// `false`: a repeat; the event was committed before.
    pub appended: bool,
}

const COLUMNS: &str = "app_turn, session_id, process, input_id, turn_id, worktree_id, \
     worktree_root, scope, start_state, start_value, end_state, end_value, state, finalized_seq";

fn row(r: &rusqlite::Row<'_>) -> rusqlite::Result<[Option<String>; 13]> {
    let mut out: [Option<String>; 13] = Default::default();
    for (i, slot) in out.iter_mut().enumerate() {
        *slot = r.get(i)?;
    }
    Ok(out)
}

fn decode(cols: [Option<String>; 13], seq: Option<i64>) -> Result<TurnCheckpointRow> {
    let [
        app_turn,
        session_id,
        process,
        input_id,
        turn_id,
        worktree_id,
        worktree_root,
        scope,
        start_state,
        start_value,
        end_state,
        end_value,
        state,
    ] = cols;
    let need = |v: Option<String>, what: &str| {
        v.ok_or_else(|| StoreError::Corrupt(format!("turn checkpoint without {what}")))
    };
    let state = match need(state, "state")?.as_str() {
        "open" => RowState::Open,
        "finalized" => RowState::Finalized,
        "abandoned" => RowState::Abandoned,
        other => {
            return Err(StoreError::Corrupt(format!(
                "turn checkpoint state {other}"
            )));
        }
    };
    Ok(TurnCheckpointRow {
        app_turn: need(app_turn, "app_turn")?.into(),
        session_id: need(session_id, "session")?.into(),
        process: process.map(ProcessIncarnation::from),
        input_id: input_id.map(InputId::from),
        turn_id: turn_id.map(TurnId::from),
        worktree_id,
        worktree_root,
        scope: need(scope, "scope")?,
        start: RowSide::from_columns(&need(start_state, "start")?, start_value)?,
        end: RowSide::from_columns(&need(end_state, "end")?, end_value)?,
        state,
        finalized_seq: seq.map(to_u64).transpose()?,
    })
}

pub(super) fn select(
    conn: &Connection,
    filter: &str,
    args: &[&dyn rusqlite::ToSql],
) -> Result<Vec<TurnCheckpointRow>> {
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {COLUMNS} FROM turn_checkpoints WHERE {filter} ORDER BY created_at, app_turn"
    ))?;
    let rows = stmt
        .query_map(args, |r| Ok((row(r)?, r.get::<_, Option<i64>>(13)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    rows.into_iter().map(|(c, s)| decode(c, s)).collect()
}

/// One row.
pub(super) fn get(conn: &Connection, app_turn: &AppTurnId) -> Result<Option<TurnCheckpointRow>> {
    Ok(select(conn, "app_turn = ?1", &[&app_turn.as_str()])?.pop())
}

/// The rows of a session, oldest first.
pub(super) fn of_session(conn: &Connection, session: &SessionId) -> Result<Vec<TurnCheckpointRow>> {
    select(conn, "session_id = ?1", &[&session.as_str()])
}

/// Every row that is neither finalized nor abandoned (start recovery).
pub(super) fn unsettled(conn: &Connection) -> Result<Vec<TurnCheckpointRow>> {
    select(conn, "state = 'open'", &[])
}

/// Stores a new row. Idempotent per input: a second plan for the same
/// session input returns the stored app turn and changes nothing.
pub(super) fn plan(conn: &Connection, new: &NewTurnCheckpoint, now: i64) -> Result<AppTurnId> {
    if let Some(input) = &new.input_id {
        let found: Option<String> = conn
            .query_row(
                "SELECT app_turn FROM turn_checkpoints WHERE session_id = ?1 AND input_id = ?2",
                params![new.session_id.as_str(), input.as_str()],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(found) = found {
            return Ok(found.into());
        }
    }
    if matches!(new.start, RowSide::Open) {
        return Err(StoreError::Corrupt("a start side is never open".to_owned()));
    }
    let (start_state, start_value) = new.start.to_columns();
    conn.execute(
        "INSERT INTO turn_checkpoints (app_turn, session_id, process, input_id, turn_id, \
         worktree_id, worktree_root, scope, start_state, start_value, end_state, end_value, \
         state, finalized_seq, created_at, updated_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 'open', NULL, 'open', NULL, ?11, ?11)",
        params![
            new.app_turn.as_str(),
            new.session_id.as_str(),
            new.process.as_ref().map(ProcessIncarnation::as_str),
            new.input_id.as_ref().map(InputId::as_str),
            new.turn_id.as_ref().map(TurnId::as_str),
            new.worktree_id,
            new.worktree_root,
            new.scope,
            start_state,
            start_value,
            now,
        ],
    )?;
    Ok(new.app_turn.clone())
}

/// Records one side. Returns `false` (and changes nothing) when the side is
/// decided already or the row is settled: a decided side never changes.
pub(super) fn record(
    conn: &Connection,
    app_turn: &AppTurnId,
    which: Which,
    side: &RowSide,
    now: i64,
) -> Result<bool> {
    let Some(found) = get(conn, app_turn)? else {
        return Ok(false);
    };
    let current = match which {
        Which::Start => &found.start,
        Which::End => &found.end,
    };
    if found.state != RowState::Open || current.decided() || matches!(side, RowSide::Open) {
        return Ok(false);
    }
    if which == Which::Start && matches!(side, RowSide::Capturing) {
        return Ok(false);
    }
    let (state_col, value_col) = which.columns();
    let (state, value) = side.to_columns();
    conn.execute(
        &format!(
            "UPDATE turn_checkpoints SET {state_col} = ?2, {value_col} = ?3, updated_at = ?4 \
             WHERE app_turn = ?1"
        ),
        params![app_turn.as_str(), state, value, now],
    )?;
    Ok(true)
}

/// Withdraws an `available {commit}` side whose ref no longer proves it
/// (gone, moved, or unreadable) and makes it `unavailable {reason}`. Only an
/// open row and only the expected commit: a settled row or another value
/// changes nothing and returns `false`.
pub(super) fn withdraw(
    conn: &Connection,
    app_turn: &AppTurnId,
    which: Which,
    commit: &str,
    reason: &str,
    now: i64,
) -> Result<bool> {
    let (state_col, value_col) = which.columns();
    let changed = conn.execute(
        &format!(
            "UPDATE turn_checkpoints SET {state_col} = 'unavailable', {value_col} = ?3, \
             updated_at = ?4 WHERE app_turn = ?1 AND state = 'open' \
             AND {state_col} = 'available' AND {value_col} = ?2"
        ),
        params![app_turn.as_str(), commit, reason, now],
    )?;
    Ok(changed == 1)
}

/// Binds the provider turn that the app turn became. Returns `false` when it
/// is bound already (to any turn) or settled.
pub(super) fn bind_turn(
    conn: &Connection,
    app_turn: &AppTurnId,
    turn_id: &TurnId,
    now: i64,
) -> Result<bool> {
    let changed = conn.execute(
        "UPDATE turn_checkpoints SET turn_id = ?2, updated_at = ?3 \
         WHERE app_turn = ?1 AND turn_id IS NULL AND state = 'open'",
        params![app_turn.as_str(), turn_id.as_str(), now],
    )?;
    Ok(changed == 1)
}

/// Settles a row whose input never started a provider turn. No event.
pub(super) fn abandon(
    conn: &Connection,
    app_turn: &AppTurnId,
    reason: &str,
    now: i64,
) -> Result<bool> {
    let changed = conn.execute(
        "UPDATE turn_checkpoints SET state = 'abandoned', \
         end_state = CASE WHEN end_state IN ('open', 'capturing') THEN 'unavailable' ELSE end_state END, \
         end_value = CASE WHEN end_state IN ('open', 'capturing') THEN ?2 ELSE end_value END, \
         updated_at = ?3 WHERE app_turn = ?1 AND state = 'open' AND turn_id IS NULL",
        params![app_turn.as_str(), reason, now],
    )?;
    Ok(changed == 1)
}

/// Commits `turn.finalized` for a row whose two sides are decided and whose
/// provider turn is bound. A repeat returns the stored `station_seq`.
///
/// # Errors
/// [`StoreError::Corrupt`] for an unknown or abandoned row, an undecided
/// side or an unbound turn: the caller records the sides first.
pub(super) fn finalize(
    conn: &Connection,
    app_turn: &AppTurnId,
    diff_summary: Option<DiffSummary>,
    now: i64,
    out: &mut Vec<EventEnvelope>,
) -> Result<Finalized> {
    let found = get(conn, app_turn)?
        .ok_or_else(|| StoreError::Corrupt(format!("unknown app turn {app_turn}")))?;
    match found.state {
        RowState::Finalized => {
            let station_seq = found.finalized_seq.ok_or_else(|| {
                StoreError::Corrupt(format!("finalized app turn {app_turn} without event"))
            })?;
            return Ok(Finalized {
                station_seq,
                appended: false,
            });
        }
        RowState::Abandoned => {
            return Err(StoreError::Corrupt(format!(
                "app turn {app_turn} was abandoned"
            )));
        }
        RowState::Open => {}
    }
    let (Some(start), Some(end), Some(turn_id)) =
        (found.start.wire(), found.end.wire(), found.turn_id.clone())
    else {
        return Err(StoreError::Corrupt(format!(
            "app turn {app_turn} is not ready for turn.finalized"
        )));
    };
    let both = matches!(
        (&start, &end),
        (
            CheckpointSide::Available { .. },
            CheckpointSide::Available { .. }
        )
    );
    let event = Event::TurnFinalized {
        turn_id,
        checkpoint: Some(TurnCheckpoint {
            worktree_id: found.worktree_id.clone(),
            scope: found.scope.clone(),
            start,
            end,
        }),
        diff_summary: diff_summary.filter(|_| both),
    };
    let envelope = log::append_one(conn, &found.session_id, event.into(), now)?;
    let seq = super::to_i64(envelope.station_seq)?;
    conn.execute(
        "UPDATE turn_checkpoints SET state = 'finalized', finalized_seq = ?2, updated_at = ?3 \
         WHERE app_turn = ?1",
        params![app_turn.as_str(), seq, now],
    )?;
    let station_seq = envelope.station_seq;
    out.push(envelope);
    Ok(Finalized {
        station_seq,
        appended: true,
    })
}

impl super::Store {
    /// Stores a new turn checkpoint row ([`plan`]); idempotent per input.
    ///
    /// # Errors
    /// A database error, or a start side `open`.
    pub async fn plan_turn_checkpoint(&self, new: NewTurnCheckpoint) -> Result<AppTurnId> {
        self.write(move |conn, now, _| plan(conn, &new, now))
            .await
            .map(|(id, _)| id)
    }

    /// Records one side ([`record`]); `false` when it was decided already.
    ///
    /// # Errors
    /// A database error.
    pub async fn record_checkpoint_side(
        &self,
        app_turn: AppTurnId,
        which: Which,
        side: RowSide,
    ) -> Result<bool> {
        self.write(move |conn, now, _| record(conn, &app_turn, which, &side, now))
            .await
            .map(|(done, _)| done)
    }

    /// Withdraws a stale `available` side ([`withdraw`]).
    ///
    /// # Errors
    /// A database error.
    pub async fn withdraw_checkpoint_side(
        &self,
        app_turn: AppTurnId,
        which: Which,
        commit: String,
        reason: String,
    ) -> Result<bool> {
        self.write(move |conn, now, _| withdraw(conn, &app_turn, which, &commit, &reason, now))
            .await
            .map(|(done, _)| done)
    }

    /// Binds the provider turn of an app turn ([`bind_turn`]).
    ///
    /// # Errors
    /// A database error.
    pub async fn bind_checkpoint_turn(&self, app_turn: AppTurnId, turn_id: TurnId) -> Result<bool> {
        self.write(move |conn, now, _| bind_turn(conn, &app_turn, &turn_id, now))
            .await
            .map(|(done, _)| done)
    }

    /// Settles a row without a provider turn ([`abandon`]).
    ///
    /// # Errors
    /// A database error.
    pub async fn abandon_turn_checkpoint(
        &self,
        app_turn: AppTurnId,
        reason: String,
    ) -> Result<bool> {
        self.write(move |conn, now, _| abandon(conn, &app_turn, &reason, now))
            .await
            .map(|(done, _)| done)
    }

    /// Commits `turn.finalized` once ([`finalize`]).
    ///
    /// # Errors
    /// A database error, or a row that is not ready.
    pub async fn finalize_turn(
        &self,
        app_turn: AppTurnId,
        diff_summary: Option<DiffSummary>,
    ) -> Result<Finalized> {
        self.write(move |conn, now, out| finalize(conn, &app_turn, diff_summary, now, out))
            .await
            .map(|(done, _)| done)
    }

    /// One row.
    ///
    /// # Errors
    /// A database error.
    pub async fn turn_checkpoint(&self, app_turn: AppTurnId) -> Result<Option<TurnCheckpointRow>> {
        super::call(&self.inner.db, move |conn| get(conn, &app_turn)).await
    }

    /// The rows of a session, oldest first.
    ///
    /// # Errors
    /// A database error.
    pub async fn turn_checkpoints(&self, session: SessionId) -> Result<Vec<TurnCheckpointRow>> {
        super::call(&self.inner.db, move |conn| of_session(conn, &session)).await
    }

    /// Every unsettled row (start recovery).
    ///
    /// # Errors
    /// A database error.
    pub async fn unsettled_turn_checkpoints(&self) -> Result<Vec<TurnCheckpointRow>> {
        super::call(&self.inner.db, move |conn| unsettled(conn)).await
    }
}
