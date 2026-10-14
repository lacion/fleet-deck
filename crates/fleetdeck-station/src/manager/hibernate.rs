//! Hibernation (R6.6, DL-169): a session that is live, idle and untouched
//! for `hibernate_after_min` minutes is stopped gracefully, so that its
//! process and memory go away while the board keeps the card. The native
//! resume (A10, DL-118) brings it back with the next input.
//!
//! - **Candidate.** `state` running, `activity` idle (no run, no turn, no
//!   open request, no background work, no running tool), no terminal holds
//!   it, no revert blocks it, it can resume and has a native resume
//!   identity, and its last event is at least `hibernate_after_min` old.
//! - **Decision.** One station-origin `session.stop { graceful }` with the
//!   stable command id `hibernate:<session>:<session_seq>`. In the same
//!   transaction the station appends `session.hibernated { after_min }`
//!   before the stop effect runs. The candidate rules run again inside the
//!   transaction, so an input that arrives in between wins: the command is
//!   then rejected and nothing is stored but the rejection receipt.
//! - **Sweep.** [`ticker`] runs [`sweep`] every [`TICK`] while the station
//!   runs. It reads `hibernate_after_min` from the current policy snapshot
//!   at each tick, so a live change applies without a restart; 0 is off
//!   and the sweep then does nothing (DL-172 §1.4).

use std::sync::Arc;
use std::time::Duration;

use fleetdeck_proto::command::{
    CommandOrigin, CommandResult, RETRY_WINDOW_MS, SessionCommand, StopMode,
};
use fleetdeck_proto::event::SessionState;
use fleetdeck_proto::session::Activity;
use fleetdeck_proto::{CommandEnvelope, CommandId, Event, RejectCode, SessionId, SessionSummary};
use tokio::sync::watch;

use super::decide::{self, reject};
use super::policy::Policy;
use super::policy_hub::PolicyHub;
use crate::coordinator::Bindings;
use crate::store::{Caller, DecideError, Decision, Store};

/// How often the station looks for sessions to hibernate.
pub const TICK: Duration = Duration::from_secs(60);

/// The prefix of the command id of a hibernation.
pub const COMMAND_PREFIX: &str = "hibernate:";

/// Whether the summary of `s` is a candidate at `now` (Unix ms) for a
/// hibernation after `after_min` minutes. The resume identity is checked in
/// the decision transaction.
#[must_use]
pub fn is_candidate(s: &SessionSummary, now: i64, after_min: u32) -> bool {
    let after_ms = i64::from(after_min).saturating_mul(60_000);
    after_min > 0
        && s.state == SessionState::Running
        && s.activity == Activity::Idle
        && !s.hibernated
        && s.current_run.is_none()
        && s.current_turn.is_none()
        && s.open_requests.is_empty()
        && s.background_tasks.is_empty()
        && s.running_tools.is_empty()
        && s.terminal.is_none()
        && s.revert.as_ref().is_none_or(|r| !r.blocks_input())
        && s.capabilities.as_ref().is_some_and(|c| c.resume)
        && now.saturating_sub(s.updated_at) >= after_ms
}

fn refused(why: &str) -> DecideError {
    reject(RejectCode::InvalidState, format!("not hibernated: {why}"))
}

/// The decision of one hibernation (see the module documentation).
fn decide_hibernate(
    d: &mut Decision<'_>,
    policy: &Policy,
    bindings: &Bindings,
    session_id: &SessionId,
    after_min: u32,
) -> Result<CommandResult, DecideError> {
    let s = d
        .summary(session_id)?
        .ok_or_else(|| reject(RejectCode::UnknownSession, "no such session"))?;
    if !is_candidate(&s, d.now(), after_min) {
        return Err(refused("the session is not idle"));
    }
    if d.native_session_id(session_id)?.is_none() {
        return Err(refused("the provider reported no native session id"));
    }
    // The running execution is the one that this hibernation stops; every
    // earlier unresolved execution still refuses (DL-182 §5).
    let current = d.current_shim(session_id)?;
    if d.unended_execution_except_current(session_id, current.as_deref())?
        .is_some()
    {
        return Err(refused("an earlier execution may still run"));
    }
    let stop = SessionCommand::Stop {
        session_id: session_id.clone(),
        how: StopMode::Graceful,
    };
    let done = decide::decide(d, policy, bindings, CommandOrigin::Local, false, stop)?;
    d.append(session_id, Event::SessionHibernated { after_min }.into())?;
    Ok(done)
}

/// Hibernates every candidate of the station and returns how many stops
/// were stored. A session that an input reached in the meantime is skipped.
///
/// # Errors
/// A database error.
pub async fn sweep(
    store: &Store,
    policy: &Policy,
    bindings: &Bindings,
    after_min: u32,
) -> crate::store::Result<usize> {
    if after_min == 0 {
        return Ok(0);
    }
    let now = store.now_ms();
    let mut stopped = 0;
    for summary in store.snapshot().await?.sessions {
        if !is_candidate(&summary, now, after_min) {
            continue;
        }
        let session_id = summary.session_id.clone();
        let envelope = CommandEnvelope {
            authority: None,
            command_id: CommandId::from(format!(
                "{COMMAND_PREFIX}{session_id}:{}",
                summary.session_seq
            )),
            target_incarnation: None,
            retry_until: now.saturating_add(RETRY_WINDOW_MS),
            origin: CommandOrigin::Local,
            command: SessionCommand::Stop {
                session_id: session_id.clone(),
                how: StopMode::Graceful,
            },
        };
        let policy = policy.clone();
        let bindings = bindings.clone();
        let id = session_id.clone();
        let decided = store
            .decide(envelope, Caller::local(), move |d| {
                decide_hibernate(d, &policy, &bindings, &id, after_min)
            })
            .await?;
        if !decided.duplicate && decided.effects.iter().any(|e| e.session_id == session_id) {
            stopped += 1;
        }
    }
    Ok(stopped)
}

/// Runs [`sweep`] every `period` (the station uses [`TICK`]) until
/// `shutdown`. Each tick reads the current policy snapshot.
pub async fn ticker(
    store: Store,
    hub: Arc<PolicyHub>,
    bindings: Bindings,
    period: Duration,
    mut shutdown: watch::Receiver<bool>,
) {
    let mut tick = tokio::time::interval(period);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = tick.tick() => {
                let policy = hub.current();
                match sweep(&store, &policy, &bindings, policy.hibernate_after_min()).await {
                    Ok(0) => {}
                    Ok(n) => tracing::info!(hibernated = n, "stopped idle sessions"),
                    Err(e) => tracing::warn!(error = %e, "hibernation sweep"),
                }
            }
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    return;
                }
            }
        }
    }
}
