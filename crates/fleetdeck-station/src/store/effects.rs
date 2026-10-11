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
//! | interrupt | `turn.completed` or `run.completed` |
//! | set mode / set model | `mode.changed` / `model.changed` with the requested value |
//! | stop | `session.ended` |
//!
//! `input.updated { refused }` retires a prompt as not dispatched (ADR-002).

use fleetdeck_core::{IdSource, new_id};
use fleetdeck_proto::command::{ApprovalDecision, SessionSpec, StopMode, UserInput};
use fleetdeck_proto::event::{InputState, ResolvedBy, RunOrigin};
use fleetdeck_proto::{CommandId, Event, EventBody, EventEnvelope, InputId, RequestId, SessionId};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use super::log;
use super::{Result, StoreError, to_i64, to_u64};

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

/// A host input as the effect carries it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputRecord {
    pub input_id: InputId,
    pub origin: RunOrigin,
    pub input: UserInput,
}

/// What an effect does.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EffectPayload {
    Start {
        spec: SessionSpec,
        prompt: Option<InputRecord>,
    },
    Resume {
        native_session_id: String,
    },
    Prompt {
        input: InputRecord,
    },
    Steer {
        input: InputRecord,
    },
    FollowUp {
        input: InputRecord,
    },
    Interrupt,
    Answer {
        request_id: RequestId,
        decision: ApprovalDecision,
        by: ResolvedBy,
    },
    SetMode {
        mode: String,
    },
    SetModel {
        model: String,
    },
    Stop {
        how: StopMode,
    },
}

impl EffectPayload {
    #[must_use]
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Start { .. } => "start",
            Self::Resume { .. } => "resume",
            Self::Prompt { .. } => "prompt",
            Self::Steer { .. } => "steer",
            Self::FollowUp { .. } => "follow_up",
            Self::Interrupt => "interrupt",
            Self::Answer { .. } => "answer",
            Self::SetMode { .. } => "set_mode",
            Self::SetModel { .. } => "set_model",
            Self::Stop { .. } => "stop",
        }
    }

    /// True for effects that act on a running process (all but start/resume).
    #[must_use]
    pub fn needs_process(&self) -> bool {
        !matches!(self, Self::Start { .. } | Self::Resume { .. })
    }

    /// The host input that the effect delivers.
    #[must_use]
    pub fn input(&self) -> Option<&InputRecord> {
        match self {
            Self::Prompt { input } | Self::Steer { input } | Self::FollowUp { input } => {
                Some(input)
            }
            Self::Start { prompt, .. } => prompt.as_ref(),
            _ => None,
        }
    }

    /// The key that the confirming evidence carries.
    fn correlation(&self) -> Option<String> {
        match self {
            Self::Prompt { input } | Self::Steer { input } | Self::FollowUp { input } => {
                Some(input.input_id.0.clone())
            }
            Self::Answer { request_id, .. } => Some(request_id.0.clone()),
            Self::SetMode { mode } => Some(mode.clone()),
            Self::SetModel { model } => Some(model.clone()),
            _ => None,
        }
    }
}

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
}

/// Reasons the store writes itself.
pub const REASON_STATION_RESTART: &str = "station_restart";
pub const REASON_WORKER_RESTART: &str = "worker_restart";
pub const REASON_PROCESS_LOST: &str = "process_lost";
pub const REASON_REFUSED: &str = "refused_by_provider";

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
        Some(live_process(conn, session)?.ok_or_else(|| StoreError::NoProcess(session.clone()))?)
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

/// Claims the oldest queued effect for the worker with `fence`.
pub(super) fn claim_next(conn: &Connection, fence: u64, now: i64) -> Result<Option<Effect>> {
    check_fence(conn, fence)?;
    let oldest = "seq = (SELECT MIN(seq) FROM effects WHERE state = 'queued')";
    let Some(mut effect) = select(conn, oldest, [])?.pop() else {
        return Ok(None);
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
        Dispatch::Sent => {}
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
        }
        Dispatch::Unknown(reason) => {
            transition(conn, &found, EffectState::OutcomeUnknown, reason, now, out)?;
        }
    }
    effect(conn, id)
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
             WHERE session_id = ?2 AND {open} AND {filter}"
        );
        let mut all: Vec<&dyn rusqlite::ToSql> = vec![&now, &session];
        all.extend_from_slice(args);
        conn.execute(&sql, all.as_slice())?;
        Ok(())
    };
    match event {
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
        Event::TurnCompleted { .. } | Event::RunCompleted { .. } => {
            confirm("kind = 'interrupt'", &[])?;
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
