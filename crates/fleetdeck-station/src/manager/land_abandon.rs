//! `fleet_git.land_abandon` and `fleet_git.land_status` (FG-8, design §11.2,
//! revision 4, M3). The abandonment drains the land before it fences it:
//!
//! 1. it takes the per-land lock; the writer holds it from the intent
//!    transaction of a remote write until the outcome is stored, so an
//!    active write ends first; the wait is bounded (120 s) and then the
//!    answer is `busy`, recording nothing;
//! 2. with the lock held, one transaction reads `last_write_outcome` of the
//!    write that it waited for, stores it as `abandon_write`, inserts the
//!    fence, and ends a `running` run `unresolved` with `code = abandoned`;
//! 3. it releases the lock and answers; a repeat answers the same values.

use fleetdeck_proto::fleet_git::{LandAbandonAnswer, LandStatus, LandStatusAnswer, LandWrite};

use super::land::LandEngine;
use crate::store::land_runs::LandRunRow;
use crate::store::{Result, Store};

/// The tail of every abandon text: what the owner checks.
fn check_yourself(row: &LandRunRow) -> String {
    format!(
        "Check {}/{}, the branch {} and the pull requests yourself.",
        row.plan.fetch_remote,
        row.default_ref.as_deref().unwrap_or("the default branch"),
        row.plan.publish_ref
    )
}

/// The end text of an abandoned run, by the write that was waited for
/// (design §11.10). It has no publication sentence.
fn abandon_text(row: &LandRunRow, write: &str) -> String {
    let check = check_yourself(row);
    match write {
        "applied" => {
            format!("The land was stopped after its last write reached the remote. {check}")
        }
        "rejected" => format!("The land was stopped; the remote refused its last write. {check}"),
        "unknown" => {
            format!("The land was stopped; the result of its last write is not known. {check}")
        }
        _ => format!("Fleet Deck could not confirm the result of this land. {check}"),
    }
}

fn write_of(text: &str) -> LandWrite {
    match text {
        "applied" => LandWrite::Applied,
        "rejected" => LandWrite::Rejected,
        "unknown" => LandWrite::Unknown,
        _ => LandWrite::None,
    }
}

/// `fleet_git.land_abandon` for `land_id`.
///
/// # Errors
/// A store error.
pub async fn abandon(engine: &LandEngine, land_id: &str) -> Result<LandAbandonAnswer> {
    let (guard, waited) = match engine.locks.try_lock(land_id) {
        Some(guard) => (guard, false),
        None => {
            match engine
                .locks
                .lock_within(land_id, engine.timeouts.lock_wait)
                .await
            {
                Some(guard) => (guard, true),
                None => return Ok(LandAbandonAnswer::Busy),
            }
        }
    };
    let fenced = engine
        .store
        .land_abandon(land_id.to_owned(), waited, abandon_text)
        .await;
    drop(guard);
    engine.locks.release(land_id);
    let fenced = fenced?;
    Ok(LandAbandonAnswer::Fenced {
        write: write_of(fenced.write),
        run_state: fenced.run_state,
    })
}

/// The status of a run (ADR-009 §4).
#[must_use]
pub fn status_of(row: &LandRunRow) -> LandStatus {
    LandStatus {
        state: row.state,
        phase: row.phase,
        reconciling: row.reconciling,
        attempt: row.attempt,
        source_oid: row.source_oid.clone(),
        base_oid: row.base_oid.clone(),
        rebased_oid: row.rebased_oid.clone(),
        branch_published_oid: row.branch_published_oid.clone(),
        default_published_oid: row.default_published_oid.clone(),
        pr_url: row.pr_url.clone(),
        files: row.files.clone(),
        code: row.code.clone(),
        text: row.message.clone(),
    }
}

/// `fleet_git.land_status` for `land_id`: a run answers `Known` (also when
/// it is fenced); a fence without a run answers `Fenced`; else `Unknown`
/// (decision Q14 B, H10).
///
/// # Errors
/// A store error.
pub async fn status(store: &Store, land_id: &str) -> Result<LandStatusAnswer> {
    let (row, fenced) = store.land_known(land_id.to_owned()).await?;
    Ok(match (row, fenced) {
        (Some(row), _) => LandStatusAnswer::Known(Box::new(status_of(&row))),
        (None, true) => LandStatusAnswer::Fenced,
        (None, false) => LandStatusAnswer::Unknown,
    })
}
