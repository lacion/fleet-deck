//! The one serialized writer of a station (FG-2, design §5.5, decision 15):
//! the only task that runs a remote write. It takes jobs from one queue, one
//! job at a time, so two writes never run together and the lease bookkeeping
//! of `git_pushes` has one writer.
//!
//! Phase 1 has one job, [`WriteJob::BranchPush`]. The land jobs of phase 3
//! join the same queue.

use std::path::Path;
use std::sync::Arc;

use fleetdeck_proto::SessionId;
use fleetdeck_proto::fleet_git::{PushMode, PushState, code};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

use super::policy_hub::PolicyHub;
use crate::store::Store;
use crate::store::git_state::{PushRow, SessionGitRow};
use crate::workspace::fleet_git::{
    OptionsProvider, PushDestination, PushJob, PushResult, PushWriter, backoff_delay,
};

/// One remote write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WriteJob {
    /// The push of the branch of one session (FG-2, design §5.5 steps 1 to 6).
    BranchPush { session_id: SessionId },
}

/// What the writer needs to run a job.
pub struct WriterEnv {
    pub store: Store,
    pub policy: Arc<PolicyHub>,
    pub push: PushWriter,
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
        let env = WriterEnv {
            store,
            policy,
            push: PushWriter::new(options),
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
    let result = env.push.submit(&job).await;
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
