//! Provider process lifecycle and station start recovery (PLAN §6.4, A10,
//! L2, L3; ADR-001 §3.2; M2 P2-5).
//!
//! - [`process_started`] records a new provider incarnation of a session.
//! - [`process_ended`] (provider process lost, station alive): queued effects
//!   of the process are retired, claimed effects become `outcome_unknown`,
//!   its open requests expire with `process_lost`.
//! - [`recover`] (station start, before any command is admitted): processes
//!   that did not survive are dead; queued effects of dead or no processes
//!   are retired; every claim is `outcome_unknown`; open requests of dead
//!   processes expire with `not_resumable`; sessions without a live process
//!   close their run and turn as failed and become `lost`. All in one
//!   transaction. Nothing is sent again.

use fleetdeck_proto::event::{ExpiredReason, SessionState, TurnStatus};
use fleetdeck_proto::{Event, EventEnvelope, SessionId};
use rusqlite::{Connection, OptionalExtension as _, params};

use super::effects::{self, ProcessIncarnation, REASON_PROCESS_LOST};
use super::{Result, StoreError, log, requests};

/// What [`recover`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RecoveryReport {
    pub dead_processes: usize,
    pub retired: usize,
    pub unknown: usize,
    pub expired_requests: usize,
    pub lost_sessions: usize,
}

pub(super) fn process_started(
    conn: &Connection,
    session: &SessionId,
    process: &ProcessIncarnation,
    pid: Option<u32>,
    now: i64,
) -> Result<()> {
    let known: bool = conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM sessions WHERE session_id = ?1)",
        [session.as_str()],
        |r| r.get(0),
    )?;
    if !known {
        return Err(StoreError::UnknownSession(session.clone()));
    }
    conn.execute(
        "INSERT INTO processes (process_incarnation, session_id, pid, state, started_at)
         VALUES (?1, ?2, ?3, 'live', ?4)",
        params![process.as_str(), session.as_str(), pid, now],
    )?;
    Ok(())
}

pub(super) fn process_ended(
    conn: &Connection,
    process: &ProcessIncarnation,
    reason: &str,
    now: i64,
    out: &mut Vec<EventEnvelope>,
) -> Result<()> {
    conn.execute(
        "UPDATE processes SET state = 'dead', ended_at = ?2, reason = ?3
         WHERE process_incarnation = ?1 AND state = 'live'",
        params![process.as_str(), now, reason],
    )?;
    effects::close_process_effects(conn, process, REASON_PROCESS_LOST, now, out)?;
    requests::expire_open(
        conn,
        "process_incarnation = ?1",
        [process.as_str()],
        ExpiredReason::ProcessLost,
        now,
        out,
    )?;
    Ok(())
}

pub(super) fn recover(
    conn: &Connection,
    live: &[ProcessIncarnation],
    now: i64,
    out: &mut Vec<EventEnvelope>,
) -> Result<RecoveryReport> {
    let mut report = RecoveryReport::default();
    let mut stmt =
        conn.prepare("SELECT process_incarnation FROM processes WHERE state = 'live'")?;
    let was_live: Vec<String> = stmt
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    for process in was_live {
        if !live.iter().any(|p| p.as_str() == process) {
            conn.execute(
                "UPDATE processes SET state = 'dead', ended_at = ?2, reason = 'station_restart'
                 WHERE process_incarnation = ?1",
                params![process, now],
            )?;
            report.dead_processes += 1;
        }
    }
    (report.retired, report.unknown) = effects::recover_effects(conn, now, out)?;
    // DL-405: a shell effect of an earlier process is never dispatched.
    let (shell_retired, shell_unknown) = super::shell_effects::recover(conn, now)?;
    if shell_retired + shell_unknown > 0 {
        tracing::info!(
            retired = shell_retired,
            unknown = shell_unknown,
            "shell effects of the earlier process"
        );
    }
    report.expired_requests = requests::expire_open(
        conn,
        "process_incarnation IS NULL OR process_incarnation IN
             (SELECT process_incarnation FROM processes WHERE state = 'dead')",
        [],
        ExpiredReason::NotResumable,
        now,
        out,
    )?;
    let mut stmt = conn.prepare(
        "SELECT session_id FROM sessions s
         WHERE state NOT IN ('exited', 'lost')
           AND NOT EXISTS (SELECT 1 FROM processes p
                           WHERE p.session_id = s.session_id AND p.state = 'live')
         ORDER BY created_at, session_id",
    )?;
    let lost: Vec<String> = stmt
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    for session in lost {
        lose_session(conn, &SessionId::from(session), now, out)?;
        report.lost_sessions += 1;
    }
    super::coordinator_outbox::recover(conn, now)?;
    // (FG-9) A moved-in session that was launching is abandoned.
    let abandoned = super::move_commit::recover(conn, now, out)?;
    if abandoned > 0 {
        tracing::info!(abandoned, "moves that were launching are abandoned");
    }
    Ok(report)
}

fn lose_session(
    conn: &Connection,
    session: &SessionId,
    now: i64,
    out: &mut Vec<EventEnvelope>,
) -> Result<()> {
    let summary =
        log::summary(conn, session)?.ok_or_else(|| StoreError::UnknownSession(session.clone()))?;
    let had_background = !summary.background_tasks.is_empty();
    let mut events = Vec::new();
    if let Some(turn_id) = summary.current_turn {
        events.push(Event::TurnCompleted {
            turn_id,
            status: TurnStatus::Failed,
            usage: None,
        });
    }
    if let Some(run_id) = summary.current_run {
        events.push(Event::RunCompleted {
            run_id,
            status: TurnStatus::Failed,
            background_pending: false,
        });
    }
    // The loss comes before the empty background list: a reader marks the
    // background children as ended by the loss, not as done by the list
    // (M2 gate correction).
    // A shim row that recovery ended `lost` names why (ADR-005 §3.2, for
    // example `execution_unknown`); otherwise the direct child is gone.
    let shim_reason: Option<String> = conn
        .query_row(
            "SELECT reason FROM shims WHERE session_id = ?1 AND state = 'lost'
             ORDER BY updated_at DESC, rowid DESC LIMIT 1",
            [session.as_str()],
            |r| r.get(0),
        )
        .optional()?
        .flatten();
    let detail = match shim_reason {
        Some(reason) => format!("station restart: the shim session is lost ({reason})"),
        None => "station restart: the provider process is gone".to_owned(),
    };
    events.push(Event::SessionStateChanged {
        state: SessionState::Lost,
        detail: Some(detail),
    });
    if had_background {
        events.push(Event::BackgroundUpdated { tasks: Vec::new() });
    }
    for event in events {
        out.push(log::append_one(conn, session, event.into(), now)?);
    }
    Ok(())
}
