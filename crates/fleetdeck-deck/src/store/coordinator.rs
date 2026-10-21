//! The receipts of `coordinator.call` (M2 P3-7b; G7, ADR-004 §9.3, §9.6).
//!
//! A station sends `coordinator.call` for a coordinator agent. The deck
//! decides it with the rules of the station's receipt store:
//!
//! 1. Another `target_incarnation` (the deck database was replaced) →
//!    `incarnation_mismatch`, not stored.
//! 2. In the decision transaction: the station must still be paired and
//!    hold the `coordinate` grant, else `forbidden`, not stored (G5).
//! 3. The grant of the caller (M3 6-5, ADR-006 §6.2 step 3): none, revoked,
//!    past `expires_at` or idle past its TTL → `forbidden`, not stored; a
//!    binding that differs from the deck's → `binding_stale`, not stored.
//!    Because this comes before the replay, a replay after a revocation
//!    never returns the old acceptance.
//!    A call without a grant id is the worker path (R2.3): only
//!    `ask_coordinator`, for a session of the calling station, at most ten
//!    per hour ([`super::coordinator_fleet::decide_worker`]); the grant
//!    checks do not apply, and the call is audited as `session:<id>`.
//! 4. A stored receipt with the same method, caller and payload hash → that
//!    receipt (also after `retry_until`); the same id with another method,
//!    caller or payload → `command_id_reused`, not stored.
//! 5. A passed `retry_until` → `expired`, not stored.
//! 6. Else the class, target and cap checks of [`super::coordinator_ops`]
//!    and the intent commit with the stored receipt; `last_used_at` moves
//!    only for an accepted new call.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::sync::atomic::Ordering;

use fleetdeck_proto::command::{CommandResult, GrantScope, SessionCommand};
use fleetdeck_proto::link::Scope;
use fleetdeck_proto::mutation::{
    AuthorityCheck, CommandAuthority, CoordinatorCallEnvelope, CoordinatorOp, Mutation,
};
use fleetdeck_proto::share::ShareKind;
use fleetdeck_proto::{CommandId, Receipt, Reject, RejectCode, SessionId, StationId};
use ring::digest::{SHA256, digest};
use rusqlite::{Connection, OptionalExtension, params};

use super::coordinator_fleet::{decide_worker, mode_refusal};
use super::coordinator_ops::{CallFlags, Context, Decision, decide, in_lineage, session_known};
use super::grant_scopes::{NOT_COVERED, expire_idle, station_covered};
use super::grants::{Grant, GrantClass, GrantState, caller_grant, get_grant};
use super::members::scope_of;
use super::{AuditResult, DeckStore, LinkV2, NewAudit, Result, call, new_id};
use crate::link::router::CommandKind;

const METHOD: &str = "coordinator.call";

/// What the audit row of a call needs, taken before the call moves.
struct AuditCall {
    actor: String,
    op: String,
    target: Option<String>,
    command_id: CommandId,
    grant: String,
}

impl AuditCall {
    fn new(station: &StationId, e: &CoordinatorCallEnvelope) -> Self {
        let op = serde_json::to_value(&e.call.op)
            .ok()
            .and_then(|v| v.get("op").and_then(|n| n.as_str().map(str::to_owned)))
            .unwrap_or_default();
        let session = |st: &StationId, s: &SessionId| Some(format!("{st}/{s}"));
        let target = match &e.call.op {
            CoordinatorOp::Send { to, .. } | CoordinatorOp::Assign { to, .. } => {
                session(&to.station, &to.session_id)
            }
            CoordinatorOp::Interrupt {
                station,
                session_id,
            }
            | CoordinatorOp::Stop {
                station,
                session_id,
            }
            | CoordinatorOp::Resume {
                station,
                session_id,
            }
            | CoordinatorOp::Rename {
                station,
                session_id,
                ..
            }
            | CoordinatorOp::SetMode {
                station,
                session_id,
                ..
            }
            | CoordinatorOp::ProjectAssign {
                station,
                session_id,
                ..
            }
            | CoordinatorOp::Answer {
                station,
                session_id,
                ..
            } => session(station, session_id),
            CoordinatorOp::ProjectStart { project_id, .. } => Some(project_id.clone()),
            other => super::coordinator_git::audit_target(other),
        };
        Self {
            actor: format!("coordinator:{station}/{}", e.call.caller),
            op,
            target,
            command_id: e.command_id.clone(),
            grant: e.call.binding.grant_id.clone(),
        }
    }
}

/// The deck setting `fleet_control` and the `coordinator.v2` state of every
/// station link, as they were when a call began (ADR-008 section 3).
#[derive(Debug, Clone, Default)]
pub(crate) struct FleetEnv {
    pub fleet_control: bool,
    pub links: BTreeMap<StationId, LinkV2>,
    /// The `fleet_git.land.v1` state of each station link (FG-10).
    pub land: BTreeMap<String, super::LinkFeature>,
    pub moves: BTreeMap<String, super::LinkFeature>,
}

impl FleetEnv {
    /// The state of the links of `station`.
    pub(crate) fn link(&self, station: &StationId) -> LinkV2 {
        self.links.get(station).copied().unwrap_or(LinkV2::Down)
    }
}

fn sha256_hex(text: &str) -> String {
    digest(&SHA256, text.as_bytes())
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

impl DeckStore {
    /// Decides one `coordinator.call` from `station` and returns its
    /// receipt (see the module rules).
    ///
    /// # Errors
    /// A database error.
    pub async fn decide_coordinator(
        &self,
        station: StationId,
        envelope: CoordinatorCallEnvelope,
    ) -> Result<Receipt> {
        let now = self.now_ms();
        let incarnation = self.incarnation().clone();
        let caller = format!("{}/{}", station, envelope.call.caller);
        let caller_session = envelope.call.caller.clone();
        let binding = envelope.call.binding.clone();
        let op = envelope.call.op.clone();
        let ctx = Context {
            sessions: self.sessions().await?,
            infos: self
                .station_infos()
                .into_iter()
                .map(|(id, (info, _))| (id, info))
                .collect(),
            buried: self.undelivered_mail().await?,
            outbox_id: new_id(self.inner.ids.as_ref()),
            env: FleetEnv::default(),
            flags: CallFlags::default(),
            land_ids: super::coordinator_git::land_ids(self, &envelope.call.op),
        };
        let audit_row = AuditCall::new(&station, &envelope);
        let other_deck = envelope.target_incarnation != incarnation;
        let worker = binding.grant_id.is_empty();
        let mutation = Mutation::Coordinator(envelope);
        let command_id = mutation.command_id().clone();
        // Another deck incarnation: the call was meant for a replaced
        // database; nothing of it is stored here.
        if other_deck && let Err(reject) = mutation.admit(now, &incarnation) {
            let receipt = Receipt::rejected(command_id, reject, now);
            if !worker {
                self.audit_call(audit_row, &receipt, &CallFlags::default())
                    .await;
            }
            return Ok(receipt);
        }
        let admitted = mutation.admit(now, &incarnation);
        let payload = sha256_hex(&mutation.canonical_payload());
        let inner = Arc::clone(&self.inner);
        let audit = (station.clone(), caller_session.clone(), command_id.clone());
        let (receipt, replay, flags) = call(&self.inner.db, move |conn| {
            // The setting and the links are read in the turn of the decision.
            let mut ctx = ctx;
            ctx.env = inner.fleet_env();
            let replay = std::cell::Cell::new(false);
            let run = || -> Result<Receipt> {
                let tx = conn.transaction()?;
                let now = super::now_ms(&inner.clock);
                let rejected = |code, message: &str| {
                    Receipt::rejected(command_id.clone(), Reject::new(code, message), now)
                };
                let scopes: Option<String> = tx
                    .query_row(
                        "SELECT scopes FROM stations WHERE station_id = ?1",
                        [station.as_str()],
                        |r| r.get(0),
                    )
                    .optional()?;
                let granted = match scopes {
                    Some(text) => {
                        serde_json::from_str::<BTreeSet<Scope>>(&text)?.contains(&Scope::Coordinate)
                    }
                    None => false,
                };
                if !granted {
                    return Ok(rejected(
                        RejectCode::Forbidden,
                        "the station is not paired or has no `coordinate` grant",
                    ));
                }
                // The exact rule, also for a worker call (design §2.3a).
                if !station_covered(&tx, &station, now)? {
                    return Ok(rejected(RejectCode::Forbidden, NOT_COVERED));
                }
                let grant = if worker {
                    None
                } else {
                    let Some(grant) = caller_grant(&tx, &station, &caller_session)? else {
                        return Ok(rejected(
                            RejectCode::Forbidden,
                            "the caller has no coordinator grant",
                        ));
                    };
                    if grant.state != GrantState::Active || now > grant.expires_at {
                        return Ok(rejected(
                            RejectCode::Forbidden,
                            "the coordinator grant is revoked or expired",
                        ));
                    }
                    if grant.binding.as_ref() != Some(&binding) {
                        return Ok(rejected(
                            RejectCode::BindingStale,
                            "the coordinator binding is not the deck's binding of the grant",
                        ));
                    }
                    if grant
                        .idle_since()
                        .is_some_and(|t| now.saturating_sub(t) > grant.idle_ttl_ms)
                    {
                        expire_idle(tx, &inner, &grant.grant_id, now)?;
                        return Ok(rejected(
                            RejectCode::Forbidden,
                            "the coordinator grant expired after its idle time",
                        ));
                    }
                    Some(grant)
                };
                let prior: Option<(String, String, String, String)> = tx
                    .query_row(
                        "SELECT method, caller, payload_sha256, receipt FROM coordinator_receipts
                     WHERE command_id = ?1",
                        [command_id.as_str()],
                        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                    )
                    .optional()?;
                if let Some((method, prior_caller, prior_payload, receipt)) = prior {
                    if method == METHOD && prior_caller == caller && prior_payload == payload {
                        replay.set(true);
                        return Ok(serde_json::from_str(&receipt)?);
                    }
                    return Ok(rejected(
                        RejectCode::CommandIdReused,
                        "this command id was used for another method, payload or caller",
                    ));
                }
                // A passed `retry_until` with no stored receipt: not stored.
                if let Err(reject) = admitted {
                    return Ok(Receipt::rejected(command_id.clone(), reject, now));
                }
                let decision = match &grant {
                    Some(grant) => decide(&tx, grant, &op, &ctx, command_id.as_str(), now)?,
                    None => {
                        let decision = decide_worker(
                            &tx,
                            &inner.ask_limits,
                            &ctx,
                            (&station, &caller_session),
                            &op,
                            command_id.as_str(),
                            now,
                        )?;
                        if matches!(decision, Decision::Accepted(_)) {
                            // The item of a worker's question names its call.
                            tx.execute(
                                "UPDATE outbox SET call_id = ?2 WHERE outbox_id = ?1",
                                params![ctx.outbox_id, command_id.as_str()],
                            )?;
                        }
                        decision
                    }
                };
                let receipt = match decision {
                    Decision::Accepted(session_id) => {
                        if let Some(grant) = &grant {
                            tx.execute(
                                "UPDATE tokens SET last_used_at = ?2 WHERE grant_id = ?1",
                                params![grant.grant_id, now],
                            )?;
                        }
                        Receipt::accepted(
                            command_id.clone(),
                            0,
                            CommandResult {
                                session_id,
                                input_id: None,
                                seq: None,
                            },
                            now,
                        )
                    }
                    Decision::Rejected(code, message) => rejected(code, &message),
                };
                tx.execute(
                    "INSERT INTO coordinator_receipts
                     (command_id, station_id, method, caller, payload_sha256, receipt, at, grant_id)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                    params![
                        command_id.as_str(),
                        station.as_str(),
                        METHOD,
                        caller,
                        payload,
                        serde_json::to_string(&receipt)?,
                        now,
                        // MCP P0 §3.4: the grant that the decision used; the
                        // worker path has none.
                        grant.as_ref().map(|g| g.grant_id.as_str())
                    ],
                )?;
                tx.commit()?;
                Ok(receipt)
            };
            let receipt = run()?;
            Ok((receipt, replay.get(), ctx.flags))
        })
        .await?;
        if flags.board.load(Ordering::SeqCst) {
            self.board_changed();
        }
        if worker {
            self.audit_worker_ask(audit, &receipt).await;
        } else if !replay {
            self.audit_call(audit_row, &receipt, &flags).await;
        }
        Ok(receipt)
    }

    /// The audit row of a call of a grant (ADR-006 §6.6, ADR-008 §5.3): the
    /// actor is the caller, the action `coordinator.<op>`, the target the
    /// session (or the project) that the op names. A replay writes none. No
    /// payload text is in the row.
    async fn audit_call(&self, call: AuditCall, receipt: &Receipt, flags: &CallFlags) {
        let note = flags
            .note
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let mut detail = note.unwrap_or_else(|| format!("grant={}", call.grant));
        let result = match &receipt.reject {
            None => AuditResult::Ok,
            Some(reject) => {
                let code = serde_json::to_value(reject.code)
                    .ok()
                    .and_then(|v| v.as_str().map(str::to_owned))
                    .unwrap_or_default();
                detail = format!("{detail} reject={code}");
                AuditResult::Refused
            }
        };
        let mut row = NewAudit::new(call.actor, format!("coordinator.{}", call.op), result)
            .detail(detail)
            .command(call.command_id);
        if let Some(target) = call.target {
            row = row.target(target);
        }
        self.audit_or_log(row).await;
    }

    /// The audit row of a worker's `ask_coordinator` (R2.3): the actor is
    /// `session:<id>`; the question is not in the row.
    async fn audit_worker_ask(
        &self,
        (station, session, command_id): (StationId, SessionId, CommandId),
        receipt: &Receipt,
    ) {
        let row = match &receipt.reject {
            None => NewAudit::new(
                format!("session:{session}"),
                "coordinator.ask",
                AuditResult::Ok,
            ),
            Some(reject) => NewAudit::new(
                format!("session:{session}"),
                "coordinator.ask",
                AuditResult::Refused,
            )
            .detail(reject.message.clone()),
        };
        self.audit_or_log(
            row.target(format!("{station}/{session}"))
                .command(command_id),
        )
        .await;
    }

    /// The number of stored coordinator receipts.
    ///
    /// # Errors
    /// A database error.
    pub async fn coordinator_receipts(&self) -> Result<u64> {
        call(&self.inner.db, |conn| {
            let n: i64 = conn.query_row("SELECT COUNT(*) FROM coordinator_receipts", [], |r| {
                r.get(0)
            })?;
            super::to_u64(n)
        })
        .await
    }
}

// ---------------------------------------------------------------------------
// Fleet control (ADR-008): the gate of a call, the target rules and the
// authority checks until the provider write.
// ---------------------------------------------------------------------------

/// True when a session holds an active grant with a mutating class (ADR-008
/// §4.2 rule 3): `state = 'active'`, not past `expires_at`; an idle-expired
/// row that was not revoked still counts (conservative). A worker grant
/// (MCP P4a W1, `kind = 'worker'`) is exempt: it does not make its session a
/// coordinator.
pub(super) fn holds_mutating_grant(
    tx: &Connection,
    station: &StationId,
    session: &SessionId,
    now: i64,
) -> Result<bool> {
    let mut stmt = tx.prepare(
        "SELECT classes FROM tokens
         WHERE station_id = ?1 AND session_id = ?2 AND state = 'active' AND ?3 <= expires_at
           AND kind = 'coordinator'",
    )?;
    let rows = stmt
        .query_map(params![station.as_str(), session.0, now], |r| {
            r.get::<_, String>(0)
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for text in rows {
        // Unknown class names fail closed: such a row counts.
        match serde_json::from_str::<std::collections::BTreeSet<GrantClass>>(&text) {
            Ok(classes) if !classes.iter().any(|c| c.is_mutating()) => {}
            _ => return Ok(true),
        }
    }
    Ok(false)
}

/// True when an active share covers the session (ADR-008 §4.2 rule 4): it
/// is not an owner session then.
pub(super) fn covered_by_share(
    tx: &Connection,
    station: &StationId,
    session: &SessionId,
    now: i64,
) -> Result<bool> {
    let mut stmt =
        tx.prepare("SELECT kind, target FROM shares WHERE revoked_at IS NULL AND expires_at > ?1")?;
    let rows = stmt
        .query_map([now], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (kind, target) in rows {
        // An unknown kind fails closed: it counts as covering.
        let Some(kind) = ShareKind::parse(&kind) else {
            return Ok(true);
        };
        if scope_of(tx, kind, &target)?.covers(station.as_str(), session.as_str()) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// The first rule of ADR-008 §4.2 that the target breaks, for `drive`,
/// `manage` and `answer` (both scopes); `None` when it passes. A session
/// that the deck does not know is `UnknownSession` (fleet scope).
pub(super) fn strict_refusal(
    tx: &Connection,
    grant: &Grant,
    (station, session): (&StationId, &SessionId),
    (must_exist, now): (bool, i64),
) -> Result<Option<(RejectCode, &'static str)>> {
    let forbidden = |m| Ok(Some((RejectCode::Forbidden, m)));
    if !grant.stations.contains(station) {
        return forbidden("the session is not on a station of the grant or not in its lineage");
    }
    if grant.scope == GrantScope::Lineage && !in_lineage(tx, grant, station, session)? {
        return forbidden("the session is not on a station of the grant or not in its lineage");
    }
    if must_exist && !session_known(tx, station, session)? {
        return Ok(Some((
            RejectCode::UnknownSession,
            "no such session on that station",
        )));
    }
    if station == &grant.station_id && session == &grant.session_id {
        return forbidden("a coordinator does not act on its own session");
    }
    if holds_mutating_grant(tx, station, session, now)? {
        return forbidden("the session is a coordinator; a coordinator never acts for another");
    }
    if covered_by_share(tx, station, session, now)? {
        return forbidden("the session is shared; a coordinator acts only on an owner session");
    }
    Ok(None)
}

/// [`strict_refusal`] as a decision. `must_exist`: the deck read model must
/// know the session.
pub(super) fn strict_target(
    tx: &Connection,
    grant: &Grant,
    target: (&StationId, &SessionId),
    (must_exist, now): (bool, i64),
) -> Result<Option<Decision>> {
    Ok(strict_refusal(tx, grant, target, (must_exist, now))?
        .map(|(code, m)| Decision::Rejected(code, m.to_owned())))
}

/// The session that a command acts on when it is a `drive`, `manage` or
/// `answer` command: the target rules of ADR-008 section 4.2 apply to it.
#[must_use]
pub(crate) fn strict_session(command: &SessionCommand) -> Option<&SessionId> {
    match command {
        SessionCommand::Interrupt { session_id }
        | SessionCommand::Stop { session_id, .. }
        | SessionCommand::Resume { session_id }
        | SessionCommand::Rename { session_id, .. }
        | SessionCommand::SetMode { session_id, .. }
        | SessionCommand::Answer { session_id, .. } => Some(session_id),
        _ => None,
    }
}

/// [`strict_session`] of an outbox payload (a router command kind as JSON).
#[must_use]
pub(crate) fn strict_session_of(payload: &str) -> Option<SessionId> {
    match serde_json::from_str::<CommandKind>(payload).ok()? {
        CommandKind::Session { command } => strict_session(&command).cloned(),
        CommandKind::Configure { .. } => None,
    }
}

/// The result of a recheck of an authority (ADR-008 section 6.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Check {
    /// Every deck-side check holds.
    Valid,
    /// The authority is gone; the reason is a stable string.
    Lost(&'static str),
    /// A station has no live link: the send waits for it.
    Wait,
}

/// The grant of `a`, or why it no longer backs the authority. These are the
/// stable reasons of `coordinator.authority_check`, in their order.
pub(crate) fn grant_state(
    tx: &Connection,
    a: &CommandAuthority,
    now: i64,
) -> Result<std::result::Result<Grant, &'static str>> {
    let Some(grant) = get_grant(tx, &a.grant_id)? else {
        return Ok(Err("grant_revoked"));
    };
    if grant.state == GrantState::Revoked {
        return Ok(Err("grant_revoked"));
    }
    if grant.station_id != a.caller_station || grant.session_id != a.caller_session {
        return Ok(Err("caller_unbound"));
    }
    if grant.binding.as_ref().map(|b| b.generation) != Some(a.generation) {
        return Ok(Err("generation_changed"));
    }
    if grant.state != GrantState::Active || now > grant.expires_at {
        return Ok(Err("grant_expired"));
    }
    Ok(Ok(grant))
}

/// The deck-side checks before a send, a resend and `start_attempt`
/// (ADR-008 section 6.2): the grant is active and bound at the generation,
/// the deck setting is on, no link lacks `coordinator.v2`, and the target
/// of a `drive`, `manage` or `answer` command still passes section 4.2.
/// `route` is the station that gets the command and the session it acts on.
pub(crate) fn recheck(
    tx: &Connection,
    env: &FleetEnv,
    a: &CommandAuthority,
    route: (&StationId, Option<&SessionId>),
    now: i64,
) -> Result<Check> {
    let grant = match grant_state(tx, a, now)? {
        Ok(grant) => grant,
        Err(reason) => return Ok(Check::Lost(reason)),
    };
    if !env.fleet_control {
        return Ok(Check::Lost("fleet_control_off"));
    }
    let (station, session) = route;
    if !grant.stations.contains(station) {
        return Ok(Check::Lost("target_excluded"));
    }
    if [&a.caller_station, station]
        .iter()
        .any(|s| env.link(s) == LinkV2::Without)
    {
        return Ok(Check::Lost("no_v2"));
    }
    if let Some(session) = session
        && strict_refusal(tx, &grant, (station, session), (false, now))?.is_some()
    {
        return Ok(Check::Lost("target_excluded"));
    }
    if [&a.caller_station, station]
        .iter()
        .any(|s| env.link(s) == LinkV2::Down)
    {
        return Ok(Check::Wait);
    }
    Ok(Check::Valid)
}

/// What the deck knows alone about an `authority_check`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AuthorityLocal {
    /// The target breaks a rule of ADR-008 section 4.2.
    pub target_excluded: bool,
}

impl DeckStore {
    /// The deck-local part of `coordinator.authority_check` (ADR-008 section
    /// 6.2): the stable reason of the first check that fails, or the facts
    /// for the steps that need the links. The command must be one that the
    /// deck sent to `station` with exactly this authority and this target.
    ///
    /// # Errors
    /// A database error.
    pub async fn authority_local(
        &self,
        station: StationId,
        check: AuthorityCheck,
    ) -> Result<std::result::Result<AuthorityLocal, &'static str>> {
        let on = self.fleet_control();
        let clock = self.inner.clock.clone();
        call(&self.inner.db, move |conn| {
            let a = CommandAuthority {
                grant_id: check.grant_id.clone(),
                caller_station: check.caller_station.clone(),
                caller_session: check.caller_session.clone(),
                generation: check.generation,
            };
            let tx = conn.transaction()?;
            let row: Option<(String, Option<String>, Option<String>, String)> = tx
                .query_row(
                    "SELECT payload, authority, required_classes, state FROM outbox
                     WHERE command_id = ?1 AND station_id = ?2",
                    params![check.command_id.as_str(), station.as_str()],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                )
                .optional()?;
            let now = super::now_ms(&clock);
            let grant = match grant_state(&tx, &a, now)? {
                Ok(grant) => grant,
                Err(reason) => return Ok(Err(reason)),
            };
            if !station_covered(&tx, &station, now)? {
                return Ok(Err(NOT_COVERED));
            }
            if !on {
                return Ok(Err("fleet_control_off"));
            }
            let Some((payload, authority, required, state)) = row else {
                // A land command is in `land_queue` (FG-8, design §11.2).
                return super::session_moves::authority_local(&tx, &station, &check, &grant, now);
            };
            let same = authority
                .and_then(|t| serde_json::from_str::<CommandAuthority>(&t).ok())
                .is_some_and(|x| x == a);
            let kind: CommandKind = serde_json::from_str(&payload)?;
            let CommandKind::Session { command } = kind else {
                return Ok(Err("unknown_command"));
            };
            let same_target = command
                .session_id()
                .is_none_or(|s| s == &check.target_session);
            if !same || !same_target {
                return Ok(Err("unknown_command"));
            }
            // The deck gave the item up (a narrowing, a revoke, its expiry):
            // a class that comes back later never revives it (MCP P4b §7.2).
            if state == "dead" {
                return Ok(Err("command_canceled"));
            }
            // A narrowing removed a class of the call (MCP P4b §7.1).
            if !grant_narrowing::required_hold(required.as_deref(), &grant.classes) {
                return Ok(Err("class_removed"));
            }
            // A command that launches a provider needs the real launch mode.
            // A station that sends none is older than the deck (version
            // skew): fail closed (ADR-008 section 17, round 2).
            let launches = matches!(
                command,
                SessionCommand::Start { .. }
                    | SessionCommand::Resume { .. }
                    | SessionCommand::Import { .. }
                    | SessionCommand::SetMode { .. }
            );
            if launches && check.launch.is_none() {
                return Ok(Err("launch_mode_missing"));
            }
            if let Some(launch) = &check.launch {
                let family = serde_json::to_value(launch.provider)
                    .ok()
                    .and_then(|v| v.as_str().map(str::to_owned));
                if mode_refusal(&grant, family.as_deref(), launch.mode.as_deref()).is_some() {
                    return Ok(Err("mode_not_allowed"));
                }
            }
            let target_excluded = match strict_session(&command) {
                Some(s) => strict_refusal(&tx, &grant, (&station, s), (false, now))?.is_some(),
                None => false,
            };
            Ok(Ok(AuthorityLocal { target_excluded }))
        })
        .await
    }
}

#[path = "coordinator_gate.rs"]
mod gate;
#[path = "grant_narrowing.rs"]
pub(super) mod grant_narrowing;
pub(super) use gate::{fleet_gate, v2_refusal};

#[cfg(test)]
#[path = "coordinator_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "coordinator_p0_tests.rs"]
mod p0_tests;
