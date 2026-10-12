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
//! - the worktree is gone, moved or another one → `unavailable
//!   {station_restart}` for every undecided side;
//! - a ref that is not ours → `unavailable {capture_failed}`.
//!
//! It never captures. Then a row with a provider turn gets `turn.finalized`
//! (idempotent per app turn) and a row without one is abandoned. With the
//! direct-child host no provider process survives the restart, so every
//! unsettled row is settled here (ADR-005 adds reattached processes later).

use std::path::Path;

use fleetdeck_proto::event::DiffSummary;

use super::boundary::{REASON_CAPTURE_FAILED, REASON_STATION_RESTART};
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
        diff_summary = w.diff_summary(from, to).await.ok();
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
    let Some(w) = worktree else {
        return match stored {
            RowSide::Open | RowSide::Capturing => unavailable(REASON_STATION_RESTART),
            decided => (decided.clone(), None),
        };
    };
    match stored {
        RowSide::Unavailable(_) => (stored.clone(), None),
        RowSide::Open => unavailable(REASON_STATION_RESTART),
        RowSide::Capturing | RowSide::Available(_) => match w.read(key).await {
            Ok(Some(found)) => match stored {
                // The stored commit wins; a ref that moved is not ours.
                RowSide::Available(commit) if *commit != found.commit => {
                    unavailable(REASON_CAPTURE_FAILED)
                }
                _ => (RowSide::Available(found.commit.clone()), Some(found)),
            },
            Ok(None) if matches!(stored, RowSide::Capturing) => unavailable(REASON_STATION_RESTART),
            // An available side whose ref disappeared keeps its record; the
            // diff needs the ref, so there is none.
            Ok(None) => (stored.clone(), None),
            Err(_) => unavailable(REASON_CAPTURE_FAILED),
        },
    }
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
        store
            .record_checkpoint_side(id.clone(), Which::Start, resolved.start)
            .await?;
        store
            .record_checkpoint_side(id.clone(), Which::End, resolved.end)
            .await?;
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
