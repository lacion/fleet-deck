//! The store reads of the usage recount (B20, design `usage-cost.md` §4.2
//! step 4). The decision reads them once, in the command transaction.

use fleetdeck_proto::SessionId;
use fleetdeck_proto::event::ProviderKind;
use rusqlite::{OptionalExtension, params};

use super::resumes::{BUSY, Purpose, exclusion};
use super::transcripts::{FirstEffect, first_effect};
use super::{Decision, Result};

/// What the decision of a recount reads from the database.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecountDb {
    /// The `BUSY` rule: the state is not `exited` or `lost`, a process of the
    /// session is live, or a start or resume effect is queued or claimed.
    pub busy: bool,
    /// A `session_transcripts` row exists (an import).
    pub imported: bool,
    /// The first `start` or `resume` effect of the session (§4.2 step 3.c).
    pub origin: FirstEffect,
    /// Another `sessions` row has the same `provider` and `native_session_id`.
    pub same_native: bool,
    /// The resume rule of the session's executions and of its conversation:
    /// the session's own shim rows, other sessions and launches of the
    /// conversation, and move holds.
    pub blocked: Option<String>,
    /// The shim ids of the rows that rest on a reboot proof.
    pub reboot_rows: Vec<String>,
}

fn kind_text(kind: ProviderKind) -> String {
    serde_json::to_value(kind)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default()
}

impl Decision<'_> {
    /// The facts of the recount of `session` (provider `kind`, conversation
    /// `native`).
    ///
    /// # Errors
    /// A database or JSON error.
    pub fn recount_db(
        &self,
        session: &SessionId,
        kind: ProviderKind,
        native: Option<&str>,
    ) -> Result<RecountDb> {
        let conn = self.connection();
        let busy: bool = conn
            .query_row(
                &format!("SELECT {BUSY} FROM sessions s WHERE s.session_id = ?1"),
                params![session.as_str()],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or(false);
        let imported: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM session_transcripts WHERE session_id = ?1)",
            params![session.as_str()],
            |r| r.get(0),
        )?;
        let same_native: bool = match native {
            Some(native) => conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM sessions
                   WHERE provider = ?1 AND native_session_id = ?2 AND session_id <> ?3)",
                params![kind_text(kind), native, session.as_str()],
                |r| r.get(0),
            )?,
            None => false,
        };
        let found = exclusion(
            conn,
            Some(session),
            kind,
            native,
            Some(session),
            Purpose::Resume,
        )?;
        Ok(RecountDb {
            busy,
            imported,
            origin: first_effect(conn, session)?,
            same_native,
            blocked: found.blocked,
            reboot_rows: found.reboot_rows.into_iter().map(|r| r.shim_id).collect(),
        })
    }
}
