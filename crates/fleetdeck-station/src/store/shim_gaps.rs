//! Shim output that the station can no longer parse, and the exit report of
//! a shim that ended while the station was down (ADR-005 §4.2, §4.5;
//! review sa-9c1141b8 MAJOR 2 and 3; DL-122).
//!
//! - [`Store::shim_adapter_gap`]: the shim no longer holds output above the
//!   committed cursor (a `below_floor` gap). The adapter state cannot follow
//!   the provider any more, so one transaction commits
//!   `session.output_gap { below_floor }` and an `error`
//!   (`adapter_state_gap`), invalidates the stored adapter checkpoint and
//!   its transitions, turns every claimed effect of the session
//!   `outcome_unknown`, blocks the session's input, and moves the cursor
//!   past the gap. The provider keeps running; only stop is admitted.
//! - [`Store::shim_record_exit`]: an exit report that recovery read from
//!   the shim's exit file is applied before the generic recovery: the
//!   unacknowledged output becomes an explicit gap, the session gets its
//!   exit events (so it is `exited`, not `lost`), the cursor is the final
//!   offset, and the process ends with `exited`.

use fleetdeck_proto::event::OutputGapReason;
use fleetdeck_proto::{Event, EventEnvelope};
use rusqlite::{OptionalExtension as _, params};

use super::effects::{self, EffectId, EffectState};
use super::{NewEvent, Result, Store, log, recovery, to_i64, to_u64};

/// The `error` code, the effect reason and the input block of an adapter
/// state gap.
pub const REASON_ADAPTER_STATE_GAP: &str = "adapter_state_gap";
/// The process end reason after an exit report read at recovery.
pub const REASON_EXITED: &str = "exited";

/// True when the adapter state of `shim_id` was invalidated by a gap: no
/// generated frame, command transition or stdin frame may be claimed for
/// it any more, whatever the caller's memory says (review of DL-122).
pub(super) fn has_adapter_gap(conn: &rusqlite::Connection, shim_id: &str) -> Result<bool> {
    let blocked: Option<Option<String>> = conn
        .query_row(
            "SELECT input_blocked FROM shims WHERE shim_id = ?1",
            [shim_id],
            |r| r.get(0),
        )
        .optional()?;
    Ok(blocked.flatten().as_deref() == Some(REASON_ADAPTER_STATE_GAP))
}

impl Store {
    /// True when the adapter state of `shim_id` was invalidated by a gap.
    ///
    /// # Errors
    /// A database error.
    pub async fn shim_has_adapter_gap(&self, shim_id: String) -> Result<bool> {
        super::call(&self.inner.db, move |conn| has_adapter_gap(conn, &shim_id)).await
    }

    /// Commits a `below_floor` gap `[from, to)` of the current `generation`
    /// (see the module documentation). `false` for a stale generation or an
    /// ended row; then nothing changes.
    ///
    /// # Errors
    /// A database error.
    pub async fn shim_adapter_gap(
        &self,
        shim_id: String,
        generation: u64,
        from: u64,
        to: u64,
    ) -> Result<bool> {
        self.write(move |conn, now, out: &mut Vec<EventEnvelope>| {
            let row: Option<(String, i64, i64)> = conn
                .query_row(
                    "SELECT session_id, owner_generation, cursor FROM shims
                     WHERE shim_id = ?1 AND state IN ('starting', 'running')",
                    [shim_id.as_str()],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .optional()?;
            let Some((session, current, cursor)) = row else {
                return Ok(false);
            };
            if to_u64(current)? != generation {
                return Ok(false);
            }
            let session = session.into();
            let events = [
                Event::OutputGap {
                    from,
                    to,
                    reason: OutputGapReason::BelowFloor,
                },
                Event::Error {
                    code: REASON_ADAPTER_STATE_GAP.to_owned(),
                    message: "the shim no longer holds provider output that the station \
                              did not commit; the session takes no input until it is stopped"
                        .to_owned(),
                    retry_at: None,
                },
            ];
            for event in events {
                out.push(log::append_one(conn, &session, event.into(), now)?);
            }
            let mut stmt = conn.prepare(
                "SELECT effect_id FROM effects WHERE session_id = ?1 AND state = 'claimed'",
            )?;
            let claimed: Vec<String> = stmt
                .query_map([session.as_str()], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            for id in claimed {
                if let Some(effect) = effects::effect(conn, &EffectId::from(id))? {
                    effects::transition(
                        conn,
                        &effect,
                        EffectState::OutcomeUnknown,
                        REASON_ADAPTER_STATE_GAP,
                        now,
                        out,
                    )?;
                }
            }
            conn.execute(
                "UPDATE shims SET adapter_checkpoint = NULL,
                     input_blocked = COALESCE(input_blocked, ?2),
                     cursor = MAX(cursor, ?3), updated_at = ?4
                 WHERE shim_id = ?1",
                params![
                    shim_id,
                    REASON_ADAPTER_STATE_GAP,
                    to_i64(to.max(to_u64(cursor)?))?,
                    now
                ],
            )?;
            super::shim_driver::clear_transitions(conn, &shim_id)?;
            Ok(true)
        })
        .await
        .map(|(ok, _)| ok)
    }

    /// Applies the exit report of an `exited` shim row read at recovery
    /// (see the module documentation): a gap from the committed cursor to
    /// `final_offset` when output was not acknowledged, then `events` (the
    /// adapter's exit events), the final cursor, and the end of the
    /// session's live process. `false` when the row is not `exited`.
    ///
    /// # Errors
    /// A database error.
    pub async fn shim_record_exit(
        &self,
        shim_id: String,
        final_offset: u64,
        events: Vec<NewEvent>,
    ) -> Result<bool> {
        self.write(move |conn, now, out: &mut Vec<EventEnvelope>| {
            let row: Option<(String, i64)> = conn
                .query_row(
                    "SELECT session_id, cursor FROM shims
                     WHERE shim_id = ?1 AND state = 'exited'",
                    [shim_id.as_str()],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            let Some((session, cursor)) = row else {
                return Ok(false);
            };
            let session = session.into();
            let cursor = to_u64(cursor)?;
            if final_offset > cursor {
                let gap = Event::OutputGap {
                    from: cursor,
                    to: final_offset,
                    reason: OutputGapReason::BelowFloor,
                };
                out.push(log::append_one(conn, &session, gap.into(), now)?);
            }
            for event in events {
                out.push(log::append_one(conn, &session, event, now)?);
            }
            conn.execute(
                "UPDATE shims SET cursor = MAX(cursor, ?2), updated_at = ?3 WHERE shim_id = ?1",
                params![shim_id, to_i64(final_offset)?, now],
            )?;
            if let Some(process) = effects::live_process(conn, &session)? {
                recovery::process_ended(conn, &process, REASON_EXITED, now, out)?;
            }
            Ok(true)
        })
        .await
        .map(|(ok, _)| ok)
    }
}
