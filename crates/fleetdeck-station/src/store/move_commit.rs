//! The store side of the launch, the commit, the release and the confirm of a
//! moved-in session (FG-9, design §12.3 steps 4 and 6, §12.4A U1, T4.2d): the
//! release transaction, the `resume_failed` and `failed` transactions, the
//! station start rule, the cancel of a queued `MoveIn` effect, and a few small
//! reads. The conditional transitions themselves are in `move_runs.rs`.

use std::collections::HashSet;

use fleetdeck_proto::command::UserInput;
use fleetdeck_proto::event::ProviderKind;
use fleetdeck_proto::fleet_git::{MoveInState, PushState, same_dest};
use fleetdeck_proto::{Event, EventEnvelope, InputId, SessionId};
use rusqlite::{Connection, params};

use super::effects::{self, EffectPayload, InputRecord};
use super::git_state::{push_row, save_push};
use super::move_runs::{self, Note, Tombstone};
use super::session_holds::{self, KIND_MOVE_IN};
use super::{NewEvent, Result, Store, call, log};

/// The `stop_code` of the push row of a moved-in session that could not
/// resume (design §12.3).
pub const STOP_MOVE_FAILED: &str = "move_failed";
/// The code of a launching row that a station restart found (design §12.3).
pub const RESTART_CODE: &str = "move_resume_failed";
/// The text of that row.
pub const RESTART_TEXT: &str = "The station restarted while the session was resuming.";
/// The code of the error event of a held prompt that could not be queued.
pub const PROMPT_NOT_SENT: &str = "move_prompt_not_sent";

/// What a release did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Released {
    /// The row is `released` (now, or before).
    Released,
    /// The row is `confirmed` (now, or before).
    Confirmed,
    /// The row is not ready to release; its state, or `None` for no row.
    NotReady(Option<MoveInState>),
}

/// What the release transaction found and queued.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReleaseOutcome {
    pub released: Released,
    /// True when this call changed the row (the confirm watch starts).
    pub changed: bool,
    /// True when the held prompt effect was queued.
    pub queued_prompt: bool,
}

fn stop_push(
    conn: &Connection,
    session: &SessionId,
    now: i64,
    out: &mut Vec<EventEnvelope>,
) -> Result<()> {
    let Some(mut row) = push_row(conn, session)? else {
        return Ok(());
    };
    row.state = PushState::Stopped;
    row.stop_code = Some(STOP_MOVE_FAILED.to_owned());
    row.next_attempt_at = None;
    save_push(conn, &row, now, out)
}

/// The release transaction (design §12.3 step 6): `resumed` becomes
/// `released` (Claude) or `confirmed` (Codex), the `move_in` hold is deleted,
/// and a branch-only move queues its held first prompt. A repeat answers the
/// stored state.
///
/// # Errors
/// A database error.
pub fn release(
    conn: &Connection,
    ids: &dyn fleetdeck_core::IdSource,
    move_id: &str,
    held_prompt: Option<&str>,
    now: i64,
    out: &mut Vec<EventEnvelope>,
) -> Result<ReleaseOutcome> {
    let done = |released, changed, queued_prompt| ReleaseOutcome {
        released,
        changed,
        queued_prompt,
    };
    let Some(row) = move_runs::get_in(conn, move_id)? else {
        return Ok(done(Released::NotReady(None), false, false));
    };
    match row.state {
        MoveInState::Released => return Ok(done(Released::Released, false, false)),
        MoveInState::Confirmed => return Ok(done(Released::Confirmed, false, false)),
        MoveInState::Resumed => {}
        other => return Ok(done(Released::NotReady(Some(other)), false, false)),
    }
    let (to, released) = if row.provider == ProviderKind::Codex {
        (MoveInState::Confirmed, Released::Confirmed)
    } else {
        (MoveInState::Released, Released::Released)
    };
    session_holds::delete(conn, &row.session_id, move_id)?;
    let event = move_runs::advance_in(
        conn,
        move_id,
        MoveInState::Resumed,
        to,
        None,
        Tombstone::Ignore,
        now,
    )?;
    out.extend(event);
    let mut queued = false;
    if let Some(text) = held_prompt {
        if effects::live_process(conn, &row.session_id)?.is_some() {
            let record = InputRecord {
                input_id: fleetdeck_core::new_id::<InputId>(ids),
                origin: fleetdeck_proto::event::RunOrigin::Continuation,
                input: UserInput::new(text),
                shell_context: Vec::new(),
            };
            effects::enqueue(
                conn,
                ids,
                now,
                None,
                &row.session_id,
                EffectPayload::prompt(record),
            )?;
            queued = true;
            // The first input of a branch-only Claude session starts the
            // 120 s rule of the confirm (design §12.3).
            if to == MoveInState::Released {
                move_runs::note_first_input(conn, move_id, now)?;
            }
        } else {
            let event = NewEvent::from(Event::Error {
                code: PROMPT_NOT_SENT.to_owned(),
                message: "The first prompt of the moved session was not sent because the session is not running."
                    .to_owned(),
                retry_at: None,
            });
            out.push(log::append_one(conn, &row.session_id, event, now)?);
        }
    }
    Ok(done(released, true, queued))
}

/// The failure after the release (design §12.3, "The rules after the
/// release"): `released` becomes `resume_failed` with the code and text, the
/// `move_in` hold is inserted again, and the push row stops with
/// `move_failed`. `false` when the row is not `released`.
///
/// # Errors
/// A database error.
pub fn resume_failed(
    conn: &Connection,
    move_id: &str,
    note: Note<'_>,
    now: i64,
    out: &mut Vec<EventEnvelope>,
) -> Result<bool> {
    let Some(row) = move_runs::get_in(conn, move_id)? else {
        return Ok(false);
    };
    let event = move_runs::advance_in(
        conn,
        move_id,
        MoveInState::Released,
        MoveInState::ResumeFailed,
        Some(note),
        Tombstone::Ignore,
        now,
    )?;
    let Some(event) = event else {
        return Ok(false);
    };
    out.push(event);
    session_holds::delete(conn, &row.session_id, move_id)?;
    session_holds::insert(conn, &row.session_id, KIND_MOVE_IN, move_id, now)?;
    stop_push(conn, &row.session_id, now, out)?;
    Ok(true)
}

/// Step U1 (design §12.4A): `resume_failed` becomes `failed`. Returns the
/// state of the row after the call (`None`: no row).
///
/// # Errors
/// A database error.
pub fn fail(
    conn: &Connection,
    move_id: &str,
    now: i64,
    out: &mut Vec<EventEnvelope>,
) -> Result<Option<MoveInState>> {
    let Some(row) = move_runs::get_in(conn, move_id)? else {
        return Ok(None);
    };
    if row.state != MoveInState::ResumeFailed {
        return Ok(Some(row.state));
    }
    let event = move_runs::advance_in(
        conn,
        move_id,
        MoveInState::ResumeFailed,
        MoveInState::Failed,
        None,
        Tombstone::Ignore,
        now,
    )?;
    out.extend(event);
    Ok(Some(MoveInState::Failed))
}

/// The station start rule (design §12.3, T4.2 Q12): a row in `launching`
/// becomes `abandoned` with `move_resume_failed`. The hold stays and no file
/// is deleted. Every other state stays as it is. Returns how many rows
/// changed.
///
/// # Errors
/// A database error.
pub fn recover(conn: &Connection, now: i64, out: &mut Vec<EventEnvelope>) -> Result<usize> {
    let mut stmt = conn.prepare("SELECT move_id FROM move_in_runs WHERE state = 'launching'")?;
    let ids: Vec<String> = stmt
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let mut changed = 0;
    for id in ids {
        let event = move_runs::advance_in(
            conn,
            &id,
            MoveInState::Launching,
            MoveInState::Abandoned,
            Some(Note {
                code: RESTART_CODE,
                text: RESTART_TEXT,
            }),
            Tombstone::Ignore,
            now,
        )?;
        if let Some(event) = event {
            out.push(event);
            changed += 1;
        }
    }
    Ok(changed)
}

impl Store {
    /// The release transaction (see [`release`]). `held_prompt` is the text
    /// of the held first prompt of a branch-only move.
    ///
    /// # Errors
    /// A database error.
    pub async fn move_release_tx(
        &self,
        move_id: String,
        held_prompt: Option<String>,
    ) -> Result<ReleaseOutcome> {
        let ids = std::sync::Arc::clone(&self.inner.ids);
        let (outcome, _) = self
            .write(move |conn, now, out| {
                release(
                    conn,
                    ids.as_ref(),
                    &move_id,
                    held_prompt.as_deref(),
                    now,
                    out,
                )
            })
            .await?;
        if outcome.queued_prompt {
            self.wake_worker();
        }
        Ok(outcome)
    }

    /// The `resume_failed` transaction (see [`resume_failed`]).
    ///
    /// # Errors
    /// A database error.
    pub async fn move_resume_failed(
        &self,
        move_id: String,
        code: String,
        text: String,
    ) -> Result<bool> {
        let (done, _) = self
            .write(move |conn, now, out| {
                let note = Note {
                    code: &code,
                    text: &text,
                };
                resume_failed(conn, &move_id, note, now, out)
            })
            .await?;
        Ok(done)
    }

    /// Step U1 (see [`fail`]).
    ///
    /// # Errors
    /// A database error.
    pub async fn move_fail_tx(&self, move_id: String) -> Result<Option<MoveInState>> {
        let (state, _) = self
            .write(move |conn, now, out| fail(conn, &move_id, now, out))
            .await?;
        Ok(state)
    }

    /// Stores a code and a text on a row that stays in `state` (a failure
    /// before the launch, with one `session.move_in` event). `false` when
    /// the row is in another state.
    ///
    /// # Errors
    /// A database error.
    pub async fn move_in_note(
        &self,
        move_id: String,
        state: MoveInState,
        code: String,
        text: String,
    ) -> Result<bool> {
        let (done, _) = self
            .write(move |conn, now, out| {
                let note = Note {
                    code: &code,
                    text: &text,
                };
                let event = move_runs::note_in(conn, &move_id, state, note, now)?;
                let changed = event.is_some();
                out.extend(event);
                Ok(changed)
            })
            .await?;
        Ok(done)
    }

    /// Retires the queued `MoveIn` effect of a session (a discard cancels it
    /// before it runs). `true` when a queued effect was retired.
    ///
    /// # Errors
    /// A database error.
    pub async fn retire_queued_move_in(&self, session_id: SessionId) -> Result<bool> {
        let now = self.now();
        call(&self.inner.db, move |conn| {
            let changed = conn.execute(
                "UPDATE effects SET state = 'retired_not_dispatched',
                        reason = 'move_cancelled', updated_at = ?2
                 WHERE session_id = ?1 AND kind = 'move_in' AND state = 'queued'",
                params![session_id.as_str(), now],
            )?;
            Ok(changed > 0)
        })
        .await
    }

    /// The `git_pushes.ref` values of one destination (design §12.3, the
    /// publication ref).
    ///
    /// # Errors
    /// A database error.
    pub async fn git_push_refs(&self, dest: String) -> Result<HashSet<String>> {
        call(&self.inner.db, move |conn| {
            // A stored `dest` can be in the form of an older release, so the
            // rows are compared with `same_dest` (Fleet Sync §1.1, R3).
            let mut stmt = conn.prepare("SELECT target_dest, ref FROM git_pushes")?;
            let rows = stmt
                .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows
                .into_iter()
                .filter(|(stored, _)| same_dest(stored, &dest))
                .map(|(_, publish_ref)| publish_ref)
                .collect::<HashSet<_>>())
        })
        .await
    }
}
