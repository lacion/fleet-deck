//! The task of a start with a setup command (DL-148): the command runs in a
//! task of its own, so the effect worker goes on with the other sessions.
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
use fleetdeck_proto::SessionId;
use fleetdeck_proto::command::{SessionSpec, StopMode};
use tokio::sync::watch;
use tokio::task::JoinHandle;

use super::SessionTarget;
use crate::store::{Dispatch, Effect, EffectPayload, InputRecord};
use crate::workspace::setup::{STOPPED_MESSAGE, SetupError};

/// The wait for the setup tasks at the station shutdown.
const SETUP_STOP_WAIT: Duration = Duration::from_secs(15);

/// One start that waits for its setup command, or starts its provider.
pub(super) struct SetupTask {
    /// `true` stops the setup command.
    cancel: watch::Sender<bool>,
    /// Taken by the shutdown, which waits for it.
    handle: Option<JoinHandle<()>>,
    /// The mode of the stop that cancelled the task: Kill replaces
    /// Graceful, and Graceful never replaces Kill.
    how: Option<StopMode>,
}

pub(super) type SetupTasks = Arc<StdMutex<HashMap<SessionId, SetupTask>>>;

/// What the task of a start with a setup command owns.
struct SetupRun {
    effect: Effect,
    /// The fence token that claimed the effect.
    fence: u64,
    spec: SessionSpec,
    prompt: Option<InputRecord>,
    cmd: String,
    env: ChildEnv,
}

/// Holds the task right after it left its setup, before the provider starts
/// (tests only).
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

    async fn hold(&self) {
        if self.armed.swap(false, std::sync::atomic::Ordering::SeqCst) {
            self.reached.notify_one();
            self.release.notified().await;
        }
    }
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
    /// Registers a start with a setup command: the command runs in a task of
    /// its own. The task starts the provider and settles the effect. A
    /// failure before the task ends the start here and returns the dispatch
    /// code.
    pub(super) async fn defer_setup(
        &self,
        effect: &Effect,
        spec: &SessionSpec,
        prompt: Option<&InputRecord>,
        cmd: &str,
    ) -> Dispatch {
        let session_id = &effect.session_id;
        let Some(env) = self
            .policy
            .as_ref()
            .and_then(|p| p.setup_env(&spec.provider_instance))
        else {
            let text = "the setup command has no environment for this provider instance";
            self.start_failed(session_id, text).await;
            return Dispatch::NotDispatched("setup_failed".into());
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
            cmd: cmd.to_owned(),
            env: env.clone(),
        };
        let (cancel, cancel_rx) = watch::channel(false);
        // The lock covers the spawn and the insert: the task removes itself
        // only after it has the lock.
        let Ok(mut tasks) = self.setups.lock() else {
            self.start_failed(session_id, "the setup tasks are unreadable")
                .await;
            return Dispatch::NotDispatched("setup_failed".into());
        };
        let handle = tokio::spawn(this.setup_then_start(run, cancel_rx));
        let task = SetupTask {
            cancel,
            handle: Some(handle),
            how: None,
        };
        tasks.insert(session_id.clone(), task);
        Dispatch::Deferred
    }

    /// The task of a start with a setup command: the command, then the
    /// provider start, then the settle of the start effect. It leaves the
    /// registry last; a stop that came in the meantime stops the new session.
    async fn setup_then_start(self: Arc<Self>, run: SetupRun, cancel: watch::Receiver<bool>) {
        let SetupRun {
            effect,
            fence,
            spec,
            prompt,
            cmd,
            env,
        } = run;
        let session_id = effect.session_id.clone();
        let stop_flag = cancel.clone();
        let result = crate::workspace::setup::run(
            &self.store,
            effect.effect_id.as_str(),
            &session_id,
            &cmd,
            Path::new(&spec.cwd),
            &env,
            crate::workspace::setup::DEFAULT_TIMEOUT,
            cancel,
        )
        .await;
        let failure = match result {
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
                self.start_hook.hold().await;
                self.start(&effect, &spec, prompt.as_ref(), None).await
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
        let how = self.leave_setup(&session_id);
        if let (true, Some(how)) = (started, how) {
            // A stop came while the provider started: the new session ends.
            let stop = Effect {
                payload: EffectPayload::Stop { how },
                process_incarnation: None,
                ..effect
            };
            let _ = self.operate(&stop).await;
        }
    }

    /// Whether a stop is pending. The lock is the one that a stop takes, so
    /// a stop is either seen here or sees the task and stops the new session.
    fn stop_pending(&self, stop: &watch::Receiver<bool>) -> bool {
        let _tasks = self.setups.lock();
        *stop.borrow()
    }

    /// Takes the session out of the registry and returns the mode of the
    /// stop that came during the task, if any.
    fn leave_setup(&self, session_id: &SessionId) -> Option<StopMode> {
        let mut tasks = self.setups.lock().ok()?;
        tasks.remove(session_id).and_then(|t| t.how)
    }

    /// Sends the cancel to the task of the session and records the mode.
    /// False when the session has no task.
    pub(super) fn cancel_setup(&self, session_id: &SessionId, how: StopMode) -> bool {
        let Ok(mut tasks) = self.setups.lock() else {
            return false;
        };
        let Some(task) = tasks.get_mut(session_id) else {
            return false;
        };
        task.how = Some(merge(task.how, how));
        task.cancel.send_replace(true);
        true
    }

    /// Cancels every task with `how` and waits for them (station shutdown).
    /// A task that started its provider stops it before it ends.
    pub(super) async fn stop_setups(&self, how: StopMode) {
        let handles: Vec<_> = match self.setups.lock() {
            Ok(mut tasks) => tasks
                .values_mut()
                .filter_map(|task| {
                    task.how = Some(merge(task.how, how));
                    // The cancel goes out under the lock, as for a stop.
                    task.cancel.send_replace(true);
                    task.handle.take()
                })
                .collect(),
            Err(_) => return,
        };
        let deadline = tokio::time::Instant::now() + SETUP_STOP_WAIT;
        for handle in handles {
            if tokio::time::timeout_at(deadline, handle).await.is_err() {
                tracing::warn!("a setup task did not stop in time at the shutdown");
            }
        }
    }
}
