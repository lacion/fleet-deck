//! The durable rows of a shim session's adapter (ADR-005 §2.1; DL-110):
//! adapter-generated stdin frames and the command transitions applied after
//! the latest checkpoint.
//!
//! - An output transaction ([`super::Store::shim_commit_output_with`])
//!   stores the frames that the output generated as `pending`
//!   [`ShimFrame`]s, with the events, the checkpoint and the cursor. It
//!   removes the transitions, because the checkpoint covers them.
//! - [`Store::shim_claim_frame`] claims a pending frame (a `shim_writes`
//!   row with an internal write id) before it leaves; the driver marks it
//!   done after the shim reported it written.
//! - [`Store::shim_begin_transition`] records a command effect, with its
//!   events and its frame count, before any of its frames is claimed. A
//!   transition whose effect claimed no frame of a frame-bearing command was
//!   never sent (no frame leaves without a claim row).

use fleetdeck_proto::{EventEnvelope, SessionId};
use fleetdeck_providers::host::shim::stdin::WriteTag;
use rusqlite::{Connection, OptionalExtension as _, params};

use super::NewEvent;
use super::effects::{self, Effect, EffectId};
use super::log;
use super::{Result, Store, StoreError, call, to_i64, to_u64};

/// The state of an adapter-generated frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameState {
    /// Committed; never claimed, so never sent.
    Pending,
    /// Claimed: it may have left.
    Claimed,
    /// The shim reported it written.
    Done,
}

impl FrameState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Claimed => "claimed",
            Self::Done => "done",
        }
    }

    fn parse(s: &str) -> Result<Self> {
        Ok(match s {
            "pending" => Self::Pending,
            "claimed" => Self::Claimed,
            "done" => Self::Done,
            other => return Err(StoreError::Corrupt(format!("frame state {other}"))),
        })
    }
}

/// An adapter-generated stdin frame that is not done.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShimFrame {
    pub frame_seq: u64,
    /// One JSON line without its newline.
    pub payload: String,
    pub state: FrameState,
    /// The latest claim (`Claimed` frames).
    pub tag: Option<WriteTag>,
}

/// A command transition after the latest checkpoint.
#[derive(Debug, Clone, PartialEq)]
pub struct ShimTransition {
    pub seq: u64,
    pub effect: Effect,
    /// The frames of the command.
    pub frames: u32,
    /// The frames of the effect with a claim row.
    pub claimed: u32,
}

fn open_row(conn: &Connection, shim_id: &str) -> Result<Option<(u64, SessionId)>> {
    let row: Option<(i64, String)> = conn
        .query_row(
            "SELECT owner_generation, session_id FROM shims
             WHERE shim_id = ?1 AND state IN ('starting', 'running')",
            [shim_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    row.map(|(g, s)| Ok((to_u64(g)?, SessionId::from(s.as_str()))))
        .transpose()
}

/// Stores `frames` as pending frames of `shim_id` (inside an output
/// transaction).
pub(super) fn store_frames(
    conn: &Connection,
    shim_id: &str,
    frames: &[String],
    now: i64,
) -> Result<()> {
    if frames.is_empty() {
        return Ok(());
    }
    let next: i64 = conn.query_row(
        "SELECT COALESCE(MAX(frame_seq), 0) + 1 FROM shim_frames WHERE shim_id = ?1",
        [shim_id],
        |r| r.get(0),
    )?;
    for (i, payload) in (next..).zip(frames) {
        conn.execute(
            "INSERT INTO shim_frames (shim_id, frame_seq, payload, state, created_at, updated_at)
             VALUES (?1, ?2, ?3, 'pending', ?4, ?4)",
            params![shim_id, i, payload, now],
        )?;
    }
    Ok(())
}

/// Removes the transitions of `shim_id` (a new checkpoint covers them).
pub(super) fn clear_transitions(conn: &Connection, shim_id: &str) -> Result<()> {
    conn.execute("DELETE FROM shim_transitions WHERE shim_id = ?1", [shim_id])?;
    Ok(())
}

impl Store {
    /// Claims pending frame `frame_seq` on the current `generation`: a new
    /// write sequence number and an internal write id
    /// (`internal:<session>:frame:<frame_seq>:<write_seq>`). `None` for a
    /// stale generation, an ended row, or a frame that is not pending.
    ///
    /// # Errors
    /// A database error.
    pub async fn shim_claim_frame(
        &self,
        shim_id: String,
        generation: u64,
        frame_seq: u64,
    ) -> Result<Option<WriteTag>> {
        self.write(move |conn, now, _| {
            let Some((current, session)) = open_row(conn, &shim_id)? else {
                return Ok(None);
            };
            if current != generation {
                return Ok(None);
            }
            let state: Option<String> = conn
                .query_row(
                    "SELECT state FROM shim_frames WHERE shim_id = ?1 AND frame_seq = ?2",
                    params![shim_id, to_i64(frame_seq)?],
                    |r| r.get(0),
                )
                .optional()?;
            if state.as_deref() != Some(FrameState::Pending.as_str()) {
                return Ok(None);
            }
            let seq: i64 = conn.query_row(
                "UPDATE shims SET next_write_seq = next_write_seq + 1, updated_at = ?2
                 WHERE shim_id = ?1 RETURNING next_write_seq - 1",
                params![shim_id, now],
                |r| r.get(0),
            )?;
            let write_id = format!("internal:{session}:frame:{frame_seq}:{seq}");
            conn.execute(
                "INSERT INTO shim_writes (shim_id, write_seq, write_id, effect_id, ordinal,
                     generation, created_at, frames)
                 VALUES (?1, ?2, ?3, NULL, 0, ?4, ?5, 1)",
                params![shim_id, seq, write_id, to_i64(generation)?, now],
            )?;
            conn.execute(
                "UPDATE shim_frames SET state = 'claimed', write_id = ?3, write_seq = ?4,
                     updated_at = ?5
                 WHERE shim_id = ?1 AND frame_seq = ?2",
                params![shim_id, to_i64(frame_seq)?, write_id, seq, now],
            )?;
            Ok(Some(WriteTag {
                write_id,
                write_seq: to_u64(seq)?,
            }))
        })
        .await
        .map(|(tag, _)| tag)
    }

    /// Moves claimed frame `frame_seq` to `state`: `Done` after the shim
    /// reported it written, `Pending` after the shim proved it unwritten (it
    /// is then sent again with a new claim).
    ///
    /// The change is fenced (DL-111): it applies only while `generation` is
    /// the current owner generation of an open row, the frame is `claimed`,
    /// and its stored claim is exactly `tag`. A delayed owner whose
    /// attachment was taken over, or whose claim was settled and claimed
    /// again by the new owner, changes nothing, so a completed frame never
    /// returns to `pending` and is never sent again. `false` for such a
    /// stale call.
    ///
    /// # Errors
    /// [`StoreError::Corrupt`] for a target state of `Claimed` (only
    /// [`Store::shim_claim_frame`] claims); a database error.
    pub async fn shim_frame_state(
        &self,
        shim_id: String,
        generation: u64,
        frame_seq: u64,
        tag: WriteTag,
        state: FrameState,
    ) -> Result<bool> {
        if state == FrameState::Claimed {
            return Err(StoreError::Corrupt(
                "a frame is claimed only by shim_claim_frame".to_owned(),
            ));
        }
        self.write(move |conn, now, _| {
            let Some((current, _)) = open_row(conn, &shim_id)? else {
                return Ok(false);
            };
            if current != generation {
                return Ok(false);
            }
            let changed = conn.execute(
                "UPDATE shim_frames SET state = ?3, updated_at = ?4
                 WHERE shim_id = ?1 AND frame_seq = ?2 AND state = 'claimed'
                   AND write_id = ?5 AND write_seq = ?6",
                params![
                    shim_id,
                    to_i64(frame_seq)?,
                    state.as_str(),
                    now,
                    tag.write_id,
                    to_i64(tag.write_seq)?
                ],
            )?;
            Ok(changed == 1)
        })
        .await
        .map(|(changed, _)| changed)
    }

    /// The frames of `shim_id` that are not done, in order.
    ///
    /// # Errors
    /// A database error.
    pub async fn shim_open_frames(&self, shim_id: String) -> Result<Vec<ShimFrame>> {
        call(&self.inner.db, move |conn| {
            let mut stmt = conn.prepare(
                "SELECT frame_seq, payload, state, write_id, write_seq FROM shim_frames
                 WHERE shim_id = ?1 AND state != 'done' ORDER BY frame_seq",
            )?;
            let rows = stmt
                .query_map([shim_id.as_str()], |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, Option<String>>(3)?,
                        r.get::<_, Option<i64>>(4)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows.into_iter()
                .map(|(seq, payload, state, write_id, write_seq)| {
                    let tag = match (write_id, write_seq) {
                        (Some(write_id), Some(s)) => Some(WriteTag {
                            write_id,
                            write_seq: to_u64(s)?,
                        }),
                        _ => None,
                    };
                    Ok(ShimFrame {
                        frame_seq: to_u64(seq)?,
                        payload,
                        state: FrameState::parse(&state)?,
                        tag,
                    })
                })
                .collect()
        })
        .await
    }

    /// Records a command transition of `effect_id` with `frames` frames
    /// and its `events`, on the current `generation`, before any frame of
    /// it is claimed. `None` for a stale generation or an ended row.
    ///
    /// # Errors
    /// A database error.
    pub async fn shim_begin_transition(
        &self,
        shim_id: String,
        generation: u64,
        effect_id: EffectId,
        frames: u32,
        events: Vec<NewEvent>,
    ) -> Result<Option<u64>> {
        self.write(move |conn, now, out: &mut Vec<EventEnvelope>| {
            let Some((current, session)) = open_row(conn, &shim_id)? else {
                return Ok(None);
            };
            if current != generation {
                return Ok(None);
            }
            let seq: i64 = conn.query_row(
                "SELECT COALESCE(MAX(seq), 0) + 1 FROM shim_transitions WHERE shim_id = ?1",
                [shim_id.as_str()],
                |r| r.get(0),
            )?;
            conn.execute(
                "INSERT INTO shim_transitions (shim_id, seq, effect_id, frames, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![shim_id, seq, effect_id.as_str(), frames, now],
            )?;
            for event in events {
                out.push(log::append_one(conn, &session, event, now)?);
            }
            Ok(Some(to_u64(seq)?))
        })
        .await
        .map(|(seq, _)| seq)
    }

    /// Removes transition `seq` (its command never reached the provider).
    ///
    /// # Errors
    /// A database error.
    pub async fn shim_drop_transition(&self, shim_id: String, seq: u64) -> Result<()> {
        self.write(move |conn, _, _| {
            conn.execute(
                "DELETE FROM shim_transitions WHERE shim_id = ?1 AND seq = ?2",
                params![shim_id, to_i64(seq)?],
            )?;
            Ok(())
        })
        .await
        .map(|((), _)| ())
    }

    /// The transitions of `shim_id` after its latest checkpoint, in order.
    ///
    /// # Errors
    /// A database error.
    pub async fn shim_transitions(&self, shim_id: String) -> Result<Vec<ShimTransition>> {
        call(&self.inner.db, move |conn| {
            let mut stmt = conn.prepare(
                "SELECT t.seq, t.effect_id, t.frames,
                     (SELECT COUNT(*) FROM shim_writes w
                      WHERE w.shim_id = t.shim_id AND w.effect_id = t.effect_id)
                 FROM shim_transitions t WHERE t.shim_id = ?1 ORDER BY t.seq",
            )?;
            let rows = stmt
                .query_map([shim_id.as_str()], |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, u32>(2)?,
                        r.get::<_, u32>(3)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            let mut out = Vec::new();
            for (seq, effect_id, frames, claimed) in rows {
                let Some(effect) = effects::effect(conn, &EffectId::from(effect_id))? else {
                    return Err(StoreError::Corrupt(
                        "a transition without its effect".into(),
                    ));
                };
                out.push(ShimTransition {
                    seq: to_u64(seq)?,
                    effect,
                    frames,
                    claimed,
                });
            }
            Ok(out)
        })
        .await
    }

    /// Blocks the input of the shim's session with `reason` (a frame that
    /// may have been partly written; ADR-005 §4.3).
    ///
    /// # Errors
    /// A database error.
    pub async fn shim_block_input(&self, shim_id: String, reason: &'static str) -> Result<()> {
        self.write(move |conn, now, _| {
            conn.execute(
                "UPDATE shims SET input_blocked = COALESCE(input_blocked, ?2), updated_at = ?3
                 WHERE shim_id = ?1",
                params![shim_id, reason, now],
            )?;
            Ok(())
        })
        .await
        .map(|((), _)| ())
    }
}
