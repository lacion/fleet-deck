//! Auto-continue (L19, ADR-007, DL-127): a session that a station restart
//! lost in the middle of its work can continue once, with a fixed station
//! input, when the owner turned it on and every rule still holds.
//!
//! - `session.set_auto_continue` stores the preference as a station event;
//!   it is off by default, and only an owner board command over a paired
//!   link can turn it on (§2.1).
//! - At a station start, after recovery and before any command, every
//!   session that this start lost is evaluated in one decision transaction
//!   (§2.2). An eligible session gets one continuation intent: a `resume`
//!   effect with the stable command id `continue:<session>:<lost turn>`,
//!   the stored spec and mode, and the fixed continuation as its first
//!   input (§2.3). An ineligible session stays `lost` with
//!   `session.auto_continue_skipped { reason }`. A shim session is continued
//!   only with the DL-182 proof that its run ended (the `execution_unknown`
//!   and `not_resumable` checks of the chain; DL-188 step 3).
//! - The worker rechecks rules 2, 3, 5, 6 and 9 right before it spawns
//!   (§2.4), and on a shim host the DL-182 proof again like a manual
//!   resume. A claimed continuation that a crash interrupts is
//!   `outcome_unknown` at the next start and is never dispatched again.

use fleetdeck_proto::command::{CommandOrigin, CommandResult, SessionCommand, UserInput};
use fleetdeck_proto::event::{RunOrigin, SessionState};
use fleetdeck_proto::{CommandEnvelope, CommandId, Event, EventBody, RejectCode, SessionId};
use fleetdeck_providers::host::shim::identity::BootIdReader;

use super::decide::{reject, result};
use super::policy::Policy;
use crate::store::auto_continue::{COMMAND_PREFIX, CONTINUATION_TEXT, LostWork, WINDOW_MS};
use crate::store::resumes::Purpose;
use crate::store::{Caller, DecideError, Decision, Effect, InputRecord, ReadResult, Store};

/// `session.set_auto_continue` in the decision transaction (§2.1). An
/// unchanged value is accepted without an event.
///
/// # Errors
/// A stored rejection, or a database error.
pub fn decide_set(
    d: &mut Decision<'_>,
    origin: CommandOrigin,
    session_id: &SessionId,
    enabled: bool,
) -> Result<CommandResult, DecideError> {
    let summary = d
        .summary(session_id)?
        .ok_or_else(|| reject(RejectCode::UnknownSession, "no such session"))?;
    if enabled && origin != CommandOrigin::Board {
        return Err(forbidden());
    }
    if summary.auto_continue != enabled {
        d.append(session_id, Event::AutoContinueChanged { enabled }.into())?;
    }
    Ok(result(session_id, None))
}

fn forbidden() -> DecideError {
    reject(
        RejectCode::Forbidden,
        "only an owner board command can turn auto-continue on (ADR-007 §2.1)",
    )
}

/// The caller check of `session.set_auto_continue { enabled: true }`: the
/// local socket (no paired peer) can never turn it on, whatever origin the
/// envelope names (§2.1).
///
/// # Errors
/// The `forbidden` rejection.
pub fn guard(command: &SessionCommand, caller: &Caller) -> Result<(), DecideError> {
    if matches!(
        command,
        SessionCommand::SetAutoContinue { enabled: true, .. }
    ) && !caller.is_peer()
    {
        return Err(forbidden());
    }
    Ok(())
}

/// The stable code of the first failed eligibility rule, or `None`.
fn ineligible(
    d: &Decision<'_>,
    policy: &Policy,
    session_id: &SessionId,
    w: &LostWork,
) -> Result<Option<&'static str>, DecideError> {
    let Some(s) = d.summary(session_id)? else {
        return Ok(Some("identity_changed"));
    };
    if !s.auto_continue {
        return Ok(Some("disabled"));
    }
    let same_instance = s
        .provider_instance
        .as_ref()
        .and_then(|i| policy.instance(i))
        .is_some_and(|kind| Some(kind) == s.provider);
    let code = if !same_instance || d.native_session_id(session_id)?.is_none() {
        "identity_changed"
    } else if crate::store::session_holds::held(d.connection(), session_id)? {
        // (FG-8) A land holds the session: no auto-continue.
        "held"
    } else if d.unended_execution(session_id)?.is_some() {
        "execution_unknown"
    } else if w.turn_id.is_none() || !(w.run_open || w.turn_open) {
        "idle"
    } else if d.owner_work_after(session_id, w.turn_at)? {
        "newer_owner_work"
    } else if w.stopping || d.stop_since(session_id, w.turn_at)? {
        "stop_requested"
    } else if w.maintenance {
        "maintenance_turn"
    } else if !w.turn_open {
        "turn_completed"
    } else if w.lost_at.saturating_sub(w.last_at) > WINDOW_MS {
        "expired"
    } else if !s.capabilities.as_ref().is_some_and(|c| c.resume) {
        "unsupported"
    } else if d.unknown_input_since(session_id, w.lost_at)? {
        "input_outcome_unknown"
    } else {
        return Ok(None);
    };
    Ok(Some(code))
}

/// The intent or the skip of one lost session, in its decision
/// transaction (§2.2, §2.3).
fn decide_lost(
    d: &mut Decision<'_>,
    policy: &Policy,
    session_id: &SessionId,
    after: u64,
) -> Result<CommandResult, DecideError> {
    let Some(w) = d.lost_work(session_id, after)? else {
        return Ok(result(session_id, None));
    };
    let skip = |d: &mut Decision<'_>, reason: &str| -> Result<CommandResult, DecideError> {
        d.append(
            session_id,
            Event::AutoContinueSkipped {
                reason: reason.to_owned(),
            }
            .into(),
        )?;
        Ok(result(session_id, None))
    };
    if let Some(code) = ineligible(d, policy, session_id, &w)? {
        return skip(d, code);
    }
    let prompt = InputRecord {
        input_id: d.new_id(),
        origin: RunOrigin::Continuation,
        input: UserInput::new(CONTINUATION_TEXT),
        shell_context: Vec::new(),
    };
    let by = w.last_at.saturating_add(WINDOW_MS);
    match super::resume::admit_resume(d, policy, session_id, Some(prompt), Some(by)) {
        Err(DecideError::Reject(_)) => skip(d, "not_resumable"),
        other => other,
    }
}

/// The sessions that a `lost` state after `after` names.
async fn lost_after(store: &Store, after: u64) -> crate::store::Result<Vec<SessionId>> {
    let mut cursor = after;
    let mut lost = Vec::new();
    loop {
        let ReadResult::Events { events, head } = store.read_after(cursor, 1000).await? else {
            return Ok(lost);
        };
        for e in &events {
            if let EventBody::Known(Event::SessionStateChanged {
                state: SessionState::Lost,
                ..
            }) = &e.event
                && !lost.contains(&e.session_id)
            {
                lost.push(e.session_id.clone());
            }
        }
        match events.last() {
            Some(last) if last.station_seq < head => cursor = last.station_seq,
            _ => return Ok(lost),
        }
    }
}

/// Evaluates every session that this station start lost after `after`
/// (the store head before recovery) and stores at most one continuation
/// intent per lost turn. A session with auto-continue off is not touched
/// (no event; DL-127). Returns the number of intents stored.
///
/// # Errors
/// A database error.
pub async fn evaluate(store: &Store, policy: &Policy, after: u64) -> crate::store::Result<usize> {
    let mut stored = 0;
    for session_id in lost_after(store, after).await? {
        let Some(summary) = store.summary(session_id.clone()).await? else {
            continue;
        };
        if !summary.auto_continue || summary.state != SessionState::Lost {
            continue;
        }
        let Some(w) = store.lost_work(session_id.clone(), after).await? else {
            continue;
        };
        let turn = w
            .turn_id
            .as_ref()
            .map_or_else(|| format!("none-{}", w.lost_seq), |t| t.as_str().to_owned());
        let envelope = CommandEnvelope {
            authority: None,
            command_id: CommandId::from(format!("{COMMAND_PREFIX}{session_id}:{turn}")),
            target_incarnation: None,
            retry_until: store
                .now_ms()
                .saturating_add(fleetdeck_proto::command::RETRY_WINDOW_MS),
            origin: CommandOrigin::Local,
            command: SessionCommand::Resume {
                session_id: session_id.clone(),
            },
        };
        let policy = policy.clone();
        let id = session_id.clone();
        let decided = store
            .decide(envelope, Caller::local(), move |d| {
                decide_lost(d, &policy, &id, after)
            })
            .await?;
        if !decided.duplicate && decided.effects.iter().any(|e| e.session_id == session_id) {
            stored += 1;
        }
    }
    Ok(stored)
}

/// The dispatch recheck of a continuation (§2.4, DL-133): the stored
/// rules of [`Store::continuation_recheck`], and with the station `policy`
/// the provider instance (rule 2), the allowed workspace roots and the
/// session limit (rule 10). On a shim host (`boot` is the boot id reader)
/// the DL-182 proof is checked again like a manual resume (DL-188 step 3).
/// `Some(code)`: the effect is not dispatched, the session stays `lost`
/// and gets the skip event.
pub async fn recheck(
    store: &Store,
    policy: Option<&Policy>,
    boot: Option<&BootIdReader>,
    effect: &Effect,
) -> Option<String> {
    let stored = store
        .continuation_recheck(effect.clone(), policy.map(Policy::max_sessions))
        .await;
    let code = match stored {
        Ok(None) => match policy.and_then(|p| policy_recheck(p, effect)) {
            None => match shim_recheck(store, policy, boot, effect).await {
                None => return None,
                Some(code) => code.to_owned(),
            },
            Some(code) => code.to_owned(),
        },
        Ok(Some(code)) => code,
        Err(e) => {
            tracing::error!(error = %e, "the continuation recheck failed; not dispatched");
            "recheck_failed".to_owned()
        }
    };
    let events = vec![
        Event::AutoContinueSkipped {
            reason: code.clone(),
        }
        .into(),
        Event::SessionStateChanged {
            state: SessionState::Lost,
            detail: Some(format!("auto-continue not dispatched: {code}")),
        }
        .into(),
    ];
    if let Err(e) = store.append(effect.session_id.clone(), events).await {
        tracing::error!(error = %e, "the continuation skip was not stored");
    }
    Some(code)
}

/// The DL-182 proof of a continuation on a shim host, read again at the
/// dispatch with the arguments of a manual resume: another execution of the
/// conversation, or a reboot proof that does not hold any more, is
/// `execution_unknown` (DL-188 step 3). A store error and a missing
/// provider kind fail closed too.
async fn shim_recheck(
    store: &Store,
    policy: Option<&Policy>,
    boot: Option<&BootIdReader>,
    effect: &Effect,
) -> Option<&'static str> {
    let boot = boot?;
    let crate::store::EffectPayload::Resume {
        native_session_id,
        spec,
        continue_by: Some(_),
        ..
    } = &effect.payload
    else {
        return None;
    };
    let session = &effect.session_id;
    let Some(kind) = policy.and_then(|p| p.instance(&spec.provider_instance)) else {
        return Some("execution_unknown");
    };
    let found = store
        .execution_exclusion(
            Some(session.clone()),
            kind,
            Some(native_session_id.clone()),
            Some(session.clone()),
            Purpose::Resume,
        )
        .await;
    match found {
        Ok(found) if found.blocked.is_none() => {
            let why = super::shims::revalidate(boot, &found.reboot_rows)?;
            tracing::info!(%why, "a continuation is not dispatched: the reboot proof does not hold");
            Some("execution_unknown")
        }
        Ok(_) => Some("execution_unknown"),
        Err(e) => {
            tracing::error!(error = %e, "the shim recheck of a continuation failed");
            Some("execution_unknown")
        }
    }
}

/// Rules 2 and 10 of a continuation against the current station policy:
/// the provider instance is still configured, and the workspace is still
/// inside an allowed root.
fn policy_recheck(policy: &Policy, effect: &Effect) -> Option<&'static str> {
    let crate::store::EffectPayload::Resume { spec, .. } = &effect.payload else {
        return None;
    };
    if policy.instance(&spec.provider_instance).is_none() {
        Some("identity_changed")
    } else if policy.workspace(&spec.cwd).is_err() {
        Some("not_resumable")
    } else {
        None
    }
}

// The shim host and its process checks are Linux only (DL-188 step 3).
#[cfg(test)]
#[cfg(target_os = "linux")]
#[path = "auto_continue_shim_tests.rs"]
mod shim_tests;
