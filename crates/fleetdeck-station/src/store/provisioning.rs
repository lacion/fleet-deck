//! The starts that prepare their workspace (a clone or a worktree) in a task
//! of their own. Such a start has no process yet, so a stop of its session is
//! possible without one. The effect stays `claimed` until the start task
//! settles it.

use fleetdeck_proto::SessionId;
use rusqlite::{Connection, params};

use super::effects::EffectPayload;
use super::{Result, Store, call};

/// The sessions whose claimed start effect carries a workspace plan, oldest
/// first. `only` limits the answer to one session.
fn workspace_starts(conn: &Connection, only: Option<&SessionId>) -> Result<Vec<SessionId>> {
    let mut stmt = conn.prepare(
        "SELECT session_id, payload FROM effects
         WHERE state = 'claimed' AND kind = 'start'
           AND session_id = COALESCE(?1, session_id)
         ORDER BY seq",
    )?;
    let rows = stmt.query_map(params![only.map(SessionId::as_str)], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
    })?;
    let mut sessions = Vec::new();
    for row in rows {
        let (session, payload) = row?;
        if matches!(
            serde_json::from_str::<EffectPayload>(&payload)?,
            EffectPayload::Start {
                workspace: Some(_),
                ..
            }
        ) {
            sessions.push(SessionId::from(session));
        }
    }
    Ok(sessions)
}

/// Whether the session has a claimed start effect with a workspace plan, so
/// its start prepares the workspace and a stop is possible without a process.
pub(crate) fn provisioning_for(conn: &Connection, session: &SessionId) -> Result<bool> {
    Ok(!workspace_starts(conn, Some(session))?.is_empty())
}

impl Store {
    /// The sessions whose start prepares a workspace now, oldest first (the
    /// restart admission lists them).
    ///
    /// # Errors
    /// A database or JSON error.
    pub async fn provisioning_starts(&self) -> Result<Vec<SessionId>> {
        call(&self.inner.db, |conn| workspace_starts(conn, None)).await
    }
}
