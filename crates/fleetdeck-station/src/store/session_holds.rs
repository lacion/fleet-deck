//! The session holds of Fleet Git (FG-8, design §11.4, §3.5 V30): at most one
//! durable hold for each session. A land holds its session from its decision
//! until its run ends; a move (phase 4) uses the same table. While a hold
//! exists, the station refuses or skips every input that could change the
//! session or its branch.

use fleetdeck_proto::{Reject, RejectCode, SessionId};
use rusqlite::{Connection, OptionalExtension, params};

use super::{DecideError, Result, Store, call};

/// The hold of a land (`holder_id` is the land id).
pub const KIND_LAND: &str = "land";
/// The hold of the source of a move (phase 4).
pub const KIND_MOVE: &str = "move";
/// The hold of the target of a move (phase 4).
pub const KIND_MOVE_IN: &str = "move_in";

/// The refusal text of a held session (design §11.4).
pub const HELD: &str = "A land holds this session. Wait until it ends.";

/// One row of `session_holds`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionHold {
    pub session_id: SessionId,
    /// `land`, `move` or `move_in`.
    pub kind: String,
    /// The land id or the move id.
    pub holder_id: String,
    pub created_at: i64,
}

/// The hold of `session_id`, if any.
///
/// # Errors
/// A database error.
pub fn get(conn: &Connection, session_id: &SessionId) -> Result<Option<SessionHold>> {
    Ok(conn
        .query_row(
            "SELECT session_id, kind, holder_id, created_at FROM session_holds
             WHERE session_id = ?1",
            params![session_id.as_str()],
            |r| {
                Ok(SessionHold {
                    session_id: SessionId::from(r.get::<_, String>(0)?),
                    kind: r.get(1)?,
                    holder_id: r.get(2)?,
                    created_at: r.get(3)?,
                })
            },
        )
        .optional()?)
}

/// Is a hold on `session_id`?
///
/// # Errors
/// A database error.
pub fn held(conn: &Connection, session_id: &SessionId) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM session_holds WHERE session_id = ?1)",
        params![session_id.as_str()],
        |r| r.get(0),
    )?)
}

/// The decision check of a command that a hold refuses (design §11.4): a
/// stored `invalid_state` rejection with [`HELD`].
///
/// # Errors
/// The rejection, or a database error.
pub fn check_free(conn: &Connection, session_id: &SessionId) -> Result<(), DecideError> {
    if held(conn, session_id)? {
        return Err(DecideError::Reject(Reject::new(
            RejectCode::InvalidState,
            HELD,
        )));
    }
    Ok(())
}

/// Inserts the hold of a session. The primary key refuses a second hold.
///
/// # Errors
/// A database error (a constraint error when the session is held already).
pub fn insert(
    conn: &Connection,
    session_id: &SessionId,
    kind: &str,
    holder_id: &str,
    now: i64,
) -> Result<()> {
    conn.execute(
        "INSERT INTO session_holds (session_id, kind, holder_id, created_at)
         VALUES (?1, ?2, ?3, ?4)",
        params![session_id.as_str(), kind, holder_id, now],
    )?;
    Ok(())
}

/// Deletes the hold of `holder_id` on `session_id`. A hold of another holder
/// stays. Returns whether a row was deleted.
///
/// # Errors
/// A database error.
pub fn delete(conn: &Connection, session_id: &SessionId, holder_id: &str) -> Result<bool> {
    Ok(conn.execute(
        "DELETE FROM session_holds WHERE session_id = ?1 AND holder_id = ?2",
        params![session_id.as_str(), holder_id],
    )? > 0)
}

impl Store {
    /// Inserts the hold of `holder_id` on a session.
    ///
    /// # Errors
    /// A database error (a constraint error when the session is held).
    pub async fn session_hold_insert(
        &self,
        session_id: SessionId,
        kind: &'static str,
        holder_id: String,
    ) -> Result<()> {
        let now = self.now();
        call(&self.inner.db, move |conn| {
            insert(conn, &session_id, kind, &holder_id, now)
        })
        .await
    }

    /// Deletes the hold of `holder_id` on a session; true when it existed.
    ///
    /// # Errors
    /// A database error.
    pub async fn session_hold_delete(
        &self,
        session_id: SessionId,
        holder_id: String,
    ) -> Result<bool> {
        call(&self.inner.db, move |conn| {
            delete(conn, &session_id, &holder_id)
        })
        .await
    }

    /// The hold of a session, if any.
    ///
    /// # Errors
    /// A database error.
    pub async fn session_hold(&self, session_id: SessionId) -> Result<Option<SessionHold>> {
        call(&self.inner.db, move |conn| get(conn, &session_id)).await
    }
}

#[cfg(test)]
#[path = "session_holds_tests.rs"]
mod tests;
