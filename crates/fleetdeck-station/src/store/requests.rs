//! Runtime requests (approvals, questions; PLAN §6.4, §6.5, L3; M2 P2-5).
//!
//! The table follows the event stream: `approval.requested` opens a row with
//! the provider's own option ids and the target process, `approval.resolved`
//! and `approval.expired` close it. A command that answers a request records
//! the first answer as the request's receipt in the decision's transaction:
//! a second answer, also with another `command_id`, gets that receipt back
//! and sends nothing (idempotent answers, L3).
//!
//! `deny_after` (§6.5): `deny_at` is set when the request opens. The session
//! manager calls [`super::Store::deny_overdue`] while no deck is connected; it
//! answers each overdue request with the provider's deny option and
//! `by: timeout`, through the effect outbox.

use fleetdeck_core::IdSource;
use fleetdeck_proto::command::ApprovalDecision;
use fleetdeck_proto::event::{
    ApprovalOption, ExpiredReason, RequestKind, ResolvedBy, ResponseChannel,
};
use fleetdeck_proto::{CommandId, Event, EventBody, EventEnvelope, NodeId, RequestId, SessionId};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use super::effects::{self, Effect, EffectPayload, ProcessIncarnation};
use super::{OfflineApprovalPolicy, Result, StoreError, log, to_i64};

/// The option ids that deny, in order of preference (Claude `deny`; Codex
/// `decline` / `cancel`; ACP-style `reject`). A request without one of them
/// cannot be denied automatically and stays open (fail closed: no allow).
pub const DENY_OPTION_IDS: [&str; 4] = ["deny", "decline", "reject", "cancel"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestState {
    Open,
    Resolved,
    Expired,
}

/// The first answer to a request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnswerReceipt {
    pub command_id: Option<CommandId>,
    pub option_id: String,
    pub by: ResolvedBy,
    pub at: i64,
}

/// One `runtime_requests` row.
#[derive(Debug, Clone, PartialEq)]
pub struct RuntimeRequest {
    pub request_id: RequestId,
    pub session_id: SessionId,
    pub process_incarnation: Option<ProcessIncarnation>,
    pub node_id: Option<NodeId>,
    pub kind: RequestKind,
    pub options: Vec<ApprovalOption>,
    pub response: ResponseChannel,
    pub state: RequestState,
    pub receipt: Option<AnswerReceipt>,
    pub deny_at: Option<i64>,
    pub requested_at: i64,
    /// `requested_at` is the proven request time (DL-108).
    pub time_proven: bool,
}

impl RuntimeRequest {
    /// The provider's deny option, if the request has one.
    #[must_use]
    pub fn deny_option(&self) -> Option<&ApprovalOption> {
        DENY_OPTION_IDS
            .iter()
            .find_map(|id| self.options.iter().find(|o| o.option_id == *id))
    }
}

/// The result of [`claim_answer`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AnswerClaim {
    /// This answer is the first: the caller enqueues the answer effect.
    First,
    /// An earlier answer exists: return its receipt and send nothing.
    Repeated(AnswerReceipt),
    /// The request is resolved or expired without an answer of ours.
    Closed(RequestState),
    UnknownRequest,
    UnknownOption,
}

fn json<T: Serialize>(v: &T) -> Result<String> {
    Ok(serde_json::to_string(v)?)
}

fn enum_name<T: Serialize>(v: &T) -> Result<String> {
    match serde_json::to_value(v)? {
        serde_json::Value::String(s) => Ok(s),
        other => Ok(other.to_string()),
    }
}

fn enum_of<T: for<'de> Deserialize<'de>>(s: &str) -> Result<T> {
    Ok(serde_json::from_value(serde_json::Value::String(
        s.to_owned(),
    ))?)
}

type RawRow = (
    String,
    String,
    Option<String>,
    Option<String>,
    String,
    String,
    String,
    String,
    Option<String>,
    Option<i64>,
    i64,
    bool,
);

const COLUMNS: &str = "request_id, session_id, process_incarnation, node_id, kind, options,
    response_channel, state, receipt, deny_at, requested_at, time_proven";

fn row(r: &rusqlite::Row<'_>) -> rusqlite::Result<RawRow> {
    Ok((
        r.get(0)?,
        r.get(1)?,
        r.get(2)?,
        r.get(3)?,
        r.get(4)?,
        r.get(5)?,
        r.get(6)?,
        r.get(7)?,
        r.get(8)?,
        r.get(9)?,
        r.get(10)?,
        r.get(11)?,
    ))
}

fn parse(raw: RawRow) -> Result<RuntimeRequest> {
    let (id, session, process, node, kind, options, channel, state, receipt, deny_at, at, proven) =
        raw;
    Ok(RuntimeRequest {
        request_id: id.into(),
        session_id: session.into(),
        process_incarnation: process.map(ProcessIncarnation::from),
        node_id: node.map(NodeId::from),
        kind: enum_of(&kind)?,
        options: serde_json::from_str(&options)?,
        response: enum_of(&channel)?,
        state: enum_of(&state)?,
        receipt: receipt.map(|r| serde_json::from_str(&r)).transpose()?,
        deny_at,
        requested_at: at,
        time_proven: proven,
    })
}

fn select(
    conn: &Connection,
    filter: &str,
    args: impl rusqlite::Params,
) -> Result<Vec<RuntimeRequest>> {
    let sql = format!("SELECT {COLUMNS} FROM runtime_requests WHERE {filter} ORDER BY rowid");
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(args, row)?;
    rows.map(|r| r.map_err(StoreError::from).and_then(parse))
        .collect()
}

pub(super) fn request(conn: &Connection, id: &RequestId) -> Result<Option<RuntimeRequest>> {
    Ok(select(conn, "request_id = ?1", [id.as_str()])?.pop())
}

fn policy(conn: &Connection, session: &SessionId) -> Result<OfflineApprovalPolicy> {
    let text: String = conn
        .query_row(
            "SELECT offline_approval_policy FROM sessions WHERE session_id = ?1",
            [session.as_str()],
            |r| r.get(0),
        )
        .optional()?
        .ok_or_else(|| StoreError::UnknownSession(session.clone()))?;
    Ok(serde_json::from_str(&text)?)
}

/// The `deny_after` deadline of a request that opened at `requested_at`.
/// A request without a proven time is due at once (ADR-005 §6; DL-108).
fn deadline(policy: OfflineApprovalPolicy, requested_at: i64, proven: bool) -> Result<Option<i64>> {
    Ok(match policy {
        OfflineApprovalPolicy::Wait => None,
        OfflineApprovalPolicy::DenyAfter { .. } if !proven => Some(requested_at),
        OfflineApprovalPolicy::DenyAfter { after_ms } => {
            Some(requested_at.saturating_add(to_i64(after_ms)?))
        }
    })
}

/// The time of the output that carried an event (ADR-005 §6; DL-108).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RequestTime {
    /// A live read: the transaction time.
    #[default]
    Now,
    /// A replayed or late commit of output that the shim read at this time.
    Read(i64),
    /// No proven time: a `deny_after` request is due at once.
    Unprovable,
}

impl RequestTime {
    /// The stored request time and whether it is proven. A read time is
    /// never later than the transaction time.
    fn at(self, now: i64) -> (i64, bool) {
        match self {
            Self::Now => (now, true),
            Self::Read(at) => (at.min(now), true),
            Self::Unprovable => (now, false),
        }
    }
}

/// True when `req` passed its own deadline at `now`, so an allow answer must
/// not go out (ADR-005 §6: queued allows are rechecked before dispatch).
fn past_deadline(conn: &Connection, req: &RuntimeRequest, now: i64) -> Result<bool> {
    let due = deadline(
        policy(conn, &req.session_id)?,
        req.requested_at,
        req.time_proven,
    )?;
    Ok(due.is_some_and(|at| at <= now))
}

fn is_deny(option_id: &str) -> bool {
    DENY_OPTION_IDS.contains(&option_id)
}

/// Keeps the table in step with the event stream (inside the append).
pub(super) fn observe(
    conn: &Connection,
    envelope: &EventEnvelope,
    now: i64,
    time: RequestTime,
) -> Result<()> {
    let EventBody::Known(event) = &envelope.event else {
        return Ok(());
    };
    let session = &envelope.session_id;
    match event {
        Event::ApprovalRequested {
            request_id,
            node_id,
            request_kind,
            options,
            response,
            ..
        } => {
            let (requested_at, proven) = time.at(now);
            let deny_at = deadline(policy(conn, session)?, requested_at, proven)?;
            let process = effects::live_process(conn, session)?;
            conn.execute(
                "INSERT INTO runtime_requests (request_id, session_id, process_incarnation,
                     node_id, kind, options, response_channel, state, deny_at, requested_at,
                     time_proven, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'open', ?8, ?9, ?10, ?11)
                 ON CONFLICT (request_id) DO NOTHING",
                params![
                    request_id.as_str(),
                    session.as_str(),
                    process.as_ref().map(ProcessIncarnation::as_str),
                    node_id.as_ref().map(NodeId::as_str),
                    enum_name(request_kind)?,
                    json(options)?,
                    enum_name(response)?,
                    deny_at,
                    requested_at,
                    proven,
                    now,
                ],
            )?;
        }
        Event::ApprovalResolved {
            request_id,
            option_id,
            by,
        } => {
            let receipt = json(&AnswerReceipt {
                command_id: None,
                option_id: option_id.clone(),
                by: *by,
                at: now,
            })?;
            conn.execute(
                "UPDATE runtime_requests
                 SET state = 'resolved', receipt = COALESCE(receipt, ?2), updated_at = ?3
                 WHERE request_id = ?1 AND state = 'open'",
                params![request_id.as_str(), receipt, now],
            )?;
        }
        Event::ApprovalExpired { request_id, .. } => {
            conn.execute(
                "UPDATE runtime_requests SET state = 'expired', updated_at = ?2
                 WHERE request_id = ?1 AND state = 'open'",
                params![request_id.as_str(), now],
            )?;
        }
        _ => {}
    }
    Ok(())
}

/// Records the first answer of a request (see [`AnswerClaim`]).
pub(super) fn claim_answer(
    conn: &Connection,
    request_id: &RequestId,
    option_id: &str,
    by: ResolvedBy,
    command_id: Option<&CommandId>,
    now: i64,
) -> Result<AnswerClaim> {
    let Some(req) = request(conn, request_id)? else {
        return Ok(AnswerClaim::UnknownRequest);
    };
    if let Some(receipt) = req.receipt {
        return Ok(AnswerClaim::Repeated(receipt));
    }
    if req.state != RequestState::Open {
        return Ok(AnswerClaim::Closed(req.state));
    }
    if !req.options.iter().any(|o| o.option_id == option_id) {
        return Ok(AnswerClaim::UnknownOption);
    }
    if !is_deny(option_id) && past_deadline(conn, &req, now)? {
        // The deny sweep answers it; a late allow never goes out.
        return Ok(AnswerClaim::Closed(RequestState::Expired));
    }
    let receipt = AnswerReceipt {
        command_id: command_id.cloned(),
        option_id: option_id.to_owned(),
        by,
        at: now,
    };
    conn.execute(
        "UPDATE runtime_requests SET receipt = ?2, deny_at = NULL, updated_at = ?3
         WHERE request_id = ?1",
        params![request_id.as_str(), json(&receipt)?, now],
    )?;
    Ok(AnswerClaim::First)
}

/// Opens the answer slot of a request again after its answer effect was
/// retired as not dispatched by the live worker (`settle`, fenced, from
/// `claimed`): the provider got nothing, so a new command can answer. Called
/// in the settlement's transaction. Only the claim of that very effect is
/// released: the request is open, belongs to the effect's session and
/// process, and its receipt has the effect's command id, option and `by`.
/// Anything else changes nothing. `deny_at` returns to the request's own
/// deadline (`requested_at` plus the session's `deny_after`), not a new one.
/// The command's receipt and the retired effect stay as they are.
pub(super) fn release_unsent_answer(
    conn: &Connection,
    effect: &Effect,
    request_id: &RequestId,
    option_id: &str,
    by: ResolvedBy,
    now: i64,
) -> Result<bool> {
    let Some(req) = request(conn, request_id)? else {
        return Ok(false);
    };
    let ours = req.receipt.as_ref().is_some_and(|r| {
        r.command_id == effect.command_id && r.option_id == option_id && r.by == by
    });
    if req.state != RequestState::Open
        || req.session_id != effect.session_id
        || req.process_incarnation != effect.process_incarnation
        || !ours
    {
        return Ok(false);
    }
    let deny_at = deadline(
        policy(conn, &req.session_id)?,
        req.requested_at,
        req.time_proven,
    )?;
    conn.execute(
        "UPDATE runtime_requests SET receipt = NULL, deny_at = ?2, updated_at = ?3
         WHERE request_id = ?1 AND state = 'open'",
        params![request_id.as_str(), deny_at, now],
    )?;
    Ok(true)
}

/// Expires the open requests of a process that is gone (or whose response
/// channel did not survive a station restart).
pub(super) fn expire_open(
    conn: &Connection,
    filter: &str,
    args: impl rusqlite::Params,
    reason: ExpiredReason,
    now: i64,
    out: &mut Vec<EventEnvelope>,
) -> Result<usize> {
    let open = select(conn, &format!("state = 'open' AND ({filter})"), args)?;
    for req in &open {
        let event = Event::ApprovalExpired {
            request_id: req.request_id.clone(),
            reason,
        };
        out.push(log::append_one(conn, &req.session_id, event.into(), now)?);
    }
    Ok(open.len())
}

/// The `deny_after` sweep (§6.5): one answer effect per overdue request.
pub(super) fn deny_overdue(
    conn: &Connection,
    ids: &dyn IdSource,
    now: i64,
    out: &mut Vec<EventEnvelope>,
) -> Result<Vec<Effect>> {
    let due = select(
        conn,
        "state = 'open' AND receipt IS NULL AND deny_at IS NOT NULL AND deny_at <= ?1",
        [now],
    )?;
    let mut sent = Vec::new();
    for req in due {
        let live = req.process_incarnation.is_some()
            && req.process_incarnation == effects::live_process(conn, &req.session_id)?;
        let Some(deny) = req.deny_option().filter(|_| live).cloned() else {
            conn.execute(
                "UPDATE runtime_requests SET deny_at = NULL, updated_at = ?2 WHERE request_id = ?1",
                params![req.request_id.as_str(), now],
            )?;
            if live {
                let event = Event::Error {
                    code: "offline_deny_impossible".to_owned(),
                    message: format!(
                        "request {} has no deny option; it stays open for the owner",
                        req.request_id
                    ),
                    retry_at: None,
                };
                out.push(log::append_one(conn, &req.session_id, event.into(), now)?);
            }
            continue;
        };
        claim_answer(
            conn,
            &req.request_id,
            &deny.option_id,
            ResolvedBy::Timeout,
            None,
            now,
        )?;
        sent.push(effects::enqueue(
            conn,
            ids,
            now,
            None,
            &req.session_id,
            EffectPayload::Answer {
                request_id: req.request_id.clone(),
                decision: ApprovalDecision {
                    option_id: deny.option_id,
                    updated_input: None,
                    text: Some("denied by the station: the deck was not reachable".to_owned()),
                },
                by: ResolvedBy::Timeout,
            },
        )?);
    }
    Ok(sent)
}

/// The claim-time recheck of a queued answer effect (ADR-005 §6; DL-108):
/// an allow for a request past its own deadline is retired as not
/// dispatched, and its answer slot opens again so the deny sweep sends the
/// safe deny. `true` when the effect was retired.
pub(super) fn retire_expired_allow(
    conn: &Connection,
    effect: &Effect,
    now: i64,
    out: &mut Vec<EventEnvelope>,
) -> Result<bool> {
    let EffectPayload::Answer {
        request_id,
        decision,
        by,
    } = &effect.payload
    else {
        return Ok(false);
    };
    if is_deny(&decision.option_id) {
        return Ok(false);
    }
    let Some(req) = request(conn, request_id)? else {
        return Ok(false);
    };
    if req.state != RequestState::Open || !past_deadline(conn, &req, now)? {
        return Ok(false);
    }
    effects::transition(
        conn,
        effect,
        effects::EffectState::RetiredNotDispatched,
        REASON_REQUEST_EXPIRED,
        now,
        out,
    )?;
    release_unsent_answer(conn, effect, request_id, &decision.option_id, *by, now)?;
    Ok(true)
}

/// The reason of an allow that was not sent because its request expired.
pub const REASON_REQUEST_EXPIRED: &str = "request_expired";
