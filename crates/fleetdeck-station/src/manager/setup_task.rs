//! The task of a start with a setup command (DL-148) or a workspace to
//! prepare: the Git work and the command run in a task of their own, so the
//! effect worker goes on with the other sessions.
//!
//! The task stays in [`SetupTasks`] from the dispatch of the start until the
//! provider start has returned and the start effect is settled. A stop that
//! reaches the task in this time (a stop dispatched during the setup or
//! during the provider start, or the station shutdown) cancels the setup, or
//! stops the new session when the provider already started. A stop that
//! reaches the target after the task left stops the live process instead
//! (see `dispatch` and `operate`).

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use fleetdeck_core::ChildEnv;
use fleetdeck_core::launch_dir::{LaunchDir, LaunchDirError};
use fleetdeck_proto::SessionId;
use fleetdeck_proto::command::{SessionSpec, StopMode};
use fleetdeck_proto::fleet_git::FleetGitStart;
use fleetdeck_proto::session::SessionBase;
use fleetdeck_providers::host::ProcessControl;
use tokio::sync::watch;
use tokio::task::JoinHandle;

use super::SessionTarget;
use super::launch::{Prepared, add_worktree_env, pinned_path};
use crate::store::{Dispatch, Effect, EffectPayload, InputRecord, WorkspacePlan};
use crate::workspace::provision::ProvisionError;
use crate::workspace::setup::{Held, STOPPED_MESSAGE, SetupError};

/// The wait for the setup tasks at the station shutdown: the Codex handshake
/// (30 s) and a stop grace fit in it.
const SETUP_STOP_WAIT: Duration = Duration::from_secs(60);

/// One start that waits for its setup command or workspace, or starts its
/// provider.
pub(super) struct SetupTask {
    /// `true` stops the workspace work or the setup command.
    cancel: watch::Sender<bool>,
    /// Taken by the shutdown, which waits for it.
    handle: Option<JoinHandle<()>>,
    /// The mode of a stop that the task has not applied yet: Kill replaces
    /// Graceful, and Graceful never replaces Kill.
    how: Option<StopMode>,
    /// The mode that the task applies now (the entry stays until it ends).
    stopping: Option<StopMode>,
}

impl SetupTask {
    /// Records a stop. A stop that is not stronger than the one that the task
    /// applies now adds nothing: that stop is already on its way.
    fn record(&mut self, how: StopMode) {
        let wanted = merge(self.how, how);
        if self.stopping == Some(StopMode::Kill)
            || (self.stopping == Some(StopMode::Graceful) && wanted == StopMode::Graceful)
        {
            return;
        }
        self.how = Some(wanted);
        // The cancel goes out under the registry lock.
        self.cancel.send_replace(true);
    }
}

/// The tasks of the starts with a setup command or a workspace, by session.
pub(super) type SetupTasks = Arc<StdMutex<HashMap<SessionId, SetupTask>>>;

/// What the task of a start with a setup command or a workspace owns.
struct SetupRun {
    effect: Effect,
    /// The fence token that claimed the effect.
    fence: u64,
    spec: SessionSpec,
    prompt: Option<InputRecord>,
    /// The Git work that the task does before anything else.
    workspace: Option<WorkspacePlan>,
    /// Whether the session has no base yet (the base of a clone is read
    /// after the clone).
    wants_base: bool,
    /// The base that was read before the workspace existed.
    planned: Option<SessionBase>,
    cmd: Option<String>,
    /// The environment of the command; set with `cmd`.
    env: Option<ChildEnv>,
    /// The Fleet Git snapshot of the start (FG-1, FG-4).
    fleet_git: Option<Box<FleetGitStart>>,
}

/// The points where a test holds a start (tests only).
#[cfg(test)]
#[derive(Default)]
pub(in crate::manager) struct Hooks {
    /// After the plan was read, right before the Git work of the workspace.
    pub provision: StartHook,
    /// Right after the command ended, before the task takes the restart
    /// handoff lock.
    pub handoff: StartHook,
    /// Right after the task left its setup, before the provider start.
    pub start: StartHook,
    /// After the provider spawn, before the first input work.
    pub input: StartHook,
    /// After the fence and the checkpoint work, right before the delivery
    /// check of the first input.
    pub checkpoint: StartHook,
    /// After the provider registration, right before the task ends.
    pub end: StartHook,
}

/// One point where a test holds a start (tests only).
#[cfg(test)]
#[derive(Default)]
pub(in crate::manager) struct StartHook {
    armed: std::sync::atomic::AtomicBool,
    /// The task waits for the release; this tells that it does.
    pub reached: tokio::sync::Notify,
    pub release: tokio::sync::Notify,
}

#[cfg(test)]
impl StartHook {
    /// The next task that reaches the hook waits there.
    pub fn arm(&self) {
        self.armed.store(true, std::sync::atomic::Ordering::SeqCst);
    }

    pub(in crate::manager) async fn hold(&self) {
        if self.armed.swap(false, std::sync::atomic::Ordering::SeqCst) {
            self.reached.notify_one();
            self.release.notified().await;
        }
    }
}

/// The wait for the stop of a provider after its task was aborted.
const ABORT_STOP_WAIT: Duration = Duration::from_secs(10);

/// Whether a stop cancelled the start (the setup, or the start after it).
pub(super) fn cancelled(cancel: Option<&watch::Receiver<bool>>) -> bool {
    cancel.is_some_and(|c| *c.borrow())
}

/// The mode that two stops give together.
fn merge(old: Option<StopMode>, new: StopMode) -> StopMode {
    if old == Some(StopMode::Kill) || new == StopMode::Kill {
        StopMode::Kill
    } else {
        StopMode::Graceful
    }
}

impl SessionTarget {
    /// Registers a start that has a setup command or a workspace to prepare:
    /// the Git work and the command run in a task of their own. The task
    /// starts the provider and settles the effect. A failure before the task
    /// ends the start here and returns the dispatch code.
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn defer_start(
        &self,
        effect: &Effect,
        spec: &SessionSpec,
        prompt: Option<&InputRecord>,
        workspace: Option<&WorkspacePlan>,
        cmd: Option<&str>,
        wants_base: bool,
        planned: Option<SessionBase>,
    ) -> Dispatch {
        let session_id = &effect.session_id;
        let fleet_git = match &effect.payload {
            EffectPayload::Start { fleet_git, .. } => fleet_git.clone(),
            _ => None,
        };
        // The environment is needed only for a command.
        let env = if cmd.is_some() {
            let env = self
                .current_policy()
                .and_then(|p| p.setup_env(&spec.provider_instance).cloned());
            let env = env.map(|mut env| {
                if let Some(fg) = &fleet_git {
                    add_worktree_env(&mut env, &fg.settings);
                }
                env
            });
            let Some(env) = env else {
                let text = "the setup command has no environment for this provider instance";
                self.start_failed(session_id, text).await;
                return Dispatch::NotDispatched("setup_failed".into());
            };
            Some(env)
        } else {
            None
        };
        let (Some(this), Some(fence)) = (self.this.upgrade(), effect.fence_token) else {
            let text = "the setup command cannot run: the effect has no owner";
            self.start_failed(session_id, text).await;
            return Dispatch::NotDispatched("setup_failed".into());
        };
        let run = SetupRun {
            effect: effect.clone(),
            fence,
            spec: spec.clone(),
            prompt: prompt.cloned(),
            workspace: workspace.cloned(),
            wants_base,
            planned,
            cmd: cmd.map(str::to_owned),
            env,
            fleet_git,
        };
        let (cancel, cancel_rx) = watch::channel(false);
        // The lock covers the spawn and the insert: the task removes itself
        // only after it has the lock.
        let Ok(mut tasks) = self.setups.lock() else {
            self.start_failed(session_id, "the setup tasks are unreadable")
                .await;
            return Dispatch::NotDispatched("setup_failed".into());
        };
        let handle = tokio::spawn(this.prepare_then_start(run, cancel_rx));
        let task = SetupTask {
            cancel,
            handle: Some(handle),
            how: None,
            stopping: None,
        };
        tasks.insert(session_id.clone(), task);
        Dispatch::Deferred
    }

    /// The checked destination parent, then the confined Git work of `plan`
    /// (B9, DL-200). The error is the failure text and the settle reason. A
    /// stopped or failed Git work deletes nothing: a new folder stays and the
    /// text names it.
    pub(super) async fn provision_plan(
        &self,
        plan: &WorkspacePlan,
        cancel: &watch::Receiver<bool>,
    ) -> Result<(), (String, &'static str)> {
        use crate::workspace::provision;
        let (WorkspacePlan::Worktree { path, .. } | WorkspacePlan::Clone { path, .. }) = plan;
        let Some(parent) = Path::new(path).parent() else {
            let e = LaunchDirError::Changed;
            return Err((e.to_string(), e.code()));
        };
        // The checked folders stay open until the Git work returned: the
        // parent, the source repository of a worktree, and an existing
        // destination (B9, DL-200). Git runs in these objects.
        let parent = self.open_checked(parent).await?;
        let repo = match plan {
            WorkspacePlan::Worktree { repo, .. } => Some(self.open_checked(Path::new(repo)).await?),
            WorkspacePlan::Clone { .. } => None,
        };
        let dest = match plan {
            WorkspacePlan::Worktree { path, .. } if Path::new(path).is_dir() => {
                Some(self.open_checked(Path::new(path)).await?)
            }
            _ => None,
        };
        let pinned = |dir: &LaunchDir| pinned_path(dir).map_err(|e| (e.to_string(), e.code()));
        let parent_dir = pinned(&parent)?;
        let repo_dir = repo.as_ref().map(pinned).transpose()?;
        let dest_dir = dest.as_ref().map(pinned).transpose()?;
        let options = self.provision_options();
        let before = provision::before(plan);
        #[cfg(test)]
        self.hooks.provision.hold().await;
        let (text, reason) = match provision::provision_cancellable(
            plan,
            &options,
            cancel.clone(),
            repo_dir.as_deref(),
            Some(&parent_dir),
            dest_dir.as_deref(),
        )
        .await
        {
            Ok(()) => return Ok(()),
            Err(ProvisionError::Failed(e)) => (
                format!("the workspace was not created: {e}"),
                "workspace_failed",
            ),
            Err(ProvisionError::Stopped) => (STOPPED_MESSAGE.to_owned(), "setup_stopped"),
        };
        let text = match provision::undo_partial(plan, &before) {
            Some(note) => format!("{text}\n{note}"),
            None => text,
        };
        Err((text, reason))
    }

    /// The Git work of the workspace plan of a start, with its cancel. `false`:
    /// the start ended here (failed or stopped), the effect is settled, and
    /// the task must return.
    async fn prepare_workspace(
        &self,
        effect: &Effect,
        fence: u64,
        plan: &WorkspacePlan,
        (fleet, wants_base): (Option<&FleetGitStart>, bool),
        cancel: &watch::Receiver<bool>,
    ) -> Option<Prepared> {
        let result = self
            .prepare_synced(effect, plan, fleet, wants_base, cancel)
            .await;
        match result {
            Ok(prepared) => Some(prepared),
            Err((text, reason)) => {
                self.end_prepare(effect, fence, &text, reason).await;
                None
            }
        }
    }

    /// Ends a start that failed before its provider start: the failure text,
    /// the settle as not dispatched with `reason`, and the setup registry.
    async fn end_prepare(&self, effect: &Effect, fence: u64, text: &str, reason: &str) {
        let session_id = &effect.session_id;
        self.start_failed(session_id, text).await;
        let settled = self
            .store
            .settle(
                effect.effect_id.clone(),
                fence,
                Dispatch::NotDispatched(reason.into()),
            )
            .await;
        if let Err(e) = settled {
            tracing::warn!(error = %e, effect = %effect.effect_id, "the start effect was not settled");
        }
        self.leave_setup(session_id);
    }

    /// The task of a start that prepares its workspace or runs a setup
    /// command: the Git work, then the command, then the provider start, then
    /// the settle of the start effect. It leaves the registry last; a stop
    /// that came in the meantime stops the new session.
    async fn prepare_then_start(self: Arc<Self>, run: SetupRun, cancel: watch::Receiver<bool>) {
        let SetupRun {
            effect,
            fence,
            mut spec,
            prompt,
            workspace,
            wants_base,
            planned,
            cmd,
            mut env,
            fleet_git,
        } = run;
        let session_id = effect.session_id.clone();
        let stop_flag = cancel.clone();
        let mut prepared = Prepared::default();
        if let Some(plan) = &workspace {
            let work = (fleet_git.as_deref(), wants_base);
            match self
                .prepare_workspace(&effect, fence, plan, work, &cancel)
                .await
            {
                Some(found) => prepared = found,
                None => return,
            }
        }
        // A fetched start point gives the base of the session (FG-1).
        let planned = prepared.base.take().or(planned);
        // A new workspace takes its identity right after the Git work, before
        // the base read and the setup command (B9, DL-200): `child_dir` and
        // `take_launch` then reopen this object. `dest` stays open until the
        // provider start returned.
        let mut dest = None;
        if workspace.is_some() && spec.launch.is_none() {
            let opened = match self.open_checked(Path::new(&spec.cwd)).await {
                Ok(dir) => pinned_path(&dir)
                    .map(|path| (dir, path))
                    .map_err(|e| (e.to_string(), e.code())),
                Err(refused) => Err(refused),
            };
            match opened {
                Ok((dir, path)) => {
                    #[cfg(target_os = "linux")]
                    {
                        spec.launch = Some(Box::new(dir.identity().clone()));
                    }
                    dest = Some((dir, path));
                }
                Err((text, code)) => {
                    self.end_prepare(&effect, fence, &text, code).await;
                    return;
                }
            }
        }
        let base = match (planned, &workspace) {
            (None, Some(WorkspacePlan::Clone { path, .. })) if wants_base => {
                let at = dest.as_ref().map_or(Path::new(path), |(_, p)| p.as_path());
                crate::workspace::base::at_head(&self.git, at).await
            }
            (planned, _) => planned,
        };
        // The artifact variables reach the setup command and the provider
        // (FG-12, decision C1).
        if let Some(fg) = fleet_git.as_deref() {
            self.artifact_step(&session_id, fg, &mut spec, env.as_mut())
                .await;
        }
        let held = match (&cmd, &env) {
            (Some(cmd), Some(env)) => match self.child_dir(&spec).await {
                // The command runs in the checked folder; `_dir` lives until
                // it returned (B9).
                Ok((_dir, cwd)) => {
                    crate::workspace::setup::run_held(
                        &self.store,
                        effect.effect_id.as_str(),
                        &session_id,
                        cmd,
                        &cwd,
                        env,
                        crate::workspace::setup::DEFAULT_TIMEOUT,
                        cancel,
                    )
                    .await
                }
                Err((text, _code)) => Held {
                    result: Err(SetupError {
                        message: text,
                        output_tail: String::new(),
                    }),
                    end_row: false,
                },
            },
            (Some(_), None) => Held {
                result: Err(SetupError {
                    message: "the setup command has no environment for this provider instance"
                        .into(),
                    output_tail: String::new(),
                }),
                end_row: false,
            },
            (None, _) => Held {
                result: Ok(()),
                end_row: false,
            },
        };
        // The row stays (the restart admission lists it) until the handoff:
        // under the lock of the restart admission, the task takes its claim
        // and ends the row, or finds the freeze set. The count drops after
        // the provider start returned: the session is registered, or the
        // start failed (PRE-PR r3 finding 2).
        #[cfg(test)]
        self.hooks.handoff.hold().await;
        let Some(claim) = self.handoff(&effect, fence, held.end_row).await else {
            // The restart is committed: the start never ran.
            self.leave_setup(&session_id);
            return;
        };
        let failure = match held.result {
            // A stop that came after the command ended is a stop. The check
            // is under the lock that a stop uses.
            Ok(()) if self.stop_pending(&stop_flag) => Some(SetupError {
                message: STOPPED_MESSAGE.into(),
                output_tail: String::new(),
            }),
            Ok(()) => None,
            Err(e) => Some(e),
        };
        let started = failure.is_none();
        let dispatch = match failure {
            None => {
                #[cfg(test)]
                self.hooks.start.hold().await;
                // A new workspace takes its identity after the provisioning;
                // a setup-command start reopens the one of the dispatch (B9).
                let dispatch = match self.take_launch(&effect, &spec).await {
                    Ok(launch) => {
                        self.start(
                            &effect,
                            &launch.spec,
                            prompt.as_ref(),
                            None,
                            Some(&stop_flag),
                        )
                        .await
                    }
                    Err(refused) => refused,
                };
                drop(dest.take());
                if matches!(dispatch, Dispatch::Confirmed | Dispatch::Deferred) {
                    self.record_base(&session_id, base).await;
                    self.record_prepared(&session_id, &prepared).await;
                }
                dispatch
            }
            Some(e) => {
                let stopped = e.message == STOPPED_MESSAGE;
                let text = if e.output_tail.is_empty() {
                    e.message
                } else {
                    format!("{}\n\n{}", e.message, e.output_tail)
                };
                self.start_failed(&session_id, &text).await;
                let reason = if stopped {
                    "setup_stopped"
                } else {
                    "setup_failed"
                };
                Dispatch::NotDispatched(reason.into())
            }
        };
        if let Err(e) = self
            .store
            .settle(effect.effect_id.clone(), fence, dispatch)
            .await
        {
            tracing::warn!(error = %e, effect = %effect.effect_id, "the start effect was not settled");
        }
        drop(claim);
        // The entry stays until the provider that this task started is gone,
        // so that a shutdown waits for it, and until no stop is left to apply.
        if started {
            #[cfg(test)]
            self.hooks.end.hold().await;
            while let Some(how) = self.finish_or_take_stop(&session_id) {
                self.stop_started(&session_id, how).await;
            }
        } else {
            self.leave_setup(&session_id);
        }
    }

    /// The handoff of a deferred start to the restart count. Under the
    /// handoff lock: with no freeze it takes the claim and ends the setup
    /// row. With the freeze set it waits for the end of the admission:
    /// committed, the setup row ends and the session ends as a failed start
    /// in one transaction (`None`); cleared, it tries again.
    async fn handoff(
        &self,
        effect: &Effect,
        fence: u64,
        end_row: bool,
    ) -> Option<crate::manager::restart_gate::InFlight> {
        loop {
            let guard = self.restart.lock_handoff().await;
            if !self.restart.restarting() {
                let claim = self.restart.enter_claim();
                if end_row {
                    crate::workspace::setup::end_row(&self.store, effect.effect_id.as_str()).await;
                }
                drop(guard);
                return Some(claim);
            }
            drop(guard);
            if self.restart.wait_resolved().await {
                break;
            }
        }
        if end_row {
            crate::workspace::setup::end_row(&self.store, effect.effect_id.as_str()).await;
        }
        let ended = self
            .store
            .fail_claimed_start(
                effect.effect_id.clone(),
                fence,
                crate::manager::restart_check::QUEUED_START_LOST.to_owned(),
            )
            .await;
        if let Err(e) = ended {
            tracing::warn!(error = %e, effect = %effect.effect_id, "the start was not ended for the restart");
        }
        None
    }

    /// Stops the live session of a start that a stop cancelled, and waits for
    /// its end. The session lock is not held while it waits.
    async fn stop_started(&self, session_id: &SessionId, how: StopMode) {
        let Some(live) = self.live(session_id) else {
            return;
        };
        let session = {
            let mut live = live.lock().await;
            if let Some(durable) = live.durable.as_ref() {
                let (process, policy) = (durable.driver.process(), durable.stop);
                drop(live);
                match how {
                    StopMode::Graceful => drop(process.stop(policy).await),
                    StopMode::Kill => drop(process.kill().await),
                }
                return;
            }
            live.session.take()
        };
        if let Some(mut session) = session
            && let Err(e) = session.stop(how).await
        {
            tracing::warn!(error = %e, "stop of a cancelled start");
        }
    }

    /// Whether a stop is pending. The lock is the one that a stop takes, so
    /// a stop is either seen here or sees the task and stops the new session.
    fn stop_pending(&self, stop: &watch::Receiver<bool>) -> bool {
        let _tasks = self.setups.lock();
        *stop.borrow()
    }

    /// Decides the end of the task under one lock. A recorded stop is taken
    /// out (the entry stays, as `stopping`) and returned. With no stop the
    /// entry is removed and `None` is returned, so no stop can come between
    /// the check and the removal.
    fn finish_or_take_stop(&self, session_id: &SessionId) -> Option<StopMode> {
        let Ok(mut tasks) = self.setups.lock() else {
            return None;
        };
        let task = tasks.get_mut(session_id)?;
        match task.how.take() {
            Some(how) => {
                task.stopping = Some(merge(task.stopping, how));
                Some(how)
            }
            None => {
                tasks.remove(session_id);
                None
            }
        }
    }

    /// Takes the session out of the registry.
    fn leave_setup(&self, session_id: &SessionId) {
        if let Ok(mut tasks) = self.setups.lock() {
            tasks.remove(session_id);
        }
    }

    /// Sends the cancel to the setup command or workspace task of the session
    /// and records the mode.
    /// False when the session has no task.
    pub(super) fn cancel_setup(&self, session_id: &SessionId, how: StopMode) -> bool {
        let Ok(mut tasks) = self.setups.lock() else {
            return false;
        };
        let Some(task) = tasks.get_mut(session_id) else {
            return false;
        };
        task.record(how);
        true
    }

    /// Cancels every setup command or workspace task with `how` and waits for
    /// them (station shutdown).
    /// A task that started its provider stops it and waits for its end before
    /// it ends. A task that does not end in time is aborted; its provider
    /// session, if it still holds one, is dropped, and a drop kills the
    /// process group of a direct session (a shim session outlives the
    /// station by design and is restored at the next start).
    pub(super) async fn stop_setups(&self, how: StopMode) {
        let handles: Vec<_> = match self.setups.lock() {
            Ok(mut tasks) => tasks
                .iter_mut()
                .filter_map(|(session_id, task)| {
                    task.record(how);
                    task.handle.take().map(|h| (session_id.clone(), h))
                })
                .collect(),
            Err(_) => return,
        };
        let deadline = tokio::time::Instant::now() + SETUP_STOP_WAIT;
        for (session_id, mut handle) in handles {
            if tokio::time::timeout_at(deadline, &mut handle)
                .await
                .is_err()
            {
                tracing::warn!(session = %session_id, "a setup task did not stop in time; aborted");
                handle.abort();
                let _ = handle.await;
                // A provider that the task registered is stopped here: a
                // direct session would die with its dropped handle, a shim
                // session would not.
                let stopped = self.stop_started(&session_id, StopMode::Kill);
                if tokio::time::timeout(ABORT_STOP_WAIT, stopped)
                    .await
                    .is_err()
                {
                    tracing::warn!(session = %session_id, "the provider of an aborted task did not stop");
                }
                self.leave_setup(&session_id);
            }
        }
    }
}
