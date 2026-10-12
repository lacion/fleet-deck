//! The durable identities of shim stdin frames and their settlement after a
//! reattach (ADR-005 §4.3; slice 5, DL-105).
//!
//! - [`Store::shim_claim_write`] claims the identity of one frame in one
//!   transaction: the current generation is checked, `next_write_seq` rises
//!   and the `shim_writes` row is inserted. The frame leaves only after
//!   this commit, so a crash at any later point leaves a row that recovery
//!   asks the shim about.
//! - [`Store::shim_writes_to_settle`] lists the `claimed` effects that have
//!   frames on a shim. Recovery asks `write_status` for each frame and
//!   applies the §4.3 table with [`Store::shim_settle_write`] before the
//!   generic M2 recovery turns every remaining claim into
//!   `outcome_unknown`.
//! - A `partial` frame blocks the session's input
//!   ([`Store::shim_input_blocked`]); the decision transaction refuses
//!   every input command and still admits interrupt and stop.

use fleetdeck_proto::{EventEnvelope, SessionId};
use fleetdeck_providers::checkpoint::AdapterCheckpoint;
use fleetdeck_providers::host::shim::settle::EffectResult;
use fleetdeck_providers::host::shim::stdin::WriteTag;
use rusqlite::{Connection, OptionalExtension as _, params};

use super::effects::{self, Effect, EffectId, EffectState};
use super::receipts::Decision;
use super::{Result, Store, StoreError, call, to_i64, to_u64};

/// The reason of a frame whose fate the shim cannot prove.
pub const REASON_SHIM_UNKNOWN: &str = "shim_write_unknown";
/// The reason, and the `input_blocked` value, of a partial frame.
pub const REASON_STDIN_PARTIAL: &str = "stdin_partial_write";

/// A claimed effect with the frames it sent to one shim, in sequence order.
#[derive(Debug, Clone, PartialEq)]
pub struct ClaimedWrite {
    pub effect: Effect,
    /// The claimed frames, in sequence order.
    pub tags: Vec<WriteTag>,
    /// The complete frame count of the effect, stored with frame 0 before
    /// any byte left (DL-107). `frames - tags.len()` frames were never
    /// claimed and never sent.
    pub frames: u32,
}

fn write_id(shim_id: &str, effect_id: Option<&EffectId>, ordinal: u32, seq: u64) -> String {
    match effect_id {
        Some(effect) => format!("{effect}:{ordinal}"),
        None => format!("internal:{shim_id}:{seq}"),
    }
}

fn current_generation(conn: &Connection, shim_id: &str) -> Result<Option<u64>> {
    let generation: Option<i64> = conn
        .query_row(
            "SELECT owner_generation FROM shims
             WHERE shim_id = ?1 AND state IN ('starting', 'running')",
            [shim_id],
            |r| r.get(0),
        )
        .optional()?;
    generation.map(to_u64).transpose()
}

fn blocked(conn: &Connection, session: &SessionId) -> Result<Option<String>> {
    Ok(conn
        .query_row(
            "SELECT input_blocked FROM shims
             WHERE session_id = ?1 AND state IN ('starting', 'running')
               AND input_blocked IS NOT NULL
             ORDER BY created_at DESC LIMIT 1",
            [session.as_str()],
            |r| r.get(0),
        )
        .optional()?)
}

impl Decision<'_> {
    /// The reason that blocks the session's input, if its open shim
    /// reported a partial stdin write.
    ///
    /// # Errors
    /// A database error.
    pub fn shim_input_blocked(&self, session: &SessionId) -> Result<Option<String>> {
        blocked(self.connection(), session)
    }
}

impl Store {
    /// Claims the identity of frame `ordinal` of `effect_id` (or of an
    /// adapter-internal frame) on the current generation of `shim_id`.
    /// `None` for a stale generation, an ended row, or an effect that is
    /// not `claimed`; then the frame must not leave.
    ///
    /// `frames` is the complete frame count of the effect. Every row of the
    /// effect stores it, so the claim of frame 0 makes the count durable
    /// before any byte leaves (DL-107). The frames of one effect are
    /// claimed in order: `ordinal` is the number of frames claimed before.
    ///
    /// # Errors
    /// A database error; [`StoreError::Corrupt`] for `ordinal >= frames`,
    /// an out-of-order ordinal, or a frame count that differs from the one
    /// stored with frame 0 (a caller bug: no row is written).
    pub async fn shim_claim_write(
        &self,
        shim_id: String,
        generation: u64,
        effect_id: Option<EffectId>,
        ordinal: u32,
        frames: u32,
    ) -> Result<Option<WriteTag>> {
        self.shim_claim_write_with(shim_id, generation, effect_id, ordinal, frames, None)
            .await
    }

    /// [`Self::shim_claim_write`] that also stores `checkpoint`, the adapter
    /// state after the transition that produced the frame, in the same
    /// transaction (ADR-005 §2.1; DL-108). Nothing is stored when the claim
    /// is refused.
    ///
    /// # Errors
    /// As [`Self::shim_claim_write`]; a checkpoint that does not serialize
    /// is [`StoreError::Corrupt`].
    pub async fn shim_claim_write_with(
        &self,
        shim_id: String,
        generation: u64,
        effect_id: Option<EffectId>,
        ordinal: u32,
        frames: u32,
        checkpoint: Option<AdapterCheckpoint>,
    ) -> Result<Option<WriteTag>> {
        let checkpoint = checkpoint
            .map(|c| c.to_json())
            .transpose()
            .map_err(|e| StoreError::Corrupt(e.to_string()))?;
        self.write(move |conn, now, _| {
            if ordinal >= frames {
                return Err(StoreError::Corrupt(format!(
                    "frame {ordinal} of an effect with {frames} frames"
                )));
            }
            if current_generation(conn, &shim_id)? != Some(generation) {
                return Ok(None);
            }
            if let Some(effect) = &effect_id {
                let state: Option<String> = conn
                    .query_row(
                        "SELECT state FROM effects WHERE effect_id = ?1",
                        [effect.as_str()],
                        |r| r.get(0),
                    )
                    .optional()?;
                if state.as_deref() != Some(EffectState::Claimed.as_str()) {
                    return Ok(None);
                }
                let (claimed, stored): (i64, Option<i64>) = conn.query_row(
                    "SELECT COUNT(*), MAX(frames) FROM shim_writes WHERE effect_id = ?1",
                    [effect.as_str()],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )?;
                if to_u64(claimed)? != u64::from(ordinal)
                    || stored.is_some_and(|n| n != i64::from(frames))
                {
                    return Err(StoreError::Corrupt(format!(
                        "frame {ordinal} of {frames} after {claimed} claimed frames of {effect}"
                    )));
                }
            }
            let seq: i64 = conn.query_row(
                "UPDATE shims SET next_write_seq = next_write_seq + 1, updated_at = ?2
                 WHERE shim_id = ?1 RETURNING next_write_seq - 1",
                params![shim_id, now],
                |r| r.get(0),
            )?;
            let write_seq = to_u64(seq)?;
            let write_id = write_id(&shim_id, effect_id.as_ref(), ordinal, write_seq);
            conn.execute(
                "INSERT INTO shim_writes (shim_id, write_seq, write_id, effect_id, ordinal,
                     generation, created_at, frames)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    shim_id,
                    seq,
                    write_id,
                    effect_id.as_ref().map(EffectId::as_str),
                    ordinal,
                    to_i64(generation)?,
                    now,
                    frames
                ],
            )?;
            if let Some(text) = &checkpoint {
                super::shims::store_checkpoint(conn, &shim_id, text, now)?;
            }
            Ok(Some(WriteTag {
                write_id,
                write_seq,
            }))
        })
        .await
        .map(|(tag, _)| tag)
    }

    /// The `claimed` effects with frames on `shim_id`, oldest first.
    ///
    /// # Errors
    /// A database error.
    pub async fn shim_writes_to_settle(&self, shim_id: String) -> Result<Vec<ClaimedWrite>> {
        call(&self.inner.db, move |conn| {
            let mut stmt = conn.prepare(
                "SELECT w.effect_id, w.write_id, w.write_seq, w.frames FROM shim_writes w
                 JOIN effects e ON e.effect_id = w.effect_id
                 WHERE w.shim_id = ?1 AND e.state = 'claimed'
                 ORDER BY e.seq, w.write_seq",
            )?;
            let rows = stmt
                .query_map([shim_id.as_str()], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, i64>(2)?,
                        r.get::<_, i64>(3)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            let mut out: Vec<ClaimedWrite> = Vec::new();
            for (effect_id, write_id, seq, frames) in rows {
                let tag = WriteTag {
                    write_id,
                    write_seq: to_u64(seq)?,
                };
                let frames = u32::try_from(frames)
                    .map_err(|_| StoreError::Corrupt(format!("frame count {frames}")))?;
                if let Some(last) = out.last_mut()
                    && last.effect.effect_id.as_str() == effect_id
                {
                    last.tags.push(tag);
                    last.frames = last.frames.max(frames);
                    continue;
                }
                let id = EffectId::from(effect_id);
                if let Some(effect) = effects::effect(conn, &id)? {
                    out.push(ClaimedWrite {
                        effect,
                        tags: vec![tag],
                        frames,
                    });
                }
            }
            Ok(out)
        })
        .await
    }

    /// Applies the §4.3 result of a claimed effect of `shim_id`:
    /// `Retired` → `retired_not_dispatched`, `Unknown` → `outcome_unknown`
    /// (a partial write also blocks the session's input). `Sent` and
    /// `AskAgain` change nothing. `false` when the effect was no longer
    /// `claimed`.
    ///
    /// # Errors
    /// A database error.
    pub async fn shim_settle_write(
        &self,
        shim_id: String,
        effect_id: EffectId,
        result: EffectResult,
    ) -> Result<bool> {
        self.write(move |conn, now, out: &mut Vec<EventEnvelope>| {
            let Some(effect) = effects::effect(conn, &effect_id)? else {
                return Ok(false);
            };
            if effect.state != EffectState::Claimed {
                return Ok(false);
            }
            let (to, reason) = match result {
                EffectResult::Retired(reason) => (EffectState::RetiredNotDispatched, reason),
                EffectResult::Unknown { block_input: false } => {
                    (EffectState::OutcomeUnknown, REASON_SHIM_UNKNOWN)
                }
                EffectResult::Unknown { block_input: true } => {
                    conn.execute(
                        "UPDATE shims SET input_blocked = ?2, updated_at = ?3
                         WHERE shim_id = ?1",
                        params![shim_id, REASON_STDIN_PARTIAL, now],
                    )?;
                    (EffectState::OutcomeUnknown, REASON_STDIN_PARTIAL)
                }
                EffectResult::Sent | EffectResult::AskAgain => return Ok(false),
            };
            effects::transition(conn, &effect, to, reason, now, out)?;
            Ok(true)
        })
        .await
        .map(|(ok, _)| ok)
    }

    /// The reason that blocks the session's input (see the module
    /// documentation).
    ///
    /// # Errors
    /// A database error.
    pub async fn shim_input_blocked(&self, session: SessionId) -> Result<Option<String>> {
        call(&self.inner.db, move |conn| blocked(conn, &session)).await
    }
}
