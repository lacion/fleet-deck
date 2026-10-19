//! The one serialized writer of a station (FG-2, design §5.5, decision 15):
//! the only task that runs a remote write. It takes jobs from one queue, one
//! job at a time, so two writes never run together and the lease bookkeeping
//! of `git_pushes` has one writer.
//!
//! The jobs are [`WriteJob::BranchPush`] (FG-2) and the two land jobs of
//! phase 3 (FG-8, `land_publish.rs`). This file also has the per-land lock
//! and the gated start of a land write ([`gated_write`], design §11.5,
//! revision 4).

use std::collections::HashMap;
use std::future::Future;
use std::path::Path;
use std::sync::{Arc, PoisonError};
use std::time::Duration;

use fleetdeck_proto::SessionId;
use fleetdeck_proto::fleet_git::{LandState, PushMode, PushState, code};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

use super::fleet_control::Refusal;
use super::land::land_text;
use super::policy_hub::PolicyHub;
use crate::store::Store;
use crate::store::git_state::{PushRow, SessionGitRow};
use crate::store::land_publish::{
    OutcomeChange, PendingChange, ReconcileChange, Step, StepEnd, WRITE_NOT_STARTED,
};
use crate::store::land_runs::LandRunRow;
use crate::workspace::fleet_git::{
    OptionsProvider, PushDestination, PushJob, PushResult, PushWriter, backoff_delay,
};

/// One remote write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WriteJob {
    /// The push of the branch of one session (FG-2, design §5.5 steps 1 to 6).
    BranchPush { session_id: SessionId },
    /// Step 5 of a land (design §11.5): the branch push, its reconciliation
    /// and its recovery, from the stored run.
    LandBranch { land_id: String },
    /// Step 7 of a land in mode `merge`: the push to the default branch.
    LandDefault { land_id: String },
}

/// What the writer needs to run a job.
pub struct WriterEnv {
    pub store: Store,
    pub policy: Arc<PolicyHub>,
    pub push: PushWriter,
    pub options: OptionsProvider,
    pub locks: Arc<LandLocks>,
    pub timeouts: LandTimeouts,
}

struct Queued {
    job: WriteJob,
    done: oneshot::Sender<()>,
}

/// The queue of the writer. A clone of the handle sends to the same queue.
#[derive(Debug, Clone)]
pub struct FleetGitWriter {
    queue: mpsc::UnboundedSender<Queued>,
}

impl FleetGitWriter {
    /// Starts the writer task. It ends when every handle is dropped, or when
    /// the task is aborted.
    #[must_use]
    pub fn start(
        store: Store,
        policy: Arc<PolicyHub>,
        options: OptionsProvider,
    ) -> (Self, JoinHandle<()>) {
        Self::start_with(
            store,
            policy,
            options,
            LandLocks::new(),
            LandTimeouts::default(),
        )
    }

    /// [`Self::start`] with the per-land locks that the abandonment handler
    /// shares, and the time limits of a land publication.
    #[must_use]
    pub fn start_with(
        store: Store,
        policy: Arc<PolicyHub>,
        options: OptionsProvider,
        locks: Arc<LandLocks>,
        timeouts: LandTimeouts,
    ) -> (Self, JoinHandle<()>) {
        let env = WriterEnv {
            store,
            policy,
            push: PushWriter::new(Arc::clone(&options)),
            options,
            locks,
            timeouts,
        };
        let (queue, mut jobs) = mpsc::unbounded_channel::<Queued>();
        let task = tokio::spawn(async move {
            while let Some(Queued { job, done }) = jobs.recv().await {
                run(&env, &job).await;
                // A caller that stopped waiting does not need the answer.
                let _ = done.send(());
            }
        });
        (Self { queue }, task)
    }

    /// Queues `job` and waits until it ended. `false`: the writer task is gone.
    pub async fn run(&self, job: WriteJob) -> bool {
        let (done, ended) = oneshot::channel();
        if self.queue.send(Queued { job, done }).is_err() {
            return false;
        }
        ended.await.is_ok()
    }
}

async fn run(env: &WriterEnv, job: &WriteJob) {
    match job {
        WriteJob::BranchPush { session_id } => {
            if let Err(e) = branch_push(env, session_id).await {
                tracing::warn!(session = %session_id, error = %e, "the branch push was not recorded");
            }
        }
        WriteJob::LandBranch { land_id } => {
            super::land_publish::branch_job(&env.land_ctx(), land_id).await;
        }
        WriteJob::LandDefault { land_id } => {
            super::land_publish::default_job(&env.land_ctx(), land_id).await;
        }
    }
}

impl WriterEnv {
    fn land_ctx(&self) -> super::land_publish::Ctx {
        super::land_publish::Ctx {
            store: self.store.clone(),
            policy: Arc::clone(&self.policy),
            options: Arc::clone(&self.options),
            locks: Arc::clone(&self.locks),
            timeouts: self.timeouts,
        }
    }
}

/// The time limits of a land publication (design §2.4, §11.2). The tests
/// give shorter ones (decision Q6, H10).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LandTimeouts {
    /// `git push`: `None` takes the station limit `limits.git_push_seconds`
    /// (60 s by default); the tests set `Some(..)`.
    pub push: Option<Duration>,
    /// One forge request (30 s).
    pub forge: Duration,
    /// How long the abandonment waits for the per-land lock (120 s).
    pub lock_wait: Duration,
    /// A read of the remote or of the repository (20 s).
    pub read: Duration,
}

impl Default for LandTimeouts {
    fn default() -> Self {
        Self {
            push: None,
            forge: Duration::from_secs(30),
            lock_wait: Duration::from_secs(120),
            read: Duration::from_secs(20),
        }
    }
}

/// One `tokio::sync::Mutex<()>` for each `land_id` (design §11.5, revision 4,
/// M3): created at the first use and removed when the run is terminal and no
/// task waits. The writer takes it for each remote write of a land; the
/// abandonment handler takes it before it records the fence. Reads do not.
#[derive(Debug, Default)]
pub struct LandLocks {
    map: std::sync::Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    #[cfg(test)]
    trace: std::sync::Mutex<Vec<&'static str>>,
}

/// The held per-land lock.
pub type LandGuard = tokio::sync::OwnedMutexGuard<()>;

impl LandLocks {
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    fn mutex(&self, land_id: &str) -> Arc<tokio::sync::Mutex<()>> {
        let mut map = self.map.lock().unwrap_or_else(PoisonError::into_inner);
        Arc::clone(map.entry(land_id.to_owned()).or_default())
    }

    /// Waits for the lock of `land_id`.
    pub async fn lock(&self, land_id: &str) -> LandGuard {
        self.mutex(land_id).lock_owned().await
    }

    /// The lock when it is free now.
    #[must_use]
    pub fn try_lock(&self, land_id: &str) -> Option<LandGuard> {
        self.mutex(land_id).try_lock_owned().ok()
    }

    /// The lock, or `None` when it is not free after `wait`.
    pub async fn lock_within(&self, land_id: &str, wait: Duration) -> Option<LandGuard> {
        tokio::time::timeout(wait, self.lock(land_id)).await.ok()
    }

    /// Forgets the lock of `land_id` when nobody holds it or waits for it.
    pub fn release(&self, land_id: &str) {
        let mut map = self.map.lock().unwrap_or_else(PoisonError::into_inner);
        if map.get(land_id).is_some_and(|m| Arc::strong_count(m) == 1) {
            map.remove(land_id);
        }
    }

    /// How many locks exist (a test reads it).
    #[cfg(test)]
    pub fn open_locks(&self) -> usize {
        self.map
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }

    /// Records a step of a gated write (the tests read the order).
    #[cfg(test)]
    pub(crate) fn trace(&self, step: &'static str) {
        self.trace
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(step);
    }

    #[cfg(not(test))]
    #[inline]
    pub(crate) fn trace(&self, _step: &'static str) {}

    /// The recorded steps, and the record cleared.
    #[cfg(test)]
    pub(crate) fn take_trace(&self) -> Vec<&'static str> {
        std::mem::take(&mut *self.trace.lock().unwrap_or_else(PoisonError::into_inner))
    }
}

/// What a gated write needs (design §11.5, revision 4, B3).
pub struct Gate<'a> {
    pub store: &'a Store,
    pub policy: &'a Arc<PolicyHub>,
    pub locks: &'a LandLocks,
    /// The run as it was when the write was prepared: its ids and its
    /// authority never change.
    pub run: &'a LandRunRow,
}

/// What [`gated_write`] did.
#[derive(Debug, PartialEq, Eq)]
pub enum Gated<R> {
    /// The write ran and its outcome was recorded; the lock is released.
    Written(R),
    /// The run was fenced or ended (or the store failed) before the write:
    /// nothing was written.
    Stopped,
    /// The authority check refused: the run ended `failed` and no process
    /// was started.
    Refused,
}

/// The gated start of a remote write of a land (design §11.5, revision 4,
/// B3). The caller did the awaited preparation (the destination check,
/// `pr_find`) without the lock. The steps, in this order:
///
/// 1. the per-land lock;
/// 2. the intent transaction `intent` (it requires `running` and no fence);
/// 3. the authority check, for a run with authority (point (b) or (d));
/// 4. `start()`, a synchronous closure: the `spawn` of `git push`, or the
///    construction of the request future. There is no `.await` between the
///    check and `start()`, and the future that `start()` returns is awaited
///    at once;
/// 5. `record(result)`, with the lock still held: it stores the outcome and,
///    for an applied write, the step result;
/// 6. the lock is released.
pub async fn gated_write<T, R, F1, F2>(
    gate: &Gate<'_>,
    intent: Step,
    start: impl FnOnce() -> F1,
    record: impl FnOnce(T) -> F2,
) -> Gated<R>
where
    F1: Future<Output = T>,
    F2: Future<Output = R>,
{
    let land_id = gate.run.land_id.clone();
    gate.locks.trace("prepare");
    let guard = gate.locks.lock(&land_id).await;
    gate.locks.trace("lock");
    match gate.store.land_step(land_id.clone(), intent).await {
        Ok(true) => {}
        Ok(false) => return Gated::Stopped,
        Err(e) => {
            tracing::error!(land = %land_id, error = %e, "a land intent was not stored");
            return Gated::Stopped;
        }
    }
    gate.locks.trace("intent");
    if let Err(refusal) = super::land_run::recheck_run(gate.policy, gate.run).await {
        gate.locks.trace("refused");
        refuse(gate, refusal).await;
        return Gated::Refused;
    }
    // No `.await` from here to the first poll of `fut` (B3).
    gate.locks.trace("check_done");
    let fut = start();
    gate.locks.trace("spawn");
    let result = fut.await;
    let recorded = record(result).await;
    gate.locks.trace("outcome");
    drop(guard);
    Gated::Written(recorded)
}

/// A refusal after the intent and before the write: one transaction under
/// the lock clears the pending write and ends the run (design §11.5, B3).
async fn refuse(gate: &Gate<'_>, refusal: Refusal) {
    let land_id = gate.run.land_id.clone();
    let fresh = match gate.store.land_run(land_id.clone()).await {
        Ok(Some(row)) => row,
        _ => gate.run.clone(),
    };
    let step = Step {
        pending: PendingChange::Clear,
        reconcile: ReconcileChange::Clear,
        outcome: OutcomeChange::Set(WRITE_NOT_STARTED),
        end: Some(StepEnd {
            state: LandState::Failed,
            code: Some(refusal.code().to_owned()),
            text: Some(land_text(
                &fresh,
                &super::land_run::authority_first(refusal),
                None,
            )),
        }),
        ..Step::default()
    };
    if let Err(e) = gate.store.land_step(land_id.clone(), step).await {
        tracing::error!(land = %land_id, error = %e, "a land authority refusal was not stored");
    }
}

/// The destination of a push, from the current settings (design §3.8 rules 2
/// and 3): the WIP URL when the project sets one, else the remote.
fn destination(view: &fleetdeck_proto::fleet_git::GitProjectView) -> PushDestination {
    match &view.settings.wip_remote {
        Some(url) => PushDestination::Wip { url: url.clone() },
        None => PushDestination::Remote {
            name: view.settings.push_remote.clone(),
            project: view.remote.clone(),
            clone_url: view.clone_url.clone(),
        },
    }
}

/// One due row (design §5.5): the recovery read of an open attempt first,
/// else the destination check, the attempt, the push and its result.
async fn branch_push(env: &WriterEnv, session_id: &SessionId) -> crate::store::Result<()> {
    let store = &env.store;
    let (Some(row), Some(session)) = (
        store.git_push(session_id.clone()).await?,
        store.git_session(session_id.clone()).await?,
    ) else {
        return Ok(());
    };
    let Some(project) = store.git_project(session.project_id.clone()).await? else {
        return Ok(());
    };
    // A project that turned push off, or whose home is not verified, keeps
    // its rows (design §3.3).
    let mode = project.view.settings.push;
    if matches!(mode, PushMode::Off | PushMode::Unknown)
        || project.check_state != fleetdeck_proto::fleet_git::HomeCheckState::Verified
        || !matches!(
            row.state,
            PushState::Due | PushState::Failing | PushState::Pushing
        )
    {
        return Ok(());
    }
    let Some(slug) = session.slug.clone() else {
        return Ok(());
    };
    let repo_dir = Path::new(&session.repo_root);
    if row.attempt_id.is_some() {
        let job = attempt_job(&row, &session, &slug);
        let Some(job) = job else {
            return Ok(());
        };
        let result = env.push.recover(&job).await;
        return settle(store, session_id, Ok(result)).await;
    }
    let allow_local = env.policy.current().allow_local_remotes();
    let verified = env
        .push
        .verify(
            repo_dir,
            &destination(&project.view),
            &row.target_dest,
            allow_local,
        )
        .await;
    let url = match verified {
        Ok(verified) => verified.url,
        Err(refusal) => {
            store
                .git_push_failed(
                    session_id.clone(),
                    (refusal.code.to_owned(), refusal.text),
                    false,
                    backoff_delay,
                )
                .await?;
            return Ok(());
        }
    };
    let opened = store.git_push_open_attempt(session_id.clone(), url).await?;
    let Some(job) = attempt_job(&opened, &session, &slug) else {
        return Ok(());
    };
    let timeout = env.policy.current().timeouts().git_push;
    let result = env.push.submit(&job, timeout).await;
    settle(store, session_id, result.map_err(|e| e.to_string())).await
}

/// The push job of the open attempt of `row` (the stored values).
fn attempt_job(row: &PushRow, session: &SessionGitRow, slug: &str) -> Option<PushJob> {
    Some(PushJob {
        repo_dir: Path::new(&session.repo_root).to_path_buf(),
        url: row.attempt_url.clone()?,
        slug: slug.to_owned(),
        publish_ref: row.ref_name.clone(),
        oid: row.attempt_oid.clone()?,
        lease: row.attempt_lease.clone().unwrap_or_default(),
    })
}

/// Records the result of a push or of its recovery read (design §5.5 steps 4
/// and 5). A refspec that was refused ran no Git: it closes the attempt.
async fn settle(
    store: &Store,
    session_id: &SessionId,
    result: Result<PushResult, String>,
) -> crate::store::Result<()> {
    match result {
        Ok(PushResult::Applied) => {
            store.git_push_applied(session_id.clone()).await?;
        }
        Ok(PushResult::NotApplied(failure)) => {
            let text = (failure.code.to_owned(), failure.text);
            store
                .git_push_failed(session_id.clone(), text, false, backoff_delay)
                .await?;
        }
        Ok(PushResult::Unknown(failure)) => {
            let text = (failure.code.to_owned(), failure.text);
            store
                .git_push_failed(session_id.clone(), text, true, backoff_delay)
                .await?;
        }
        Err(refused) => {
            tracing::error!(session = %session_id, error = %refused, "a push ref was refused");
            let text = (
                code::INTERNAL.to_owned(),
                format!("Fleet Git refused to push this ref: {refused}"),
            );
            store
                .git_push_failed(session_id.clone(), text, false, backoff_delay)
                .await?;
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "fleet_git_writer_tests.rs"]
mod tests;
