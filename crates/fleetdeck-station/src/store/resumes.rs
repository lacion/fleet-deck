//! Decision-time facts for `session.resume` (DL-118): the stored native
//! resume identity and the evidence that the previous execution ended.

use fleetdeck_proto::SessionId;
use rusqlite::OptionalExtension as _;

use super::Result;
use super::receipts::Decision;

/// Shim loss reasons that do not prove that the previous provider
/// execution ended (ADR-005 §4.6, §6): a resume could run two executions of
/// one conversation.
pub const UNPROVEN_SHIM_LOSS: &[&str] = &[
    "execution_unknown",
    "attach_refused",
    "shim_incompatible",
    "identity_mismatch",
];

impl Decision<'_> {
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

    /// Why the previous execution of `session` may still run: an open shim
    /// row, or a shim row that ended `lost` without proof of termination.
    /// `None` when every recorded execution is proven ended.
    ///
    /// # Errors
    /// A database error.
    pub fn unended_execution(&self, session: &SessionId) -> Result<Option<String>> {
        let conn = self.connection();
        let open: Option<String> = conn
            .query_row(
                "SELECT shim_id FROM shims
                 WHERE session_id = ?1 AND state IN ('starting', 'running') LIMIT 1",
                [session.as_str()],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(shim) = open {
            return Ok(Some(format!("shim {shim} is still open")));
        }
        let mut stmt = conn.prepare(
            "SELECT reason FROM shims WHERE session_id = ?1 AND state = 'lost'
             AND reason IS NOT NULL",
        )?;
        let reasons: Vec<String> = stmt
            .query_map([session.as_str()], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        Ok(reasons.into_iter().find(|reason| {
            UNPROVEN_SHIM_LOSS
                .iter()
                .any(|unproven| reason.starts_with(unproven))
        }))
    }
}
