//! The `MoveIn` effect (FG-9, design §12.3 step 4, T4.2d): the Git work, the
//! import, the launch admission under the per-move lock, the spawn and the
//! commit. The effect returns `Deferred` and a task of its own settles it, so
//! the effect worker goes on with the other sessions.
//!
//! The order of the steps: the tombstone, the publication check (M8), the
//! fetch, the worktree, the publication ref (M9), the publication
//! transaction, the import (native), the launch identity, then the lock, the
//! conditional transition to `launching`, and the provider start (its own
//! authority gate is step 4, the spawn step 5). A failure before `launching`
//! stores a code and a text on the unchanged row and ends no session.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use fleetdeck_proto::command::SessionSpec;
use fleetdeck_proto::fleet_git::{FleetGitStart, LocalRemotes, MoveInState, code, remote_identity};
use tokio::sync::watch;

use super::SessionTarget;
use super::launch::Prepared;
use crate::manager::land::move_in::{MoveState, RunSlot};
use crate::store::move_runs::Tombstone;
use crate::store::{Dispatch, Effect, EffectPayload};
use crate::workspace::fleet_git::{
    PushDestination, add_move_worktree, check_publication, copy_includes, fetch_publication,
    pick_publish_ref, place_files, redact,
};
use crate::workspace::provision::after_cancel;

#[path = "move_watch.rs"]
mod watch_commit;

/// The dispatch reason of a move that a discard stopped.
const CANCELLED: &str = "move_cancelled";
/// The wait for a Git group that a cancel stopped.
const GROUP_WAIT: Duration = Duration::from_secs(5);

impl SessionTarget {
    /// Gives the target the memory of the moves that it shares with the
    /// session manager.
    #[must_use]
    pub(in crate::manager) fn with_moves(mut self, moves: Arc<MoveState>) -> Self {
        self.moves = Some(moves);
        self
    }

    /// Dispatches a `MoveIn` effect: the task registers first and then reads
    /// the tombstone, so a discard sees the task or the task sees the
    /// tombstone (design §12.3, "Abort", step 4).
    pub(super) fn dispatch_move_in(&self, effect: &Effect) -> Dispatch {
        let EffectPayload::MoveIn { move_id, .. } = &effect.payload else {
            return Dispatch::NotDispatched("not_a_move".into());
        };
        let (Some(this), Some(fence), Some(moves)) =
            (self.this.upgrade(), effect.fence_token, self.moves.clone())
        else {
            return Dispatch::NotDispatched("move_unavailable".into());
        };
        let Some(slot) = moves.register(move_id) else {
            return Dispatch::NotDispatched("move_running".into());
        };
        tokio::spawn(this.run_move_in(effect.clone(), fence, moves, slot));
        Dispatch::Deferred
    }

    /// The task of one `MoveIn` effect: the steps, the settle, and the end of
    /// its registry entry (in this order, so a waiter sees the settle).
    async fn run_move_in(
        self: Arc<Self>,
        effect: Effect,
        fence: u64,
        moves: Arc<MoveState>,
        slot: Arc<RunSlot>,
    ) {
        let EffectPayload::MoveIn { move_id, .. } = &effect.payload else {
            return;
        };
        let dispatch = self.move_in_steps(&effect, &moves, &slot).await;
        let settled = self
            .store
            .settle(effect.effect_id.clone(), fence, dispatch)
            .await;
        if let Err(e) = settled {
            tracing::warn!(error = %e, effect = %effect.effect_id, "the move effect was not settled");
        }
        moves.finish(move_id);
    }

    /// Stores a code and a text on the row that stays in `state`, and gives
    /// the dispatch of the failure (no session ends).
    async fn pre_fail(
        &self,
        move_id: &str,
        state: MoveInState,
        code: &str,
        text: String,
    ) -> Dispatch {
        let noted = self
            .store
            .move_in_note(move_id.to_owned(), state, code.to_owned(), text)
            .await;
        if let Err(e) = noted {
            tracing::warn!(error = %e, "the move failure was not stored");
        }
        Dispatch::NotDispatched(code.to_owned())
    }

    /// A store error in a step: logged, and the effect settles `internal`.
    fn internal(e: &crate::store::StoreError) -> Dispatch {
        tracing::error!(error = %e, "a move step could not read the store");
        Dispatch::NotDispatched("internal".into())
    }

    /// The steps of the effect (see the module documentation). The result is
    /// the dispatch to settle with: `NotDispatched` before the spawn,
    /// `Confirmed` once a process ran.
    #[allow(clippy::too_many_lines)]
    async fn move_in_steps(
        &self,
        effect: &Effect,
        moves: &Arc<MoveState>,
        slot: &Arc<RunSlot>,
    ) -> Dispatch {
        let EffectPayload::MoveIn {
            move_id,
            provider_instance,
            title,
            callsign,
            home,
            worktree,
            publication,
            fleet_git,
            ..
        } = &effect.payload
        else {
            return Dispatch::NotDispatched("not_a_move".into());
        };
        let mut cancel = slot.cancel.subscribe();
        let cancelled = || Dispatch::NotDispatched(CANCELLED.to_owned());
        // 1. The tombstone, after the registration.
        match self.store.move_tombstoned(move_id.clone()).await {
            Ok(false) => {}
            Ok(true) => return Dispatch::NotDispatched(code::MOVE_DISCARDED.to_owned()),
            Err(e) => return Self::internal(&e),
        }
        let row = match self.store.move_in_run(move_id.clone()).await {
            Ok(Some(row)) if row.state == MoveInState::Staged => row,
            Ok(_) => return cancelled(),
            Err(e) => return Self::internal(&e),
        };
        let Some(policy) = self.current_policy() else {
            return self
                .pre_fail(
                    move_id,
                    MoveInState::Staged,
                    code::INTERNAL,
                    "The station has no policy.".into(),
                )
                .await;
        };
        let Some(adapter) = self.adapters.get(provider_instance) else {
            return self
                .pre_fail(
                    move_id,
                    MoveInState::Staged,
                    "unknown_provider",
                    "The provider instance of the move has no adapter on this station.".into(),
                )
                .await;
        };
        let kind = adapter.kind();
        let station = policy.name().to_owned();
        let source = row
            .source_station
            .clone()
            .unwrap_or_else(|| "the source station".to_owned());
        let options = self.provision_options();
        let (destination, own_dest, target) = own_target(fleet_git);
        let group = Arc::new(AtomicU32::new(0));
        let home_path = Path::new(home);
        let worktree_path = Path::new(worktree);
        // 2. The publication check (M8), then the exact fetch.
        let checked = or_cancel(
            &mut cancel,
            &group,
            check_publication(
                &options,
                home_path,
                &destination,
                &own_dest,
                publication,
                policy.allow_local_remotes(),
                &station,
            ),
        )
        .await;
        let url = match checked {
            None => return cancelled(),
            Some(Err(r)) => {
                return self
                    .pre_fail(move_id, MoveInState::Staged, r.code, r.text)
                    .await;
            }
            Some(Ok(url)) => url,
        };
        let fetched = or_cancel(
            &mut cancel,
            &group,
            fetch_publication(&options, &group, home_path, &url.url, publication, move_id),
        )
        .await;
        match fetched {
            None => return cancelled(),
            Some(Err(r)) => {
                return self
                    .pre_fail(move_id, MoveInState::Staged, r.code, r.text)
                    .await;
            }
            Some(Ok(())) => {}
        }
        // 3. The local branch and the worktree (M9).
        let added = or_cancel(
            &mut cancel,
            &group,
            add_move_worktree(
                &options,
                &group,
                home_path,
                worktree_path,
                publication,
                move_id,
                &station,
                &fleet_git.settings.push_remote,
            ),
        )
        .await;
        match added {
            None => return cancelled(),
            Some(Err(r)) => {
                return self
                    .pre_fail(move_id, MoveInState::Staged, r.code, r.text)
                    .await;
            }
            Some(Ok(())) => {}
        }
        // The include files of the project (FG-4, design §7.2).
        let mut prepared = Prepared::default();
        if !fleet_git.settings.worktree_include.is_empty() {
            prepared.skipped = copy_includes(
                home_path,
                worktree_path,
                &fleet_git.settings.worktree_include,
                &options,
            )
            .await;
        }
        // 4. The publication ref (M9) and the publication transaction.
        let used = match self.store.git_push_refs(url.dest.clone()).await {
            Ok(used) => used,
            Err(e) => return Self::internal(&e),
        };
        let picked = or_cancel(
            &mut cancel,
            &group,
            pick_publish_ref(
                &options,
                &group,
                home_path,
                &url.url,
                &fleet_git.slug,
                callsign,
                &used,
            ),
        )
        .await;
        let publish_ref = match picked {
            None => return cancelled(),
            Some(Err(r)) => {
                return self
                    .pre_fail(move_id, MoveInState::Staged, r.code, r.text)
                    .await;
            }
            Some(Ok(name)) => name,
        };
        let published = self
            .store
            .move_publish(
                effect.session_id.clone(),
                move_id.clone(),
                publish_ref,
                target,
                url.dest.clone(),
                publication.oid.clone(),
            )
            .await;
        match published {
            Ok(true) => {}
            Ok(false) => return cancelled(),
            Err(e) => return Self::internal(&e),
        }
        let mut spec = SessionSpec {
            effort: None,
            provider_instance: provider_instance.clone(),
            cwd: worktree.clone(),
            mode: None,
            model: None,
            title: title.clone(),
            coordinator: None,
            launch: None,
            extra_env: None,
        };
        // The `worktree_env` of the project reaches the provider (FG-4).
        if let Some(with_env) = super::launch::spec_with_worktree_env(&spec, Some(fleet_git)) {
            spec = with_env;
        }
        // 5. The import of a native move.
        let expected = match &row.native_session_id {
            Some(_) => MoveInState::Imported,
            None => MoveInState::Staged,
        };
        if row.native_session_id.is_some() {
            let placed = self.import_native(move_id, &spec, worktree, kind).await;
            if let Err(dispatch) = placed {
                return dispatch;
            }
            let advanced = self
                .store
                .move_in_advance(
                    move_id.clone(),
                    MoveInState::Staged,
                    MoveInState::Imported,
                    None,
                    Tombstone::Refuse,
                )
                .await;
            match advanced {
                Ok(true) => {}
                Ok(false) => return cancelled(),
                Err(e) => return Self::internal(&e),
            }
        }
        if *cancel.borrow() {
            return cancelled();
        }
        // 6. The launch identity of the new folder.
        let dir = match self.open_checked(worktree_path).await {
            Ok(dir) => dir,
            Err((text, why)) => return self.pre_fail(move_id, expected, why, text).await,
        };
        #[cfg(target_os = "linux")]
        {
            spec.launch = Some(Box::new(dir.identity().clone()));
        }
        let launch = match self.take_launch(effect, &spec).await {
            Ok(launch) => launch,
            Err(Dispatch::NotDispatched(why)) => {
                let text = format!("The folder of the new session could not be opened ({why}).");
                return self.pre_fail(move_id, expected, &why, text).await;
            }
            Err(other) => return other,
        };
        let wants_base = !self.has_base(&effect.session_id).await;
        let base = if wants_base {
            crate::workspace::base::inferred(&self.git, &launch.pinned).await
        } else {
            None
        };
        // 7. The lock, the conditional transition, the start. The lock is
        // held until the start returned (design §12.3, the gated launch).
        let guard = moves.locks().lock(move_id).await;
        if *cancel.borrow() {
            return cancelled();
        }
        let admitted = self
            .store
            .move_in_advance(
                move_id.clone(),
                expected,
                MoveInState::Launching,
                None,
                Tombstone::Refuse,
            )
            .await;
        match admitted {
            Ok(true) => {}
            Ok(false) => return cancelled(),
            Err(e) => return Self::internal(&e),
        }
        let started = self
            .start(
                effect,
                &launch.spec,
                None,
                row.native_session_id.as_deref(),
                Some(&cancel),
            )
            .await;
        drop(guard);
        drop(dir);
        match started {
            Dispatch::Confirmed | Dispatch::Deferred | Dispatch::Sent => {}
            Dispatch::NotDispatched(reason) | Dispatch::Unknown(reason) => {
                return self
                    .launch_failed(move_id, &reason, kind, (&station, &source))
                    .await;
            }
        }
        self.record_base(&effect.session_id, base).await;
        self.record_prepared(&effect.session_id, &prepared).await;
        // 8. The commit (provider specific).
        let watch = watch_commit::Watch {
            move_id,
            session: &effect.session_id,
            provider: kind,
            native: row.native_session_id.as_deref(),
            names: (&station, &source),
            moves,
        };
        self.commit_watch(&watch, &mut cancel).await
    }

    /// The import of a native move: the provider home of the launch, then
    /// `place_files`. A failure stores its code and text on the `staged` row.
    async fn import_native(
        &self,
        move_id: &str,
        spec: &SessionSpec,
        worktree: &str,
        kind: fleetdeck_proto::event::ProviderKind,
    ) -> Result<(), Dispatch> {
        let staged = match self.store.move_staging(move_id.to_owned()).await {
            Ok(Some(row)) if row.state == crate::store::move_staging::StagingState::Ready => row,
            Ok(_) => {
                return Err(self
                    .pre_fail(
                        move_id,
                        MoveInState::Staged,
                        code::MOVE_CHECKSUM,
                        "The conversation files did not arrive complete. Try again.".into(),
                    )
                    .await);
            }
            Err(e) => return Err(Self::internal(&e)),
        };
        let Some(facts) = self.launch_facts(spec) else {
            return Err(self
                .pre_fail(
                    move_id,
                    MoveInState::Staged,
                    code::MOVE_RESUME_FAILED,
                    "This station has no provider home for the conversation files.".into(),
                )
                .await);
        };
        let placed = place_files(
            &self.store,
            move_id,
            facts.provider_home,
            kind,
            worktree.to_owned(),
            staged.manifest,
            PathBuf::from(staged.path),
        )
        .await;
        match placed {
            Ok(_) => Ok(()),
            Err(e) => Err(self
                .pre_fail(move_id, MoveInState::Staged, e.code, e.message)
                .await),
        }
    }

    /// The start returned a failure after `launching`: a refused authority
    /// gate is `launch_refused`; every other failure is `abandoned` with
    /// `move_resume_failed` (design §12.3, "The gated launch of a move").
    async fn launch_failed(
        &self,
        move_id: &str,
        reason: &str,
        kind: fleetdeck_proto::event::ProviderKind,
        (station, source): (&str, &str),
    ) -> Dispatch {
        let (to, why, text) = if reason == code::AUTHORITY_LOST {
            (
                MoveInState::LaunchRefused,
                code::AUTHORITY_LOST,
                "The coordinator lost its permission during the move.".to_owned(),
            )
        } else if reason == code::AUTHORITY_UNKNOWN {
            (
                MoveInState::LaunchRefused,
                code::AUTHORITY_UNKNOWN,
                "Fleet Deck could not confirm the coordinator's permission, so the move stopped."
                    .to_owned(),
            )
        } else {
            (
                MoveInState::Abandoned,
                code::MOVE_RESUME_FAILED,
                watch_commit::resume_text(kind, station, source, &redact(reason)),
            )
        };
        let moved = self
            .store
            .move_in_advance(
                move_id.to_owned(),
                MoveInState::Launching,
                to,
                Some((why.to_owned(), text)),
                Tombstone::Ignore,
            )
            .await;
        if let Err(e) = moved {
            tracing::warn!(error = %e, "the launch failure was not stored");
        }
        Dispatch::NotDispatched(why.to_owned())
    }
}

/// The push destination of the target, its `own_dest`, and its branch target
/// (design §3.8, §5.3): the `wip_remote` URL when the project sets one, else
/// the remote `push_remote`.
fn own_target(fg: &FleetGitStart) -> (PushDestination, String, String) {
    match fg.settings.wip_remote.as_deref() {
        Some(url) => {
            // The URL passed the identity rules at the decision.
            let dest = remote_identity(url, LocalRemotes::Allow)
                .map(|identity| identity.dest)
                .unwrap_or_default();
            (
                PushDestination::Wip {
                    url: url.to_owned(),
                },
                dest,
                url.to_owned(),
            )
        }
        None => (
            PushDestination::Remote {
                name: fg.settings.push_remote.clone(),
                project: fg.remote.clone(),
                clone_url: fg.clone_url.clone(),
            },
            fg.remote.dest.clone(),
            fg.settings.push_remote.clone(),
        ),
    }
}

/// Runs `work` until it ends (`Some`) or the cancel flag turns true (`None`):
/// a cancel stops the Git process group first (design §12.3, "Abort").
async fn or_cancel<T>(
    cancel: &mut watch::Receiver<bool>,
    group: &Arc<AtomicU32>,
    work: impl Future<Output = T>,
) -> Option<T> {
    tokio::select! {
        done = work => Some(done),
        () = async { let _ = cancel.wait_for(|c| *c).await; } => {
            let _ = after_cancel(group.load(Ordering::SeqCst), GROUP_WAIT).await;
            None
        }
    }
}
