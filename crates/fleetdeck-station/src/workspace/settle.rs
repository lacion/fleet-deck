//! Start recovery of turn checkpoint rows (PLAN §6.6 "turn.finalized after
//! the refs"; DL-082).
//!
//! A station crash can leave a row with a side `capturing` (the ref may or
//! may not exist) or an end side `open` (the turn did not reach its
//! completion boundary). Recovery settles each side **from the refs alone**:
//!
//! - `capturing` and the ref exists → `available {commit}`: the publication
//!   finished before the crash;
//! - `capturing` and no ref → `unavailable {station_restart}`;
//! - `open` → `unavailable {station_restart}`: the workspace changed after
//!   the crash, so a capture now would not be the turn's boundary;
//! - `available {commit}` is checked again (DL-083): the ref must still name
//!   that commit. A ref that is gone (or a worktree that is gone, moved or
//!   another one) → `unavailable {checkpoint_missing}`; a ref that names
//!   another commit or is not a checkpoint of this worktree →
//!   `unavailable {checkpoint_changed}`. So `turn.finalized` never names a
//!   checkpoint that does not exist;
//! - a capturing side whose ref is not ours → `unavailable {capture_failed}`;
//!   a gone/moved/other worktree makes undecided sides
//!   `unavailable {station_restart}`.
//!
//! It never captures. Then a row with a provider turn gets `turn.finalized`
//! (idempotent per app turn) and a row without one is abandoned. With the
//! direct-child host no provider process survives the restart, so every
//! unsettled row is settled here (ADR-005 adds reattached processes later).

use std::path::Path;

use fleetdeck_proto::event::DiffSummary;

use super::boundary::{
    REASON_CAPTURE_FAILED, REASON_CHECKPOINT_CHANGED, REASON_CHECKPOINT_MISSING,
    REASON_STATION_RESTART,
};
use super::checkpoint::{Checkpoint, CheckpointKey, Phase, Worktree};
use super::git::GitOptions;
use crate::store::{AppTurnId, RowSide, RowState, Store, StoreError, TurnCheckpointRow, Which};

/// The settled sides of one row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolution {
    pub start: RowSide,
    pub end: RowSide,
    pub diff_summary: Option<DiffSummary>,
}

/// What [`settle_unsettled`] did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SettleReport {
    pub finalized: Vec<AppTurnId>,
    pub abandoned: Vec<AppTurnId>,
}

/// Resolves the undecided sides of `row` from its refs (module docs).
pub async fn resolve(
    row: &TurnCheckpointRow,
    incarnation: &str,
    options: GitOptions,
) -> Resolution {
    let worktree = match (&row.worktree_root, &row.worktree_id) {
        (Some(root), Some(id)) => match Worktree::open(Path::new(root), options).await {
            Ok(w) if w.id() == id => Some(w),
            _ => None,
        },
        _ => None,
    };
    let key = |phase| CheckpointKey {
        incarnation: incarnation.to_owned(),
        session: row.session_id.as_str().to_owned(),
        turn: row.app_turn.as_str().to_owned(),
        phase,
    };
    let start = side(worktree.as_ref(), &row.start, &key(Phase::Start)).await;
    let end = side(worktree.as_ref(), &row.end, &key(Phase::Completion)).await;
    let mut diff_summary = None;
    if let (Some(w), (RowSide::Available(_), Some(from)), (RowSide::Available(_), Some(to))) =
        (worktree.as_ref(), &start, &end)
    {
        diff_summary = w.diff_summary(from, to, &[]).await.ok();
    }
    Resolution {
        start: start.0,
        end: end.0,
        diff_summary,
    }
}

async fn side(
    worktree: Option<&Worktree>,
    stored: &RowSide,
    key: &CheckpointKey,
) -> (RowSide, Option<Checkpoint>) {
    let unavailable = |why: &str| (RowSide::Unavailable(why.to_owned()), None);
    match stored {
        RowSide::Unavailable(_) => (stored.clone(), None),
        RowSide::Open => unavailable(REASON_STATION_RESTART),
        RowSide::Capturing => {
            let Some(w) = worktree else {
                return unavailable(REASON_STATION_RESTART);
            };
            match w.read(key).await {
                Ok(Some(found)) => (RowSide::Available(found.commit.clone()), Some(found)),
                Ok(None) => unavailable(REASON_STATION_RESTART),
                Err(_) => unavailable(REASON_CAPTURE_FAILED),
            }
        }
        // A recorded checkpoint is kept only while its ref still proves it.
        RowSide::Available(commit) => {
            let Some(w) = worktree else {
                return unavailable(REASON_CHECKPOINT_MISSING);
            };
            match w.read(key).await {
                Ok(Some(found)) if found.commit == *commit => (stored.clone(), Some(found)),
                Ok(Some(_)) | Err(_) => unavailable(REASON_CHECKPOINT_CHANGED),
                Ok(None) => unavailable(REASON_CHECKPOINT_MISSING),
            }
        }
    }
}

/// Stores one resolved side: records an undecided side, or withdraws a
/// recorded `available` side that its ref no longer proves.
async fn store_side(
    store: &Store,
    id: &AppTurnId,
    which: Which,
    stored: &RowSide,
    resolved: RowSide,
) -> Result<(), StoreError> {
    match (stored, resolved) {
        (RowSide::Available(commit), RowSide::Unavailable(reason)) => {
            store
                .withdraw_checkpoint_side(id.clone(), which, commit.clone(), reason)
                .await?;
        }
        (_, resolved) => {
            store
                .record_checkpoint_side(id.clone(), which, resolved)
                .await?;
        }
    }
    Ok(())
}

/// Checks the sides of a row against its refs again, stores the result and
/// commits `turn.finalized` (idempotent per app turn). The live session
/// manager and start recovery share this path, so `turn.finalized` never
/// names a ref that does not exist.
///
/// # Errors
/// A store error, or a row without a bound provider turn.
pub async fn finalize_row(
    store: &Store,
    row: &TurnCheckpointRow,
    options: GitOptions,
) -> Result<crate::store::Finalized, StoreError> {
    let incarnation = store.incarnation().as_str().to_owned();
    let resolved = resolve(row, &incarnation, options).await;
    let id = row.app_turn.clone();
    store_side(store, &id, Which::Start, &row.start, resolved.start).await?;
    store_side(store, &id, Which::End, &row.end, resolved.end).await?;
    store.finalize_turn(id, resolved.diff_summary).await
}

/// Settles every unsettled row after a station start (module docs).
///
/// # Errors
/// A store error; rows settled before it stay settled.
pub async fn settle_unsettled(
    store: &Store,
    options: GitOptions,
) -> Result<SettleReport, StoreError> {
    let incarnation = store.incarnation().as_str().to_owned();
    let mut report = SettleReport::default();
    for row in store.unsettled_turn_checkpoints().await? {
        if row.state != RowState::Open {
            continue;
        }
        let resolved = resolve(&row, &incarnation, options.clone()).await;
        let id = row.app_turn.clone();
        store_side(store, &id, Which::Start, &row.start, resolved.start).await?;
        store_side(store, &id, Which::End, &row.end, resolved.end).await?;
        if row.turn_id.is_some() {
            store
                .finalize_turn(id.clone(), resolved.diff_summary)
                .await?;
            report.finalized.push(id);
        } else {
            store
                .abandon_turn_checkpoint(id.clone(), REASON_STATION_RESTART.to_owned())
                .await?;
            report.abandoned.push(id);
        }
    }
    Ok(report)
}
