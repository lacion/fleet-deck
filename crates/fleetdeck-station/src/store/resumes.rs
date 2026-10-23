//! Decision-time facts for `session.resume` (DL-118): the stored native
//! resume identity and the evidence that the previous execution ended.
//!
//! What proves that a shim execution ended (DL-182, design `shim-resume.md`
//! §1a and §4; [`proves_end`]):
//!
//! - `never_started`: an intact launch journal proves no spawn.
//! - `exited` with reason [`PROVEN_EXIT`] and `inspection_version >= 1`:
//!   the shim saw the provider exit through `waitpid` and found no member of
//!   its process group or session by a complete inspection. An `exited`
//!   row without the inspection version (an older shim) is `exit_unverified`.
//! - a `proof` next to the row: [`PROOF_REBOOT`] (revalidated at dispatch)
//!   or [`PROOF_OWNER`].
//!
//! Every other row — open, or `lost` for any reason (`execution_unknown`,
//! `attach_refused`, `shim_incompatible`, `identity_mismatch`, an
//! unsupported or corrupt adapter checkpoint, no driver, or a reason this
//! build does not know) — fails closed: the shim may still drive the
//! provider, so a resume could run a second execution of one conversation.

use fleetdeck_proto::SessionId;
use fleetdeck_proto::event::ProviderKind;
use rusqlite::{Connection, OptionalExtension as _};

use super::Result;
use super::receipts::Decision;
use super::session_holds;
use super::shims::{ShimRow, ShimState, row as shim_row};

/// The `exited` reason of a clean exit with a verified zero count.
pub const PROVEN_EXIT: &str = "exited";
/// The proof of a verified reboot (DL-182 §4.1).
pub const PROOF_REBOOT: &str = "ended_by_reboot";
/// The proof of an audited owner reconciliation (DL-182 §4.2).
pub const PROOF_OWNER: &str = "reconciled_by_owner";
/// The `exited` reason of a report without a complete inspection.
pub const EXIT_UNVERIFIED: &str = "exit_unverified";

/// True when the clean exit of `row` is a complete, verified one.
#[must_use]
pub fn clean_exit(row: &ShimRow) -> bool {
    row.state == ShimState::Exited
        && row.reason.as_deref() == Some(PROVEN_EXIT)
        && row.inspection_version >= 1
}

/// True when `row` proves that its execution ended (see the module
/// documentation). A reboot proof still needs its revalidation at
/// dispatch.
#[must_use]
pub fn proves_end(row: &ShimRow) -> bool {
    match row.state {
        ShimState::NeverStarted => true,
        ShimState::Exited | ShimState::Lost => row.proof.is_some() || clean_exit(row),
        ShimState::Starting | ShimState::Running => false,
    }
}

/// The reason of `row` as a person reads it: a legacy clean exit without
/// the inspection version is `exit_unverified`.
#[must_use]
pub fn effective_reason(row: &ShimRow) -> String {
    match (row.state, row.reason.as_deref()) {
        (ShimState::Exited, Some(PROVEN_EXIT)) if row.inspection_version < 1 => {
            EXIT_UNVERIFIED.to_owned()
        }
        (_, reason) => reason.unwrap_or_default().to_owned(),
    }
}

/// `shim <id> is <state> (<reason>)`.
#[must_use]
pub fn describe(row: &ShimRow) -> String {
    format!(
        "shim {} is {} ({})",
        row.shim_id,
        row.state.as_str(),
        effective_reason(row)
    )
}

/// The wire name of a provider kind (the `sessions.provider` text).
fn kind_text(kind: ProviderKind) -> String {
    serde_json::to_value(kind)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default()
}

/// Every shim row of `session`, oldest first.
fn session_rows(conn: &Connection, session: &SessionId) -> Result<Vec<ShimRow>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {} FROM shims WHERE session_id = ?1 ORDER BY created_at, shim_id",
        super::shims::COLUMNS
    ))?;
    let raws = stmt
        .query_map([session.as_str()], shim_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    raws.into_iter().map(|r| r.decode()).collect()
}

/// What a new execution of a native conversation is for. An import names a
/// conversation that this station may have started without ever learning
/// its id, so it fails closed for unresolved runs with an unknown id; a
/// resume continues a conversation whose id the session stored itself
/// (design `shim-resume.md` §13, review fix 1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Purpose {
    Resume,
    Import,
}

/// What stands between a native conversation and a new execution of it
/// (DL-182 §3, rule of Q7).
#[derive(Debug, Default)]
pub struct Exclusion {
    /// The first thing that blocks, in words.
    pub blocked: Option<String>,
    /// The rows that count as proven only through a reboot proof; the
    /// dispatch revalidates them (§4.1).
    pub reboot_rows: Vec<ShimRow>,
}

impl Exclusion {
    fn note(&mut self, row: ShimRow, because: Option<&str>) {
        if !proves_end(&row) {
            if self.blocked.is_none() {
                self.blocked = Some(match because {
                    Some(who) => format!("{who}: {}", describe(&row)),
                    None => describe(&row),
                });
            }
        } else if row.proof.as_deref() == Some(PROOF_REBOOT) && !clean_exit(&row) {
            self.reboot_rows.push(row);
        }
    }

    fn block(&mut self, why: String) {
        if self.blocked.is_none() {
            self.blocked = Some(why);
        }
    }

    /// Notes every shim row of `session` (its own executions).
    fn note_session(&mut self, conn: &Connection, session: &SessionId) -> Result<()> {
        for row in session_rows(conn, session)? {
            self.note(row, None);
        }
        Ok(())
    }

    /// Blocks when a move reserves the conversation of `holder` or took it
    /// away (FG-9 property 1): the holder is the source of a move, the
    /// target that still receives it, or a source that moved and was not
    /// undone. An import or a resume of the same native id must not start
    /// a second writer.
    fn note_move(&mut self, conn: &Connection, holder: &SessionId) -> Result<()> {
        match session_holds::get(conn, holder)?.map(|h| h.kind) {
            Some(kind) if kind == session_holds::KIND_MOVE => {
                self.block(format!(
                    "session `{holder}` is being moved to another station"
                ));
            }
            Some(kind) if kind == session_holds::KIND_MOVE_IN => {
                self.block(format!(
                    "session `{holder}` is still arriving from another station"
                ));
            }
            _ => {}
        }
        if session_holds::moved_text(conn, holder)?.is_some() {
            self.block(format!(
                "session `{holder}` moved this conversation to another station"
            ));
        }
        Ok(())
    }
}

/// True (as a SQL value) when session `s` has not ended: its state is not
/// final, a process of it is live, or a start or resume effect of it is
/// not settled.
pub(super) const BUSY: &str = "(s.state NOT IN ('exited', 'lost')
                 OR EXISTS (SELECT 1 FROM processes p
                            WHERE p.session_id = s.session_id AND p.state = 'live')
                 OR EXISTS (SELECT 1 FROM effects e
                            WHERE e.session_id = s.session_id
                              AND e.kind IN ('start', 'resume')
                              AND e.state IN ('queued', 'claimed')))";

/// The rows of `session` and of every other execution of the native
/// conversation `native` of provider `kind` (any provider instance, any
/// host): the rule is
///
/// - (a) a session of the same kind whose stored native id is `native`
///   blocks while it is live, starting, holds an unsettled start or resume
///   effect, or has a shim row without proof;
/// - (b) a shim row of the same kind whose `native_target` is `native`
///   blocks while it has no proof. A row without `native_target` is a new
///   conversation: it cannot be `native` and blocks nothing here;
/// - (c) for [`Purpose::Import`] only: a session of the same kind whose
///   native id is still unknown may hold any conversation. It blocks while
///   it is busy (the same test as (a)), and each of its shim rows without
///   proof blocks.
///
/// `except` is the session that continues the conversation (its own rows
/// are the business of [`unended_execution`]).
pub(super) fn exclusion(
    conn: &Connection,
    own: Option<&SessionId>,
    kind: ProviderKind,
    native: Option<&str>,
    except: Option<&SessionId>,
    purpose: Purpose,
) -> Result<Exclusion> {
    let mut out = Exclusion::default();
    if let Some(own) = own {
        out.note_session(conn, own)?;
    }
    let Some(native) = native else {
        return Ok(out);
    };
    let kind = kind_text(kind);
    let except = except.map_or("", SessionId::as_str);
    // (a) the sessions that hold the conversation.
    let mut stmt = conn.prepare(&format!(
        "SELECT s.session_id, {BUSY}
         FROM sessions s
         WHERE s.provider = ?1 AND s.native_session_id = ?2 AND s.session_id <> ?3
         ORDER BY s.created_at, s.session_id"
    ))?;
    let holders = stmt
        .query_map(rusqlite::params![kind, native, except], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, bool>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (holder, busy) in holders {
        let holder = SessionId::from(holder);
        out.note_move(conn, &holder)?;
        if busy {
            out.block(format!("session `{holder}` still runs this conversation"));
        }
        for row in session_rows(conn, &holder)? {
            out.note(row, Some(&format!("session `{holder}`")));
        }
    }
    // (b) the launches that name the conversation.
    let mut stmt = conn.prepare(&format!(
        "SELECT {} FROM shims h JOIN sessions s ON s.session_id = h.session_id
         WHERE s.provider = ?1 AND h.native_target = ?2 AND h.session_id <> ?3
         ORDER BY h.created_at, h.shim_id",
        super::shims::COLUMNS
            .split(',')
            .map(|c| format!("h.{}", c.trim()))
            .collect::<Vec<_>>()
            .join(", ")
    ))?;
    let rows = stmt
        .query_map(rusqlite::params![kind, native, except], shim_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for raw in rows {
        let row = raw.decode()?;
        let who = format!("session `{}`", row.session_id);
        out.note(row, Some(&who));
    }
    // (c) the runs whose conversation id is not known (an import only).
    if purpose == Purpose::Import {
        let mut stmt = conn.prepare(&format!(
            "SELECT s.session_id, {BUSY}
             FROM sessions s
             WHERE s.provider = ?1 AND s.native_session_id IS NULL AND s.session_id <> ?2
             ORDER BY s.created_at, s.session_id"
        ))?;
        let unknown = stmt
            .query_map(rusqlite::params![kind, except], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, bool>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for (other, busy) in unknown {
            let other = SessionId::from(other);
            if busy {
                out.block(format!(
                    "session `{other}` has a run whose conversation id is not known yet; \
                     an import waits until that session reports its id or its run is proven ended"
                ));
            }
            for row in session_rows(conn, &other)? {
                out.note(row, Some(&format!("session `{other}`")));
            }
        }
    }
    Ok(out)
}

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

    /// The session that the terminal of shell `shell_id` holds
    /// (`summary.terminal.shell_id`, DL-140): a new shell with that id would
    /// make the hold's recovery read the wrong handover.
    ///
    /// # Errors
    /// A database error.
    pub fn shell_holds(&self, shell_id: &str) -> Result<Option<SessionId>> {
        Ok(self
            .connection()
            .query_row(
                "SELECT session_id FROM sessions \
                 WHERE json_extract(summary, '$.terminal.shell_id') = ?1 LIMIT 1",
                [shell_id],
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
    /// [`proves_end`]). `None` when every recorded shim execution is
    /// proven ended (a direct-host process ends with the station).
    ///
    /// # Errors
    /// A database error.
    pub fn unended_execution(&self, session: &SessionId) -> Result<Option<String>> {
        unended_execution(self.connection(), session)
    }

    /// [`Self::unended_execution`] for the hibernation of a running
    /// session: the row `current` is the execution that the hibernation
    /// stops, so it does not count (DL-182 §5). Every earlier unresolved
    /// row still blocks.
    ///
    /// # Errors
    /// A database error.
    pub fn unended_execution_except_current(
        &self,
        session: &SessionId,
        current: Option<&str>,
    ) -> Result<Option<String>> {
        Ok(session_rows(self.connection(), session)?
            .into_iter()
            .filter(|row| Some(row.shim_id.as_str()) != current)
            .find(|row| !proves_end(row))
            .map(|row| describe(&row)))
    }

    /// The shim row that runs now for `session` (state `running`), if any.
    ///
    /// # Errors
    /// A database error.
    pub fn current_shim(&self, session: &SessionId) -> Result<Option<String>> {
        Ok(self
            .connection()
            .query_row(
                "SELECT shim_id FROM shims WHERE session_id = ?1 AND state = 'running'
                 ORDER BY created_at DESC, shim_id DESC LIMIT 1",
                [session.as_str()],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// Why another execution of the native conversation `native` of
    /// provider `kind` may still run: the rule of [`exclusion`], across
    /// every session and provider instance of the station. `except` is
    /// the session that continues the conversation. `None` when the
    /// conversation is free (DL-182 §3).
    ///
    /// # Errors
    /// A database error.
    pub fn unended_native(
        &self,
        kind: ProviderKind,
        native: &str,
        except: Option<&SessionId>,
        purpose: Purpose,
    ) -> Result<Option<String>> {
        Ok(exclusion(self.connection(), None, kind, Some(native), except, purpose)?.blocked)
    }

    /// Records that the owner reconciled `shim_id` (DL-182 §4.2). The
    /// state and the reason of the row stay as they are.
    ///
    /// # Errors
    /// A database error.
    pub fn set_owner_proof(&self, shim_id: &str) -> Result<bool> {
        let n = self.connection().execute(
            "UPDATE shims SET proof = ?2, updated_at = ?3
             WHERE shim_id = ?1 AND proof IS NULL AND state NOT IN ('starting', 'running')",
            rusqlite::params![shim_id, PROOF_OWNER, self.now()],
        )?;
        Ok(n == 1)
    }

    /// One shim row.
    ///
    /// # Errors
    /// A database error.
    pub fn shim_row(&self, shim_id: &str) -> Result<Option<ShimRow>> {
        super::shims::get(self.connection(), shim_id)
    }
}

/// See [`Decision::unended_execution`].
///
/// # Errors
/// A database error.
pub(super) fn unended_execution(conn: &Connection, session: &SessionId) -> Result<Option<String>> {
    Ok(session_rows(conn, session)?
        .into_iter()
        .find(|row| !proves_end(row))
        .map(|row| describe(&row)))
}

/// The condition of a row whose end is not proven and that has ended
/// itself: the rows the boot pass and the owner's check look at.
const UNRESOLVED: &str = "proof IS NULL
    AND (state = 'lost'
         OR (state = 'exited' AND (COALESCE(reason, '') <> 'exited' OR inspection_version < 1)))";

impl super::Store {
    /// The executions of `session` itself: why one of them may still run
    /// (`blocked`), and the rows that rest on a reboot proof, which the
    /// caller revalidates (the own-rows part of [`exclusion`], with no
    /// conversation). The move of a session uses it to prove that the
    /// source ended.
    ///
    /// # Errors
    /// A database error.
    pub async fn own_execution_exclusion(&self, session: SessionId) -> Result<Exclusion> {
        super::call(&self.inner.db, move |conn| {
            let mut out = Exclusion::default();
            out.note_session(conn, &session)?;
            Ok(out)
        })
        .await
    }

    /// The exclusion of a conversation for the dispatch of a resume or an
    /// import (see [`exclusion`]): what blocks it, and the rows that rest
    /// on a reboot proof, which the caller revalidates.
    ///
    /// # Errors
    /// A database error.
    pub async fn execution_exclusion(
        &self,
        own: Option<SessionId>,
        kind: ProviderKind,
        native: Option<String>,
        except: Option<SessionId>,
        purpose: Purpose,
    ) -> Result<Exclusion> {
        super::call(&self.inner.db, move |conn| {
            exclusion(
                conn,
                own.as_ref(),
                kind,
                native.as_deref(),
                except.as_ref(),
                purpose,
            )
        })
        .await
    }

    /// Every shim row of `session`, oldest first.
    ///
    /// # Errors
    /// A database error.
    pub async fn shim_rows(&self, session: SessionId) -> Result<Vec<ShimRow>> {
        super::call(&self.inner.db, move |conn| session_rows(conn, &session)).await
    }

    /// The ended rows without a proof whose end is not proven: `lost`
    /// rows, and `exited` rows with a reason other than a verified clean
    /// exit. With `session`, only that session's rows. Oldest first.
    ///
    /// # Errors
    /// A database error.
    pub async fn shims_unresolved(&self, session: Option<SessionId>) -> Result<Vec<ShimRow>> {
        super::call(&self.inner.db, move |conn| {
            let mut stmt = conn.prepare(&format!(
                "SELECT {} FROM shims WHERE {UNRESOLVED}
                   AND (?1 IS NULL OR session_id = ?1)
                 ORDER BY created_at, shim_id",
                super::shims::COLUMNS
            ))?;
            let raws = stmt
                .query_map([session.as_ref().map(SessionId::as_str)], shim_row)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            raws.into_iter().map(|r| r.decode()).collect()
        })
        .await
    }

    /// Records the proof `ended_by_reboot` next to an unresolved row, with
    /// the boot id that was read (DL-182 §4.1). `false` when the row is
    /// not unresolved any more. The state and the reason stay.
    ///
    /// # Errors
    /// A database error.
    pub async fn shim_prove_reboot(&self, shim_id: String, boot_id: String) -> Result<bool> {
        self.write(move |conn, now, _| {
            let n = conn.execute(
                &format!(
                    "UPDATE shims SET proof = '{PROOF_REBOOT}', proof_boot_id = ?2, updated_at = ?3
                     WHERE shim_id = ?1 AND {UNRESOLVED}"
                ),
                rusqlite::params![shim_id, boot_id, now],
            )?;
            Ok(n == 1)
        })
        .await
        .map(|(ok, _)| ok)
    }
}

#[cfg(test)]
#[path = "resumes_tests.rs"]
mod tests;
