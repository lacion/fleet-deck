//! Durable revert rows (PLAN §6.6, L13; DL-087).
//!
//! - **The row is the fence.** A partial unique index admits one active
//!   revert per session. The command decision inserts the row, the
//!   `revert.updated {admitted}` event and the revert effect in one
//!   transaction; input decisions read the projection and refuse new input
//!   while a revert is active.
//! - **Every external step is durable.** The executor commits
//!   `rollback_sent` before it asks the provider to roll back its
//!   conversation, `rolled_back` after the provider confirmed, and
//!   `restoring` before it writes a file. Recovery reads these steps; it
//!   never repeats a rollback that may have happened.
//! - **Terminal steps.** `succeeded` (the conversation and the workspace
//!   both match the target), `failed` (nothing changed) and
//!   `reconciliation_required` (uncertain or partial; the session refuses
//!   input until an owner resolves it), and `reconciled` (the owner
//!   accepted the state, DL-097). An owner can also send a
//!   `reconciliation_required` revert whose rollback is confirmed back to
//!   `rolled_back` for one more file restore.

use fleetdeck_proto::event::RevertState;
use fleetdeck_proto::{Event, EventEnvelope, SessionId, TurnId};
use rusqlite::{Connection, OptionalExtension, params};

use super::effects::ProcessIncarnation;
use super::turn_checkpoints::{self, AppTurnId, TurnCheckpointRow};
use super::{Decision, Result, StoreError, log};

/// The steps of a revert row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RevertStep {
    Admitted,
    /// The rollback request may be on the wire.
    RollbackSent,
    /// The provider confirmed the conversation rollback.
    RolledBack,
    /// The file restore may have started.
    Restoring,
    Succeeded,
    Failed,
    ReconciliationRequired,
    /// The owner resolved a `reconciliation_required` revert (DL-097).
    Reconciled,
}

impl RevertStep {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Admitted => "admitted",
            Self::RollbackSent => "rollback_sent",
            Self::RolledBack => "rolled_back",
            Self::Restoring => "restoring",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::ReconciliationRequired => "reconciliation_required",
            Self::Reconciled => "reconciled",
        }
    }

    fn parse(s: &str) -> Result<Self> {
        Ok(match s {
            "admitted" => Self::Admitted,
            "rollback_sent" => Self::RollbackSent,
            "rolled_back" => Self::RolledBack,
            "restoring" => Self::Restoring,
            "succeeded" => Self::Succeeded,
            "failed" => Self::Failed,
            "reconciliation_required" => Self::ReconciliationRequired,
            "reconciled" => Self::Reconciled,
            other => return Err(StoreError::Corrupt(format!("revert step {other}"))),
        })
    }

    /// True for a step that the executor or recovery still moves on.
    #[must_use]
    pub fn is_running(self) -> bool {
        matches!(
            self,
            Self::Admitted | Self::RollbackSent | Self::RolledBack | Self::Restoring
        )
    }

    /// The wire state that this step reports; `None` for the internal
    /// write-ahead steps.
    fn wire(self) -> Option<RevertState> {
        match self {
            Self::Admitted => Some(RevertState::Admitted),
            Self::RolledBack => Some(RevertState::ConversationRolledBack),
            Self::Succeeded => Some(RevertState::Succeeded),
            Self::Failed => Some(RevertState::Failed),
            Self::ReconciliationRequired => Some(RevertState::ReconciliationRequired),
            Self::Reconciled => Some(RevertState::Reconciled),
            Self::RollbackSent | Self::Restoring => None,
        }
    }
}

/// A new revert, from the command decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewRevert {
    pub revert_id: String,
    pub session_id: SessionId,
    pub process: ProcessIncarnation,
    pub turn_id: TurnId,
    /// The first reverted app turn (its start checkpoint is the target).
    pub app_turn: AppTurnId,
    /// The last finalized app turn (its completion checkpoint is the
    /// expected workspace before the restore).
    pub current_app_turn: AppTurnId,
    /// The conversation turns that the provider drops.
    pub turns: u32,
    pub target_commit: String,
    pub current_commit: String,
    pub worktree_root: String,
}

/// One stored revert.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RevertRow {
    pub revert: NewRevert,
    pub step: RevertStep,
    pub reason: Option<String>,
    /// True once the provider confirmed the conversation rollback.
    pub rolled_back: bool,
}

const COLUMNS: &str = "revert_id, session_id, process, turn_id, app_turn, current_app_turn, \
     turns, target_commit, current_commit, worktree_root, step, reason, rolled_back_at";

type Decoded = (Vec<String>, i64, Option<String>, Option<i64>);

fn decode(r: &rusqlite::Row<'_>) -> rusqlite::Result<Decoded> {
    let mut text = Vec::with_capacity(10);
    for i in 0..6 {
        text.push(r.get(i)?);
    }
    let turns: i64 = r.get(6)?;
    for i in 7..11 {
        text.push(r.get(i)?);
    }
    Ok((text, turns, r.get(11)?, r.get(12)?))
}

fn build((text, turns, reason, rolled_back_at): Decoded) -> Result<RevertRow> {
    let [
        id,
        session,
        process,
        turn,
        app,
        current,
        target,
        now,
        root,
        step,
    ]: [String; 10] = text
        .try_into()
        .map_err(|_| StoreError::Corrupt("revert row shape".to_owned()))?;
    Ok(RevertRow {
        revert: NewRevert {
            revert_id: id,
            session_id: session.into(),
            process: ProcessIncarnation::from(process),
            turn_id: turn.into(),
            app_turn: app.into(),
            current_app_turn: current.into(),
            turns: u32::try_from(turns)
                .map_err(|_| StoreError::Corrupt(format!("revert turns {turns}")))?,
            target_commit: target,
            current_commit: now,
            worktree_root: root,
        },
        step: RevertStep::parse(&step)?,
        reason,
        rolled_back: rolled_back_at.is_some(),
    })
}

pub(super) fn select(
    conn: &Connection,
    filter: &str,
    args: &[&dyn rusqlite::ToSql],
) -> Result<Vec<RevertRow>> {
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {COLUMNS} FROM reverts WHERE {filter} ORDER BY created_at, revert_id"
    ))?;
    let rows = stmt
        .query_map(args, decode)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    rows.into_iter().map(build).collect()
}

fn get(conn: &Connection, revert_id: &str) -> Result<Option<RevertRow>> {
    Ok(select(conn, "revert_id = ?1", &[&revert_id])?.pop())
}

fn event(row: &RevertRow, state: RevertState) -> Event {
    Event::RevertUpdated {
        revert_id: row.revert.revert_id.clone(),
        turn_id: row.revert.turn_id.clone(),
        turns: row.revert.turns,
        state,
        reason: row.reason.clone(),
    }
}

/// Moves a revert from one of `from` to `to` and appends the step's event.
/// Returns `false` (and changes nothing) when the row is in another step.
fn advance(
    conn: &Connection,
    revert_id: &str,
    from: &[RevertStep],
    to: RevertStep,
    reason: Option<&str>,
    now: i64,
    out: &mut Vec<EventEnvelope>,
) -> Result<bool> {
    let Some(found) = get(conn, revert_id)? else {
        return Err(StoreError::Corrupt(format!("unknown revert {revert_id}")));
    };
    if !from.contains(&found.step) {
        return Ok(false);
    }
    let rolled_back = found.rolled_back || to == RevertStep::RolledBack;
    conn.execute(
        "UPDATE reverts SET step = ?2, reason = ?3, updated_at = ?4, \
         rolled_back_at = COALESCE(rolled_back_at, ?5) WHERE revert_id = ?1",
        params![
            revert_id,
            to.as_str(),
            reason,
            now,
            (to == RevertStep::RolledBack).then_some(now)
        ],
    )?;
    let rows = revertable(conn, &found.revert.session_id)?;
    let removed = match to {
        // The reverted turns leave the revertable history: the target turn
        // and every later turn of the session.
        RevertStep::Succeeded => rows
            .iter()
            .position(|r| r.app_turn == found.revert.app_turn)
            .map_or(&rows[..0], |at| &rows[at..]),
        // After an owner reconciliation the station no longer knows how
        // many turns the conversation holds, so no earlier turn stays
        // revertable (DL-097).
        RevertStep::Reconciled => &rows[..],
        _ => &rows[..0],
    };
    for row in removed {
        conn.execute(
            "UPDATE turn_checkpoints SET reverted_by = ?2, updated_at = ?3 WHERE app_turn = ?1",
            params![row.app_turn.as_str(), revert_id, now],
        )?;
    }
    if let Some(state) = to.wire() {
        let row = RevertRow {
            step: to,
            reason: reason.map(str::to_owned),
            rolled_back,
            ..found
        };
        let ev = event(&row, state);
        out.push(log::append_one(
            conn,
            &row.revert.session_id,
            ev.into(),
            now,
        )?);
    }
    Ok(true)
}

/// The number of a session's effects that can still write input to the
/// provider: queued effects, and claimed input or revert effects.
pub(super) fn pending_inputs(conn: &Connection, session: &SessionId) -> Result<u64> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM effects WHERE session_id = ?1 AND (state = 'queued' \
         OR (state = 'claimed' AND kind IN ('start', 'prompt', 'steer', 'follow_up', \
         'revert')))",
        [session.as_str()],
        |r| r.get(0),
    )?;
    Ok(u64::try_from(n).unwrap_or(0))
}

/// The turn checkpoint rows of a session that no revert removed.
fn revertable(conn: &Connection, session: &SessionId) -> Result<Vec<TurnCheckpointRow>> {
    turn_checkpoints::select(
        conn,
        "session_id = ?1 AND reverted_by IS NULL",
        &[&session.as_str()],
    )
}

impl Decision<'_> {
    /// The session's turn checkpoint rows that no revert removed, oldest
    /// first.
    ///
    /// # Errors
    /// A database error.
    pub fn revertable_turns(&self, session: &SessionId) -> Result<Vec<TurnCheckpointRow>> {
        revertable(self.connection(), session)
    }

    /// The number of the session's effects that can still write input to
    /// the provider: queued effects, and claimed input or revert effects.
    ///
    /// # Errors
    /// A database error.
    pub fn pending_input_effects(&self, session: &SessionId) -> Result<u64> {
        pending_inputs(self.connection(), session)
    }

    /// The session's revert that has not ended (`succeeded`, `failed` or
    /// `reconciled`), if any.
    ///
    /// # Errors
    /// A database error.
    pub fn open_revert(&self, session: &SessionId) -> Result<Option<RevertRow>> {
        Ok(select(
            self.connection(),
            "session_id = ?1 AND step NOT IN ('succeeded', 'failed', 'reconciled')",
            &[&session.as_str()],
        )?
        .pop())
    }

    /// Stores a new revert with its `revert.updated {admitted}` event.
    ///
    /// # Errors
    /// A database error (the unique index refuses a second open revert).
    pub fn admit_revert(&mut self, new: &NewRevert) -> Result<()> {
        self.connection().execute(
            "INSERT INTO reverts (revert_id, session_id, process, turn_id, app_turn, \
             current_app_turn, turns, target_commit, current_commit, worktree_root, step, \
             reason, created_at, updated_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 'admitted', NULL, ?11, ?11)",
            params![
                new.revert_id,
                new.session_id.as_str(),
                new.process.as_str(),
                new.turn_id.as_str(),
                new.app_turn.as_str(),
                new.current_app_turn.as_str(),
                i64::from(new.turns),
                new.target_commit,
                new.current_commit,
                new.worktree_root,
                self.now(),
            ],
        )?;
        let row = RevertRow {
            revert: new.clone(),
            step: RevertStep::Admitted,
            reason: None,
            rolled_back: false,
        };
        self.append(&new.session_id, event(&row, RevertState::Admitted).into())?;
        Ok(())
    }

    /// One revert.
    ///
    /// # Errors
    /// A database error.
    pub fn revert(&self, revert_id: &str) -> Result<Option<RevertRow>> {
        get(self.connection(), revert_id)
    }

    /// Moves a revert from one of `from` to `to` with its event, in the
    /// decision's transaction (the owner's resolution, DL-097); `false`
    /// when the row was in another step.
    ///
    /// # Errors
    /// A database error, or an unknown revert.
    pub fn advance_revert(
        &mut self,
        revert_id: &str,
        from: &[RevertStep],
        to: RevertStep,
        reason: Option<&str>,
    ) -> Result<bool> {
        self.with_log(|conn, now, out| advance(conn, revert_id, from, to, reason, now, out))
    }
}

impl super::Store {
    /// One revert.
    ///
    /// # Errors
    /// A database error.
    pub async fn revert(&self, revert_id: String) -> Result<Option<RevertRow>> {
        super::call(&self.inner.db, move |conn| get(conn, &revert_id)).await
    }

    /// Every revert whose step still moves on (start recovery).
    ///
    /// # Errors
    /// A database error.
    pub async fn running_reverts(&self) -> Result<Vec<RevertRow>> {
        super::call(&self.inner.db, move |conn| {
            select(
                conn,
                "step IN ('admitted', 'rollback_sent', 'rolled_back', 'restoring')",
                &[],
            )
        })
        .await
    }

    /// Moves a revert from one of `from` to `to` in one transaction with
    /// its event; `false` when the row was in another step.
    ///
    /// # Errors
    /// A database error, or an unknown revert.
    pub async fn advance_revert(
        &self,
        revert_id: String,
        from: &'static [RevertStep],
        to: RevertStep,
        reason: Option<String>,
    ) -> Result<bool> {
        self.write(move |conn, now, out| {
            advance(conn, &revert_id, from, to, reason.as_deref(), now, out)
        })
        .await
        .map(|(moved, _)| moved)
    }

    /// The reverts of a session, oldest first.
    ///
    /// # Errors
    /// A database error.
    pub async fn session_reverts(&self, session: SessionId) -> Result<Vec<RevertRow>> {
        super::call(&self.inner.db, move |conn| {
            select(conn, "session_id = ?1", &[&session.as_str()])
        })
        .await
    }

    /// The session's turn checkpoint rows that no revert removed, oldest
    /// first: the same history as the admission (DL-093).
    ///
    /// # Errors
    /// A database error.
    pub async fn revertable_turn_checkpoints(
        &self,
        session: SessionId,
    ) -> Result<Vec<TurnCheckpointRow>> {
        super::call(&self.inner.db, move |conn| revertable(conn, &session)).await
    }

    /// The revert id that removed an app turn, if any.
    ///
    /// # Errors
    /// A database error.
    pub async fn reverted_by(&self, app_turn: AppTurnId) -> Result<Option<String>> {
        super::call(&self.inner.db, move |conn| {
            Ok(conn
                .query_row(
                    "SELECT reverted_by FROM turn_checkpoints WHERE app_turn = ?1",
                    [app_turn.as_str()],
                    |r| r.get::<_, Option<String>>(0),
                )
                .optional()?
                .flatten())
        })
        .await
    }
}
