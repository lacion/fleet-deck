//! The advance of the folder generation after a shell settlement, and the
//! start-up settlement of the shell rows (B8, DL-196).
//!
//! A settled row keeps `advance_pending = 1` until the advance ran under the
//! folder fence ([`super::turns::Fences`]). So no turn start checkpoint is
//! taken at the old generation after a settlement. [`task`] wakes on a
//! notify and at a slow tick; [`start_up`] runs once before the shell
//! admission opens.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{Notify, watch};

use fleetdeck_proto::event::{Event, ToolStatus};

use super::turns::Fences;
use crate::store::resumes::proves_end;
use crate::store::shell_runs::{ShellRow, ShellState};
use crate::store::{Result, Store};

/// The environment variable that marks every process of one `!` command (B8).
pub const MARKER_VAR: &str = "FLEETDECK_SHELL_ID";
/// The wait for the marked processes of one row at the start-up.
const MARKED_WAIT: Duration = Duration::from_secs(5);

/// The tick that covers a settlement made inside a store transaction (a
/// retirement), which has no notify.
const TICK: Duration = Duration::from_secs(1);

/// Advances the generation of every settled row, under the fence of its
/// root. Returns how many advanced.
///
/// # Errors
/// A database error.
pub async fn advance_pending(store: &Store, fences: &Fences) -> Result<usize> {
    let pending = store.shell_pending_advances().await?;
    let mut done = 0;
    for (effect_id, root) in pending {
        let fence = fences.for_worktree(Path::new(&root));
        let _guard = fence.lock().await;
        store.shell_advance(effect_id, root, false).await?;
        done += 1;
    }
    Ok(done)
}

/// The station start, before any command is admitted: a row whose effect
/// was retired or never claimed settles `cancelled`; a Claude row that the
/// last process may have started loses its marked processes and settles
/// `unknown`; every pending advance runs; then the shell admission opens.
///
/// # Errors
/// A database error.
pub async fn start_up(store: &Store, fences: &Fences) -> Result<()> {
    let cancelled = store.shell_cancel_unclaimed().await?;
    if cancelled > 0 {
        tracing::info!(cancelled, "settled shell commands that never started");
    }
    settle_claimed(store).await?;
    settle_codex_sent(store).await?;
    advance_pending(store, fences).await?;
    store.shell_open_admission().await
}

/// The Claude rows that the last station process may have started: kills the
/// processes that carry the marker of the row, then settles it `unknown`
/// (guarded by the state read) and tells the session (B8).
async fn settle_claimed(store: &Store) -> Result<()> {
    for row in store.shell_claimed_rows().await? {
        let (var, id) = (MARKER_VAR, row.effect_id.clone());
        let killed = tokio::task::spawn_blocking(move || {
            crate::workspace::setup_cleanup::kill_marked(var, &id, MARKED_WAIT)
        })
        .await;
        match killed {
            Ok(Ok(0)) => {}
            Ok(Ok(left)) => {
                tracing::error!(effect = %row.effect_id, left, "a shell process may still run");
            }
            Ok(Err(e)) => tracing::error!(error = %e, "the shell processes cannot be listed"),
            Err(e) => tracing::error!(error = %e, "the shell scan stopped"),
        }
        settle_unknown(store, &row).await?;
    }
    Ok(())
}

/// Settles a row `unknown` (guarded by the state that the caller read) and
/// tells the session. `false` when another path settled the row first.
async fn settle_unknown(store: &Store, row: &ShellRow) -> Result<bool> {
    let settled = store
        .shell_settle_from(row.effect_id.clone(), row.state, ShellState::Unknown)
        .await?;
    if !settled {
        return Ok(false);
    }
    let mut refs = serde_json::Map::new();
    refs.insert("shell".to_owned(), "unknown".into());
    let done = Event::ToolCompleted {
        node_id: format!("shell-{}", row.effect_id).into(),
        status: ToolStatus::Failed,
        files: Vec::new(),
        exit_code: None,
        provider_refs: refs,
    };
    if let Err(e) = store
        .append(row.session_id.clone(), vec![done.into()])
        .await
    {
        tracing::warn!(error = %e, effect = %row.effect_id, "the shell end was not stored");
    }
    Ok(true)
}

/// Does the newest shim row of the session prove that its execution ended?
/// `None` for a session without a shim (a direct host).
async fn shim_proof(store: &Store, row: &ShellRow) -> Result<Option<bool>> {
    let rows = store.shim_rows(row.session_id.clone()).await?;
    Ok(rows.last().map(proves_end))
}

/// The Codex rows that the last station process may have sent: a row under
/// a shim whose end is proven settles `unknown`, one without proof awaits it;
/// a row of a direct host lost its process with the station and settles
/// `unknown` (B8).
async fn settle_codex_sent(store: &Store) -> Result<()> {
    for row in store.shell_codex_sent_rows().await? {
        match shim_proof(store, &row).await? {
            Some(false) => {
                store
                    .shell_set_awaiting_proof(row.effect_id.clone(), row.state)
                    .await?;
            }
            Some(true) | None => {
                settle_unknown(store, &row).await?;
            }
        }
    }
    Ok(())
}

/// A Codex row under a shim whose end the shim now proves settles `unknown`
/// (B8). A row of a direct host is not touched.
async fn settle_proven(store: &Store) -> Result<()> {
    let open = store.shell_runs_unsettled().await?;
    for row in open.iter().filter(|r| {
        r.provider == "codex" && matches!(r.state, ShellState::Running | ShellState::AwaitingProof)
    }) {
        if shim_proof(store, row).await? == Some(true) {
            settle_unknown(store, row).await?;
        }
    }
    Ok(())
}

/// The advance task: runs at the start, on each notify and at each tick,
/// until the shutdown.
pub async fn task(
    store: Store,
    fences: Fences,
    notify: Arc<Notify>,
    mut shutdown: watch::Receiver<bool>,
) {
    loop {
        if let Err(e) = settle_proven(&store).await {
            tracing::error!(error = %e, "shell proof check failed");
        }
        if let Err(e) = advance_pending(&store, &fences).await {
            tracing::error!(error = %e, "shell advance failed");
        }
        tokio::select! {
            () = notify.notified() => {}
            () = tokio::time::sleep(TICK) => {}
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    return;
                }
            }
        }
    }
}
