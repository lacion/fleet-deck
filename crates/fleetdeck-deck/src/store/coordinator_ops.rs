//! Authorization and intent of one coordinator operation (M3 6-5, ADR-006
//! §6.2 step 6, §6.3, §6.4). Everything here runs inside the decision
//! transaction of [`super::coordinator`]: the class and target checks, the
//! spawn-cap check and increment, and the intent rows (outbox item, mail,
//! task, lineage) commit together with the receipt.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;

use fleetdeck_proto::RejectCode;
use fleetdeck_proto::board::DeckSession;
use fleetdeck_proto::command::{
    SessionCommand, SessionSpec, StopMode, UserInput, WorkspaceRequest,
};
use fleetdeck_proto::link::StationInfo;
use fleetdeck_proto::mutation::{CoordinatorDelivery, CoordinatorOp, CoordinatorTarget};
use fleetdeck_proto::orchestration::{
    ASSIGNMENT_PREFIX, MAX_TASK_CHARS, TaskCompletion, TaskState, mail_frame, task_frame,
};
use fleetdeck_proto::{ProviderInstanceId, SessionId, StationId};
use rusqlite::{Connection, OptionalExtension, params};

use super::Result;
use super::coordinator::FleetEnv;
use super::coordinator_fleet::{self, ask, send_group};
use super::fleet::write_completion;
use super::grants::{Grant, GrantClass};
use super::outbox::{NewOutboxItem, insert_item};
use crate::link::router::CommandKind;
use crate::routing::{Needs, pick_session};

/// How long a coordinator intent waits in the deck outbox: 24 h.
pub(super) const INTENT_TTL_MS: i64 = 24 * 60 * 60 * 1000;
/// How long an intent of a fleet-control grant waits: 10 minutes (ADR-008
/// section 6.1).
pub(super) const FLEET_INTENT_TTL_MS: i64 = 10 * 60 * 1000;

/// Claude native modes, most restrictive first (the provider adapter's
/// `NATIVE_MODES`).
const CLAUDE_MODES: &[&str] = &[
    "default",
    "plan",
    "acceptEdits",
    "dontAsk",
    "auto",
    "bypassPermissions",
];
/// Codex approval policies and sandboxes, most restrictive first.
const CODEX_POLICIES: &[&str] = &["untrusted", "on-request", "never"];
const CODEX_SANDBOXES: &[&str] = &["read-only", "workspace-write", "danger-full-access"];
/// The mode of a Codex start without a mode (the adapter's default).
const CODEX_DEFAULT: &str = "untrusted/workspace-write";

/// The facts that the decision reads before its transaction.
pub(crate) struct Context {
    pub sessions: Vec<DeckSession>,
    pub infos: BTreeMap<StationId, StationInfo>,
    pub buried: BTreeMap<SessionId, u32>,
    pub outbox_id: String,
    /// The deck setting `fleet_control` and the `coordinator.v2` state of the
    /// links when the call began (ADR-008 section 3).
    pub env: FleetEnv,
    /// What the decision tells the caller of the transaction.
    pub flags: CallFlags,
    /// The new land id and command id of a `Land` op (FG-10).
    pub land_ids: Option<(String, String)>,
}

/// Facts that a decision hands back to [`super::coordinator`]: a change of
/// the board view and more audit detail.
#[derive(Debug, Default)]
pub(crate) struct CallFlags {
    /// The board view changed (`project_assign`); the store tells the board
    /// after the commit.
    pub board: AtomicBool,
    /// More audit detail (an answer: the request and the option kind).
    pub note: Mutex<Option<String>>,
}

/// The result of step 6.
pub(crate) enum Decision {
    Accepted(Option<SessionId>),
    Rejected(RejectCode, String),
}

fn no(code: RejectCode, message: impl Into<String>) -> Result<Decision> {
    Ok(Decision::Rejected(code, message.into()))
}

/// The classes that an op needs (§6.3, ADR-008 §4.1): all of them.
pub(super) fn classes_of(op: &CoordinatorOp) -> BTreeSet<GrantClass> {
    let classes = match op {
        CoordinatorOp::Send { .. }
        | CoordinatorOp::Assign { .. }
        | CoordinatorOp::AssignAuto { .. }
        | CoordinatorOp::SendGroup { .. }
        | CoordinatorOp::AskCoordinator { .. }
        | CoordinatorOp::CompleteTask { .. } => vec![GrantClass::Mail],
        CoordinatorOp::Interrupt { .. } | CoordinatorOp::Stop { .. } => vec![GrantClass::Drive],
        CoordinatorOp::Spawn { .. } => vec![GrantClass::Spawn],
        CoordinatorOp::Resume { .. }
        | CoordinatorOp::Rename { .. }
        | CoordinatorOp::SetMode { .. }
        | CoordinatorOp::ProjectAssign { .. } => vec![GrantClass::Manage],
        CoordinatorOp::ProjectStart { .. } => vec![GrantClass::Spawn, GrantClass::Manage],
        CoordinatorOp::Answer { .. } => vec![GrantClass::Answer],
        CoordinatorOp::Land { .. } | CoordinatorOp::Move { .. } => vec![GrantClass::Manage],
    };
    classes.into_iter().collect()
}

/// True for a mode that skips the permission checks (ADR-008 §5.2): Claude
/// `bypassPermissions`, or Codex approval `never` with the sandbox
/// `danger-full-access`. No coordinator operation starts, resumes or sets a
/// session into one.
#[must_use]
pub fn is_bypass(family: &str, mode: Option<&str>) -> bool {
    match family {
        "claude" => mode == Some("bypassPermissions"),
        "codex" => mode
            .and_then(|m| m.split_once('/'))
            .is_some_and(|(policy, sandbox)| policy == "never" && sandbox == "danger-full-access"),
        _ => false,
    }
}

/// Is `mode` (or the provider default) within `ceiling` for `family`?
/// Unknown families and unknown modes fail closed. No order across
/// families is inferred (ADR-006 §4).
#[must_use]
pub fn mode_within(family: &str, mode: Option<&str>, ceiling: &str) -> bool {
    let at = |list: &[&str], m: &str| list.iter().position(|x| *x == m);
    match family {
        "claude" => {
            let m = mode.unwrap_or("default");
            matches!((at(CLAUDE_MODES, m), at(CLAUDE_MODES, ceiling)), (Some(a), Some(b)) if a <= b)
        }
        "codex" => {
            let parse = |s: &str| {
                let (p, b) = s.split_once('/')?;
                Some((at(CODEX_POLICIES, p)?, at(CODEX_SANDBOXES, b)?))
            };
            match (parse(mode.unwrap_or(CODEX_DEFAULT)), parse(ceiling)) {
                (Some((p, s)), Some((cp, cs))) => p <= cp && s <= cs,
                _ => false,
            }
        }
        _ => false,
    }
}

/// Whether `ceiling` is a valid mode ceiling for `family` (MCP P0 §2.2 rule 5):
/// a known mode of the family that is not a bypass mode.
#[must_use]
pub fn valid_ceiling(family: &str, ceiling: &str) -> bool {
    mode_within(family, Some(ceiling), ceiling) && !is_bypass(family, Some(ceiling))
}

pub(super) fn family_of(
    info: Option<&StationInfo>,
    instance: &ProviderInstanceId,
) -> Option<String> {
    let p = info?.providers.iter().find(|p| &p.instance == instance)?;
    serde_json::to_value(p.provider)
        .ok()?
        .as_str()
        .map(str::to_owned)
}

pub(super) fn session_known(
    tx: &Connection,
    station: &StationId,
    session: &SessionId,
) -> Result<bool> {
    Ok(tx
        .query_row(
            "SELECT 1 FROM sessions WHERE station_id = ?1 AND session_id = ?2",
            params![station.as_str(), session.0],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

pub(super) fn enqueue(
    tx: &Connection,
    ctx: &Context,
    station: &StationId,
    command: SessionCommand,
    now: i64,
) -> Result<()> {
    enqueue_as(tx, &ctx.outbox_id, station, command, now)
}

pub(super) fn enqueue_as(
    tx: &Connection,
    outbox_id: &str,
    station: &StationId,
    command: SessionCommand,
    now: i64,
) -> Result<()> {
    let item = NewOutboxItem {
        station_id: station.clone(),
        origin: "coordinator".to_owned(),
        payload: serde_json::to_value(CommandKind::Session { command })?,
        expires_at: now.saturating_add(INTENT_TTL_MS),
    };
    insert_item(tx, outbox_id, &item, now)?;
    Ok(())
}

pub(super) fn check_text(text: &str) -> Option<Decision> {
    let t = text.trim();
    (t.is_empty() || t.chars().count() > MAX_TASK_CHARS).then(|| {
        Decision::Rejected(
            RejectCode::InvalidCommand,
            format!("the text needs 1 to {MAX_TASK_CHARS} characters"),
        )
    })
}

/// The checks and the intent of one op for `grant` (§6.2 step 6). An
/// accepted call marks its outbox items with the call and, for a
/// fleet-control grant, with the authority and the 10-minute expiry (ADR-008
/// §6.1).
pub(crate) fn decide(
    tx: &Connection,
    grant: &Grant,
    op: &CoordinatorOp,
    ctx: &Context,
    command_id: &str,
    now: i64,
) -> Result<Decision> {
    let decision = decide_op(tx, grant, op, ctx, command_id, now)?;
    if matches!(decision, Decision::Accepted(_)) {
        let fleet = grant.is_fleet_control();
        let authority = match (fleet, grant.authority()) {
            (true, Some(a)) => Some(serde_json::to_string(&a)?),
            _ => None,
        };
        // The full class set that admitted the call (MCP P4b §7.1).
        let required = authority
            .as_ref()
            .map(|_| serde_json::to_string(&classes_of(op)))
            .transpose()?;
        let ttl = if fleet {
            FLEET_INTENT_TTL_MS
        } else {
            INTENT_TTL_MS
        };
        // The item of the call and the items of a group mail (`<id>-<n>`).
        tx.execute(
            "UPDATE outbox SET call_id = ?2, authority = ?3, required_classes = ?5,
                 expires_at = MIN(expires_at, created_at + ?4)
             WHERE outbox_id = ?1 OR substr(outbox_id, 1, length(?1) + 1) = ?1 || '-'",
            params![ctx.outbox_id, command_id, authority, ttl, required],
        )?;
    }
    Ok(decision)
}

fn decide_op(
    tx: &Connection,
    grant: &Grant,
    op: &CoordinatorOp,
    ctx: &Context,
    command_id: &str,
    now: i64,
) -> Result<Decision> {
    let required = classes_of(op);
    if let Some(class) = required.iter().find(|c| !grant.classes.contains(c)) {
        return no(
            RejectCode::Forbidden,
            format!("the grant has no `{class:?}` class"),
        );
    }
    if let Some(d) = super::coordinator::fleet_gate(grant, op, ctx) {
        return Ok(d);
    }
    let sender = format!("coordinator {}", grant.session_id);
    match op {
        CoordinatorOp::Send { to, text, delivery } => {
            if let Some(d) = check_target(tx, grant, to)? {
                return Ok(d);
            }
            if let Some(d) = check_text(text) {
                return Ok(d);
            }
            let input = UserInput::new(mail_frame(&sender, text.trim()));
            let session_id = to.session_id.clone();
            let (command, name) = match delivery {
                CoordinatorDelivery::FollowUp => {
                    (SessionCommand::FollowUp { session_id, input }, "follow_up")
                }
                CoordinatorDelivery::Steer => {
                    (SessionCommand::Steer { session_id, input }, "steer")
                }
            };
            enqueue(tx, ctx, &to.station, command, now)?;
            tx.execute(
                "INSERT INTO mail (mail_id, sender, station_id, session_id, delivery, text,
                     outbox_id, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    command_id,
                    sender,
                    to.station.as_str(),
                    to.session_id.0,
                    name,
                    text.trim(),
                    ctx.outbox_id,
                    now
                ],
            )?;
            Ok(Decision::Accepted(Some(to.session_id.clone())))
        }
        CoordinatorOp::Assign { task, to } => {
            if let Some(d) = check_target(tx, grant, to)? {
                return Ok(d);
            }
            if let Some(d) = check_text(task) {
                return Ok(d);
            }
            assign_to(
                tx,
                ctx,
                grant,
                &sender,
                command_id,
                task,
                &to.station,
                &to.session_id,
                now,
            )?;
            Ok(Decision::Accepted(Some(to.session_id.clone())))
        }
        CoordinatorOp::SendGroup {
            group,
            text,
            delivery,
        } => send_group(tx, grant, ctx, (*group, text, *delivery), command_id, now),
        CoordinatorOp::AskCoordinator { question } => {
            ask(tx, grant, ctx, question, command_id, now)
        }
        CoordinatorOp::CompleteTask {
            task_id,
            result,
            state,
            reason,
            question,
        } => {
            let completion = TaskCompletion {
                state: state.unwrap_or(TaskState::Done),
                result: result.clone(),
                reason: reason.clone(),
                question: question.clone(),
            };
            complete(tx, grant, task_id, &completion, now)
        }
        CoordinatorOp::Interrupt {
            station,
            session_id,
        }
        | CoordinatorOp::Stop {
            station,
            session_id,
        } => {
            let must_exist = grant.scope == fleetdeck_proto::command::GrantScope::Fleet;
            if let Some(d) = super::coordinator::strict_target(
                tx,
                grant,
                (station, session_id),
                (must_exist, now),
            )? {
                return Ok(d);
            }
            let session_id = session_id.clone();
            let command = if matches!(op, CoordinatorOp::Interrupt { .. }) {
                SessionCommand::Interrupt { session_id }
            } else {
                SessionCommand::Stop {
                    session_id,
                    how: StopMode::Graceful,
                }
            };
            enqueue(tx, ctx, station, command, now)?;
            Ok(Decision::Accepted(None))
        }
        CoordinatorOp::Spawn {
            station,
            spec,
            prompt,
        } => {
            if !grant.stations.contains(station) {
                return no(RejectCode::Forbidden, "the station is not in the grant");
            }
            start(
                tx,
                grant,
                ctx,
                (station, spec.clone(), prompt.clone(), None),
                command_id,
                now,
            )
        }
        CoordinatorOp::Resume {
            station,
            session_id,
        } => coordinator_fleet::resume(tx, grant, ctx, (station, session_id), now),
        CoordinatorOp::Rename {
            station,
            session_id,
            title,
        } => coordinator_fleet::rename(tx, grant, ctx, (station, session_id), title, now),
        CoordinatorOp::SetMode {
            station,
            session_id,
            mode,
        } => coordinator_fleet::set_mode(tx, grant, ctx, (station, session_id), mode, now),
        CoordinatorOp::ProjectAssign {
            station,
            session_id,
            target,
        } => coordinator_fleet::project_assign(tx, grant, ctx, (station, session_id), target, now),
        CoordinatorOp::ProjectStart {
            project_id,
            prompt,
            provider_instance,
            worktree,
        } => coordinator_fleet::project_start(
            tx,
            grant,
            ctx,
            (project_id, prompt, provider_instance.as_ref(), *worktree),
            command_id,
            now,
        ),
        CoordinatorOp::Answer { .. } => coordinator_fleet::answer(tx, grant, ctx, op, now),
        CoordinatorOp::Land {
            station,
            session_id,
        } => super::coordinator_git::land(
            tx,
            grant,
            ctx,
            (station, session_id),
            (command_id, now),
            required,
        ),
        CoordinatorOp::Move {
            station,
            session_id,
            to_station,
        } => super::coordinator_git::move_to(
            tx,
            grant,
            ctx,
            (station, session_id, to_station),
            (command_id, now),
            required,
        ),
        CoordinatorOp::AssignAuto { task, requirements } => {
            if let Some(d) = check_text(task) {
                return Ok(d);
            }
            let mine: Vec<DeckSession> = ctx
                .sessions
                .iter()
                .filter(|s| grant.stations.contains(&s.station_id))
                .filter(|s| {
                    !(s.station_id == grant.station_id && s.summary.session_id == grant.session_id)
                })
                .cloned()
                .collect();
            let needs = Needs {
                repo: requirements.repo.as_deref(),
                provider: requirements.provider,
            };
            if let Some(s) = pick_session(&mine, &needs, &ctx.buried) {
                let (station, session) = (s.station_id.clone(), s.summary.session_id.clone());
                if let Some(d) = super::coordinator::v2_refusal(grant, ctx, &station) {
                    return Ok(d);
                }
                assign_to(
                    tx, ctx, grant, &sender, command_id, task, &station, &session, now,
                )?;
                return Ok(Decision::Accepted(Some(session)));
            }
            let Some(repo) = requirements.repo.clone() else {
                return no(
                    RejectCode::InvalidState,
                    "no session fits and no repository to start in",
                );
            };
            if !grant.classes.contains(&GrantClass::Spawn) {
                return no(RejectCode::InvalidState, "no session fits the task");
            }
            let Some((station, instance)) = spawn_target(grant, ctx, requirements.provider, None)
            else {
                return no(
                    RejectCode::InvalidState,
                    "no session fits and no station of the grant can start one",
                );
            };
            if let Some(d) = super::coordinator::v2_refusal(grant, ctx, &station) {
                return Ok(d);
            }
            let spec = SessionSpec {
                effort: None,
                provider_instance: instance,
                cwd: repo,
                mode: None,
                model: None,
                title: None,
                coordinator: None,
                launch: None,
                extra_env: None,
            };
            let prompt = Some(UserInput::new(format!(
                "{ASSIGNMENT_PREFIX}{}",
                task.trim()
            )));
            start(
                tx,
                grant,
                ctx,
                (&station, spec, prompt, None),
                command_id,
                now,
            )
        }
    }
}

fn check_target(
    tx: &Connection,
    grant: &Grant,
    to: &CoordinatorTarget,
) -> Result<Option<Decision>> {
    if !grant.stations.contains(&to.station) {
        return Ok(Some(Decision::Rejected(
            RejectCode::Forbidden,
            "the target station is not in the grant".to_owned(),
        )));
    }
    if !session_known(tx, &to.station, &to.session_id)? {
        return Ok(Some(Decision::Rejected(
            RejectCode::UnknownSession,
            format!("no session `{}` on that station", to.session_id),
        )));
    }
    Ok(None)
}

#[allow(clippy::too_many_arguments)]
fn assign_to(
    tx: &Connection,
    ctx: &Context,
    grant: &Grant,
    sender: &str,
    command_id: &str,
    task: &str,
    station: &StationId,
    session: &SessionId,
    now: i64,
) -> Result<()> {
    let command = SessionCommand::FollowUp {
        session_id: session.clone(),
        input: UserInput::new(task_frame(sender, command_id, task.trim())),
    };
    enqueue(tx, ctx, station, command, now)?;
    tx.execute(
        "INSERT INTO tasks (task_id, title, creator, station_id, session_id, state,
             coordinator_station, coordinator_session, outbox_id, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, 'open', ?6, ?7, ?8, ?9, ?9)",
        params![
            command_id,
            task.trim(),
            sender,
            station.as_str(),
            session.0,
            grant.station_id.as_str(),
            grant.session_id.0,
            ctx.outbox_id,
            now
        ],
    )?;
    Ok(())
}

fn complete(
    tx: &Connection,
    grant: &Grant,
    task_id: &str,
    completion: &TaskCompletion,
    now: i64,
) -> Result<Decision> {
    let row: Option<(String, String, String)> = tx
        .query_row(
            "SELECT station_id, session_id, state FROM tasks WHERE task_id = ?1",
            [task_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let Some((station, session, state)) = row else {
        return no(RejectCode::InvalidCommand, format!("no task `{task_id}`"));
    };
    if station != grant.station_id.0 || session != grant.session_id.0 {
        return no(
            RejectCode::Forbidden,
            "the task is not assigned to the caller",
        );
    }
    if !matches!(state.as_str(), "open" | "question") {
        return no(RejectCode::InvalidState, "the task has ended");
    }
    if let Err(message) = completion.check() {
        return no(RejectCode::InvalidCommand, message);
    }
    write_completion(tx, task_id, completion, now)?;
    Ok(Decision::Accepted(Some(grant.session_id.clone())))
}

pub(super) fn in_lineage(
    tx: &Connection,
    grant: &Grant,
    station: &StationId,
    session: &SessionId,
) -> Result<bool> {
    Ok(tx
        .query_row(
            // A spawned session joins the lineage when the station's start
            // receipt names it: the start command of the lineage row's
            // outbox item carries `result.session_id` (DL-211).
            "SELECT 1 FROM coordinator_lineage l
             WHERE l.grant_id = ?1 AND l.station_id = ?2
               AND (l.session_id = ?3
                    OR (l.session_id IS NULL AND EXISTS (
                        SELECT 1 FROM outbox o JOIN commands c ON c.command_id = o.command_id
                        WHERE o.outbox_id = l.start_outbox_id
                          AND json_extract(c.receipt, '$.result.session_id') = ?3)))",
            params![grant.grant_id, station.as_str(), session.0],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

/// The first station of the grant (by id, or only `only`) with a signed-in
/// provider of the wanted kind whose family has a mode ceiling.
pub(super) fn spawn_target(
    grant: &Grant,
    ctx: &Context,
    kind: Option<fleetdeck_proto::event::ProviderKind>,
    only: Option<&StationId>,
) -> Option<(StationId, ProviderInstanceId)> {
    grant
        .stations
        .iter()
        .filter(|s| only.is_none_or(|o| o == *s))
        .find_map(|station| {
            let info = ctx.infos.get(station)?;
            info.providers
                .iter()
                .filter(|p| p.auth == "signed_in")
                .filter(|p| kind.is_none_or(|k| k == p.provider))
                .find(|p| {
                    family_of(Some(info), &p.instance)
                        .is_some_and(|f| grant.max_modes.contains_key(&f))
                })
                .map(|p| (station.clone(), p.instance.clone()))
        })
}

/// A start intent under the spawn rules (§6.3), the mode rules (ADR-008
/// §5.2) and the cap (§6.4). `what` is the station, the spec, the prompt and
/// the workspace request of the start.
pub(super) fn start(
    tx: &Connection,
    grant: &Grant,
    ctx: &Context,
    what: (
        &StationId,
        SessionSpec,
        Option<UserInput>,
        Option<Box<WorkspaceRequest>>,
    ),
    command_id: &str,
    now: i64,
) -> Result<Decision> {
    let (station, spec, prompt, workspace) = what;
    // MCP P0 §2.6: no transitive authority (ADR-006 §4).
    if spec.coordinator.is_some() {
        return no(
            RejectCode::Forbidden,
            "a coordinator never starts a session with a coordinator grant",
        );
    }
    let Some(family) = family_of(ctx.infos.get(station), &spec.provider_instance) else {
        return no(
            RejectCode::UnknownProvider,
            format!(
                "the station has no provider instance `{}`",
                spec.provider_instance
            ),
        );
    };
    let Some(ceiling) = grant.max_modes.get(&family) else {
        return no(
            RejectCode::Forbidden,
            format!("the grant has no mode ceiling for `{family}`"),
        );
    };
    if is_bypass(&family, spec.mode.as_deref()) {
        return no(
            RejectCode::Forbidden,
            "a coordinator never starts a session in a bypass mode",
        );
    }
    if !mode_within(&family, spec.mode.as_deref(), ceiling) {
        return no(
            RejectCode::Forbidden,
            format!("the mode is above the grant's ceiling `{ceiling}`"),
        );
    }
    // The cap check and the increment are in this write transaction; the
    // deck has one writer, so two decisions never take the same slot.
    let used: i64 = tx.query_row(
        "SELECT spawn_used FROM tokens WHERE grant_id = ?1",
        [grant.grant_id.as_str()],
        |r| r.get(0),
    )?;
    if used >= i64::from(grant.spawn_cap) {
        return no(
            RejectCode::SpawnCapReached,
            format!("the grant has used its {} starts", grant.spawn_cap),
        );
    }
    let known = tx
        .query_row(
            "SELECT 1 FROM stations WHERE station_id = ?1",
            [station.as_str()],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if !known {
        return no(RejectCode::InvalidCommand, "the station is not paired");
    }
    enqueue(
        tx,
        ctx,
        station,
        SessionCommand::Start {
            spec,
            prompt,
            workspace,
            setup_cmd: None,
            batch: None,
            issue: None,
            origin_text: false,
            fleet_git: None,
        },
        now,
    )?;
    tx.execute(
        "UPDATE tokens SET spawn_used = spawn_used + 1 WHERE grant_id = ?1",
        [grant.grant_id.as_str()],
    )?;
    tx.execute(
        "INSERT INTO coordinator_lineage
             (command_id, grant_id, station_id, session_id, start_outbox_id, created_at)
         VALUES (?1, ?2, ?3, NULL, ?4, ?5)",
        params![
            command_id,
            grant.grant_id,
            station.as_str(),
            ctx.outbox_id,
            now
        ],
    )?;
    Ok(Decision::Accepted(None))
}

#[cfg(test)]
#[path = "fleet_control_tests.rs"]
mod fleet_control_tests;
