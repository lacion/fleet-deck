//! The cleanup of the temporary worktrees of lands (FG-8, design §11.6,
//! decision 48). It runs at the station start, every 15 minutes and after each
//! land phase change. It takes each `land_temps` row in `reserved` or
//! `present` whose run is terminal, or in a phase after `checked`, or in a
//! newer attempt (also for terminal runs). It deletes the folder with
//! `delete_checked` and then the Git admin entry with `delete_admin_checked`;
//! it never runs `git worktree prune` or `git worktree remove`.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use fleetdeck_proto::fleet_git::LandPhase;
use tokio::sync::{Notify, watch};

use crate::store::land_runs::LandRunRow;
use crate::store::land_temps::{LandTempRow, LandTempState};
use crate::store::{Store, StoreError};
use crate::workspace::trash::{Checked, delete_admin_checked, delete_checked};

/// The wait between two runs (design §11.6).
#[cfg(not(test))]
pub const INTERVAL: Duration = Duration::from_secs(15 * 60);
/// The wait between two runs, short in the tests.
#[cfg(test)]
pub const INTERVAL: Duration = Duration::from_millis(200);

/// The reason of a row that was kept because the folder is not the recorded
/// one.
pub const TEMP_CHANGED: &str = "temp_changed";
/// The reason of a row that has no recorded identity.
pub const TEMP_UNKNOWN: &str = "temp_unknown";

/// The task: a run at the start, then after a ping or every [`INTERVAL`].
pub(super) async fn run(store: Store, notify: Arc<Notify>, mut shutdown: watch::Receiver<bool>) {
    loop {
        if let Err(e) = run_once(&store).await {
            tracing::warn!(error = %e, "the land temporary folders were not cleaned");
        }
        tokio::select! {
            () = notify.notified() => {}
            () = tokio::time::sleep(INTERVAL) => {}
            _ = shutdown.wait_for(|stop| *stop) => return,
        }
    }
}

/// Is the row ready for the cleanup (design §11.6)?
fn due(row: &LandTempRow, run: Option<&LandRunRow>) -> bool {
    let Some(run) = run else {
        // No run row: nothing will use the folder.
        return true;
    };
    run.state.is_terminal()
        || run.attempt > row.attempt
        || matches!(
            run.phase,
            LandPhase::BranchPushIntent
                | LandPhase::BranchPushed
                | LandPhase::PrIntent
                | LandPhase::DefaultPushIntent
                | LandPhase::Done
        )
}

/// One pass over the open rows.
///
/// # Errors
/// A database error.
pub(crate) async fn run_once(store: &Store) -> Result<usize, StoreError> {
    let mut finished = 0;
    for row in store.land_temps_open().await? {
        let run = store.land_run(row.land_id.clone()).await?;
        if !due(&row, run.as_ref()) {
            continue;
        }
        let (state, code) = clean(&row).await;
        let Some(state) = state else { continue };
        let ended = store
            .land_temp_finish(row.land_id.clone(), row.attempt, state, code)
            .await?;
        finished += usize::from(ended);
    }
    Ok(finished)
}

/// Cleans one row: `Some(state)` ends the row, `None` leaves it for the next
/// pass (a file error).
async fn clean(row: &LandTempRow) -> (Option<LandTempState>, Option<String>) {
    let kept = |why: &str| (Some(LandTempState::Kept), Some(why.to_owned()));
    let (Some(dev), Some(ino)) = (row.dev, row.ino) else {
        // `reserved`, no identity: the station does not know the folder.
        return if Path::new(&row.path).exists() {
            kept(TEMP_UNKNOWN)
        } else {
            (Some(LandTempState::Removed), None)
        };
    };
    let (path, admin) = (row.path.clone(), row.admin_dir.clone());
    let done = tokio::task::spawn_blocking(move || {
        delete_checked(&path, (dev, ino))?;
        if let Some(admin) = admin {
            let common = Path::new(&admin)
                .parent()
                .and_then(Path::parent)
                .map(Path::to_path_buf)
                .unwrap_or_else(|| PathBuf::from("/"));
            delete_admin_checked(&admin, &common, &path)?;
        }
        Ok::<(), Checked>(())
    })
    .await;
    match done {
        Ok(Ok(())) => (Some(LandTempState::Removed), None),
        Ok(Err(Checked::Changed)) => {
            tracing::warn!(land = %row.land_id, path = %row.path, "a land temporary folder changed; it is kept");
            kept(TEMP_CHANGED)
        }
        Ok(Err(Checked::Io(why))) => {
            tracing::warn!(land = %row.land_id, "a land temporary folder was not deleted: {why}");
            (None, None)
        }
        Err(e) => {
            tracing::warn!(land = %row.land_id, "the cleanup task failed: {e}");
            (None, None)
        }
    }
}
