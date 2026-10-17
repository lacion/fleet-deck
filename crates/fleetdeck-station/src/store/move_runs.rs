//! The move runs of the station store (FG-9, design §12.3, §3.5 V31): the rows
//! of the source (`move_out_runs`), the rows of the target (`move_in_runs`),
//! the durable tombstones (`move_tombstones`) and the publication
//! transaction of a moved-in session (revision 4, NEW-1).
//!
//! Every state change of a `move_in_runs` row is a conditional transition:
//! `UPDATE … WHERE move_id = ? AND state = <expected>`. Zero changed rows means
//! that another party changed the state first, and the caller is refused
//! (design §12.3, "The per-move lock and the tombstone"). The callers (T4.2b
//! to T4.2d) add their own statements in the same transaction.

use fleetdeck_proto::event::ProviderKind;
use fleetdeck_proto::fleet_git::{MoveInInfo, MoveInState, MovePublication};
use fleetdeck_proto::mutation::CommandAuthority;
use fleetdeck_proto::{Event, EventEnvelope, SessionId};
use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};

use super::land_runs::{from_text, text_of};
use super::{NewEvent, Result, Store, StoreError, call, log};

/// The state of a `move_out_runs` row (design §12.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MoveOutState {
    /// The source holds the session; the move runs.
    Holding,
    /// `session.move_finish` made the session `moved`.
    Finished,
    /// `session.move_abort` released the hold.
    Aborted,
    /// `session.move_undo` made the session resumable again (revision 4).
    Undone,
}

impl MoveOutState {
    /// The stored text.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Holding => "holding",
            Self::Finished => "finished",
            Self::Aborted => "aborted",
            Self::Undone => "undone",
        }
    }

    fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "holding" => Self::Holding,
            "finished" => Self::Finished,
            "aborted" => Self::Aborted,
            "undone" => Self::Undone,
            _ => return None,
        })
    }
}

/// One row of `move_out_runs`.
#[derive(Debug, Clone, PartialEq)]
pub struct MoveOutRow {
    pub move_id: String,
    pub command_id: String,
    pub session_id: SessionId,
    pub to_station: String,
    pub state: MoveOutState,
    /// Captured under the hold (design §12.3 step 1).
    pub publication: MovePublication,
    pub authority: Option<CommandAuthority>,
    pub created_at: i64,
    pub updated_at: i64,
}

/// The values of a new source row (design §12.3 step 1).
#[derive(Debug, Clone, PartialEq)]
pub struct NewMoveOut {
    pub move_id: String,
    pub command_id: String,
    pub session_id: SessionId,
    pub to_station: String,
    pub publication: MovePublication,
    pub authority: Option<CommandAuthority>,
}

/// A file that the target placed in the provider home (design §12.3 step 4):
/// the identity by which a discard deletes it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportedFile {
    pub path: String,
    pub dev: u64,
    pub ino: u64,
}

/// The staging folder of an import and its identity (design §12.3 step 3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Staging {
    pub path: String,
    pub dev: u64,
    pub ino: u64,
}

/// One row of `move_in_runs`.
#[derive(Debug, Clone, PartialEq)]
pub struct MoveInRow {
    pub move_id: String,
    pub command_id: String,
    /// The new session.
    pub session_id: SessionId,
    pub provider: ProviderKind,
    /// `None` for a branch-only move.
    pub native_session_id: Option<String>,
    pub state: MoveInState,
    pub staging: Option<Staging>,
    pub imported: Vec<ImportedFile>,
    pub authority: Option<CommandAuthority>,
    pub first_input_at: Option<i64>,
    pub code: Option<String>,
    pub message: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

/// The values of a new target row; the state is `staged` (design §12.3
/// step 4, decision).
#[derive(Debug, Clone, PartialEq)]
pub struct NewMoveIn {
    pub move_id: String,
    pub command_id: String,
    pub session_id: SessionId,
    pub provider: ProviderKind,
    pub native_session_id: Option<String>,
    pub authority: Option<CommandAuthority>,
}

fn json<T: Serialize>(value: &T) -> Result<String> {
    serde_json::to_string(value).map_err(|e| StoreError::Corrupt(format!("move JSON: {e}")))
}

fn json_error(column: usize, e: serde_json::Error) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(column, rusqlite::types::Type::Text, Box::new(e))
}

fn bad_text(column: usize, what: &str) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        column,
        rusqlite::types::Type::Text,
        what.to_owned().into(),
    )
}

fn to_u64(v: i64) -> u64 {
    u64::try_from(v).unwrap_or(0)
}

fn to_i64(v: u64) -> i64 {
    i64::try_from(v).unwrap_or(i64::MAX)
}

// ---------------------------------------------------------------- move_out_runs

const OUT_COLUMNS: &str = "move_id, command_id, session_id, to_station, state, publication, authority, created_at, updated_at";

fn out_of(r: &Row<'_>) -> rusqlite::Result<MoveOutRow> {
    let state: String = r.get(4)?;
    let publication: String = r.get(5)?;
    let authority: Option<String> = r.get(6)?;
    Ok(MoveOutRow {
        move_id: r.get(0)?,
        command_id: r.get(1)?,
        session_id: SessionId::from(r.get::<_, String>(2)?),
        to_station: r.get(3)?,
        state: MoveOutState::parse(&state).ok_or_else(|| bad_text(4, "unknown move state"))?,
        publication: serde_json::from_str(&publication).map_err(|e| json_error(5, e))?,
        authority: authority
            .map(|a| serde_json::from_str(&a).map_err(|e| json_error(6, e)))
            .transpose()?,
        created_at: r.get(7)?,
        updated_at: r.get(8)?,
    })
}

/// Inserts the source row in state `holding`. The caller inserts the
/// `move` session hold in the same transaction.
///
/// # Errors
/// A database error (a constraint error for a move id or command id that is
/// stored already).
pub fn insert_out(conn: &Connection, new: &NewMoveOut, now: i64) -> Result<()> {
    let authority = new.authority.as_ref().map(json).transpose()?;
    conn.execute(
        "INSERT INTO move_out_runs
           (move_id, command_id, session_id, to_station, state, publication, authority,
            created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, 'holding', ?5, ?6, ?7, ?7)",
        params![
            new.move_id,
            new.command_id,
            new.session_id.as_str(),
            new.to_station,
            json(&new.publication)?,
            authority,
            now,
        ],
    )?;
    Ok(())
}

/// The source row of `move_id`.
///
/// # Errors
/// A database error.
pub fn get_out(conn: &Connection, move_id: &str) -> Result<Option<MoveOutRow>> {
    Ok(conn
        .query_row(
            &format!("SELECT {OUT_COLUMNS} FROM move_out_runs WHERE move_id = ?1"),
            params![move_id],
            out_of,
        )
        .optional()?)
}

/// The source row that the command `command_id` made.
///
/// # Errors
/// A database error.
pub fn out_by_command(conn: &Connection, command_id: &str) -> Result<Option<MoveOutRow>> {
    Ok(conn
        .query_row(
            &format!("SELECT {OUT_COLUMNS} FROM move_out_runs WHERE command_id = ?1"),
            params![command_id],
            out_of,
        )
        .optional()?)
}

/// The source row of `session_id` in state `holding`, if any.
///
/// # Errors
/// A database error.
pub fn holding_out(conn: &Connection, session_id: &SessionId) -> Result<Option<MoveOutRow>> {
    Ok(conn
        .query_row(
            &format!(
                "SELECT {OUT_COLUMNS} FROM move_out_runs
                 WHERE session_id = ?1 AND state = 'holding'"
            ),
            params![session_id.as_str()],
            out_of,
        )
        .optional()?)
}

/// The conditional transition of a source row: `false` when the row is not in
/// state `from` (nothing changed).
///
/// # Errors
/// A database error.
pub fn transition_out(
    conn: &Connection,
    move_id: &str,
    from: MoveOutState,
    to: MoveOutState,
    now: i64,
) -> Result<bool> {
    let changed = conn.execute(
        "UPDATE move_out_runs SET state = ?3, updated_at = ?4
         WHERE move_id = ?1 AND state = ?2",
        params![move_id, from.as_str(), to.as_str(), now],
    )?;
    Ok(changed > 0)
}

// ----------------------------------------------------------------- move_in_runs

const IN_COLUMNS: &str = "move_id, command_id, session_id, provider, native_session_id, state,
     staging_path, staging_dev, staging_ino, imported, authority, first_input_at, code, message,
     created_at, updated_at";

fn in_of(r: &Row<'_>) -> rusqlite::Result<MoveInRow> {
    let provider: String = r.get(3)?;
    let state: String = r.get(5)?;
    let path: Option<String> = r.get(6)?;
    let dev: Option<i64> = r.get(7)?;
    let ino: Option<i64> = r.get(8)?;
    let imported: String = r.get(9)?;
    let authority: Option<String> = r.get(10)?;
    Ok(MoveInRow {
        move_id: r.get(0)?,
        command_id: r.get(1)?,
        session_id: SessionId::from(r.get::<_, String>(2)?),
        provider: from_text(&provider, ProviderKind::Unknown),
        native_session_id: r.get(4)?,
        state: from_text(&state, MoveInState::Unknown),
        staging: path.map(|path| Staging {
            path,
            dev: dev.map_or(0, to_u64),
            ino: ino.map_or(0, to_u64),
        }),
        imported: serde_json::from_str(&imported).map_err(|e| json_error(9, e))?,
        authority: authority
            .map(|a| serde_json::from_str(&a).map_err(|e| json_error(10, e)))
            .transpose()?,
        first_input_at: r.get(11)?,
        code: r.get(12)?,
        message: r.get(13)?,
        created_at: r.get(14)?,
        updated_at: r.get(15)?,
    })
}

/// Inserts the target row in state `staged`. The caller inserts the
/// `move_in` session hold and the `session_git` row (`created_branch = 0`,
/// `publish_ref = NULL`) in the same transaction.
///
/// # Errors
/// A database error (a constraint error for a move id or command id that is
/// stored already).
pub fn insert_in(conn: &Connection, new: &NewMoveIn, now: i64) -> Result<()> {
    let authority = new.authority.as_ref().map(json).transpose()?;
    conn.execute(
        "INSERT INTO move_in_runs
           (move_id, command_id, session_id, provider, native_session_id, state, authority,
            created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, 'staged', ?6, ?7, ?7)",
        params![
            new.move_id,
            new.command_id,
            new.session_id.as_str(),
            text_of(&new.provider),
            new.native_session_id,
            authority,
            now,
        ],
    )?;
    Ok(())
}

/// The target row of `move_id`.
///
/// # Errors
/// A database error.
pub fn get_in(conn: &Connection, move_id: &str) -> Result<Option<MoveInRow>> {
    Ok(conn
        .query_row(
            &format!("SELECT {IN_COLUMNS} FROM move_in_runs WHERE move_id = ?1"),
            params![move_id],
            in_of,
        )
        .optional()?)
}

/// The target row that the command `command_id` made.
///
/// # Errors
/// A database error.
pub fn in_by_command(conn: &Connection, command_id: &str) -> Result<Option<MoveInRow>> {
    Ok(conn
        .query_row(
            &format!("SELECT {IN_COLUMNS} FROM move_in_runs WHERE command_id = ?1"),
            params![command_id],
            in_of,
        )
        .optional()?)
}

/// The target row of the new session `session_id`, if any.
///
/// # Errors
/// A database error.
pub fn in_of_session(conn: &Connection, session_id: &SessionId) -> Result<Option<MoveInRow>> {
    Ok(conn
        .query_row(
            &format!("SELECT {IN_COLUMNS} FROM move_in_runs WHERE session_id = ?1"),
            params![session_id.as_str()],
            in_of,
        )
        .optional()?)
}

/// What a tombstone does to a transition (design §12.3: the launch admission
/// and the commit transaction both refuse a tombstoned move id).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tombstone {
    /// The transition runs whatever the tombstone says.
    Ignore,
    /// The transition changes zero rows when the move id has a tombstone.
    Refuse,
}

/// An optional code and text that a transition stores with the new state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Note<'a> {
    pub code: &'a str,
    pub text: &'a str,
}

/// The `MoveInInfo` of the `session.move_in` event of a row (design §3.6).
#[must_use]
pub fn info_of(row: &MoveInRow) -> MoveInInfo {
    MoveInInfo {
        move_id: row.move_id.clone(),
        state: row.state,
        code: row.code.clone(),
        text: row.message.clone(),
    }
}

/// The conditional transition of a target row, with its event: one `UPDATE …
/// WHERE state = from` (and, with [`Tombstone::Refuse`], no tombstone), then
/// one `session.move_in` event on the new session. Returns the event, or
/// `None` when nothing changed (another party changed the state first, or the
/// move id is tombstoned). A `note` sets `code` and `message`; without one the
/// stored values stay.
///
/// # Errors
/// A database error.
pub fn advance_in(
    conn: &Connection,
    move_id: &str,
    from: MoveInState,
    to: MoveInState,
    note: Option<Note<'_>>,
    tombstone: Tombstone,
    now: i64,
) -> Result<Option<EventEnvelope>> {
    let refuse = tombstone == Tombstone::Refuse;
    let changed = conn.execute(
        "UPDATE move_in_runs SET state = ?3, updated_at = ?4,
                code = COALESCE(?5, code), message = COALESCE(?6, message)
         WHERE move_id = ?1 AND state = ?2
           AND NOT (?7 AND EXISTS (SELECT 1 FROM move_tombstones WHERE move_id = ?1))",
        params![
            move_id,
            text_of(&from),
            text_of(&to),
            now,
            note.map(|n| n.code),
            note.map(|n| n.text),
            refuse,
        ],
    )?;
    if changed == 0 {
        return Ok(None);
    }
    append_info(conn, move_id, now)
}

/// Stores a code and a text on a row that stays in `state` (a failure before
/// the launch, design §12.3, decision Q7), with one `session.move_in` event.
/// `None` when the row is in another state.
///
/// # Errors
/// A database error.
pub fn note_in(
    conn: &Connection,
    move_id: &str,
    state: MoveInState,
    note: Note<'_>,
    now: i64,
) -> Result<Option<EventEnvelope>> {
    let changed = conn.execute(
        "UPDATE move_in_runs SET code = ?3, message = ?4, updated_at = ?5
         WHERE move_id = ?1 AND state = ?2",
        params![move_id, text_of(&state), note.code, note.text, now],
    )?;
    if changed == 0 {
        return Ok(None);
    }
    append_info(conn, move_id, now)
}

fn append_info(conn: &Connection, move_id: &str, now: i64) -> Result<Option<EventEnvelope>> {
    let Some(row) = get_in(conn, move_id)? else {
        return Ok(None);
    };
    let event = NewEvent::from(Event::SessionMoveIn(info_of(&row)));
    Ok(Some(log::append_one(conn, &row.session_id, event, now)?))
}

/// Records the staging folder of an import (design §12.3 step 3). `false`
/// when the row is not `staged`.
///
/// # Errors
/// A database error.
pub fn set_staging(conn: &Connection, move_id: &str, staging: &Staging, now: i64) -> Result<bool> {
    let changed = conn.execute(
        "UPDATE move_in_runs SET staging_path = ?2, staging_dev = ?3, staging_ino = ?4,
                updated_at = ?5
         WHERE move_id = ?1 AND state = 'staged'",
        params![
            move_id,
            staging.path,
            to_i64(staging.dev),
            to_i64(staging.ino),
            now
        ],
    )?;
    Ok(changed > 0)
}

/// Appends one placed file to `imported`, whatever the state of the row: the
/// record is written before the rename, so that a discard can delete the file
/// by its identity after the effect settled (design §12.3 step 4). Run it in a
/// transaction of its own. `false` when the row does not exist.
///
/// # Errors
/// A database error.
pub fn append_imported(
    conn: &Connection,
    move_id: &str,
    file: &ImportedFile,
    now: i64,
) -> Result<bool> {
    let Some(row) = get_in(conn, move_id)? else {
        return Ok(false);
    };
    let mut files = row.imported;
    files.push(file.clone());
    let changed = conn.execute(
        "UPDATE move_in_runs SET imported = ?2, updated_at = ?3 WHERE move_id = ?1",
        params![move_id, json(&files)?, now],
    )?;
    Ok(changed > 0)
}

/// Stores `first_input_at` for a row in `released` that has none (design
/// §12.3, revision 4): the transaction that admits the first input after the
/// release. `false` when nothing changed.
///
/// # Errors
/// A database error.
pub fn note_first_input(conn: &Connection, move_id: &str, now: i64) -> Result<bool> {
    let changed = conn.execute(
        "UPDATE move_in_runs SET first_input_at = ?2, updated_at = ?2
         WHERE move_id = ?1 AND state = 'released' AND first_input_at IS NULL",
        params![move_id, now],
    )?;
    Ok(changed > 0)
}

// ------------------------------------------------------------------ tombstones

/// Records the tombstone of `move_id` (idempotent: the first time stays). A
/// tombstone is never removed; it is also written for a move id that the
/// station does not know (design §12.3, discard).
///
/// # Errors
/// A database error.
pub fn insert_tombstone(conn: &Connection, move_id: &str, now: i64) -> Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO move_tombstones (move_id, at) VALUES (?1, ?2)",
        params![move_id, now],
    )?;
    Ok(())
}

/// Does `move_id` have a tombstone?
///
/// # Errors
/// A database error.
pub fn is_tombstoned(conn: &Connection, move_id: &str) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM move_tombstones WHERE move_id = ?1)",
        params![move_id],
        |r| r.get(0),
    )?)
}

// ------------------------------------------------------- the publication tx

/// The values of the publication transaction of a moved-in session (design
/// §12.3 step 4, "Publication ref (M9)").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Publish<'a> {
    pub session_id: &'a SessionId,
    pub move_id: &'a str,
    /// `fd/<target slug>/<callsign>[-n]`.
    pub publish_ref: &'a str,
    /// The branch target of the target station: the `wip_remote` URL or the
    /// remote name.
    pub target: &'a str,
    /// `RemoteIdentity.dest` of the verified URL.
    pub dest: &'a str,
    /// `publication.oid`.
    pub oid: &'a str,
}

/// The publication transaction (revision 4, NEW-1): sets `publish_ref` and
/// `created_branch = 1` on the `session_git` row of a session that has
/// neither, but only while its `move_in_runs` row is `staged` and the move id
/// has no tombstone, then inserts the `git_pushes` row (state `due`, empty
/// lease). `false` when the first statement changed no row: nothing is
/// written. The caller runs it inside a transaction ([`Store::move_publish`]
/// does), so a failing insert rolls the update back.
///
/// # Errors
/// A database error.
pub fn publish(conn: &Connection, p: &Publish<'_>, now: i64) -> Result<bool> {
    let changed = conn.execute(
        "UPDATE session_git SET publish_ref = ?1, created_branch = 1
          WHERE session_id = ?2 AND created_branch = 0 AND publish_ref IS NULL
            AND EXISTS (SELECT 1 FROM move_in_runs WHERE move_id = ?3 AND state = 'staged')
            AND NOT EXISTS (SELECT 1 FROM move_tombstones WHERE move_id = ?3)",
        params![p.publish_ref, p.session_id.as_str(), p.move_id],
    )?;
    if changed == 0 {
        return Ok(false);
    }
    conn.execute(
        "INSERT INTO git_pushes
           (session_id, target, target_dest, ref, last_pushed_oid, desired_oid, state, updated_at)
         VALUES (?1, ?2, ?3, ?4, NULL, ?5, 'due', ?6)",
        params![
            p.session_id.as_str(),
            p.target,
            p.dest,
            p.publish_ref,
            p.oid,
            now
        ],
    )?;
    Ok(true)
}

impl Store {
    /// The publication transaction of [`publish`], in one transaction.
    ///
    /// # Errors
    /// A database error (the update is rolled back).
    pub async fn move_publish(
        &self,
        session_id: SessionId,
        move_id: String,
        publish_ref: String,
        target: String,
        dest: String,
        oid: String,
    ) -> Result<bool> {
        let now = self.now();
        call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            let done = publish(
                &tx,
                &Publish {
                    session_id: &session_id,
                    move_id: &move_id,
                    publish_ref: &publish_ref,
                    target: &target,
                    dest: &dest,
                    oid: &oid,
                },
                now,
            )?;
            tx.commit()?;
            Ok(done)
        })
        .await
    }

    /// The source row of `move_id`.
    ///
    /// # Errors
    /// A database error.
    pub async fn move_out_run(&self, move_id: String) -> Result<Option<MoveOutRow>> {
        call(&self.inner.db, move |conn| get_out(conn, &move_id)).await
    }

    /// The target row of `move_id`.
    ///
    /// # Errors
    /// A database error.
    pub async fn move_in_run(&self, move_id: String) -> Result<Option<MoveInRow>> {
        call(&self.inner.db, move |conn| get_in(conn, &move_id)).await
    }

    /// The conditional transition of a target row, with its event (see
    /// [`advance_in`]). `false` when nothing changed.
    ///
    /// # Errors
    /// A database error.
    pub async fn move_in_advance(
        &self,
        move_id: String,
        from: MoveInState,
        to: MoveInState,
        note: Option<(String, String)>,
        tombstone: Tombstone,
    ) -> Result<bool> {
        let now = self.now();
        let event = call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            let note = note.as_ref().map(|(code, text)| Note { code, text });
            let event = advance_in(&tx, &move_id, from, to, note, tombstone, now)?;
            tx.commit()?;
            Ok(event)
        })
        .await?;
        if let Some(event) = &event {
            self.publish(event.station_seq);
        }
        Ok(event.is_some())
    }

    /// Does `move_id` have a tombstone?
    ///
    /// # Errors
    /// A database error.
    pub async fn move_tombstoned(&self, move_id: String) -> Result<bool> {
        call(&self.inner.db, move |conn| is_tombstoned(conn, &move_id)).await
    }
}

#[cfg(test)]
#[path = "move_runs_tests.rs"]
mod tests;
