//! Stored facts of auto-continue (L19, ADR-007 §2.2–§2.4, DL-127): the
//! work that a station restart interrupted, the owner commands after it,
//! and the dispatch recheck of a stored continuation.

use fleetdeck_proto::event::SessionState;
use fleetdeck_proto::{Event, EventBody, SessionId, TurnId};
use rusqlite::{Connection, OptionalExtension as _, params};

use super::effects::{Effect, EffectPayload};
use super::receipts::Decision;
use super::{Result, Store, call};

/// The default continuation window: the station start must be at most this
/// long after the last committed event of the session (ADR-007 rule 9).
pub const WINDOW_MS: i64 = 10 * 60 * 1000;
/// The command id prefix of a continuation intent:
/// `continue:<session_id>:<lost turn id>` (ADR-007 §2.3).
pub const COMMAND_PREFIX: &str = "continue:";
/// The fixed continuation input (ADR-007 §2.3).
pub const CONTINUATION_TEXT: &str =
    "The station restarted while you were working. Continue the task where you stopped.";

/// Effect kinds that are owner work (rule 5). Answers, interrupts and stops
/// are not.
const OWNER_WORK: &str = "('prompt', 'steer', 'follow_up', 'set_mode', 'set_model', 'revert')";
/// Effect kinds that carry provider input (rule 11).
const INPUTS: &str = "('prompt', 'steer', 'follow_up', 'answer')";

/// The work of a session as it was before a station start lost it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LostWork {
    /// The last started turn before the restart.
    pub turn_id: Option<TurnId>,
    /// When that turn started (Unix ms).
    pub turn_at: i64,
    /// The last turn had no `turn.completed` before the restart.
    pub turn_open: bool,
    /// A run had no `run.completed` before the restart.
    pub run_open: bool,
    /// The open run is a maintenance run (`/compact`).
    pub maintenance: bool,
    /// The session was `stopping`.
    pub stopping: bool,
    /// The last committed event before the restart (Unix ms).
    pub last_at: i64,
    /// The `session.state` `lost` event of this start.
    pub lost_seq: u64,
    /// When the loss was committed (Unix ms).
    pub lost_at: i64,
}

fn events_of(conn: &Connection, session: &SessionId) -> Result<Vec<(u64, i64, EventBody)>> {
    let mut stmt = conn.prepare(
        "SELECT station_seq, at, payload FROM events WHERE session_id = ?1
         ORDER BY station_seq",
    )?;
    let rows = stmt.query_map([session.as_str()], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, i64>(1)?,
            r.get::<_, String>(2)?,
        ))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (seq, at, payload) = row?;
        out.push((
            u64::try_from(seq).unwrap_or(0),
            at,
            serde_json::from_str::<EventBody>(&payload)?,
        ));
    }
    Ok(out)
}

/// The work that the first `lost` state after `after` interrupted; `None`
/// when no loss of the session follows `after`.
fn lost_work(conn: &Connection, session: &SessionId, after: u64) -> Result<Option<LostWork>> {
    let mut w = LostWork::default();
    for (seq, at, body) in events_of(conn, session)? {
        let EventBody::Known(event) = body else {
            continue;
        };
        if seq > after {
            if let Event::SessionStateChanged {
                state: SessionState::Lost,
                ..
            } = event
            {
                w.lost_seq = seq;
                w.lost_at = at;
                return Ok(Some(w));
            }
            continue;
        }
        w.last_at = at;
        match event {
            Event::RunStarted { input_summary, .. } => {
                w.run_open = true;
                w.maintenance = input_summary.trim_start().starts_with("/compact");
            }
            Event::RunCompleted { .. } => w.run_open = false,
            Event::TurnStarted { turn_id, .. } => {
                w.turn_id = Some(turn_id);
                w.turn_at = at;
                w.turn_open = true;
            }
            Event::TurnCompleted { turn_id, .. } if w.turn_id.as_ref() == Some(&turn_id) => {
                w.turn_open = false;
            }
            Event::SessionStateChanged { state, .. } => {
                w.stopping = state == SessionState::Stopping;
            }
            _ => {}
        }
    }
    Ok(None)
}

fn exists(conn: &Connection, sql: &str, session: &SessionId, at: i64) -> Result<bool> {
    Ok(conn
        .query_row(sql, params![session.as_str(), at], |_| Ok(()))
        .optional()?
        .is_some())
}

fn owner_work_after(conn: &Connection, session: &SessionId, at: i64) -> Result<bool> {
    exists(
        conn,
        &format!(
            "SELECT 1 FROM effects WHERE session_id = ?1 AND created_at > ?2
             AND kind IN {OWNER_WORK} LIMIT 1"
        ),
        session,
        at,
    )
}

fn stop_since(conn: &Connection, session: &SessionId, at: i64) -> Result<bool> {
    exists(
        conn,
        "SELECT 1 FROM effects WHERE session_id = ?1 AND created_at >= ?2
         AND kind = 'stop' LIMIT 1",
        session,
        at,
    )
}

/// Another start or resume of `session` admitted at or after `at` (an
/// owner `session.resume` after the intent is newer owner work).
fn other_start_since(
    conn: &Connection,
    session: &SessionId,
    effect_id: &super::EffectId,
    at: i64,
) -> Result<bool> {
    Ok(conn
        .query_row(
            "SELECT 1 FROM effects WHERE session_id = ?1 AND created_at >= ?2
             AND kind IN ('start', 'resume') AND effect_id != ?3 LIMIT 1",
            params![session.as_str(), at, effect_id.as_str()],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

/// Rules 7 and 8 at dispatch: a maintenance run (`/compact`) started, or a
/// turn completed or the session ended, after the intent stored at `at`.
/// The intent follows the `lost` state of its station start, which follows
/// the recovery events of that start; so only events after the last `lost`
/// state committed at or before `at` are new.
fn activity_since(conn: &Connection, session: &SessionId, at: i64) -> Result<Option<&'static str>> {
    let lost_seq: i64 = conn.query_row(
        "SELECT COALESCE(MAX(station_seq), 0) FROM events WHERE session_id = ?1
         AND kind = 'session.state' AND at <= ?2
         AND json_extract(payload, '$.state') = 'lost'",
        params![session.as_str(), at],
        |r| r.get(0),
    )?;
    let mut stmt = conn.prepare(
        "SELECT payload FROM events WHERE session_id = ?1 AND station_seq > ?2
         AND kind IN ('run.started', 'turn.completed', 'session.ended')
         ORDER BY station_seq",
    )?;
    let rows = stmt.query_map(params![session.as_str(), lost_seq], |r| {
        r.get::<_, String>(0)
    })?;
    for row in rows {
        match serde_json::from_str::<EventBody>(&row?)? {
            EventBody::Known(Event::RunStarted { input_summary, .. })
                if input_summary.trim_start().starts_with("/compact") =>
            {
                return Ok(Some("maintenance_turn"));
            }
            EventBody::Known(Event::TurnCompleted { .. } | Event::SessionEnded { .. }) => {
                return Ok(Some("turn_completed"));
            }
            _ => {}
        }
    }
    Ok(None)
}

/// The number of sessions that have not ended (rule 10, a free slot).
fn active_sessions(conn: &Connection) -> Result<u64> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sessions WHERE state NOT IN ('exited', 'lost')",
        [],
        |r| r.get(0),
    )?;
    Ok(u64::try_from(n).unwrap_or(0))
}

impl Decision<'_> {
    /// The sessions that a `session.state` event after `after` names.
    ///
    /// # Errors
    /// A database error.
    pub fn sessions_changed_after(&self, after: u64) -> Result<Vec<SessionId>> {
        let mut stmt = self.connection().prepare(
            "SELECT DISTINCT session_id FROM events
             WHERE station_seq > ?1 AND kind = 'session.state' ORDER BY session_id",
        )?;
        let rows = stmt.query_map([i64::try_from(after).unwrap_or(i64::MAX)], |r| {
            r.get::<_, String>(0)
        })?;
        Ok(rows
            .collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .map(SessionId::from)
            .collect())
    }

    /// See [`LostWork`].
    ///
    /// # Errors
    /// A database or JSON error.
    pub fn lost_work(&self, session: &SessionId, after: u64) -> Result<Option<LostWork>> {
        lost_work(self.connection(), session, after)
    }

    /// Rule 5: an owner command of `session` was admitted after `at`.
    ///
    /// # Errors
    /// A database error.
    pub fn owner_work_after(&self, session: &SessionId, at: i64) -> Result<bool> {
        owner_work_after(self.connection(), session, at)
    }

    /// Rule 6: a `session.stop` of `session` was admitted at or after `at`.
    ///
    /// # Errors
    /// A database error.
    pub fn stop_since(&self, session: &SessionId, at: i64) -> Result<bool> {
        stop_since(self.connection(), session, at)
    }

    /// Rule 11: an input of `session` became `outcome_unknown` at or after
    /// `at` (the recovery of this start).
    ///
    /// # Errors
    /// A database error.
    pub fn unknown_input_since(&self, session: &SessionId, at: i64) -> Result<bool> {
        exists(
            self.connection(),
            &format!(
                "SELECT 1 FROM effects WHERE session_id = ?1 AND updated_at >= ?2
                 AND state = 'outcome_unknown' AND kind IN {INPUTS} LIMIT 1"
            ),
            session,
            at,
        )
    }
}

impl Store {
    /// See [`LostWork`].
    ///
    /// # Errors
    /// A database or JSON error.
    pub async fn lost_work(&self, session: SessionId, after: u64) -> Result<Option<LostWork>> {
        call(&self.inner.db, move |conn| lost_work(conn, &session, after)).await
    }

    /// The dispatch recheck of a continuation (ADR-007 §2.4 rules 2, 3,
    /// 5, 6, 9 and 10, and the A10 maintenance and completion rechecks,
    /// rules 7 and 8, DL-133) against the committed state. `max_sessions`
    /// is the station's session limit (rule 10, a free slot). `None`:
    /// dispatch it; otherwise the stable code of the failed rule. An
    /// effect that is not a continuation is never rechecked.
    ///
    /// # Errors
    /// A database or JSON error.
    pub async fn continuation_recheck(
        &self,
        effect: Effect,
        max_sessions: Option<u32>,
    ) -> Result<Option<String>> {
        let EffectPayload::Resume {
            native_session_id,
            spec,
            continue_by: Some(continue_by),
            ..
        } = effect.payload.clone()
        else {
            return Ok(None);
        };
        let now = self.now();
        call(&self.inner.db, move |conn| {
            let session = &effect.session_id;
            let created_at: i64 = conn.query_row(
                "SELECT created_at FROM effects WHERE effect_id = ?1",
                [effect.effect_id.as_str()],
                |r| r.get(0),
            )?;
            let stored: Option<String> = conn
                .query_row(
                    "SELECT native_session_id FROM sessions WHERE session_id = ?1",
                    [session.as_str()],
                    |r| r.get(0),
                )
                .optional()?
                .flatten();
            let summary = super::log::summary(conn, session)?;
            let code = if stored.as_deref() != Some(native_session_id.as_str())
                || summary.as_ref().and_then(|s| s.provider_instance.as_ref())
                    != Some(&spec.provider_instance)
            {
                Some("identity_changed")
            } else if super::resumes::unended_execution(conn, session)?.is_some() {
                Some("execution_unknown")
            } else if owner_work_after(conn, session, created_at)?
                || other_start_since(conn, session, &effect.effect_id, created_at)?
            {
                Some("newer_owner_work")
            } else if stop_since(conn, session, created_at)? {
                Some("stop_requested")
            } else if let Some(code) = activity_since(conn, session, created_at)? {
                Some(code)
            } else if now > continue_by {
                Some("expired")
            } else if !summary
                .as_ref()
                .and_then(|s| s.capabilities.as_ref())
                .is_some_and(|c| c.resume)
            {
                Some("unsupported")
            } else if summary
                .as_ref()
                .is_some_and(|s| s.revert.as_ref().is_some_and(|r| r.blocks_input()))
                || super::sharing::open_revert_over(conn, &spec.cwd)?.is_some()
                || super::worktree_ops::open_over(conn, &spec.cwd)?.is_some()
                || max_sessions
                    .is_some_and(|max| active_sessions(conn).is_ok_and(|n| n >= u64::from(max)))
            {
                Some("not_resumable")
            } else {
                None
            };
            Ok(code.map(str::to_owned))
        })
        .await
    }
}
