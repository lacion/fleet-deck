//! The decide and dispatch halves of the shell service: the receipt
//! transaction that enqueues a durable shell effect, and the runner of the
//! queued effects after the commit (DL-405).

use std::sync::Arc;

use fleetdeck_proto::SessionId;
use fleetdeck_proto::command::{CommandResult, RejectCode};
use fleetdeck_proto::shell::{ShellChange, ShellEnvelope};

use super::handover;
use super::{Shells, StartError, reject};
use crate::manager::policy::Policy;
use crate::store::effects::EffectId;
use crate::store::shell_effects;
use crate::store::{DecideError, Decision, Store, StoreError};

/// Decides one shell command in its receipt transaction: the policy check
/// and one durable `queued` effect, no PTY I/O (DL-405).
///
/// # Errors
/// The check's reject (stored as the receipt), or a store error.
pub fn decide(
    d: &mut Decision<'_>,
    shells: &Shells,
    policy: &Policy,
    envelope: &ShellEnvelope,
) -> Result<CommandResult, DecideError> {
    let pending = shell_effects::pending(d.connection())?;
    shells.check(&envelope.change, policy, &pending)?;
    match &envelope.change {
        ShellChange::Open { shell_id, .. } => handover::free_of_holds(d, shell_id)?,
        ShellChange::Handover {
            shell_id,
            session_id,
            ..
        } => {
            handover::free_of_holds(d, shell_id)?;
            handover::admit(d, policy, &shells.config.instances, session_id, shell_id)?;
        }
        ShellChange::Release { shell_id } => {
            handover::decide_release(d, shells, &pending, shell_id)?;
            return Ok(CommandResult::default());
        }
        _ => {}
    }
    let effect_id: EffectId = d.new_id();
    shell_effects::enqueue(
        d.connection(),
        effect_id.as_str(),
        &envelope.command_id,
        &envelope.change,
        d.now(),
    )?;
    Ok(CommandResult::default())
}

/// Runs the queued shell effects in commit order: each is claimed before
/// its PTY I/O and ends `confirmed` or `failed`. Returns how many ran.
///
/// # Errors
/// A store error; the effect then stays claimed, and the next start
/// recovery marks it `outcome_unknown`.
pub async fn dispatch(
    store: &Store,
    shells: &Arc<Shells>,
    policy: &Policy,
) -> Result<usize, StoreError> {
    let _turn = shells.dispatch.lock().await;
    let mut done = 0;
    while let Some(effect) = store.claim_shell_effect().await? {
        let worker = Arc::clone(shells);
        let policy = policy.clone();
        let now = store.now_ms();
        let change = effect.change;
        let outcome = if let ShellChange::Handover {
            shell_id,
            session_id,
            cols,
            rows,
        } = &change
        {
            start_terminal(store, worker, shell_id, session_id, (*cols, *rows), now).await?
        } else {
            tokio::task::spawn_blocking(move || worker.execute(&change, &policy, now))
                .await
                .unwrap_or_else(|e| Err(format!("shell task: {e}")))
        };
        if let Err(e) = &outcome {
            tracing::warn!(command = %effect.command_id, error = %e, "shell effect failed");
        }
        store.finish_shell_effect(effect.effect_id, outcome).await?;
        done += 1;
    }
    Ok(done)
}

/// The PTY I/O of one `shell.handover`: starts the TUI and records its pid
/// in the hold, or ends the hold when the TUI did not start.
async fn start_terminal(
    store: &Store,
    shells: Arc<Shells>,
    shell_id: &str,
    session_id: &SessionId,
    (cols, rows): (u16, u16),
    now: i64,
) -> Result<Result<(), String>, StoreError> {
    let started = match store.handover_launch(session_id.clone()).await? {
        None => Err(StartError::NotStarted(reject(
            RejectCode::InvalidState,
            "the session has no resume identity",
        ))),
        Some(launch) => {
            let (shell, session) = (shell_id.to_owned(), session_id.clone());
            tokio::task::spawn_blocking(move || {
                shells.handover(&shell, &session, &launch, cols, rows, now)
            })
            .await
            .unwrap_or_else(|e| {
                // The task may have died after the child started.
                Err(StartError::MayRun {
                    pid: None,
                    message: format!("shell task: {e}"),
                })
            })
        }
    };
    match started {
        Ok(pid) => {
            if let Some(pid) = pid {
                store
                    .record_terminal_pid(session_id.clone(), shell_id.to_owned(), pid)
                    .await?;
            }
            Ok(Ok(()))
        }
        Err(StartError::NotStarted(e)) => {
            let reason = format!("the terminal did not start: {}", e.message);
            store
                .release_terminal(session_id.clone(), shell_id.to_owned(), reason)
                .await?;
            Ok(Err(e.message))
        }
        Err(StartError::MayRun { pid, message }) => {
            // The process may run: the hold stays, with the pid when known.
            if let Some(pid) = pid {
                store
                    .record_terminal_pid(session_id.clone(), shell_id.to_owned(), pid)
                    .await?;
            }
            let reason =
                format!("the terminal failed to start and its process may still run: {message}");
            store
                .note_terminal(session_id.clone(), shell_id.to_owned(), pid, reason)
                .await?;
            Ok(Err(message))
        }
    }
}
