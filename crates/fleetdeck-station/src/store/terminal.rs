//! The terminal hold of a session (DL-140, `docs/v2/UI-PLAN.md` §3).
//!
//! While a TUI runs on a session, `summary.terminal` holds it: the hold
//! starts with the admission of `shell.handover` and ends with a
//! `session.terminal` that has no `shell_id`. Every write below is one
//! transaction and conditional on the current hold (`summary.terminal` names
//! the same `shell_id`); a write for a hold that is gone does nothing and
//! returns `false`, so a late exit report never clears a newer hold.

use std::path::PathBuf;

use fleetdeck_proto::session::TerminalHold;
use fleetdeck_proto::{Event, EventEnvelope, ProviderInstanceId, SessionId};
use rusqlite::{Connection, OptionalExtension as _};

use super::receipts::Decision;
use super::{Result, Store, call, log};

/// What the station needs to start the provider TUI of an ended session,
/// read from its projection and its native resume identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HandoverLaunch {
    pub cwd: PathBuf,
    pub provider_instance: ProviderInstanceId,
    pub native: String,
    pub mode: Option<String>,
    pub model: Option<String>,
}

/// Appends `event` to `session` when the session is held by `shell_id`.
fn append_if_held(
    conn: &Connection,
    now: i64,
    events: &mut Vec<EventEnvelope>,
    session: &SessionId,
    shell_id: &str,
    event: Event,
) -> Result<bool> {
    let held = log::summary(conn, session)?
        .and_then(|s| s.terminal)
        .is_some_and(|h| h.shell_id == shell_id);
    if !held {
        return Ok(false);
    }
    events.push(log::append_one(conn, session, event.into(), now)?);
    Ok(true)
}

/// The session that the shell `shell_id` holds.
fn held_by_shell(conn: &Connection, shell_id: &str) -> Result<Option<SessionId>> {
    Ok(conn
        .query_row(
            "SELECT session_id FROM sessions
             WHERE json_extract(summary, '$.terminal.shell_id') = ?1 LIMIT 1",
            [shell_id],
            |r| r.get::<_, String>(0),
        )
        .optional()?
        .map(SessionId::from))
}

impl Decision<'_> {
    /// The session that the shell `shell_id` holds, if any (decision
    /// transaction of `shell.release`).
    ///
    /// # Errors
    /// A database error.
    pub fn session_held_by_shell(&self, shell_id: &str) -> Result<Option<SessionId>> {
        held_by_shell(self.connection(), shell_id)
    }
}

impl Store {
    /// The launch facts of `session`: `None` when the session, its
    /// workspace, its provider instance or its native resume identity is
    /// missing.
    ///
    /// # Errors
    /// A database or JSON error.
    pub async fn handover_launch(&self, session: SessionId) -> Result<Option<HandoverLaunch>> {
        call(&self.inner.db, move |conn| {
            let Some(summary) = log::summary(conn, &session)? else {
                return Ok(None);
            };
            let native: Option<String> = conn
                .query_row(
                    "SELECT native_session_id FROM sessions WHERE session_id = ?1",
                    [session.as_str()],
                    |r| r.get(0),
                )
                .optional()?
                .flatten();
            let (Some(native), Some(cwd), Some(provider_instance)) =
                (native, summary.cwd, summary.provider_instance)
            else {
                return Ok(None);
            };
            Ok(Some(HandoverLaunch {
                cwd: PathBuf::from(cwd),
                provider_instance,
                native,
                mode: summary.mode,
                model: summary.model,
            }))
        })
        .await
    }

    /// Stores the pid of the TUI process in the hold. `false` when `shell_id`
    /// does not hold `session` any more.
    ///
    /// # Errors
    /// A database error.
    pub async fn record_terminal_pid(
        &self,
        session: SessionId,
        shell_id: String,
        pid: u32,
    ) -> Result<bool> {
        self.terminal_write(session, shell_id.clone(), move |shell_id| {
            Event::SessionTerminal {
                shell_id: Some(shell_id),
                pid: Some(pid),
                reason: Some("terminal started".to_owned()),
            }
        })
        .await
    }

    /// Ends the hold of `shell_id` on `session`. `false` when `shell_id`
    /// does not hold `session` any more.
    ///
    /// # Errors
    /// A database error.
    pub async fn release_terminal(
        &self,
        session: SessionId,
        shell_id: String,
        reason: String,
    ) -> Result<bool> {
        self.terminal_write(session, shell_id, move |_| Event::SessionTerminal {
            shell_id: None,
            pid: None,
            reason: Some(reason),
        })
        .await
    }

    /// Appends a `session.terminal` that keeps the hold and states why (the
    /// start recovery). `pid` `None` keeps the stored pid. `false` when
    /// `shell_id` does not hold `session` any more.
    ///
    /// # Errors
    /// A database error.
    pub async fn note_terminal(
        &self,
        session: SessionId,
        shell_id: String,
        pid: Option<u32>,
        reason: String,
    ) -> Result<bool> {
        self.terminal_write(session, shell_id, move |shell_id| Event::SessionTerminal {
            shell_id: Some(shell_id),
            pid,
            reason: Some(reason),
        })
        .await
    }

    async fn terminal_write(
        &self,
        session: SessionId,
        shell_id: String,
        event: impl FnOnce(String) -> Event + Send + 'static,
    ) -> Result<bool> {
        let (done, _) = self
            .write(move |conn, now, events| {
                let event = event(shell_id.clone());
                append_if_held(conn, now, events, &session, &shell_id, event)
            })
            .await?;
        Ok(done)
    }

    /// Every held session with its hold, oldest session first.
    ///
    /// # Errors
    /// A database or JSON error.
    pub async fn held_sessions(&self) -> Result<Vec<(SessionId, TerminalHold)>> {
        call(&self.inner.db, move |conn| {
            let mut stmt = conn.prepare(
                "SELECT session_id FROM sessions
                 WHERE json_extract(summary, '$.terminal') IS NOT NULL
                 ORDER BY created_at, session_id",
            )?;
            let ids = stmt
                .query_map([], |r| r.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            let mut out = Vec::new();
            for id in ids {
                let id = SessionId::from(id);
                if let Some(hold) = log::summary(conn, &id)?.and_then(|s| s.terminal) {
                    out.push((id, hold));
                }
            }
            Ok(out)
        })
        .await
    }

    /// The state of the newest shell effect that is a handover of
    /// `shell_id`.
    ///
    /// # Errors
    /// A database error.
    pub async fn handover_effect_state(&self, shell_id: String) -> Result<Option<String>> {
        call(&self.inner.db, move |conn| {
            Ok(conn
                .query_row(
                    "SELECT state FROM shell_effects WHERE shell_id = ?1 AND op = 'handover'
                     ORDER BY created_at DESC, effect_id DESC LIMIT 1",
                    [shell_id],
                    |r| r.get(0),
                )
                .optional()?)
        })
        .await
    }
}
