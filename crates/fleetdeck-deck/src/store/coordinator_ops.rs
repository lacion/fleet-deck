//! Authorization and intent of one coordinator operation (M3 6-5, ADR-006
//! §6.2 step 6, §6.3, §6.4). Everything here runs inside the decision
//! transaction of [`super::coordinator`]: the class and target checks, the
//! spawn-cap check and increment, and the intent rows (outbox item, mail,
//! task, lineage) commit together with the receipt.

use std::collections::BTreeMap;

use fleetdeck_proto::RejectCode;
use fleetdeck_proto::board::DeckSession;
use fleetdeck_proto::command::{SessionCommand, SessionSpec, StopMode, UserInput};
use fleetdeck_proto::link::StationInfo;
use fleetdeck_proto::mutation::{CoordinatorDelivery, CoordinatorOp, CoordinatorTarget};
use fleetdeck_proto::orchestration::{ASSIGNMENT_PREFIX, MAX_TASK_CHARS, mail_frame, task_frame};
use fleetdeck_proto::{ProviderInstanceId, SessionId, StationId};
use rusqlite::{Connection, OptionalExtension, params};

use super::Result;
use super::grants::{Grant, GrantClass};
use super::outbox::{NewOutboxItem, insert_item};
use crate::link::router::CommandKind;
use crate::routing::{Needs, pick_session};

/// How long a coordinator intent waits in the deck outbox: 24 h.
const INTENT_TTL_MS: i64 = 24 * 60 * 60 * 1000;

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
}

/// The result of step 6.
pub(crate) enum Decision {
    Accepted(Option<SessionId>),
    Rejected(RejectCode, String),
}

fn no(code: RejectCode, message: impl Into<String>) -> Result<Decision> {
    Ok(Decision::Rejected(code, message.into()))
}

/// The class that an op needs (§6.3).
fn class_of(op: &CoordinatorOp) -> GrantClass {
    match op {
        CoordinatorOp::Send { .. }
        | CoordinatorOp::Assign { .. }
        | CoordinatorOp::AssignAuto { .. }
        | CoordinatorOp::CompleteTask { .. } => GrantClass::Mail,
        CoordinatorOp::Interrupt { .. } | CoordinatorOp::Stop { .. } => GrantClass::Drive,
        CoordinatorOp::Spawn { .. } => GrantClass::Spawn,
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

fn family_of(info: Option<&StationInfo>, instance: &ProviderInstanceId) -> Option<String> {
    let p = info?.providers.iter().find(|p| &p.instance == instance)?;
    serde_json::to_value(p.provider)
        .ok()?
        .as_str()
        .map(str::to_owned)
}

fn session_known(tx: &Connection, station: &StationId, session: &SessionId) -> Result<bool> {
    Ok(tx
        .query_row(
            "SELECT 1 FROM sessions WHERE station_id = ?1 AND session_id = ?2",
            params![station.as_str(), session.0],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

fn enqueue(
    tx: &Connection,
    ctx: &Context,
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
    insert_item(tx, &ctx.outbox_id, &item, now)?;
    Ok(())
}

fn check_text(text: &str) -> Option<Decision> {
    let t = text.trim();
    (t.is_empty() || t.chars().count() > MAX_TASK_CHARS).then(|| {
        Decision::Rejected(
            RejectCode::InvalidCommand,
            format!("the text needs 1 to {MAX_TASK_CHARS} characters"),
        )
    })
}

/// The checks and the intent of one op for `grant` (§6.2 step 6).
pub(crate) fn decide(
    tx: &Connection,
    grant: &Grant,
    op: &CoordinatorOp,
    ctx: &Context,
    command_id: &str,
    now: i64,
) -> Result<Decision> {
    let class = class_of(op);
    if !grant.classes.contains(&class) {
        return no(
            RejectCode::Forbidden,
            format!("the grant has no `{class:?}` class"),
        );
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
                &sender,
                command_id,
                task,
                &to.station,
                &to.session_id,
                now,
            )?;
            Ok(Decision::Accepted(Some(to.session_id.clone())))
        }
        CoordinatorOp::CompleteTask { task_id, result } => {
            complete(tx, grant, task_id, result.as_deref(), now)
        }
        CoordinatorOp::Interrupt {
            station,
            session_id,
        }
        | CoordinatorOp::Stop {
            station,
            session_id,
        } => {
            if !grant.stations.contains(station) || !in_lineage(tx, grant, station, session_id)? {
                return no(
                    RejectCode::Forbidden,
                    "the session is not on a station of the grant or not in its lineage",
                );
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
                station,
                spec.clone(),
                prompt.clone(),
                command_id,
                now,
            )
        }
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
                assign_to(tx, ctx, &sender, command_id, task, &station, &session, now)?;
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
            let Some((station, instance)) = spawn_target(grant, ctx, requirements.provider) else {
                return no(
                    RejectCode::InvalidState,
                    "no session fits and no station of the grant can start one",
                );
            };
            let spec = SessionSpec {
                provider_instance: instance,
                cwd: repo,
                mode: None,
                model: None,
                title: None,
            };
            let prompt = Some(UserInput::new(format!(
                "{ASSIGNMENT_PREFIX}{}",
                task.trim()
            )));
            start(tx, grant, ctx, &station, spec, prompt, command_id, now)
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
    sender: &str,
    command_id: &str,
    task: &str,
    station: &StationId,
    session: &SessionId,
    now: i64,
) -> Result<()> {
    let command = SessionCommand::FollowUp {
        session_id: session.clone(),
        input: UserInput::new(task_frame(command_id, task.trim())),
    };
    enqueue(tx, ctx, station, command, now)?;
    tx.execute(
        "INSERT INTO tasks (task_id, title, creator, station_id, session_id, state,
             outbox_id, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, 'open', ?6, ?7, ?7)",
        params![
            command_id,
            task.trim(),
            sender,
            station.as_str(),
            session.0,
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
    result: Option<&str>,
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
    if state != "open" {
        return no(RejectCode::InvalidState, "the task has ended");
    }
    if result.is_some_and(|r| r.chars().count() > MAX_TASK_CHARS) {
        return no(RejectCode::InvalidCommand, "the result is too long");
    }
    tx.execute(
        "UPDATE tasks SET state = 'done', result = ?2, updated_at = ?3, completed_at = ?3
         WHERE task_id = ?1 AND state = 'open'",
        params![task_id, result, now],
    )?;
    Ok(Decision::Accepted(Some(grant.session_id.clone())))
}

fn in_lineage(
    tx: &Connection,
    grant: &Grant,
    station: &StationId,
    session: &SessionId,
) -> Result<bool> {
    Ok(tx
        .query_row(
            "SELECT 1 FROM coordinator_lineage
             WHERE grant_id = ?1 AND station_id = ?2 AND session_id = ?3",
            params![grant.grant_id, station.as_str(), session.0],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

/// The first station of the grant (by id) with a signed-in provider of the
/// wanted kind whose family has a mode ceiling.
fn spawn_target(
    grant: &Grant,
    ctx: &Context,
    kind: Option<fleetdeck_proto::event::ProviderKind>,
) -> Option<(StationId, ProviderInstanceId)> {
    grant.stations.iter().find_map(|station| {
        let info = ctx.infos.get(station)?;
        info.providers
            .iter()
            .filter(|p| p.auth == "signed_in")
            .filter(|p| kind.is_none_or(|k| k == p.provider))
            .find(|p| {
                family_of(Some(info), &p.instance).is_some_and(|f| grant.max_modes.contains_key(&f))
            })
            .map(|p| (station.clone(), p.instance.clone()))
    })
}

/// A start intent under the spawn rules (§6.3) and the cap (§6.4).
#[allow(clippy::too_many_arguments)]
fn start(
    tx: &Connection,
    grant: &Grant,
    ctx: &Context,
    station: &StationId,
    spec: SessionSpec,
    prompt: Option<UserInput>,
    command_id: &str,
    now: i64,
) -> Result<Decision> {
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
            workspace: None,
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
