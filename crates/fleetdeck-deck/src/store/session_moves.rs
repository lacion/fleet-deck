//! The session moves of the deck (design §12.3; FG-9, T4.4b). One row of
//! `session_moves` for each move of a session to another station. The driver
//! (`fleet_git_move.rs`) reads the row by its state and sends the next step.
//!
//! - [`MoveRow`], [`MoveState`] and the stored names.
//! - [`DeckStore::move_request`] and [`request_in_tx`]: the owner route and the
//!   coordinator op start a move (the checks are in
//!   [`super::session_moves_ready`]).
//! - [`record_move`]: the `session.move_in` event hook of the read model.
//! - The authority lookup of `coordinator.authority_check` for the `MoveIn`
//!   command of a coordinator move ([`authority_local`]).
//!
//! The transitions and the reads of the driver are in
//! [`super::session_moves_steps`].

use fleetdeck_proto::fleet_git::{MoveInInfo, MoveInState, MovePublication};
use fleetdeck_proto::mutation::{AuthorityCheck, CommandAuthority};
use fleetdeck_proto::{EventEnvelope, SessionId, StationId};
use rusqlite::{Connection, OptionalExtension, Row, Transaction, params};
use serde::Serialize;

use super::coordinator::AuthorityLocal;
pub use super::session_moves_ready::{MoveEnv, MoveRefusal, MoveTarget};
use super::session_moves_ready::{Ready, check};
use super::{DeckError, DeckStore, Result, call, new_id, to_u64};

/// The life of a coordinator's move request (ADR-008 fleet intent TTL).
pub use super::land_queue::AUTHORITY_TTL_MS;

/// The columns of a move row, in the order of [`read`].
pub(super) const COLS: &str = "move_id, request_command_id, src_station, src_session, dst_station, dst_session, kind, state, out_command_id, in_command_id, finish_command_id, undo_command_id, undo_request_command_id, publication, step_attempts, next_step_at, code, message, bytes, requested_by, authority, authority_expires_at, created_at, committed_at, ended_at";

/// The state of a move on the deck (design §12.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MoveState {
    Holding,
    Exporting,
    Importing,
    Starting,
    Committed,
    Finishing,
    Releasing,
    Done,
    Aborting,
    Failed,
    ResumeFailed,
    Undoing,
    Undone,
}

impl MoveState {
    /// The stored name.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Holding => "holding",
            Self::Exporting => "exporting",
            Self::Importing => "importing",
            Self::Starting => "starting",
            Self::Committed => "committed",
            Self::Finishing => "finishing",
            Self::Releasing => "releasing",
            Self::Done => "done",
            Self::Aborting => "aborting",
            Self::Failed => "failed",
            Self::ResumeFailed => "resume_failed",
            Self::Undoing => "undoing",
            Self::Undone => "undone",
        }
    }

    fn parse(name: &str) -> Result<Self> {
        Ok(match name {
            "holding" => Self::Holding,
            "exporting" => Self::Exporting,
            "importing" => Self::Importing,
            "starting" => Self::Starting,
            "committed" => Self::Committed,
            "finishing" => Self::Finishing,
            "releasing" => Self::Releasing,
            "done" => Self::Done,
            "aborting" => Self::Aborting,
            "failed" => Self::Failed,
            "resume_failed" => Self::ResumeFailed,
            "undoing" => Self::Undoing,
            "undone" => Self::Undone,
            other => return Err(DeckError::Corrupt(format!("move state `{other}`"))),
        })
    }

    /// True for a state that frees the `session_moves_open` index.
    #[must_use]
    pub fn is_closed(self) -> bool {
        matches!(self, Self::Done | Self::Failed | Self::Undone)
    }
}

/// One row of `session_moves`.
#[derive(Debug, Clone, PartialEq)]
pub struct MoveRow {
    pub move_id: String,
    pub request_command_id: String,
    pub src_station: String,
    pub src_session: String,
    pub dst_station: String,
    pub dst_session: Option<String>,
    /// `native` or `branch_only`.
    pub kind: String,
    pub state: MoveState,
    pub out_command_id: String,
    pub in_command_id: String,
    pub finish_command_id: String,
    pub undo_command_id: Option<String>,
    pub undo_request_command_id: Option<String>,
    pub publication: Option<MovePublication>,
    pub step_attempts: u64,
    pub next_step_at: Option<i64>,
    pub code: Option<String>,
    pub message: Option<String>,
    pub bytes: Option<u64>,
    pub requested_by: String,
    pub authority: Option<CommandAuthority>,
    pub authority_expires_at: Option<i64>,
    pub created_at: i64,
    pub committed_at: Option<i64>,
    pub ended_at: Option<i64>,
}

fn json_of<T: serde::de::DeserializeOwned>(text: Option<String>) -> Result<Option<T>> {
    Ok(text.map(|t| serde_json::from_str(&t)).transpose()?)
}

pub(super) fn read(r: &Row<'_>) -> Result<MoveRow> {
    Ok(MoveRow {
        move_id: r.get(0)?,
        request_command_id: r.get(1)?,
        src_station: r.get(2)?,
        src_session: r.get(3)?,
        dst_station: r.get(4)?,
        dst_session: r.get(5)?,
        kind: r.get(6)?,
        state: MoveState::parse(&r.get::<_, String>(7)?)?,
        out_command_id: r.get(8)?,
        in_command_id: r.get(9)?,
        finish_command_id: r.get(10)?,
        undo_command_id: r.get(11)?,
        undo_request_command_id: r.get(12)?,
        publication: json_of(r.get(13)?)?,
        step_attempts: to_u64(r.get(14)?)?,
        next_step_at: r.get(15)?,
        code: r.get(16)?,
        message: r.get(17)?,
        bytes: r.get::<_, Option<i64>>(18)?.map(to_u64).transpose()?,
        requested_by: r.get(19)?,
        authority: json_of(r.get(20)?)?,
        authority_expires_at: r.get(21)?,
        created_at: r.get(22)?,
        committed_at: r.get(23)?,
        ended_at: r.get(24)?,
    })
}

pub(super) fn find(conn: &Connection, move_id: &str) -> Result<Option<MoveRow>> {
    conn.query_row(
        &format!("SELECT {COLS} FROM session_moves WHERE move_id = ?1"),
        [move_id],
        |r| Ok(read(r)),
    )
    .optional()?
    .transpose()
}

/// The row of a move for the board.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MoveView {
    pub move_id: String,
    pub state: MoveState,
    pub kind: String,
    pub src_station: String,
    pub src_session: String,
    pub dst_station: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dst_session: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    pub created_at: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub committed_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<i64>,
}

impl From<MoveRow> for MoveView {
    fn from(r: MoveRow) -> Self {
        Self {
            move_id: r.move_id,
            state: r.state,
            kind: r.kind,
            src_station: r.src_station,
            src_session: r.src_session,
            dst_station: r.dst_station,
            dst_session: r.dst_session,
            bytes: r.bytes,
            code: r.code,
            message: r.message,
            created_at: r.created_at,
            committed_at: r.committed_at,
            ended_at: r.ended_at,
        }
    }
}

/// What a request asks (T4.4b Q5, Q16).
#[derive(Debug, Clone)]
pub struct MoveRequest {
    /// `owner`, or `coordinator:<grant>`.
    pub requested_by: String,
    pub request_command_id: String,
    pub src_station: String,
    pub src_session: String,
    pub dst_station: String,
    /// The authority of a coordinator move (design §12.3, B3).
    pub authority: Option<CommandAuthority>,
}

/// The answer to a request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MoveRequested {
    Started { move_id: String },
    Refused(MoveRefusal),
}

/// The three command ids of a new move, from one unique part.
#[must_use]
pub fn command_ids(part: &str) -> [String; 3] {
    [
        format!("moveout-{part}"),
        format!("movein-{part}"),
        format!("movefinish-{part}"),
    ]
}

/// The body of [`DeckStore::move_request`] in a transaction of the caller (a
/// coordinator move commits with its receipt). A refusal writes nothing.
pub(super) fn request_in_tx(
    tx: &Connection,
    env: &MoveEnv,
    (move_id, part): (&str, &str),
    req: &MoveRequest,
    now: i64,
) -> Result<MoveRequested> {
    if let Some(row) = by_request(tx, &req.requested_by, &req.request_command_id)? {
        if row.src_station != req.src_station
            || row.src_session != req.src_session
            || row.dst_station != req.dst_station
        {
            return Ok(MoveRequested::Refused(MoveRefusal::new(
                "command_id_reused",
                "This command id was used for another request.",
            )));
        }
        return Ok(MoveRequested::Started {
            move_id: row.move_id,
        });
    }
    let ready: Ready = match check(
        tx,
        env,
        (&req.src_station, &req.src_session),
        &req.dst_station,
    )? {
        Ok(ready) => ready,
        Err(refusal) => return Ok(MoveRequested::Refused(refusal)),
    };
    let [out, inn, finish] = command_ids(part);
    tx.execute(
        "INSERT INTO session_moves (move_id, request_command_id, src_station, src_session,
             dst_station, kind, state, out_command_id, in_command_id, finish_command_id,
             requested_by, authority, authority_expires_at, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'holding', ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
        params![
            move_id,
            req.request_command_id,
            req.src_station,
            req.src_session,
            req.dst_station,
            ready.kind,
            out,
            inn,
            finish,
            req.requested_by,
            req.authority
                .as_ref()
                .map(serde_json::to_string)
                .transpose()?,
            req.authority.as_ref().map(|_| now + AUTHORITY_TTL_MS),
            now,
        ],
    )?;
    Ok(MoveRequested::Started {
        move_id: move_id.to_owned(),
    })
}

fn by_request(tx: &Connection, by: &str, command: &str) -> Result<Option<MoveRow>> {
    tx.query_row(
        &format!(
            "SELECT {COLS} FROM session_moves WHERE requested_by = ?1 AND request_command_id = ?2"
        ),
        params![by, command],
        |r| Ok(read(r)),
    )
    .optional()?
    .transpose()
}

impl DeckStore {
    /// Starts a move of the owner (design §12.2, §12.3 step 1): the checks of
    /// §12.2 and the new row in `holding`. The driver sends the first step.
    ///
    /// # Errors
    /// A database or stored-JSON error. A refusal is the `Ok` value.
    pub async fn move_request(&self, req: MoveRequest, env: MoveEnv) -> Result<MoveRequested> {
        let now = self.now_ms();
        let move_id = format!("move-{}", new_id::<String>(self.inner.ids.as_ref()));
        let part = new_id::<String>(self.inner.ids.as_ref());
        let out = call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            let out = request_in_tx(&tx, &env, (&move_id, &part), &req, now)?;
            tx.commit()?;
            Ok(out)
        })
        .await?;
        if matches!(out, MoveRequested::Started { .. }) {
            self.board_changed();
        }
        Ok(out)
    }
}

/// The `session.move_in` event hook of the read model (FG-9, T4.4b Q9): the
/// event of a move that the deck drives, from the target station.
///
/// - `starting` and `resumed`, `released` or `confirmed`: the commit point
///   passed, the row is `committed`.
/// - `starting` and `abandoned`, `discarded` or `launch_refused`: a failure
///   before the commit point, the row is `aborting`.
/// - `committed` to `done` and `resume_failed`: the row is `resume_failed`
///   (a Claude identity can fail after the release).
pub(super) fn record_move(
    tx: &Transaction<'_>,
    station: &StationId,
    e: &EventEnvelope,
    now: i64,
) -> Result<()> {
    let fleetdeck_proto::event::EventBody::Known(fleetdeck_proto::event::Event::SessionMoveIn(
        info,
    )) = &e.event
    else {
        return Ok(());
    };
    let Some(row) = find(tx, &info.move_id)? else {
        return Ok(());
    };
    if row.dst_station != station.as_str() {
        tracing::warn!(move_id = %info.move_id, "session.move_in from another station");
        return Ok(());
    }
    if row.dst_session.is_none() {
        tx.execute(
            "UPDATE session_moves SET dst_session = ?2 WHERE move_id = ?1 AND dst_session IS NULL",
            params![row.move_id, e.session_id.as_str()],
        )?;
    }
    apply_info(tx, &row, info, now)
}

fn apply_info(tx: &Connection, row: &MoveRow, info: &MoveInInfo, now: i64) -> Result<()> {
    use MoveInState as S;
    use MoveState as M;
    let note = (
        info.code
            .clone()
            .unwrap_or_else(|| "move_resume_failed".to_owned()),
        info.text.clone().unwrap_or_default(),
    );
    match (row.state, info.state) {
        (M::Starting, S::Resumed | S::Released | S::Confirmed) => {
            super::session_moves_steps::set_in_tx(
                tx,
                &row.move_id,
                &[M::Starting],
                M::Committed,
                &super::session_moves_steps::MovePatch::default(),
                now,
            )?;
        }
        (M::Starting, S::Abandoned | S::Discarded | S::LaunchRefused) => {
            super::session_moves_steps::set_in_tx(
                tx,
                &row.move_id,
                &[M::Starting],
                M::Aborting,
                &super::session_moves_steps::MovePatch::note(&note.0, &note.1),
                now,
            )?;
        }
        (M::Committed | M::Finishing | M::Releasing | M::Done, S::ResumeFailed) => {
            super::session_moves_steps::set_in_tx(
                tx,
                &row.move_id,
                &[M::Committed, M::Finishing, M::Releasing, M::Done],
                M::ResumeFailed,
                &super::session_moves_steps::MovePatch::note(&note.0, &note.1),
                now,
            )?;
        }
        _ => {}
    }
    Ok(())
}

/// The deck-local part of `coordinator.authority_check` for a land or a move
/// (design §11.2, §12.3, B3): a land command is looked up first.
pub(super) fn authority_local(
    tx: &Connection,
    station: &StationId,
    check: &AuthorityCheck,
    grant: &super::grants::Grant,
    now: i64,
) -> Result<std::result::Result<AuthorityLocal, &'static str>> {
    match super::land_outbox::authority_local(tx, station, check, grant, now)? {
        Err("unknown_command") => move_authority_local(tx, station, check, grant, now),
        other => Ok(other),
    }
}

/// The part of [`authority_local`] for a move: the station asks about the
/// `MoveIn` command of a `starting` coordinator move. The command must carry
/// exactly this authority, and the session must be the new session (or the
/// row does not know it yet). The target rule of ADR-008 §4.2 gives
/// `target_excluded` when the source or the target station left the grant.
fn move_authority_local(
    tx: &Connection,
    station: &StationId,
    check: &AuthorityCheck,
    grant: &super::grants::Grant,
    now: i64,
) -> Result<std::result::Result<AuthorityLocal, &'static str>> {
    let found: Option<MoveRow> = tx
        .query_row(
            &format!(
                "SELECT {COLS} FROM session_moves
                 WHERE in_command_id = ?1 AND dst_station = ?2 AND state = 'starting'"
            ),
            params![check.command_id.as_str(), station.as_str()],
            |r| Ok(read(r)),
        )
        .optional()?
        .transpose()?;
    let asked = CommandAuthority {
        grant_id: check.grant_id.clone(),
        caller_station: check.caller_station.clone(),
        caller_session: check.caller_session.clone(),
        generation: check.generation,
    };
    let Some(row) = found.filter(|r| {
        r.authority.as_ref() == Some(&asked)
            && r.dst_session
                .as_deref()
                .is_none_or(|s| s == check.target_session.as_str())
    }) else {
        return Ok(Err("unknown_command"));
    };
    let src = StationId::from(row.src_station.clone());
    let session = SessionId::from(row.src_session.clone());
    let excluded = !grant.stations.contains(station)
        || super::coordinator::strict_refusal(tx, grant, (&src, &session), (false, now))?.is_some();
    Ok(Ok(AuthorityLocal {
        target_excluded: excluded,
    }))
}

#[cfg(test)]
#[path = "session_moves_tests.rs"]
mod tests;
