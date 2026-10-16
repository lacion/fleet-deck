//! The revert executor and its start recovery (PLAN §6.6, L13; DL-087).
//!
//! Order of the external steps, each one durable before its I/O:
//!
//! 1. Under the session's workspace fence: recheck the row, the process,
//!    the checkpoint refs and the workspace (its tracked content must equal
//!    the completion checkpoint of the latest turn). A failed check ends the
//!    revert `failed` with nothing changed.
//! 2. Commit `rollback_sent`, then ask the provider to roll back its
//!    conversation. A refusal ends `failed` (nothing changed); an uncertain
//!    answer ends `reconciliation_required`.
//! 3. Commit `rolled_back` (the conversation matches the target), then
//!    `restoring`, then restore the tracked files from the start checkpoint.
//!    Only a workspace that exactly matches the target ends `succeeded`;
//!    a changed workspace or a partial restore ends
//!    `reconciliation_required`. A filesystem-only restore is never
//!    reported as a conversation rollback.
//!
//! Start recovery works from the stored step and never repeats a rollback:
//! `admitted` → `failed` (nothing was sent), `rollback_sent` →
//! `reconciliation_required`, `rolled_back`/`restoring` → finish the file
//! restore from the refs.
//!
//! The owner's `retry_restore` (DL-097) sends a `reconciliation_required`
//! revert with a confirmed rollback back to `rolled_back`; its effect runs
//! the file restore again with the same rules.

use std::path::Path;

use fleetdeck_providers::{LiveSession, ProviderError};

use super::turns::SessionTurns;
use crate::store::shell_runs::Barrier;
use crate::store::{
    Dispatch, ProcessIncarnation, RevertRow, RevertStep, RowState, Store, StoreError,
};
use crate::workspace::{
    Checkpoint, CheckpointKey, GitOptions, Limits, Phase, PrivatePaths, Rejected, Worktree,
};

/// The station services that a revert uses.
#[derive(Clone, Copy)]
pub struct RevertEnv<'a> {
    pub store: &'a Store,
    pub git: &'a GitOptions,
    pub limits: Limits,
    /// The private paths of the station: a restore never reads or writes
    /// a private file (FDV2-003).
    pub private: &'a PrivatePaths,
}

/// Why a revert ended.
pub mod reason {
    pub const PROCESS_CHANGED: &str = "process_changed";
    pub const NOT_ADMITTED: &str = "revert_not_admitted";
    pub const TURNS_CHANGED: &str = "turns_changed";
    pub const WORKTREE_MISSING: &str = "worktree_missing";
    pub const CHECKPOINT_CHANGED: &str = "checkpoint_changed";
    pub const WORKSPACE_CHANGED: &str = "workspace_changed";
    pub const WORKTREE_BUSY: &str = "worktree_busy";
    pub const ROLLBACK_REFUSED: &str = "rollback_refused";
    pub const ROLLBACK_UNCERTAIN: &str = "rollback_uncertain";
    pub const RESTORE_FAILED: &str = "restore_failed";
    pub const RESTORE_INCOMPLETE: &str = "restore_incomplete";
    /// "This checkpoint was recorded before a security fix and cannot be
    /// restored."
    pub const CHECKPOINT_UNVERIFIED: &str = "checkpoint_unverified";
    pub const STATION_RESTART: &str = "station_restart";
    /// A shell command ran in the folder after the target turn (B8).
    pub const SHELL_RAN_AFTER_TURN: &str = "shell_ran_after_turn";
    /// A shell command of the folder is not settled (B8).
    pub const SHELL_RUNNING: &str = "shell_running";
}

/// The reason code of the shell barrier for a revert row (B8), if it holds.
async fn shell_block(
    env: RevertEnv<'_>,
    row: &RevertRow,
) -> Result<Option<&'static str>, StoreError> {
    Ok(env
        .store
        .shell_barrier(
            row.revert.worktree_root.clone(),
            row.revert.app_turn.0.clone(),
        )
        .await?
        .map(|b| match b {
            Barrier::Running => reason::SHELL_RUNNING,
            Barrier::RanAfterTurn => reason::SHELL_RAN_AFTER_TURN,
        }))
}

/// The conversation rollback of a revert: the direct driver's
/// [`LiveSession::rollback`], or the durable shim driver (DL-116).
/// `Ok` is a confirmed rollback, [`ProviderError::Uncertain`] an unknown
/// outcome, every other error a definite refusal (nothing rolled back).
#[async_trait::async_trait]
pub trait Rollback: Send {
    async fn roll_back(&mut self, turns: u32) -> Result<(), ProviderError>;
}

#[async_trait::async_trait]
impl<T: LiveSession + ?Sized> Rollback for T {
    async fn roll_back(&mut self, turns: u32) -> Result<(), ProviderError> {
        self.rollback(turns).await
    }
}

/// Runs an admitted revert on the live session.
pub async fn run<R: Rollback + ?Sized>(
    env: RevertEnv<'_>,
    turns: &SessionTurns,
    session: &mut R,
    process: &ProcessIncarnation,
    revert_id: &str,
) -> Dispatch {
    // B8: the shell barrier is checked before the wait on the fence.
    match barrier_at_dispatch(env, revert_id).await {
        Ok(Some(dispatch)) => return dispatch,
        Ok(None) => {}
        Err(e) => {
            tracing::error!(error = %e, revert_id, "revert store failure");
            return Dispatch::Unknown(format!("store: {e}"));
        }
    }
    let _fence = turns.fence().await;
    match execute(env, session, process, revert_id).await {
        Ok(dispatch) => dispatch,
        Err(e) => {
            tracing::error!(error = %e, revert_id, "revert store failure");
            Dispatch::Unknown(format!("store: {e}"))
        }
    }
}

/// The shell barrier at dispatch (B8): an `admitted` revert fails, a
/// `rolled_back` one (the owner's retry of the restore) goes back to
/// `reconciliation_required`; nothing is written to the files.
async fn barrier_at_dispatch(
    env: RevertEnv<'_>,
    revert_id: &str,
) -> Result<Option<Dispatch>, StoreError> {
    let Some(row) = env.store.revert(revert_id.to_owned()).await? else {
        return Ok(None);
    };
    let (from, to): (&'static [RevertStep], RevertStep) = match row.step {
        RevertStep::Admitted => (&[RevertStep::Admitted], RevertStep::Failed),
        RevertStep::RolledBack => (
            &[RevertStep::RolledBack],
            RevertStep::ReconciliationRequired,
        ),
        _ => return Ok(None),
    };
    let Some(why) = shell_block(env, &row).await? else {
        return Ok(None);
    };
    step(env, revert_id, from, to, Some(why)).await?;
    Ok(Some(Dispatch::NotDispatched(why.to_owned())))
}

async fn execute<R: Rollback + ?Sized>(
    env: RevertEnv<'_>,
    session: &mut R,
    process: &ProcessIncarnation,
    revert_id: &str,
) -> Result<Dispatch, StoreError> {
    let Some(row) = env.store.revert(revert_id.to_owned()).await? else {
        return Ok(Dispatch::NotDispatched(reason::NOT_ADMITTED.into()));
    };
    if row.step == RevertStep::RolledBack {
        // The owner's retry of the file restore (DL-097): the rollback is
        // confirmed; the restore runs again under the same rules.
        return retry_restore(env, &row, process).await;
    }
    if row.step != RevertStep::Admitted {
        return Ok(Dispatch::NotDispatched(reason::NOT_ADMITTED.into()));
    }
    if let Err(why) = check(env, &row, process).await? {
        return fail(env, revert_id, why).await;
    }
    if !step(
        env,
        revert_id,
        &[RevertStep::Admitted],
        RevertStep::RollbackSent,
        None,
    )
    .await?
    {
        return Ok(Dispatch::NotDispatched(reason::NOT_ADMITTED.into()));
    }
    match session.roll_back(row.revert.turns).await {
        Ok(()) => {}
        Err(ProviderError::Uncertain(detail)) => {
            tracing::warn!(revert_id, %detail, "rollback outcome unknown");
            let why = reason::ROLLBACK_UNCERTAIN;
            step(
                env,
                revert_id,
                &[RevertStep::RollbackSent],
                RevertStep::ReconciliationRequired,
                Some(why),
            )
            .await?;
            return Ok(Dispatch::Unknown(why.into()));
        }
        Err(e) => {
            let why = format!("{}: {e}", reason::ROLLBACK_REFUSED);
            step(
                env,
                revert_id,
                &[RevertStep::RollbackSent],
                RevertStep::Failed,
                Some(&why),
            )
            .await?;
            return Ok(Dispatch::NotDispatched(why));
        }
    }
    step(
        env,
        revert_id,
        &[RevertStep::RollbackSent],
        RevertStep::RolledBack,
        None,
    )
    .await?;
    restore(env, &row).await
}

/// Finishes the file restore of a revert whose conversation rollback was
/// confirmed (`rolled_back` or `restoring`).
///
/// # Errors
/// A store error.
pub async fn restore(env: RevertEnv<'_>, row: &RevertRow) -> Result<Dispatch, StoreError> {
    let id = row.revert.revert_id.as_str();
    let moved = step(
        env,
        id,
        &[RevertStep::RolledBack, RevertStep::Restoring],
        RevertStep::Restoring,
        None,
    )
    .await?;
    if !moved {
        return Ok(Dispatch::NotDispatched(reason::NOT_ADMITTED.into()));
    }
    let outcome = restore_files(env, row).await;
    let (to, why) = match outcome {
        Ok(()) => (RevertStep::Succeeded, None),
        Err(why) => (RevertStep::ReconciliationRequired, Some(why)),
    };
    step(env, id, &[RevertStep::Restoring], to, why).await?;
    Ok(match why {
        None => Dispatch::Confirmed,
        Some(why) => Dispatch::Unknown(why.into()),
    })
}

/// The owner's retry of a file restore after a confirmed rollback. A
/// changed process or a busy worktree sends the revert back to
/// `reconciliation_required` with nothing written.
async fn retry_restore(
    env: RevertEnv<'_>,
    row: &RevertRow,
    process: &ProcessIncarnation,
) -> Result<Dispatch, StoreError> {
    let id = row.revert.revert_id.as_str();
    let why = if *process != row.revert.process {
        Some(reason::PROCESS_CHANGED)
    } else if env
        .store
        .worktree_conflict(
            row.revert.worktree_root.clone(),
            row.revert.session_id.clone(),
        )
        .await?
        .is_some()
    {
        Some(reason::WORKTREE_BUSY)
    } else {
        // B8: every restore retry meets the shell barrier.
        shell_block(env, row).await?
    };
    if let Some(why) = why {
        step(
            env,
            id,
            &[RevertStep::RolledBack],
            RevertStep::ReconciliationRequired,
            Some(why),
        )
        .await?;
        return Ok(Dispatch::NotDispatched(why.into()));
    }
    restore(env, row).await
}

async fn restore_files(env: RevertEnv<'_>, row: &RevertRow) -> Result<(), &'static str> {
    let worktree = Worktree::open(Path::new(&row.revert.worktree_root), env.git.clone())
        .await
        .map_err(|_| reason::WORKTREE_MISSING)?;
    let target = read(env, &worktree, row, Phase::Start, &row.revert.app_turn.0).await?;
    if target.commit != row.revert.target_commit {
        return Err(reason::CHECKPOINT_CHANGED);
    }
    let current = read(
        env,
        &worktree,
        row,
        Phase::Completion,
        &row.revert.current_app_turn.0,
    )
    .await?;
    // The completion ref must still be the admitted one: a repointed ref
    // would make a changed workspace look expected (DL-093). Checked before
    // any file changes, also in recovery.
    if current.commit != row.revert.current_commit {
        return Err(reason::CHECKPOINT_CHANGED);
    }
    let plan = match worktree
        .plan_restore(&target, env.limits, env.private)
        .await
    {
        Ok(Ok(plan)) => plan,
        Ok(Err(Rejected::OldCapture)) => return Err(reason::CHECKPOINT_UNVERIFIED),
        Ok(Err(_)) | Err(_) => return Err(reason::RESTORE_FAILED),
    };
    if plan.current_tree == target.tree {
        // A restore that a crash interrupted after its last write.
        return Ok(());
    }
    if plan.current_tree != current.tree {
        return Err(reason::WORKSPACE_CHANGED);
    }
    match worktree.apply_restore(&plan, env.private).await {
        Ok(report) if report.matches => Ok(()),
        Ok(_) => Err(reason::RESTORE_INCOMPLETE),
        Err(_) => Err(reason::RESTORE_FAILED),
    }
}

/// The checks of step 1; `Ok(Err(reason))` for a refused revert.
async fn check(
    env: RevertEnv<'_>,
    row: &RevertRow,
    process: &ProcessIncarnation,
) -> Result<Result<(), &'static str>, StoreError> {
    if &row.revert.process != process {
        return Ok(Err(reason::PROCESS_CHANGED));
    }
    // Another session in the worktree became busy after the admission
    // (DL-093); the shared worktree fence keeps it from starting a write
    // while the revert runs.
    if env
        .store
        .worktree_conflict(
            row.revert.worktree_root.clone(),
            row.revert.session_id.clone(),
        )
        .await?
        .is_some()
    {
        return Ok(Err(reason::WORKTREE_BUSY));
    }
    // The same non-reverted history as the admission (DL-093).
    let rows = env
        .store
        .revertable_turn_checkpoints(row.revert.session_id.clone())
        .await?;
    let Some(at) = rows.iter().position(|r| r.app_turn == row.revert.app_turn) else {
        return Ok(Err(reason::TURNS_CHANGED));
    };
    let later = &rows[at..];
    let last = later.iter().rev().find(|r| r.state == RowState::Finalized);
    if later.iter().any(|r| r.state == RowState::Open)
        || last.map(|r| &r.app_turn) != Some(&row.revert.current_app_turn)
    {
        return Ok(Err(reason::TURNS_CHANGED));
    }
    let Ok(worktree) = Worktree::open(Path::new(&row.revert.worktree_root), env.git.clone()).await
    else {
        return Ok(Err(reason::WORKTREE_MISSING));
    };
    let target = match read(env, &worktree, row, Phase::Start, &row.revert.app_turn.0).await {
        Ok(cp) if cp.commit == row.revert.target_commit => cp,
        Ok(_) | Err(_) => return Ok(Err(reason::CHECKPOINT_CHANGED)),
    };
    let current = match read(
        env,
        &worktree,
        row,
        Phase::Completion,
        &row.revert.current_app_turn.0,
    )
    .await
    {
        Ok(cp) if cp.commit == row.revert.current_commit => cp,
        Ok(_) | Err(_) => return Ok(Err(reason::CHECKPOINT_CHANGED)),
    };
    match worktree
        .plan_restore(&target, env.limits, env.private)
        .await
    {
        Ok(Ok(plan)) if plan.current_tree == current.tree => Ok(Ok(())),
        Ok(Ok(_)) => Ok(Err(reason::WORKSPACE_CHANGED)),
        Ok(Err(rejected)) => Ok(Err(rejected.code())),
        Err(_) => Ok(Err(reason::RESTORE_FAILED)),
    }
}

async fn read(
    env: RevertEnv<'_>,
    worktree: &Worktree,
    row: &RevertRow,
    phase: Phase,
    app_turn: &str,
) -> Result<Checkpoint, &'static str> {
    let key = CheckpointKey {
        incarnation: env.store.incarnation().as_str().to_owned(),
        session: row.revert.session_id.as_str().to_owned(),
        turn: app_turn.to_owned(),
        phase,
    };
    match worktree.read(&key).await {
        Ok(Some(cp)) => Ok(cp),
        Ok(None) | Err(_) => Err(reason::CHECKPOINT_CHANGED),
    }
}

async fn fail(env: RevertEnv<'_>, revert_id: &str, why: &str) -> Result<Dispatch, StoreError> {
    step(
        env,
        revert_id,
        &[RevertStep::Admitted],
        RevertStep::Failed,
        Some(why),
    )
    .await?;
    Ok(Dispatch::NotDispatched(why.to_owned()))
}

async fn step(
    env: RevertEnv<'_>,
    revert_id: &str,
    from: &'static [RevertStep],
    to: RevertStep,
    why: Option<&str>,
) -> Result<bool, StoreError> {
    let moved = env
        .store
        .advance_revert(revert_id.to_owned(), from, to, why.map(str::to_owned))
        .await?;
    if moved {
        // A crash test kills the station after this commit, before the
        // next external step (for example after a confirmed rollback and
        // before the file restore).
        crate::fault::hit(crate::fault::FaultPoint::AfterStep, to.as_str());
    }
    Ok(moved)
}

/// Start recovery of the reverts that a station stop interrupted.
///
/// # Errors
/// A store error.
pub async fn recover(env: RevertEnv<'_>) -> Result<usize, StoreError> {
    let running = env.store.running_reverts().await?;
    let n = running.len();
    for row in running {
        let id = row.revert.revert_id.as_str();
        match row.step {
            RevertStep::Admitted => {
                step(
                    env,
                    id,
                    &[RevertStep::Admitted],
                    RevertStep::Failed,
                    Some(reason::STATION_RESTART),
                )
                .await?;
            }
            RevertStep::RollbackSent => {
                step(
                    env,
                    id,
                    &[RevertStep::RollbackSent],
                    RevertStep::ReconciliationRequired,
                    Some(reason::ROLLBACK_UNCERTAIN),
                )
                .await?;
            }
            RevertStep::RolledBack | RevertStep::Restoring => {
                // B8: no restore across a shell command, also in recovery.
                if let Some(why) = shell_block(env, &row).await? {
                    step(
                        env,
                        id,
                        &[RevertStep::RolledBack, RevertStep::Restoring],
                        RevertStep::ReconciliationRequired,
                        Some(why),
                    )
                    .await?;
                    continue;
                }
                restore(env, &row).await?;
            }
            RevertStep::Succeeded
            | RevertStep::Failed
            | RevertStep::ReconciliationRequired
            | RevertStep::Reconciled => {}
        }
    }
    Ok(n)
}

#[cfg(test)]
#[path = "revert_tests.rs"]
mod revert_tests;
