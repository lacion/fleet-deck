//! The durable rows of the worktree trash (DL-150).
//!
//! - **The row is the custody.** A row that is not `restored`, `purged` or
//!   `released` holds the original folder and the trash folder against any
//!   other use ([`open_over`]); a partial unique index admits one open row
//!   per session. The decision of a trash command inserts the row and the
//!   effect in one transaction.
//! - **The claim guard.** Every effect run starts with [`Store::worktree_op_claim`]:
//!   it reads the row and continues only when the row is in the state that
//!   the action expects. A queued effect whose row moved on (a release, a
//!   restart re-run) ends `superseded` and changes nothing. Every later
//!   state change of a run is a compare-and-set on the same state.
//! - **Release only from `failed`.** A `failed` row never has a queued or
//!   claimed effect, so a release cannot race a run.

use fleetdeck_proto::SessionId;
use rusqlite::{Connection, params};

use super::effects::{self, EffectPayload, WorktreeAction};
use super::sharing::overlaps;
use super::{Decision, Result, Store, StoreError, call};

/// The states of a row (DL-150).
pub const TRASHING: &str = "trashing";
pub const TRASHED: &str = "trashed";
pub const RESTORING: &str = "restoring";
pub const RESTORED: &str = "restored";
pub const PURGING: &str = "purging";
pub const PURGED: &str = "purged";
pub const FAILED: &str = "failed";
pub const RELEASED: &str = "released";

/// The phases of a `failed` row.
pub const PHASE_TRASH: &str = "trash";
pub const PHASE_RESTORE: &str = "restore";
pub const PHASE_PURGE: &str = "purge";

/// The `WHERE` rule of an open row.
const OPEN: &str = "state NOT IN ('restored', 'purged', 'released')";

/// One row of `worktree_ops`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeOp {
    /// The command id of the trash command.
    pub op_id: String,
    pub session_id: SessionId,
    /// The main checkout; empty until the first effect step read it.
    pub main: String,
    /// The Git admin directory of the worktree; empty like `main`.
    pub admin_dir: String,
    pub original_path: String,
    pub trash_path: String,
    pub dev: u64,
    pub ino: u64,
    pub branch: Option<String>,
    pub state: String,
    pub phase: Option<String>,
    pub message: Option<String>,
    pub trashed_at: i64,
    pub updated_at: i64,
    /// The branch ref that the cleanup proved (`refs/heads/<branch>`); `None`
    /// for a board trash (FG-3, design §6.3).
    pub expect_branch: Option<String>,
    /// The tip that the cleanup proved; `None` for a board trash.
    pub expect_oid: Option<String>,
}

/// The facts that the trash decision records.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewWorktreeOp {
    pub op_id: String,
    pub session_id: SessionId,
    pub original_path: String,
    pub trash_path: String,
    pub dev: u64,
    pub ino: u64,
    /// `(refs/heads/<branch>, tip)` that the trash effect checks again right
    /// before the move (FG-3); `None` for a board trash.
    pub expect: Option<(String, String)>,
}

const COLUMNS: &str = "op_id, session_id, main, admin_dir, original_path, trash_path, dev, ino, \
     branch, state, phase, message, trashed_at, updated_at, expect_branch, expect_oid";

/// An inode or device number goes into SQLite as the `i64` with the same
/// bits.
fn to_db(v: u64) -> i64 {
    i64::from_ne_bytes(v.to_ne_bytes())
}

fn from_db(v: i64) -> u64 {
    u64::from_ne_bytes(v.to_ne_bytes())
}

fn decode(r: &rusqlite::Row<'_>) -> rusqlite::Result<WorktreeOp> {
    Ok(WorktreeOp {
        op_id: r.get(0)?,
        session_id: SessionId::from(r.get::<_, String>(1)?),
        main: r.get(2)?,
        admin_dir: r.get(3)?,
        original_path: r.get(4)?,
        trash_path: r.get(5)?,
        dev: from_db(r.get(6)?),
        ino: from_db(r.get(7)?),
        branch: r.get(8)?,
        state: r.get(9)?,
        phase: r.get(10)?,
        message: r.get(11)?,
        trashed_at: r.get(12)?,
        updated_at: r.get(13)?,
        expect_branch: r.get(14)?,
        expect_oid: r.get(15)?,
    })
}

fn select(conn: &Connection, filter: &str, args: impl rusqlite::Params) -> Result<Vec<WorktreeOp>> {
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {COLUMNS} FROM worktree_ops WHERE {filter} ORDER BY trashed_at, op_id"
    ))?;
    let rows = stmt
        .query_map(args, decode)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

fn get(conn: &Connection, op_id: &str) -> Result<Option<WorktreeOp>> {
    Ok(select(conn, "op_id = ?1", [op_id])?.pop())
}

/// The open row of a session, if any.
fn open_of(conn: &Connection, session: &SessionId) -> Result<Option<WorktreeOp>> {
    Ok(select(
        conn,
        &format!("session_id = ?1 AND {OPEN}"),
        [session.as_str()],
    )?
    .pop())
}

/// The open row whose original or trash path overlaps `path`.
pub(super) fn open_over(conn: &Connection, path: &str) -> Result<Option<WorktreeOp>> {
    Ok(select(conn, OPEN, [])?
        .into_iter()
        .find(|r| overlaps(&r.original_path, path) || overlaps(&r.trash_path, path)))
}

/// Moves a row from `from` to `to`. `false` when the row is in another
/// state (nothing changes).
fn transition(
    conn: &Connection,
    op_id: &str,
    from: &str,
    to: &str,
    phase: Option<&str>,
    message: Option<&str>,
    now: i64,
) -> Result<bool> {
    let n = conn.execute(
        "UPDATE worktree_ops SET state = ?3, phase = ?4, message = ?5, updated_at = ?6
         WHERE op_id = ?1 AND state = ?2",
        params![op_id, from, to, phase, message, now],
    )?;
    Ok(n == 1)
}

impl Decision<'_> {
    /// The command id of this decision (the id of a new trash operation).
    #[must_use]
    pub fn operation_id(&self) -> String {
        self.command_id().as_str().to_owned()
    }

    /// The open trash operation of a session.
    ///
    /// # Errors
    /// A database error.
    pub fn open_worktree_op(&self, session: &SessionId) -> Result<Option<WorktreeOp>> {
        open_of(self.connection(), session)
    }

    /// The open trash operation whose original or trash folder overlaps
    /// `path` (the custody hold).
    ///
    /// # Errors
    /// A database error.
    pub fn open_worktree_op_over(&self, path: &str) -> Result<Option<WorktreeOp>> {
        open_over(self.connection(), path)
    }

    /// The number of the session's effects that are queued or claimed.
    ///
    /// # Errors
    /// A database error.
    pub fn unsettled_effects(&self, session: &SessionId) -> Result<u64> {
        let n: i64 = self.connection().query_row(
            "SELECT COUNT(*) FROM effects WHERE session_id = ?1
               AND state IN ('queued', 'claimed')",
            [session.as_str()],
            |r| r.get(0),
        )?;
        Ok(u64::try_from(n).unwrap_or(0))
    }

    /// Stores a new operation in the state `trashing`.
    ///
    /// # Errors
    /// A database error (the unique index refuses a second open row of the
    /// session).
    pub fn insert_worktree_op(&mut self, new: &NewWorktreeOp) -> Result<()> {
        self.connection().execute(
            "INSERT INTO worktree_ops (op_id, session_id, main, admin_dir, original_path,
                 trash_path, dev, ino, branch, state, phase, message, trashed_at, updated_at,
                 expect_branch, expect_oid)
             VALUES (?1, ?2, '', '', ?3, ?4, ?5, ?6, NULL, 'trashing', NULL, NULL, ?7, ?7, ?8, ?9)",
            params![
                new.op_id,
                new.session_id.as_str(),
                new.original_path,
                new.trash_path,
                to_db(new.dev),
                to_db(new.ino),
                self.now(),
                new.expect.as_ref().map(|(branch, _)| branch),
                new.expect.as_ref().map(|(_, oid)| oid),
            ],
        )?;
        Ok(())
    }

    /// Moves an operation from `from` to `to`; `false` when it is in
    /// another state.
    ///
    /// # Errors
    /// A database error.
    pub fn move_worktree_op(&mut self, op_id: &str, from: &str, to: &str) -> Result<bool> {
        transition(self.connection(), op_id, from, to, None, None, self.now())
    }

    /// Queues the effect of an operation.
    ///
    /// # Errors
    /// A database error.
    pub fn enqueue_worktree(
        &mut self,
        session: &SessionId,
        op_id: &str,
        action: WorktreeAction,
    ) -> Result<()> {
        self.enqueue(
            session,
            EffectPayload::Worktree {
                op_id: op_id.to_owned(),
                action,
            },
        )?;
        Ok(())
    }
}

impl Store {
    /// One operation.
    ///
    /// # Errors
    /// A database error.
    pub async fn worktree_op(&self, op_id: String) -> Result<Option<WorktreeOp>> {
        call(&self.inner.db, move |conn| get(conn, &op_id)).await
    }

    /// Every open operation (any state but `restored`, `purged` and
    /// `released`), oldest first.
    ///
    /// # Errors
    /// A database error.
    pub async fn worktree_ops_open(&self) -> Result<Vec<WorktreeOp>> {
        call(&self.inner.db, move |conn| select(conn, OPEN, [])).await
    }

    /// The open operation whose original or trash folder overlaps `path`
    /// (the custody hold), outside a command.
    ///
    /// # Errors
    /// A database error.
    pub async fn worktree_op_over(&self, path: String) -> Result<Option<WorktreeOp>> {
        call(&self.inner.db, move |conn| open_over(conn, &path)).await
    }

    /// The claim guard: the row, when it is in the state `expected`;
    /// otherwise `None` (the effect is superseded).
    ///
    /// # Errors
    /// A database error.
    pub async fn worktree_op_claim(
        &self,
        op_id: String,
        expected: &'static str,
    ) -> Result<Option<WorktreeOp>> {
        call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            let row = get(&tx, &op_id)?.filter(|r| r.state == expected);
            tx.commit()?;
            Ok(row)
        })
        .await
    }

    /// Records what the first trash step read from Git; only while the row
    /// is `trashing`. `false` when the row is in another state.
    ///
    /// # Errors
    /// A database error.
    pub async fn worktree_op_set_git(
        &self,
        op_id: String,
        main: String,
        admin_dir: String,
        branch: Option<String>,
    ) -> Result<bool> {
        let now = self.now();
        call(&self.inner.db, move |conn| {
            let n = conn.execute(
                "UPDATE worktree_ops SET main = ?2, admin_dir = ?3, branch = ?4, updated_at = ?5
                 WHERE op_id = ?1 AND state = 'trashing'",
                params![op_id, main, admin_dir, branch, now],
            )?;
            Ok(n == 1)
        })
        .await
    }

    /// Compare-and-set of a run: `from` to `to` (a final state of the run).
    /// `false` when the row is in another state.
    ///
    /// # Errors
    /// A database error.
    pub async fn worktree_op_finish(
        &self,
        op_id: String,
        from: &'static str,
        to: &'static str,
    ) -> Result<bool> {
        let now = self.now();
        call(&self.inner.db, move |conn| {
            transition(conn, &op_id, from, to, None, None, now)
        })
        .await
    }

    /// Ends a trash run without a move because the worktree changed after the
    /// cleanup checks: `trashing` to `released` with `message` (FG-3, design
    /// §6.3). `false` when the row is in another state.
    ///
    /// # Errors
    /// A database error.
    pub async fn worktree_op_release_changed(
        &self,
        op_id: String,
        message: &'static str,
    ) -> Result<bool> {
        let now = self.now();
        call(&self.inner.db, move |conn| {
            transition(conn, &op_id, TRASHING, RELEASED, None, Some(message), now)
        })
        .await
    }

    /// Ends a run with `failed`, the phase and the message.
    ///
    /// # Errors
    /// A database error.
    pub async fn worktree_op_fail(
        &self,
        op_id: String,
        from: &'static str,
        phase: &'static str,
        message: String,
    ) -> Result<bool> {
        let now = self.now();
        call(&self.inner.db, move |conn| {
            transition(conn, &op_id, from, FAILED, Some(phase), Some(&message), now)
        })
        .await
    }

    /// Station start: one new effect for every row that a restart left in
    /// `trashing`, `restoring` or `purging` (the effect of the earlier
    /// process is retired or unknown). Returns how many it queued.
    ///
    /// # Errors
    /// A database error.
    pub async fn requeue_worktree_ops(&self) -> Result<usize> {
        let now = self.now();
        let ids = std::sync::Arc::clone(&self.inner.ids);
        let queued = call(&self.inner.db, move |conn| {
            let tx = conn.transaction()?;
            let rows = select(&tx, "state IN ('trashing', 'restoring', 'purging')", [])?;
            for row in &rows {
                let action = match row.state.as_str() {
                    TRASHING => WorktreeAction::Trash,
                    RESTORING => WorktreeAction::Restore,
                    PURGING => WorktreeAction::Purge,
                    other => return Err(StoreError::Corrupt(format!("worktree op state {other}"))),
                };
                effects::enqueue(
                    &tx,
                    ids.as_ref(),
                    now,
                    None,
                    &row.session_id,
                    EffectPayload::Worktree {
                        op_id: row.op_id.clone(),
                        action,
                    },
                )?;
            }
            tx.commit()?;
            Ok(rows.len())
        })
        .await?;
        if queued > 0 {
            self.wake_worker();
        }
        Ok(queued)
    }
}

/// The state that the effect of `action` expects.
#[must_use]
pub fn expected_state(action: WorktreeAction) -> &'static str {
    match action {
        WorktreeAction::Trash => TRASHING,
        WorktreeAction::Restore => RESTORING,
        WorktreeAction::Purge => PURGING,
    }
}

impl WorktreeOp {
    /// True while the row holds its folders.
    #[must_use]
    pub fn is_open(&self) -> bool {
        !matches!(self.state.as_str(), RESTORED | PURGED | RELEASED)
    }
}
