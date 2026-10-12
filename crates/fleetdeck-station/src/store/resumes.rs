//! Decision-time facts for `session.resume` (DL-118): the stored native
//! resume identity and the evidence that the previous execution ended.

use fleetdeck_proto::SessionId;
use rusqlite::OptionalExtension as _;

use super::Result;
use super::receipts::Decision;

/// The only shim row outcomes that prove that the execution ended
/// (ADR-005 §4.6, §6; DL-119). Every other row — open, or `lost` for any
/// reason (`execution_unknown`, `attach_refused`, `shim_incompatible`,
/// `identity_mismatch`, an unsupported or corrupt adapter checkpoint, no
/// driver, or a reason this build does not know) — fails closed: the shim
/// may still drive the provider, so a resume could run a second execution
/// of one conversation.
///
/// - `never_started`: an intact launch journal proves no spawn.
/// - `exited` with reason [`PROVEN_EXIT`]: the shim reported the provider's
///   exit with no remaining member of its process group or session.
pub const PROVEN_EXIT: &str = "exited";

impl Decision<'_> {
    /// Stores the native session id that `session.import` continues
    /// (DL-308); the provider confirms it in `session.started`.
    ///
    /// # Errors
    /// A database error.
    pub fn set_native_session_id(&mut self, session: &SessionId, native: &str) -> Result<()> {
        self.with_log(|conn, _, _| {
            conn.execute(
                "UPDATE sessions SET native_session_id = ?2 WHERE session_id = ?1",
                [session.as_str(), native],
            )?;
            Ok(())
        })
    }

    /// A session of this station that has not ended and continues the
    /// native session `native`: a second import of it would run two
    /// processes on one conversation.
    ///
    /// # Errors
    /// A database error.
    pub fn native_session_in_use(&self, native: &str) -> Result<Option<SessionId>> {
        Ok(self
            .connection()
            .query_row(
                "SELECT session_id FROM sessions WHERE native_session_id = ?1 \
                 AND state NOT IN ('exited', 'lost') LIMIT 1",
                [native],
                |r| r.get::<_, String>(0),
            )
            .optional()?
            .map(SessionId::from))
    }

    /// A session of this station that a terminal holds (`summary.terminal`)
    /// and that continues the native session `native` (DL-140): a resume,
    /// a handover or an import of it would run a second process on one
    /// conversation.
    ///
    /// # Errors
    /// A database error.
    pub fn native_session_held(&self, native: &str) -> Result<Option<SessionId>> {
        Ok(self
            .connection()
            .query_row(
                "SELECT session_id FROM sessions WHERE native_session_id = ?1 \
                 AND json_extract(summary, '$.terminal') IS NOT NULL LIMIT 1",
                [native],
                |r| r.get::<_, String>(0),
            )
            .optional()?
            .map(SessionId::from))
    }

    /// The native resume identity that the provider reported in
    /// `session.started` (Claude `session_id`, Codex `thread_id`).
    ///
    /// # Errors
    /// A database error.
    pub fn native_session_id(&self, session: &SessionId) -> Result<Option<String>> {
        Ok(self
            .connection()
            .query_row(
                "SELECT native_session_id FROM sessions WHERE session_id = ?1",
                [session.as_str()],
                |r| r.get::<_, Option<String>>(0),
            )
            .optional()?
            .flatten())
    }

    /// Why the previous execution of `session` may still run: any shim
    /// row of the session that does not prove its end (see
    /// [`PROVEN_EXIT`]). `None` when every recorded shim execution is
    /// proven ended (a direct-host process ends with the station).
    ///
    /// # Errors
    /// A database error.
    pub fn unended_execution(&self, session: &SessionId) -> Result<Option<String>> {
        unended_execution(self.connection(), session)
    }
}

/// See [`Decision::unended_execution`].
///
/// # Errors
/// A database error.
pub(super) fn unended_execution(
    conn: &rusqlite::Connection,
    session: &SessionId,
) -> Result<Option<String>> {
    Ok(conn
        .query_row(
            "SELECT shim_id, state, COALESCE(reason, '') FROM shims
                 WHERE session_id = ?1
                   AND NOT (state = 'never_started'
                            OR (state = 'exited' AND reason = ?2))
                 ORDER BY created_at, shim_id LIMIT 1",
            rusqlite::params![session.as_str(), PROVEN_EXIT],
            |r| {
                Ok(format!(
                    "shim {} is {} ({})",
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?
                ))
            },
        )
        .optional()?)
}
