//! The effect outbox (PLAN §6.4, ADR-001 §3, ADR-002 §3; M2 P2-5).
//!
//! An effect is `queued → claimed → confirmed`, or ends as
//! `retired_not_dispatched` or `outcome_unknown`:
//!
//! - A command decision enqueues effects in its own transaction, targeted at
//!   the session's live provider process (its *incarnation*).
//! - The worker persists the claim with its fence token **before** the I/O. A
//!   claim is never dispatched again: a claimed effect without provider
//!   evidence becomes `outcome_unknown`, never `queued`.
//! - Confirmation is provider evidence, folded in the transaction that stores
//!   the evidence event ([`observe`]); a pipe write is not a confirmation.
//!
//! | Effect | Confirmed by |
//! | --- | --- |
//! | prompt, steer, follow-up | `input.updated { started }` for its input id |
//! | start, resume | `session.started` |
//! | answer | `tool.output` / `tool.completed` of the request's node |
//! | interrupt | `interrupt.result { accepted }` with its effect id (DL-036) |
//! | set mode / set model | `mode.changed` / `model.changed` with the requested value |
//! | stop | `session.ended` |
//!
//! `input.updated { refused }` retires a prompt as not dispatched (ADR-002).

use fleetdeck_core::{IdSource, new_id};
use fleetdeck_proto::event::InputState;
use fleetdeck_proto::{CommandId, Event, EventBody, EventEnvelope, SessionId};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use fleetdeck_providers::adapter::answered_request;

use super::{Result, StoreError, to_i64, to_u64};
use super::{log, requests};

macro_rules! text_id {
    ($(#[$m:meta])* $name:ident) => {
        $(#[$m])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub String);

        impl $name {
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl From<String> for $name {
            fn from(s: String) -> Self {
                Self(s)
            }
        }

        impl From<&str> for $name {
            fn from(s: &str) -> Self {
                Self(s.to_owned())
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}

text_id!(
    /// One outbox effect (ULID).
    EffectId
);
text_id!(
    /// One provider process of a session (ULID minted at spawn).
    ProcessIncarnation
);

pub use super::payload::{EffectPayload, InputRecord, WorkspacePlan, WorktreeAction};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EffectState {
    Queued,
    Claimed,
    Confirmed,
    RetiredNotDispatched,
    OutcomeUnknown,
}

impl EffectState {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Claimed => "claimed",
            Self::Confirmed => "confirmed",
            Self::RetiredNotDispatched => "retired_not_dispatched",
            Self::OutcomeUnknown => "outcome_unknown",
        }
    }

    fn parse(s: &str) -> Result<Self> {
        Ok(match s {
            "queued" => Self::Queued,
            "claimed" => Self::Claimed,
            "confirmed" => Self::Confirmed,
            "retired_not_dispatched" => Self::RetiredNotDispatched,
            "outcome_unknown" => Self::OutcomeUnknown,
            other => return Err(StoreError::Corrupt(format!("effect state {other}"))),
        })
    }
}

/// One outbox row.
#[derive(Debug, Clone, PartialEq)]
pub struct Effect {
    pub effect_id: EffectId,
    pub command_id: Option<CommandId>,
    pub session_id: SessionId,
    pub process_incarnation: Option<ProcessIncarnation>,
    pub payload: EffectPayload,
    pub state: EffectState,
    pub fence_token: Option<u64>,
    pub reason: Option<String>,
}

/// The result of one dispatch, as the worker reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Dispatch {
    /// The I/O happened; confirmation waits for provider evidence.
    Sent,
    /// The I/O itself is the provider's acceptance (for example a spawn that
    /// returned a live session).
    Confirmed,
    /// Refused before any I/O (unsupported, closed request, no process).
    NotDispatched(String),
    /// The I/O failed half-way: the station cannot tell what arrived.
    Unknown(String),
    /// The target settles the effect itself later (a start that waits for
    /// its setup command, DL-148). The worker does not settle it; it stays
    /// `claimed`.
    Deferred,
}

/// Reasons the store writes itself.
pub const REASON_STATION_RESTART: &str = "station_restart";
pub const REASON_WORKER_RESTART: &str = "worker_restart";
pub const REASON_PROCESS_LOST: &str = "process_lost";
pub const REASON_REFUSED: &str = "refused_by_provider";
/// The reason prefix of a start effect whose first input was refused before
/// any I/O (GATE-M2 P2-a); also the `error` code.
pub const FIRST_INPUT_NOT_SENT: &str = "first_input_not_sent";
/// The reason prefix of a start effect whose first input may have reached
/// the provider (GATE-M2 P2-a).
pub const FIRST_INPUT_UNKNOWN: &str = "first_input_unknown";

const COLUMNS: &str = "effect_id, command_id, session_id, process_incarnation, payload, state,
    fence_token, reason";

fn effect_of(r: &rusqlite::Row<'_>) -> rusqlite::Result<RawEffect> {
    Ok(RawEffect {
        effect_id: r.get(0)?,
        command_id: r.get(1)?,
        session_id: r.get(2)?,
        process: r.get(3)?,
        payload: r.get(4)?,
        state: r.get(5)?,
        fence: r.get(6)?,
        reason: r.get(7)?,
    })
}

struct RawEffect {
    effect_id: String,
    command_id: Option<String>,
    session_id: String,
    process: Option<String>,
    payload: String,
    state: String,
    fence: Option<i64>,
    reason: Option<String>,
}

impl RawEffect {
    fn parse(self) -> Result<Effect> {
        Ok(Effect {
            effect_id: self.effect_id.into(),
            command_id: self.command_id.map(CommandId::from),
            session_id: self.session_id.into(),
            process_incarnation: self.process.map(ProcessIncarnation::from),
            payload: serde_json::from_str(&self.payload)?,
            state: EffectState::parse(&self.state)?,
            fence_token: self.fence.map(to_u64).transpose()?,
            reason: self.reason,
        })
    }
}

fn select(conn: &Connection, filter: &str, args: impl rusqlite::Params) -> Result<Vec<Effect>> {
    let sql = format!("SELECT {COLUMNS} FROM effects WHERE {filter} ORDER BY seq");
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(args, effect_of)?;
    rows.map(|r| r.map_err(StoreError::from).and_then(RawEffect::parse))
        .collect()
}

pub(super) fn effect(conn: &Connection, id: &EffectId) -> Result<Option<Effect>> {
    Ok(select(conn, "effect_id = ?1", [id.as_str()])?.pop())
}

pub(super) fn session_effects(conn: &Connection, session: &SessionId) -> Result<Vec<Effect>> {
    select(conn, "session_id = ?1", [session.as_str()])
}

/// The start and resume effects that are accepted and not claimed yet, the
/// oldest first (the restart admission lists them, PRE-PR r2 finding 3).
pub(super) fn queued_start_effects(conn: &Connection) -> Result<Vec<Effect>> {
    select(conn, "state = 'queued' AND kind IN ('start', 'resume')", [])
}

/// Ends every queued start or resume effect for a planned restart, in the
/// caller's transaction: the effect is retired (`RetiredNotDispatched`,
/// [`REASON_STATION_RESTART`]) and its session gets the events of a failed
/// start (`start_failed` with `message`). Returns the sessions. No effect
/// stays queued next to an ended session.
pub(super) fn fail_queued_starts(
    conn: &Connection,
    message: &str,
    now: i64,
    out: &mut Vec<EventEnvelope>,
) -> Result<Vec<SessionId>> {
    let queued = queued_start_effects(conn)?;
    let mut sessions = Vec::with_capacity(queued.len());
    for effect in &queued {
        transition(
            conn,
            effect,
            EffectState::RetiredNotDispatched,
            REASON_STATION_RESTART,
            now,
            out,
        )?;
        for event in [
            Event::Error {
                code: "start_failed".into(),
                message: message.to_owned(),
                retry_at: None,
            },
            Event::SessionEnded {
                reason: "start_failed".into(),
                exit_code: None,
            },
        ] {
            out.push(log::append_one(
                conn,
                &effect.session_id,
                event.into(),
                now,
            )?);
        }
        sessions.push(effect.session_id.clone());
    }
    Ok(sessions)
}

/// The live process of a session, if any.
pub(super) fn live_process(
    conn: &Connection,
    session: &SessionId,
) -> Result<Option<ProcessIncarnation>> {
    Ok(conn
        .query_row(
            "SELECT process_incarnation FROM processes
             WHERE session_id = ?1 AND state = 'live'
             ORDER BY started_at DESC, rowid DESC LIMIT 1",
            [session.as_str()],
            |r| r.get::<_, String>(0),
        )
        .optional()?
        .map(ProcessIncarnation::from))
}

pub(super) fn enqueue(
    conn: &Connection,
    ids: &dyn IdSource,
    now: i64,
    command_id: Option<&CommandId>,
    session: &SessionId,
    payload: EffectPayload,
) -> Result<Effect> {
    let process = if payload.needs_process() {
        match live_process(conn, session)? {
            Some(process) => Some(process),
            // A stop during the setup command of a start has no process to
            // target (DL-148).
            None if matches!(payload, EffectPayload::Stop { .. })
                && super::setup_runs::setup_run_for(conn, session)? =>
            {
                None
            }
            None => return Err(StoreError::NoProcess(session.clone())),
        }
    } else {
        None
    };
    let effect = Effect {
        effect_id: new_id(ids),
        command_id: command_id.cloned(),
        session_id: session.clone(),
        process_incarnation: process,
        payload,
        state: EffectState::Queued,
        fence_token: None,
        reason: None,
    };
    conn.execute(
        "INSERT INTO effects (effect_id, command_id, session_id, process_incarnation, kind,
             payload, correlation, state, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'queued', ?8, ?8)",
        params![
            effect.effect_id.as_str(),
            effect.command_id.as_ref().map(CommandId::as_str),
            session.as_str(),
            effect
                .process_incarnation
                .as_ref()
                .map(ProcessIncarnation::as_str),
            effect.payload.kind(),
            serde_json::to_string(&effect.payload)?,
            effect.payload.correlation(),
            now,
        ],
    )?;
    Ok(effect)
}

fn current_fence(conn: &Connection) -> Result<u64> {
    let fence: i64 = conn.query_row("SELECT worker_fence FROM meta WHERE id = 1", [], |r| {
        r.get(0)
    })?;
    to_u64(fence)
}

fn check_fence(conn: &Connection, fence: u64) -> Result<()> {
    let current = current_fence(conn)?;
    if current == fence {
        Ok(())
    } else {
        Err(StoreError::Fenced { fence, current })
    }
}

/// Starts a new worker generation. Claims of older workers that have no
/// evidence become `outcome_unknown` (a late confirmation can still settle
/// them, ADR-001 §3.2 "effect worker restarts, process alive").
pub(super) fn register_worker(
    conn: &Connection,
    now: i64,
    out: &mut Vec<EventEnvelope>,
) -> Result<u64> {
    let fence = current_fence(conn)? + 1;
    conn.execute(
        "UPDATE meta SET worker_fence = ?1 WHERE id = 1",
        [to_i64(fence)?],
    )?;
    for effect in select(
        conn,
        "state = 'claimed' AND fence_token < ?1",
        [to_i64(fence)?],
    )? {
        transition(
            conn,
            &effect,
            EffectState::OutcomeUnknown,
            REASON_WORKER_RESTART,
            now,
            out,
        )?;
    }
    Ok(fence)
}

/// Claims the oldest queued effect for the worker with `fence`. With
/// `hold_starts` a queued start or resume effect stays queued, and so does
/// any later effect of a session that has one (the restart freeze, PRE-PR
/// r1).
pub(super) fn claim_next(
    conn: &Connection,
    fence: u64,
    hold_starts: bool,
    now: i64,
    out: &mut Vec<EventEnvelope>,
) -> Result<Option<Effect>> {
    check_fence(conn, fence)?;
    let oldest = if hold_starts {
        "seq = (SELECT MIN(e.seq) FROM effects e WHERE e.state = 'queued' \
         AND e.kind NOT IN ('start', 'resume') \
         AND NOT EXISTS (SELECT 1 FROM effects h WHERE h.state = 'queued' \
             AND h.kind IN ('start', 'resume') \
             AND h.session_id = e.session_id AND h.seq < e.seq))"
    } else {
        "seq = (SELECT MIN(seq) FROM effects WHERE state = 'queued')"
    };
    let mut effect = loop {
        let Some(effect) = select(conn, oldest, [])?.pop() else {
            return Ok(None);
        };
        // ADR-005 §6 (DL-108): a queued allow of an expired request never
        // goes out.
        if !requests::retire_expired_allow(conn, &effect, now, out)? {
            break effect;
        }
    };
    conn.execute(
        "UPDATE effects SET state = 'claimed', fence_token = ?2, updated_at = ?3
         WHERE effect_id = ?1 AND state = 'queued'",
        params![effect.effect_id.as_str(), to_i64(fence)?, now],
    )?;
    effect.state = EffectState::Claimed;
    effect.fence_token = Some(fence);
    Ok(Some(effect))
}

/// Records the dispatch result of a claimed effect. A fenced-out worker
/// changes nothing; an effect that evidence already settled stays settled.
pub(super) fn settle(
    conn: &Connection,
    id: &EffectId,
    fence: u64,
    dispatch: &Dispatch,
    now: i64,
    out: &mut Vec<EventEnvelope>,
) -> Result<Option<Effect>> {
    check_fence(conn, fence)?;
    let Some(found) = effect(conn, id)? else {
        return Ok(None);
    };
    if found.state != EffectState::Claimed || found.fence_token != Some(fence) {
        return Ok(Some(found));
    }
    match dispatch {
        Dispatch::Sent | Dispatch::Deferred => {}
        Dispatch::Confirmed => {
            transition(conn, &found, EffectState::Confirmed, "", now, out)?;
        }
        Dispatch::NotDispatched(reason) => {
            transition(
                conn,
                &found,
                EffectState::RetiredNotDispatched,
                reason,
                now,
                out,
            )?;
            // The live worker proved that no answer reached the provider:
            // the request's answer slot opens again for a new command
            // (never a retry of this one).
            requests::release_if_answer(conn, &found, now)?;
        }
        Dispatch::Unknown(reason) => {
            transition(conn, &found, EffectState::OutcomeUnknown, reason, now, out)?;
        }
    }
    effect(conn, id)
}

/// The start effect spawned a process, but its first input failed
/// (GATE-M2 P2-a). The spawn stays confirmed; the effect records the input
/// outcome as its reason, and the log gets the evidence: an input refused
/// before any I/O is `discarded` (`first_input_not_sent`), an input that may
/// have reached the provider is `delivery_unknown` and is never sent again.
pub(super) fn first_input_failed(
    conn: &Connection,
    id: &EffectId,
    dispatch: &Dispatch,
    now: i64,
    out: &mut Vec<EventEnvelope>,
) -> Result<bool> {
    let Some(found) = effect(conn, id)? else {
        return Ok(false);
    };
    let EffectPayload::Start {
        prompt: Some(input),
        ..
    } = &found.payload
    else {
        return Ok(false);
    };
    let (reason, events) = match dispatch {
        Dispatch::NotDispatched(why) => (
            format!("{FIRST_INPUT_NOT_SENT}: {why}"),
            vec![
                Event::InputUpdated {
                    input_id: input.input_id.clone(),
                    turn_id: None,
                    state: InputState::Discarded,
                },
                Event::Error {
                    code: FIRST_INPUT_NOT_SENT.to_owned(),
                    message: format!(
                        "the session started, but its first input {} was not sent: {why}",
                        input.input_id
                    ),
                    retry_at: None,
                },
            ],
        ),
        Dispatch::Unknown(why) => (
            format!("{FIRST_INPUT_UNKNOWN}: {why}"),
            vec![Event::Error {
                code: "delivery_unknown".to_owned(),
                message: format!(
                    "the station cannot tell whether the first input {} reached the provider \
                     ({why}); it does not send it again",
                    input.input_id
                ),
                retry_at: None,
            }],
        ),
        Dispatch::Sent | Dispatch::Confirmed | Dispatch::Deferred => return Ok(false),
    };
    conn.execute(
        "UPDATE effects SET reason = ?2, updated_at = ?3 WHERE effect_id = ?1",
        params![found.effect_id.as_str(), reason, now],
    )?;
    for event in events {
        out.push(log::append_one(conn, &found.session_id, event.into(), now)?);
    }
    Ok(true)
}

/// Moves one effect to a final state and appends the events that tell the
/// owner about it.
pub(super) fn transition(
    conn: &Connection,
    effect: &Effect,
    to: EffectState,
    reason: &str,
    now: i64,
    out: &mut Vec<EventEnvelope>,
) -> Result<()> {
    let reason = (!reason.is_empty()).then_some(reason);
    conn.execute(
        "UPDATE effects SET state = ?2, reason = COALESCE(?3, reason), updated_at = ?4
         WHERE effect_id = ?1",
        params![effect.effect_id.as_str(), to.as_str(), reason, now],
    )?;
    for event in transition_events(effect, to, reason.unwrap_or("")) {
        out.push(log::append_one(
            conn,
            &effect.session_id,
            event.into(),
            now,
        )?);
    }
    Ok(())
}

fn transition_events(effect: &Effect, to: EffectState, reason: &str) -> Vec<Event> {
    let kind = effect.payload.kind();
    let input = effect.payload.input();
    // The row of a worktree operation and the inventory carry its outcome
    // (DL-150).
    if matches!(effect.payload, EffectPayload::Worktree { .. }) {
        return Vec::new();
    }
    match (to, input) {
        (EffectState::RetiredNotDispatched, Some(input)) => vec![Event::InputUpdated {
            input_id: input.input_id.clone(),
            turn_id: None,
            state: InputState::Discarded,
        }],
        (EffectState::OutcomeUnknown, Some(input)) => vec![Event::Error {
            code: "delivery_unknown".to_owned(),
            message: format!(
                "the station cannot tell whether input {} reached the provider ({reason}); \
                 it does not send it again",
                input.input_id
            ),
            retry_at: None,
        }],
        // The session events of a lost process already tell the owner.
        (_, None) if reason == REASON_PROCESS_LOST || reason == REASON_STATION_RESTART => {
            Vec::new()
        }
        (EffectState::RetiredNotDispatched, None) => vec![Event::Error {
            code: "not_dispatched".to_owned(),
            message: format!("{kind} was not sent: {reason}"),
            retry_at: None,
        }],
        (EffectState::OutcomeUnknown, None) => vec![Event::Error {
            code: "delivery_unknown".to_owned(),
            message: format!(
                "the station cannot tell whether {kind} reached the provider ({reason})"
            ),
            retry_at: None,
        }],
        _ => Vec::new(),
    }
}

/// Folds provider evidence into the outbox: called for every appended
/// event, inside its transaction. It appends no events itself.
pub(super) fn observe(conn: &Connection, envelope: &EventEnvelope, now: i64) -> Result<()> {
    let EventBody::Known(event) = &envelope.event else {
        return Ok(());
    };
    let session = envelope.session_id.as_str();
    let open = "state IN ('claimed', 'outcome_unknown')";
    let confirm = |filter: &str, args: &[&dyn rusqlite::ToSql]| -> Result<()> {
        let sql = format!(
            "UPDATE effects SET state = 'confirmed', updated_at = ?1
             WHERE session_id = ?2 AND {open} AND {filter}
             RETURNING kind"
        );
        let mut all: Vec<&dyn rusqlite::ToSql> = vec![&now, &session];
        all.extend_from_slice(args);
        let mut stmt = conn.prepare(&sql)?;
        let kinds = stmt
            .query_map(all.as_slice(), |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for kind in kinds {
            // Crash tests: the evidence and the confirmation are in this
            // uncommitted transaction (M2 P2-8).
            crate::fault::hit(crate::fault::FaultPoint::BeforeConfirm, &kind);
        }
        Ok(())
    };
    match event {
        // A message answer starts as a provider input (GATE-M2 P2-b): its
        // start confirms the answer effect of its request.
        Event::InputUpdated {
            input_id,
            state: InputState::Started,
            ..
        } if answered_request(input_id).is_some() => {
            if let Some(request) = answered_request(input_id) {
                confirm("kind = 'answer' AND correlation = ?3", &[&request.as_str()])?;
            }
        }
        Event::InputUpdated {
            input_id,
            state: InputState::Refused,
            ..
        } if answered_request(input_id).is_some() => {
            if let Some(request) = answered_request(input_id) {
                conn.execute(
                    "UPDATE effects SET state = 'retired_not_dispatched', reason = ?4, updated_at = ?1
                     WHERE session_id = ?2 AND state IN ('claimed', 'outcome_unknown')
                       AND kind = 'answer' AND correlation = ?3",
                    params![now, session, request.as_str(), REASON_REFUSED],
                )?;
            }
        }
        Event::InputUpdated {
            input_id,
            state: InputState::Started,
            ..
        } => confirm(
            "kind IN ('prompt', 'steer', 'follow_up') AND correlation = ?3",
            &[&input_id.as_str()],
        )?,
        Event::InputUpdated {
            input_id,
            state: InputState::Refused,
            ..
        } => {
            conn.execute(
                "UPDATE effects SET state = 'retired_not_dispatched', reason = ?4, updated_at = ?1
                 WHERE session_id = ?2 AND state IN ('queued', 'claimed', 'outcome_unknown')
                   AND kind IN ('prompt', 'steer', 'follow_up') AND correlation = ?3",
                params![now, session, input_id.as_str(), REASON_REFUSED],
            )?;
        }
        Event::SessionStarted { .. } => confirm("kind IN ('start', 'resume')", &[])?,
        Event::ToolOutput { node_id, .. } | Event::ToolCompleted { node_id, .. } => confirm(
            "kind = 'answer' AND correlation IN
                 (SELECT request_id FROM runtime_requests WHERE session_id = ?2 AND node_id = ?3)",
            &[&node_id.as_str()],
        )?,
        // Only the provider's answer to this effect's own control request
        // settles an interrupt (ADR-001 §3.1); a turn that ends is no
        // evidence, it can end for any reason.
        Event::InterruptResult {
            control_id: Some(effect_id),
            accepted: true,
            ..
        } => confirm(
            "kind = 'interrupt' AND effect_id = ?3",
            &[&effect_id.as_str()],
        )?,
        Event::InterruptResult {
            control_id: Some(effect_id),
            accepted: false,
            ..
        } => {
            conn.execute(
                "UPDATE effects SET state = 'retired_not_dispatched', reason = ?4, updated_at = ?1
                 WHERE session_id = ?2 AND state IN ('claimed', 'outcome_unknown')
                   AND kind = 'interrupt' AND effect_id = ?3",
                params![now, session, effect_id.as_str(), REASON_REFUSED],
            )?;
        }
        Event::ModeChanged { mode } => {
            confirm("kind = 'set_mode' AND correlation = ?3", &[&mode.as_str()])?;
        }
        Event::ModelChanged { model } => {
            confirm(
                "kind = 'set_model' AND correlation = ?3",
                &[&model.as_str()],
            )?;
        }
        Event::SessionEnded { .. } => confirm("kind = 'stop'", &[])?,
        _ => {}
    }
    Ok(())
}

/// Settles the effects of a process that is gone: queued → retired, claimed
/// → outcome unknown.
pub(super) fn close_process_effects(
    conn: &Connection,
    process: &ProcessIncarnation,
    reason: &str,
    now: i64,
    out: &mut Vec<EventEnvelope>,
) -> Result<()> {
    for effect in select(
        conn,
        "process_incarnation = ?1 AND state IN ('queued', 'claimed')",
        [process.as_str()],
    )? {
        let to = if effect.state == EffectState::Queued {
            EffectState::RetiredNotDispatched
        } else {
            EffectState::OutcomeUnknown
        };
        transition(conn, &effect, to, reason, now, out)?;
    }
    Ok(())
}

/// Station start: every queued effect whose target is not live (or that
/// would start a process) is retired; every claim is unknown.
pub(super) fn recover_effects(
    conn: &Connection,
    now: i64,
    out: &mut Vec<EventEnvelope>,
) -> Result<(usize, usize)> {
    let retire = select(
        conn,
        "state = 'queued' AND (process_incarnation IS NULL OR process_incarnation IN
             (SELECT process_incarnation FROM processes WHERE state = 'dead'))",
        [],
    )?;
    for effect in &retire {
        transition(
            conn,
            effect,
            EffectState::RetiredNotDispatched,
            REASON_STATION_RESTART,
            now,
            out,
        )?;
    }
    let unknown = select(conn, "state = 'claimed'", [])?;
    for effect in &unknown {
        transition(
            conn,
            effect,
            EffectState::OutcomeUnknown,
            REASON_STATION_RESTART,
            now,
            out,
        )?;
    }
    Ok((retire.len(), unknown.len()))
}
