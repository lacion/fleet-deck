//! The cleanup rows of the station store (FG-3, design §6.3): the sessions
//! that the cleanup loop looks at, and the cleanup state of a session
//! (`session_git.cleanup_state`, `cleanup_at`) with its `session.cleanup`
//! event.
//!
//! `session_git.ended_at` is kept by the event log ([`super::log`]): the
//! latest change of the session state to `exited` or `lost`.

use fleetdeck_proto::fleet_git::CleanupState;
use fleetdeck_proto::{Event, SessionId};
use rusqlite::params;

use super::{NewEvent, Result, Store, call, log};

/// The push state of a session that the cleanup reads (design §6.3 item 7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CleanupPush {
    /// The text of the state: `idle`, `due`, `pushing`, `pushed`, ...
    pub state: String,
    pub last_pushed_oid: Option<String>,
    pub desired_oid: Option<String>,
}

/// A worktree session that the cleanup loop checks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CleanupRow {
    pub session_id: SessionId,
    pub project_id: String,
    /// The recorded local branch.
    pub branch: String,
    pub repo_root: String,
    pub created_branch: bool,
    /// The latest end of the session; `None` while it lives.
    pub ended_at: Option<i64>,
    /// `waiting`, `kept:<reason>` or `None`.
    pub cleanup_state: Option<String>,
    /// `None` for a session without a push row.
    pub push: Option<CleanupPush>,
}

/// The stored text of a cleanup state: `waiting`, `kept:<reason>`,
/// `trashed`.
fn stored_text(state: CleanupState, reason: Option<&str>) -> String {
    match state {
        CleanupState::Waiting => "waiting".to_owned(),
        CleanupState::Trashed => "trashed".to_owned(),
        CleanupState::Kept | CleanupState::Unknown => {
            format!("kept:{}", reason.unwrap_or_default())
        }
    }
}

impl Store {
    /// The sessions with `kind = worktree` and a recorded branch whose
    /// cleanup state is not `trashed`, with their push row (design §6.3).
    ///
    /// # Errors
    /// A database error.
    pub(crate) async fn git_cleanup_rows(&self) -> Result<Vec<CleanupRow>> {
        call(&self.inner.db, |conn| {
            let mut stmt = conn.prepare(
                "SELECT s.session_id, s.project_id, s.branch, s.repo_root, s.created_branch,
                        s.ended_at, s.cleanup_state,
                        p.state, p.last_pushed_oid, p.desired_oid
                 FROM session_git s LEFT JOIN git_pushes p USING (session_id)
                 WHERE s.kind = 'worktree' AND s.branch IS NOT NULL
                   AND (s.cleanup_state IS NULL OR s.cleanup_state != 'trashed')
                 ORDER BY s.session_id",
            )?;
            let rows = stmt.query_map([], |r| {
                let id: String = r.get(0)?;
                let created: i64 = r.get(4)?;
                let push_state: Option<String> = r.get(7)?;
                let last_pushed_oid: Option<String> = r.get(8)?;
                let desired_oid: Option<String> = r.get(9)?;
                Ok(CleanupRow {
                    session_id: SessionId::from(id),
                    project_id: r.get(1)?,
                    branch: r.get(2)?,
                    repo_root: r.get(3)?,
                    created_branch: created != 0,
                    ended_at: r.get(5)?,
                    cleanup_state: r.get(6)?,
                    push: push_state.map(|state| CleanupPush {
                        state,
                        last_pushed_oid,
                        desired_oid,
                    }),
                })
            })?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
                .map_err(Into::into)
        })
        .await
    }

    /// Records the cleanup state of a session (design §6.3). It writes
    /// `cleanup_state` and `cleanup_at`, and appends `session.cleanup`, only
    /// when the pair (state, reason) changed. A `waiting` row that stays
    /// `waiting` changes nothing, also with a new due time. `reason` of
    /// `waiting` is the due time in unix ms. Returns `true` when it wrote.
    /// A session without a `session_git` row is not touched.
    ///
    /// # Errors
    /// A database error.
    pub(crate) async fn git_cleanup_record(
        &self,
        session_id: SessionId,
        state: CleanupState,
        reason: Option<String>,
    ) -> Result<bool> {
        let now = self.now();
        let (wrote, events) = call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            let current: Option<Option<String>> = tx
                .query_row(
                    "SELECT cleanup_state FROM session_git WHERE session_id = ?1",
                    params![session_id.as_str()],
                    |r| r.get(0),
                )
                .map(Some)
                .or_else(|e| match e {
                    rusqlite::Error::QueryReturnedNoRows => Ok(None),
                    other => Err(other),
                })?;
            let Some(current) = current else {
                return Ok((false, Vec::new()));
            };
            let new = stored_text(state, reason.as_deref());
            if current.as_deref() == Some(new.as_str()) {
                return Ok((false, Vec::new()));
            }
            tx.execute(
                "UPDATE session_git SET cleanup_state = ?2, cleanup_at = ?3
                 WHERE session_id = ?1",
                params![session_id.as_str(), new, now],
            )?;
            let event = log::append_one(
                &tx,
                &session_id,
                NewEvent::from(Event::SessionCleanup { state, reason }),
                now,
            )?;
            tx.commit()?;
            Ok((true, vec![event]))
        })
        .await?;
        if let Some(last) = events.last() {
            self.publish(last.station_seq);
        }
        Ok(wrote)
    }
}

#[cfg(test)]
#[path = "git_cleanup_tests.rs"]
pub(crate) mod tests;
