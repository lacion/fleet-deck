//! The recovery of the land runs of an earlier process (FG-8, design §11.5,
//! "Reconciliation after a crash or a lost answer", decision 55, M1). At the
//! station start, and at `next_reconcile_at` for a run with `reconciling = 1`,
//! the engine dispatches on `pending_action`, not on the phase, and uses only
//! the stored `pending_*` operands:
//!
//! - no pending write, phase `admitted` to `checked`: nothing external ran;
//!   the run ends `failed` with `station_restarted`;
//! - no pending write, phase `branch_pushed`: step 6 or 7 goes on;
//! - a pending write: the read of its table row, then the same rules as a
//!   live run (`land_publish.rs`).
//!
//! A reconciliation reads first and writes only after a confirmed read; the
//! write goes through the gated start.

use fleetdeck_proto::fleet_git::{LandPhase, code};

use super::land::Shared;
use super::land_publish::Ctx;
use crate::store::land_runs::LandRunRow;

/// Recovers one run.
pub(super) async fn run(shared: &Shared, row: LandRunRow) {
    let ctx = shared.ctx();
    if row.pending_action.is_some() || row.phase == LandPhase::BranchPushed {
        super::land_run::drive_loop(shared, row).await;
        return;
    }
    match row.phase {
        LandPhase::Admitted
        | LandPhase::Captured
        | LandPhase::WorktreeReady
        | LandPhase::Rebased
        | LandPhase::Checking
        | LandPhase::Checked => restarted(&ctx, &row).await,
        LandPhase::BranchPushIntent | LandPhase::PrIntent | LandPhase::DefaultPushIntent => {
            // An intent phase always has its operands (one transaction).
            ctx.fail(
                &row,
                code::INTERNAL,
                "The land failed: the journal of the land is not consistent.",
            )
            .await;
        }
        _ => {}
    }
}

async fn restarted(ctx: &Ctx, row: &LandRunRow) {
    ctx.fail(
        row,
        code::STATION_RESTARTED,
        "The station restarted during the land. Land again.",
    )
    .await;
}
