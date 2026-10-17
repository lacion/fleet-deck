//! The transitions and the reads of the move driver (design §12.3; FG-9,
//! T4.4b). Every state change is one conditional `UPDATE` by the expected
//! state, so a late answer or an event cannot move a row that another party
//! moved first.

use fleetdeck_proto::fleet_git::MovePublication;
use fleetdeck_proto::mutation::CommandAuthority;
use fleetdeck_proto::{SessionId, StationId};
use rusqlite::{Connection, OptionalExtension, params};

use super::coordinator::{Check, recheck};
use super::land_ready::summary_of;
use super::session_moves::{COLS, MoveRow, MoveState, MoveView, find, read};
use super::session_moves_ready::{MoveEnv, MoveTarget, project_for, targets};
use super::{DeckStore, Result, call, new_id, slug_reservations};

/// What a transition stores with the new state.
#[derive(Debug, Clone, Default)]
pub struct MovePatch {
    pub kind: Option<String>,
    pub publication: Option<MovePublication>,
    pub bytes: Option<u64>,
    pub dst_session: Option<String>,
    pub note: Note,
}

/// What a transition does with `code` and `message`.
#[derive(Debug, Clone, Default)]
pub enum Note {
    #[default]
    Keep,
    Set(String, String),
    Clear,
}

impl MovePatch {
    /// A patch that sets the code and the message.
    #[must_use]
    pub fn note(code: &str, message: &str) -> Self {
        Self {
            note: Note::Set(code.to_owned(), message.to_owned()),
            ..Self::default()
        }
    }

    /// A patch that clears the code and the message.
    #[must_use]
    pub fn clear() -> Self {
        Self {
            note: Note::Clear,
            ..Self::default()
        }
    }
}

/// One conditional transition. The attempt count and the next time reset.
/// Returns whether the row changed.
pub(super) fn set_in_tx(
    tx: &Connection,
    id: &str,
    from: &[MoveState],
    to: MoveState,
    patch: &MovePatch,
    now: i64,
) -> Result<bool> {
    let list = from
        .iter()
        .map(|s| format!("'{}'", s.name()))
        .collect::<Vec<_>>()
        .join(", ");
    let changed = tx.execute(
        &format!(
            "UPDATE session_moves SET state = ?2, step_attempts = 0, next_step_at = NULL
             WHERE move_id = ?1 AND state IN ({list})"
        ),
        params![id, to.name()],
    )? > 0;
    if !changed {
        return Ok(false);
    }
    match to {
        MoveState::Committed => {
            tx.execute(
                "UPDATE session_moves SET committed_at = COALESCE(committed_at, ?2)
                 WHERE move_id = ?1",
                params![id, now],
            )?;
        }
        MoveState::Done | MoveState::Failed | MoveState::Undone => {
            tx.execute(
                "UPDATE session_moves SET ended_at = ?2 WHERE move_id = ?1",
                params![id, now],
            )?;
        }
        MoveState::ResumeFailed => {
            tx.execute(
                "UPDATE session_moves SET ended_at = NULL WHERE move_id = ?1",
                [id],
            )?;
        }
        _ => {}
    }
    if let Some(kind) = &patch.kind {
        tx.execute(
            "UPDATE session_moves SET kind = ?2 WHERE move_id = ?1",
            params![id, kind],
        )?;
    }
    if let Some(p) = &patch.publication {
        tx.execute(
            "UPDATE session_moves SET publication = ?2 WHERE move_id = ?1",
            params![id, serde_json::to_string(p)?],
        )?;
    }
    if let Some(bytes) = patch.bytes {
        tx.execute(
            "UPDATE session_moves SET bytes = ?2 WHERE move_id = ?1",
            params![id, i64::try_from(bytes).unwrap_or(i64::MAX)],
        )?;
    }
    if let Some(session) = &patch.dst_session {
        tx.execute(
            "UPDATE session_moves SET dst_session = ?2 WHERE move_id = ?1",
            params![id, session],
        )?;
    }
    match &patch.note {
        Note::Keep => {}
        Note::Set(code, message) => {
            tx.execute(
                "UPDATE session_moves SET code = ?2, message = ?3 WHERE move_id = ?1",
                params![id, code, message],
            )?;
        }
        Note::Clear => {
            tx.execute(
                "UPDATE session_moves SET code = NULL, message = NULL WHERE move_id = ?1",
                [id],
            )?;
        }
    }
    Ok(true)
}

/// The id of the open move of a session on its source station.
pub(super) fn open_row_of(tx: &Connection, station: &str, session: &str) -> Result<Option<String>> {
    Ok(tx
        .query_row(
            "SELECT move_id FROM session_moves
             WHERE src_station = ?1 AND src_session = ?2
               AND state NOT IN ('done', 'failed', 'undone')",
            [station, session],
            |r| r.get(0),
        )
        .optional()?)
}

/// The wait after the `n`th send: `unit` ms, then double, up to 60 units.
#[must_use]
pub fn backoff_ms(sends: u64, unit: i64) -> i64 {
    let shift = u32::try_from(sends).unwrap_or(u32::MAX).min(16);
    (unit << shift).min(unit * 60)
}

/// What the authority gate decides for one send (design §12.3, B3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MoveGate {
    Go,
    Wait,
    Lost { code: &'static str, reason: String },
}

/// The text of an authority end (design §12.7, §11.10 with "move").
#[must_use]
pub fn authority_text(code: &str, reason: &str) -> String {
    match code {
        "authority_expired" => {
            "The coordinator's move request expired before it started (10 minutes).".to_owned()
        }
        "authority_unknown" => {
            "Fleet Deck could not confirm the coordinator's permission, so the move stopped."
                .to_owned()
        }
        _ => format!("The coordinator lost its permission during the move ({reason})."),
    }
}

/// What the driver needs to send `MoveIn` (design §12.3 step 4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InFacts {
    pub project_id: String,
    /// The folder of the target home.
    pub home_folder: String,
    /// The instance of the summary; `None` for a session that never had
    /// input (FG-9): the export answer has it then.
    pub instance: Option<String>,
    pub title: Option<String>,
    pub callsign: Option<String>,
    pub digest: Option<String>,
    pub src_name: String,
    pub dst_name: String,
}

fn unit_of(row: &MoveRow) -> (StationId, SessionId, StationId) {
    (
        StationId::from(row.src_station.clone()),
        SessionId::from(row.src_session.clone()),
        StationId::from(row.dst_station.clone()),
    )
}

impl DeckStore {
    /// The moves that are not closed.
    ///
    /// # Errors
    /// A database or stored-JSON error.
    pub async fn move_open(&self) -> Result<Vec<MoveRow>> {
        call(&self.inner.db, |conn| {
            let mut stmt = conn.prepare(&format!(
                "SELECT {COLS} FROM session_moves
                 WHERE state NOT IN ('done', 'failed', 'undone') ORDER BY created_at, move_id"
            ))?;
            let rows = stmt
                .query_map([], |r| Ok(read(r)))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows.into_iter().collect()
        })
        .await
    }

    /// One move.
    ///
    /// # Errors
    /// A database or stored-JSON error.
    pub async fn move_row(&self, move_id: String) -> Result<Option<MoveRow>> {
        call(&self.inner.db, move |conn| find(conn, &move_id)).await
    }

    /// The newest move where the session is the source or the target.
    ///
    /// # Errors
    /// A database or stored-JSON error.
    pub async fn move_of_session(
        &self,
        station: String,
        session: String,
    ) -> Result<Option<MoveView>> {
        call(&self.inner.db, move |conn| {
            Ok(conn
                .query_row(
                    &format!(
                        "SELECT {COLS} FROM session_moves
                         WHERE (src_station = ?1 AND src_session = ?2)
                            OR (dst_station = ?1 AND dst_session = ?2)
                         ORDER BY created_at DESC, move_id DESC LIMIT 1"
                    ),
                    [&station, &session],
                    |r| Ok(read(r)),
                )
                .optional()?
                .transpose()?
                .map(MoveView::from))
        })
        .await
    }

    /// The stations of the dialog of a move and the first failed condition
    /// of each (T4.4b Q18).
    ///
    /// # Errors
    /// A database or stored-JSON error.
    pub async fn move_targets(
        &self,
        station: String,
        session: String,
        envs: Vec<(String, MoveEnv)>,
    ) -> Result<Vec<MoveTarget>> {
        call(&self.inner.db, move |conn| {
            targets(conn, (&station, &session), &envs)
        })
        .await
    }

    /// One transition (see [`set_in_tx`]).
    ///
    /// # Errors
    /// A database error.
    pub async fn move_advance(
        &self,
        move_id: String,
        from: &'static [MoveState],
        to: MoveState,
        patch: MovePatch,
    ) -> Result<bool> {
        let now = self.now_ms();
        let changed = call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            let changed = set_in_tx(&tx, &move_id, from, to, &patch, now)?;
            tx.commit()?;
            Ok(changed)
        })
        .await?;
        if changed {
            self.board_changed();
        }
        Ok(changed)
    }

    /// Counts one send of the current step and sets the next time
    /// (`backoff_ms`).
    ///
    /// # Errors
    /// A database error.
    pub async fn move_sent(&self, move_id: String, unit: i64) -> Result<()> {
        let now = self.now_ms();
        call(&self.inner.db, move |conn| {
            let attempts: Option<i64> = conn
                .query_row(
                    "SELECT step_attempts FROM session_moves WHERE move_id = ?1",
                    [&move_id],
                    |r| r.get(0),
                )
                .optional()?;
            let sends = u64::try_from(attempts.unwrap_or(0)).unwrap_or(0);
            conn.execute(
                "UPDATE session_moves SET step_attempts = step_attempts + 1, next_step_at = ?2
                 WHERE move_id = ?1",
                params![move_id, now + backoff_ms(sends + 1, unit)],
            )?;
            Ok(())
        })
        .await
    }

    /// Sets the next time of the step and, with `note`, the code and the
    /// message (a retry after a rejected receipt).
    ///
    /// # Errors
    /// A database error.
    pub async fn move_retry_in(
        &self,
        move_id: String,
        delay_ms: i64,
        note: Option<(String, String)>,
    ) -> Result<()> {
        let at = self.now_ms() + delay_ms;
        call(&self.inner.db, move |conn| {
            conn.execute(
                "UPDATE session_moves SET next_step_at = ?2, step_attempts = step_attempts + 1
                 WHERE move_id = ?1",
                params![move_id, at],
            )?;
            if let Some((code, message)) = note {
                conn.execute(
                    "UPDATE session_moves SET code = ?2, message = ?3 WHERE move_id = ?1",
                    params![move_id, code, message],
                )?;
            }
            Ok(())
        })
        .await?;
        self.board_changed();
        Ok(())
    }

    /// Sets the next time of the step with no count of a send.
    ///
    /// # Errors
    /// A database error.
    pub async fn move_next_at(&self, move_id: String, delay_ms: i64) -> Result<()> {
        let at = self.now_ms() + delay_ms;
        call(&self.inner.db, move |conn| {
            conn.execute(
                "UPDATE session_moves SET next_step_at = ?2 WHERE move_id = ?1",
                params![move_id, at],
            )?;
            Ok(())
        })
        .await
    }

    /// Stores the id of the new session when the row does not know it yet.
    ///
    /// # Errors
    /// A database error.
    pub async fn move_set_dst(&self, move_id: String, session: String) -> Result<()> {
        call(&self.inner.db, move |conn| {
            conn.execute(
                "UPDATE session_moves SET dst_session = ?2
                 WHERE move_id = ?1 AND dst_session IS NULL",
                params![move_id, session],
            )?;
            Ok(())
        })
        .await
    }

    /// The name of a station, else its id.
    pub async fn move_station_name(&self, id: &str) -> String {
        let id = id.to_owned();
        let name = id.clone();
        call(&self.inner.db, move |conn| {
            slug_reservations::station_name(conn, &id)
        })
        .await
        .unwrap_or(name)
    }

    /// The deck restart (design §12.3, "Deck restart"): `holding`,
    /// `exporting` and `importing` abort with `move_lost`; the next time of
    /// the other rows resets. Returns the ids of the `starting` rows, which
    /// the driver asks first with the restart rule.
    ///
    /// # Errors
    /// A database error.
    pub async fn move_recover(&self) -> Result<Vec<String>> {
        let now = self.now_ms();
        let starting = call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            let lost: Vec<MoveRow> = {
                let mut stmt = tx.prepare(&format!(
                    "SELECT {COLS} FROM session_moves
                     WHERE state IN ('holding', 'exporting', 'importing')"
                ))?;
                let rows = stmt
                    .query_map([], |r| Ok(read(r)))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                rows.into_iter().collect::<Result<_>>()?
            };
            for row in lost {
                let name = slug_reservations::station_name(&tx, &row.src_station)?;
                let text = format!(
                    "The move stopped when the deck restarted. The session stays on {name}."
                );
                set_in_tx(
                    &tx,
                    &row.move_id,
                    &[row.state],
                    MoveState::Aborting,
                    &MovePatch::note("move_lost", &text),
                    now,
                )?;
            }
            tx.execute(
                "UPDATE session_moves SET next_step_at = NULL
                 WHERE state IN ('starting', 'committed', 'finishing', 'releasing', 'aborting',
                                 'undoing')",
                [],
            )?;
            let mut stmt =
                tx.prepare("SELECT move_id FROM session_moves WHERE state = 'starting'")?;
            let ids = stmt
                .query_map([], |r| r.get(0))?
                .collect::<rusqlite::Result<Vec<String>>>()?;
            drop(stmt);
            tx.commit()?;
            Ok(ids)
        })
        .await?;
        self.board_changed();
        Ok(starting)
    }

    /// The authority gate of a coordinator move before one send of steps 1
    /// to 4 (design §12.3, B3): the route is the source station with its
    /// session, then the target station. An owner move always passes.
    ///
    /// # Errors
    /// A database error.
    pub async fn move_gate(&self, move_id: String) -> Result<MoveGate> {
        let now = self.now_ms();
        let inner = std::sync::Arc::clone(&self.inner);
        call(&self.inner.db, move |conn| {
            let fleet = inner.fleet_env();
            let tx = conn.transaction()?;
            let Some(row) = find(&tx, &move_id)? else {
                return Ok(MoveGate::Go);
            };
            let Some(authority) = &row.authority else {
                return Ok(MoveGate::Go);
            };
            if now > row.authority_expires_at.unwrap_or(i64::MAX) {
                return Ok(MoveGate::Lost {
                    code: "authority_expired",
                    reason: String::new(),
                });
            }
            let (src, session, dst) = unit_of(&row);
            let a: &CommandAuthority = authority;
            let mut wait = false;
            for route in [(&src, Some(&session)), (&dst, None)] {
                match recheck(&tx, &fleet, a, route, now)? {
                    Check::Valid => {}
                    Check::Wait => wait = true,
                    Check::Lost(reason) => {
                        return Ok(MoveGate::Lost {
                            code: "authority_lost",
                            reason: reason.to_owned(),
                        });
                    }
                }
            }
            Ok(if wait { MoveGate::Wait } else { MoveGate::Go })
        })
        .await
    }

    /// What `MoveIn` needs: the project of the source session, the folder of
    /// the target home, the instance, title and callsign of the source.
    ///
    /// # Errors
    /// A database or stored-JSON error.
    pub async fn move_in_facts(&self, move_id: String) -> Result<Option<InFacts>> {
        call(&self.inner.db, move |conn| {
            let Some(row) = find(conn, &move_id)? else {
                return Ok(None);
            };
            let Some(summary) = summary_of(conn, &row.src_station, &row.src_session)? else {
                return Ok(None);
            };
            let Some(project) = project_for(conn, &row.src_station, &summary)? else {
                return Ok(None);
            };
            let Some(home) =
                super::project_homes::home_on(conn, &project.project_id, &row.dst_station)?
            else {
                return Ok(None);
            };
            Ok(Some(InFacts {
                project_id: project.project_id,
                home_folder: home.folder().unwrap_or_default().to_owned(),
                instance: summary.provider_instance.as_ref().map(ToString::to_string),
                title: summary.title.clone(),
                callsign: summary.callsign.clone(),
                digest: summary.digest.clone(),
                src_name: slug_reservations::station_name(conn, &row.src_station)?,
                dst_name: slug_reservations::station_name(conn, &row.dst_station)?,
            }))
        })
        .await
    }

    /// The project of a session and the folder of the home of `dst`, before a
    /// move starts (the route checks the start of the target with them).
    ///
    /// # Errors
    /// A database or stored-JSON error.
    pub async fn move_probe(
        &self,
        station: String,
        session: String,
        dst: String,
    ) -> Result<Option<(String, String)>> {
        call(&self.inner.db, move |conn| {
            let Some(summary) = summary_of(conn, &station, &session)? else {
                return Ok(None);
            };
            let Some(project) = project_for(conn, &station, &summary)? else {
                return Ok(None);
            };
            let home = super::project_homes::home_on(conn, &project.project_id, &dst)?;
            Ok(home.and_then(|h| h.folder().map(|f| (project.project_id, f.to_owned()))))
        })
        .await
    }

    /// The text of the newest assistant message of a session, from the
    /// condensed timeline (not a subagent message), or `None`.
    ///
    /// # Errors
    /// A database or stored-JSON error.
    pub async fn move_last_message(
        &self,
        station: String,
        session: String,
    ) -> Result<Option<String>> {
        call(&self.inner.db, move |conn| {
            let mut stmt = conn.prepare(
                "SELECT envelope FROM timeline
                 WHERE station_id = ?1 AND session_id = ?2 AND kind = 'message.completed'
                 ORDER BY station_seq DESC LIMIT 20",
            )?;
            let rows = stmt
                .query_map([&station, &session], |r| r.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            for text in rows {
                let envelope: fleetdeck_proto::EventEnvelope = serde_json::from_str(&text)?;
                if let fleetdeck_proto::event::EventBody::Known(
                    fleetdeck_proto::event::Event::MessageCompleted {
                        parent_node_id: None,
                        text,
                        ..
                    },
                ) = envelope.event
                    && !text.trim().is_empty()
                {
                    return Ok(Some(text));
                }
            }
            Ok(None)
        })
        .await
    }

    /// The owner asks for "Undo move" (design §12.4A): a `resume_failed` move
    /// becomes `undoing` with a new `undo_command_id`. The same request
    /// command id again returns the first answer.
    ///
    /// # Errors
    /// A database or stored-JSON error. A refusal is the `Ok` value.
    pub async fn move_undo_request(
        &self,
        move_id: String,
        request_command_id: String,
    ) -> Result<std::result::Result<MoveRow, super::session_moves::MoveRefusal>> {
        let now = self.now_ms();
        let undo = format!("moveundo-{}", new_id::<String>(self.inner.ids.as_ref()));
        let out = call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            let Some(row) = find(&tx, &move_id)? else {
                return Ok(Err(super::session_moves::MoveRefusal::new(
                    "unknown_move",
                    "Unknown move.",
                )));
            };
            if row.undo_request_command_id.as_deref() == Some(request_command_id.as_str()) {
                return Ok(Ok(row));
            }
            let reused = tx
                .query_row(
                    "SELECT 1 FROM session_moves WHERE undo_request_command_id = ?1",
                    [&request_command_id],
                    |_| Ok(()),
                )
                .optional()?
                .is_some();
            if reused {
                return Ok(Err(super::session_moves::MoveRefusal::new(
                    "command_id_reused",
                    "This command id was used for another request.",
                )));
            }
            if row.state != MoveState::ResumeFailed {
                return Ok(Err(super::session_moves::MoveRefusal::new(
                    "move_not_undoable",
                    "Only a move whose new session could not resume can be undone.",
                )));
            }
            let changed = set_in_tx(
                &tx,
                &move_id,
                &[MoveState::ResumeFailed],
                MoveState::Undoing,
                &MovePatch::default(),
                now,
            )?;
            if changed {
                tx.execute(
                    "UPDATE session_moves SET undo_command_id = ?2, undo_request_command_id = ?3
                     WHERE move_id = ?1",
                    params![move_id, undo, request_command_id],
                )?;
            }
            let row =
                find(&tx, &move_id)?.ok_or_else(|| super::DeckError::Corrupt("move row".into()))?;
            tx.commit()?;
            Ok(Ok(row))
        })
        .await;
        let out = out?;
        if out.is_ok() {
            self.board_changed();
        }
        Ok(out)
    }
}
