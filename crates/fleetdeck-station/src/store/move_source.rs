//! The store helpers of the source side of a move (FG-9, design §12.3 steps 1
//! and 5, §12.4A): the push row of the source session stops when the session
//! moves and leaves `stopped` when the move is undone, and `session.move_abort`
//! releases the hold.

use fleetdeck_proto::SessionId;
use fleetdeck_proto::fleet_git::PushState;

use super::git_state::{push_row, save_push};
use super::move_runs::{self, MoveOutState};
use super::session_holds;
use super::{Decision, Result, Store, call};

/// The `stop_code` of a push row of a moved session (design §3.5).
pub const STOP_MOVED: &str = "moved";

impl Decision<'_> {
    /// Stops the push row of a session that moved: `stopped`, `stop_code =
    /// moved` (design §12.3 step 5). A session without a push row, or a row
    /// that is stopped for this reason already, changes nothing.
    ///
    /// # Errors
    /// A database error.
    pub(crate) fn move_stop_push(&mut self, session_id: &SessionId) -> Result<()> {
        self.with_log(|conn, now, events| {
            let Some(mut row) = push_row(conn, session_id)? else {
                return Ok(());
            };
            if row.state == PushState::Stopped && row.stop_code.as_deref() == Some(STOP_MOVED) {
                return Ok(());
            }
            row.state = PushState::Stopped;
            row.stop_code = Some(STOP_MOVED.to_owned());
            row.next_attempt_at = None;
            save_push(conn, &row, now, events)
        })
    }

    /// The push row of an undone move leaves `stopped` (design §12.4A step
    /// U2): `stop_code` is cleared, the state is `pushed` when `desired_oid` is
    /// empty or equals the lease, else `due`. A row that is not stopped for a
    /// move changes nothing.
    ///
    /// # Errors
    /// A database error.
    pub(crate) fn move_restore_push(&mut self, session_id: &SessionId) -> Result<()> {
        self.with_log(|conn, now, events| {
            let Some(mut row) = push_row(conn, session_id)? else {
                return Ok(());
            };
            if row.state != PushState::Stopped || row.stop_code.as_deref() != Some(STOP_MOVED) {
                return Ok(());
            }
            row.stop_code = None;
            if row.desired_oid.is_none() || row.desired_oid == row.last_pushed_oid {
                row.state = PushState::Pushed;
                row.next_attempt_at = None;
            } else {
                row.state = PushState::Due;
                row.next_attempt_at = Some(now);
            }
            save_push(conn, &row, now, events)
        })
    }
}

impl Store {
    /// `session.move_abort` on the source (design §12.3, "Abort"): a row in
    /// `holding` becomes `aborted` and its hold is deleted, in one
    /// transaction. Any other state, and an unknown id, change nothing.
    /// Returns whether the hold was released.
    ///
    /// # Errors
    /// A database error.
    pub async fn move_abort_out(&self, move_id: String) -> Result<bool> {
        let now = self.now();
        call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            let Some(row) = move_runs::get_out(&tx, &move_id)? else {
                return Ok(false);
            };
            let changed = move_runs::transition_out(
                &tx,
                &move_id,
                MoveOutState::Holding,
                MoveOutState::Aborted,
                now,
            )?;
            if changed {
                session_holds::delete(&tx, &row.session_id, &move_id)?;
            }
            tx.commit()?;
            Ok(changed)
        })
        .await
    }
}
