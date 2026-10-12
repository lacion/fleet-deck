//! Worktree sharing (PLAN §6.6; DL-093). Sessions whose working directories
//! overlap a worktree (one path inside the other) share its workspace: a
//! revert of one of them needs every other one quiet, and no other one takes
//! input while the revert is open. The checks read only durable state
//! (`sessions`, their projections, `effects`, `reverts`), so they hold in
//! the command transaction.

use std::path::Path;

use fleetdeck_proto::event::SessionState;
use fleetdeck_proto::{SessionId, SessionSummary};
use rusqlite::Connection;

use super::reverts::{self, RevertRow};
use super::{Decision, Result, log};

/// True when one path is inside the other (whole components).
#[must_use]
pub fn overlaps(a: &str, b: &str) -> bool {
    let (a, b) = (Path::new(a), Path::new(b));
    a.starts_with(b) || b.starts_with(a)
}

/// Why a live session keeps the workspace busy, if it does.
fn busy(conn: &Connection, s: &SessionSummary) -> Result<Option<&'static str>> {
    Ok(if s.state != SessionState::Running {
        Some("it is starting or stopping")
    } else if s.current_turn.is_some() || s.current_run.is_some() {
        Some("a turn runs")
    } else if !s.open_requests.is_empty() {
        Some("a request is open")
    } else if !s.background_tasks.is_empty() {
        Some("background work runs")
    } else if reverts::pending_inputs(conn, &s.session_id)? > 0 {
        Some("an input is pending")
    } else if open_turns(conn, &s.session_id)? > 0 {
        // A natively queued input, a turn between its start and its
        // `turn.finalized`, or a completion capture (PLAN §6.6).
        Some("a turn or a queued input is not finalized")
    } else {
        None
    })
}

/// The session's turn checkpoint rows that are still open.
fn open_turns(conn: &Connection, session: &SessionId) -> Result<i64> {
    Ok(conn.query_row(
        "SELECT COUNT(*) FROM turn_checkpoints WHERE session_id = ?1 AND state = 'open'",
        [session.as_str()],
        |r| r.get(0),
    )?)
}

/// The live sessions whose directory overlaps `root`, except `except`.
fn sharing(conn: &Connection, root: &str, except: &SessionId) -> Result<Vec<SessionSummary>> {
    let mut stmt = conn.prepare_cached(
        "SELECT session_id, cwd FROM sessions WHERE state NOT IN ('exited', 'lost') \
         ORDER BY created_at, session_id",
    )?;
    let ids = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut out = Vec::new();
    for (id, cwd) in ids {
        let id = SessionId::from(id);
        if &id == except || !overlaps(&cwd, root) {
            continue;
        }
        if let Some(summary) = log::summary(conn, &id)? {
            out.push(summary);
        }
    }
    Ok(out)
}

/// The first other live session in `root` that keeps it busy, as a reason
/// text; `None` when every one is quiet.
fn conflict(conn: &Connection, root: &str, except: &SessionId) -> Result<Option<String>> {
    for s in sharing(conn, root, except)? {
        if let Some(why) = busy(conn, &s)? {
            return Ok(Some(format!(
                "session `{}` in the same worktree is busy: {why}",
                s.session_id
            )));
        }
    }
    Ok(None)
}

/// The open revert (not `succeeded` or `failed`) of any session whose
/// worktree overlaps `cwd`.
fn open_revert_over(conn: &Connection, cwd: &str) -> Result<Option<RevertRow>> {
    Ok(reverts::select(
        conn,
        "step NOT IN ('succeeded', 'failed', 'reconciled')",
        &[],
    )?
    .into_iter()
    .find(|r| overlaps(&r.revert.worktree_root, cwd)))
}

impl Decision<'_> {
    /// The first other live session in the worktree `root` that keeps it
    /// busy (a turn, a run, a request, background work, a pending input, an
    /// open turn checkpoint row (a natively queued input or a capture) or
    /// a start), as a reason text.
    ///
    /// # Errors
    /// A database error.
    pub fn worktree_conflict(&self, root: &str, except: &SessionId) -> Result<Option<String>> {
        conflict(self.connection(), root, except)
    }

    /// The open revert of any session whose worktree overlaps `cwd`.
    ///
    /// # Errors
    /// A database error.
    pub fn open_revert_over(&self, cwd: &str) -> Result<Option<RevertRow>> {
        open_revert_over(self.connection(), cwd)
    }
}

impl super::Store {
    /// [`Decision::worktree_conflict`] outside a command (the revert
    /// executor's recheck under the worktree fence).
    ///
    /// # Errors
    /// A database error.
    pub async fn worktree_conflict(
        &self,
        root: String,
        except: SessionId,
    ) -> Result<Option<String>> {
        super::call(&self.inner.db, move |conn| conflict(conn, &root, &except)).await
    }
}

#[cfg(test)]
mod tests {
    use super::overlaps;

    #[test]
    fn overlap_is_by_whole_components_in_both_directions() {
        assert!(overlaps("/w/app", "/w/app"));
        assert!(overlaps("/w/app/sub", "/w/app"));
        assert!(overlaps("/w", "/w/app"));
        assert!(!overlaps("/w/app2", "/w/app"));
        assert!(!overlaps("/w/other", "/w/app"));
    }
}
