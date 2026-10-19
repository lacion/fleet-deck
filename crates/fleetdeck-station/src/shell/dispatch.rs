//! The decide and dispatch halves of the shell service: the receipt
//! transaction that enqueues a durable shell effect, and the runner of the
//! queued effects after the commit (DL-405).

use std::sync::Arc;

use fleetdeck_core::launch_dir::LaunchDir;
use fleetdeck_proto::SessionId;
use fleetdeck_proto::command::{CommandResult, Reject, RejectCode};
use fleetdeck_proto::shell::{ShellChange, ShellEnvelope};

use super::handover;
use super::{SHELLS_OFF, Shells, StartError, reject};
use crate::manager::policy::Policy;
use crate::manager::policy_hub::PolicyHub;
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
    // A folder in the trash, or being moved, takes no shell (DL-150). The
    // check comes first: the original folder is missing while it is held.
    if let ShellChange::Open { cwd, .. } = &envelope.change {
        crate::manager::trash::custody(d, cwd)?;
        if let Ok(canonical) = policy.workspace(cwd) {
            crate::manager::trash::custody(d, &canonical.to_string_lossy())?;
        }
    }
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
/// its PTY I/O and ends `confirmed` or `failed`. Each effect reads the
/// current policy snapshot, so a queued open or handover that the new
/// snapshot refuses fails (DL-172 §1.4). Returns how many ran.
///
/// # Errors
/// A store error; the effect then stays claimed, and the next start
/// recovery marks it `outcome_unknown`.
pub async fn dispatch(
    store: &Store,
    shells: &Arc<Shells>,
    hub: &PolicyHub,
) -> Result<usize, StoreError> {
    let _turn = shells.dispatch.lock().await;
    let mut done = 0;
    while let Some(effect) = store.claim_shell_effect().await? {
        let worker = Arc::clone(shells);
        let policy = hub.current();
        let now = store.now_ms();
        let change = effect.change;
        let held = custody_refusal(store, &policy, &change).await?;
        let outcome = if let Some(text) = held {
            Err(text)
        } else if let ShellChange::Handover {
            shell_id,
            session_id,
            cols,
            rows,
        } = &change
        {
            start_terminal(
                store,
                worker,
                &policy,
                shell_id,
                session_id,
                (*cols, *rows),
                now,
            )
            .await?
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

/// A shell open in a folder that a worktree operation holds: the effect
/// was admitted before the hold began, and it fails now, before it spawns
/// the PTY (DL-150). The decision of a trash or a purge refuses while an
/// open is queued or claimed, so a claimed open that passes this check is
/// seen by every later trash decision.
async fn custody_refusal(
    store: &Store,
    policy: &Policy,
    change: &ShellChange,
) -> Result<Option<String>, StoreError> {
    let ShellChange::Open { cwd, .. } = change else {
        return Ok(None);
    };
    let mut paths = vec![cwd.clone()];
    if let Ok(canonical) = policy.workspace(cwd) {
        paths.push(canonical.to_string_lossy().into_owned());
    }
    for path in paths {
        if store.worktree_op_over(path).await?.is_some() {
            return Ok(Some(crate::manager::trash::HELD.to_owned()));
        }
    }
    Ok(None)
}

/// The stored folder of a handover under the current policy: it must be
/// text and inside the allowed roots now (NV-24). The result is the
/// canonical path.
fn launch_workspace(policy: &Policy, cwd: &std::path::Path) -> Result<std::path::PathBuf, Reject> {
    let Some(text) = cwd.to_str() else {
        return Err(reject(
            RejectCode::InvalidState,
            "the session directory is not valid text",
        ));
    };
    policy.workspace(text).map_err(|e| {
        reject(
            RejectCode::InvalidState,
            format!("the session directory is not allowed now: {e}"),
        )
    })
}

/// The PTY I/O of one `shell.handover`: starts the TUI and records its pid
/// in the hold, or ends the hold when the TUI did not start.
async fn start_terminal(
    store: &Store,
    shells: Arc<Shells>,
    policy: &Policy,
    shell_id: &str,
    session_id: &SessionId,
    (cols, rows): (u16, u16),
    now: i64,
) -> Result<Result<(), String>, StoreError> {
    let max_shells = policy.max_shells();
    let ring = policy.scrollback_limits();
    let started = if !policy.allow_shell() {
        // Shells turned off after the commit (DL-172 §1.4).
        Err(StartError::NotStarted(reject(
            RejectCode::Unsupported,
            SHELLS_OFF,
        )))
    } else {
        match store.handover_launch(session_id.clone()).await? {
            None => Err(StartError::NotStarted(reject(
                RejectCode::InvalidState,
                "the session has no resume identity",
            ))),
            Some(mut launch) => match launch_workspace(policy, &launch.cwd) {
                Err(refusal) => Err(StartError::NotStarted(refusal)),
                // The TUI starts in the checked folder object (B9).
                Ok(cwd) => match LaunchDir::open_in(policy.allowed_roots(), &cwd) {
                    Err(e) => Err(StartError::NotStarted(reject(
                        RejectCode::InvalidState,
                        format!("the session directory is not allowed now: {e}"),
                    ))),
                    Ok(dir) => {
                        launch.cwd = cwd;
                        let allow_bypass = policy.allow_bypass_permissions();
                        let (shell, session) = (shell_id.to_owned(), session_id.clone());
                        tokio::task::spawn_blocking(move || {
                            shells.handover(
                                &shell,
                                &session,
                                &launch,
                                &dir,
                                (cols, rows),
                                now,
                                max_shells,
                                allow_bypass,
                                ring,
                            )
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
                },
            },
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
