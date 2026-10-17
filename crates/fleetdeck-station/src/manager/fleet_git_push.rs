//! The push triggers and the push loop of Fleet Git (FG-2, design §5.4,
//! §5.6, O11).
//!
//! Two tasks run on each station:
//!
//! - the trigger task follows the event log: a `turn.completed` (push mode
//!   `on_turn`) or a `session.ended` (`on_turn` and `on_exit`) of a session
//!   with a push row reads the tip of the session branch into `desired_oid`;
//! - the runner finds the rows that are due and gives each one to the writer
//!   (`manager/fleet_git_writer.rs`). A row that a restart left in `pushing`
//!   runs its recovery read first.
//!
//! Both pause while a deck link without `fleet_git.v1` is up
//! ([`Store::fleet_git_paused`]) and keep their rows (design §3.3).

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use fleetdeck_proto::event::SessionState;
use fleetdeck_proto::fleet_git::PushMode;
use fleetdeck_proto::{Event, EventBody, EventEnvelope, SessionId};
use tokio::sync::watch;
use tokio::task::JoinHandle;

use super::fleet_git_writer::{FleetGitWriter, WriteJob};
use super::policy_hub::PolicyHub;
use crate::store::{ReadResult, Store};
use crate::workspace::fleet_git::OptionsProvider;
use crate::workspace::{Git, GitOptions};

/// How often the runner looks for due rows, and the trigger task for a pause
/// to end.
const TICK: Duration = Duration::from_secs(1);
/// The most events that the trigger task reads at once.
const BATCH: usize = 256;
/// The time limit of the read of a branch tip.
const TIP_TIMEOUT: Duration = Duration::from_secs(10);

/// Starts the writer, the trigger task, the runner and the land engine (FG-8).
/// The handles end when `shutdown` becomes `true`.
#[must_use]
pub fn start(
    store: Store,
    policy: Arc<PolicyHub>,
    options: OptionsProvider,
    shutdown: watch::Receiver<bool>,
) -> (Vec<JoinHandle<()>>, super::land::LandEngine) {
    let (writer, writer_task) =
        FleetGitWriter::start(store.clone(), Arc::clone(&policy), Arc::clone(&options));
    let triggers = tokio::spawn(follow(store.clone(), shutdown.clone()));
    let (land, land_tasks) = super::land::start(store.clone(), policy, options, shutdown.clone());
    let runner = tokio::spawn(run(store, writer, shutdown));
    let mut tasks = vec![writer_task, triggers, runner];
    tasks.extend(land_tasks);
    (tasks, land)
}

/// Waits `time`, or returns `true` at the shutdown.
async fn nap(shutdown: &mut watch::Receiver<bool>, time: Duration) -> bool {
    tokio::select! {
        () = tokio::time::sleep(time) => {}
        _ = shutdown.wait_for(|stop| *stop) => return true,
    }
    *shutdown.borrow()
}

/// The runner: each tick, every due row goes to the writer, one after the
/// other.
async fn run(store: Store, writer: FleetGitWriter, mut shutdown: watch::Receiver<bool>) {
    loop {
        if nap(&mut shutdown, TICK).await {
            return;
        }
        if store.fleet_git_paused().load(Ordering::Acquire) {
            continue;
        }
        let due = match store.git_push_due(store.now_ms()).await {
            Ok(due) => due,
            Err(e) => {
                tracing::warn!(error = %e, "the due push rows are unreadable");
                continue;
            }
        };
        for (row, _) in due {
            if *shutdown.borrow() {
                return;
            }
            // A land or a move holds the session: the row waits (design §5.4).
            match store.session_hold(row.session_id.clone()).await {
                Ok(None) => {}
                Ok(Some(_)) => continue,
                Err(e) => {
                    tracing::warn!(error = %e, "a session hold is unreadable");
                    continue;
                }
            }
            let job = WriteJob::BranchPush {
                session_id: row.session_id,
            };
            if !writer.run(job).await {
                return;
            }
        }
    }
}

/// The trigger task: follows the event log from the stored position
/// (`fleet_git_push_cursor`; the first run starts at the head).
async fn follow(store: Store, mut shutdown: watch::Receiver<bool>) {
    let mut head = store.subscribe();
    let mut cursor = match store.git_push_cursor().await {
        Ok(Some(cursor)) => cursor,
        Ok(None) => {
            let now = store.head();
            save_cursor(&store, now).await;
            now
        }
        Err(e) => {
            tracing::warn!(error = %e, "the push trigger position is unreadable");
            return;
        }
    };
    loop {
        if store.fleet_git_paused().load(Ordering::Acquire) {
            if nap(&mut shutdown, TICK).await {
                return;
            }
            continue;
        }
        match store.read_after(cursor, BATCH).await {
            Ok(ReadResult::Events { events, head: at }) => {
                let full = events.len() >= BATCH;
                for event in &events {
                    on_event(&store, event).await;
                    cursor = event.station_seq;
                }
                if !events.is_empty() {
                    save_cursor(&store, cursor).await;
                }
                if full {
                    continue;
                }
                let waiting = at.max(cursor);
                tokio::select! {
                    _ = head.wait_for(|h| *h > waiting) => {}
                    _ = shutdown.wait_for(|stop| *stop) => return,
                    () = tokio::time::sleep(TICK * 5) => {}
                }
            }
            // The log lost events after the position (retention), or the
            // position is ahead of the log: look at every live row (design
            // §3.3, decision 18).
            Ok(ReadResult::SnapshotRequired { head: at }) => {
                cursor = at;
                rescan(&store).await;
                save_cursor(&store, cursor).await;
            }
            Err(e) => {
                tracing::warn!(error = %e, "the push trigger cannot read the event log");
                cursor = store.head();
                rescan(&store).await;
                save_cursor(&store, cursor).await;
                if nap(&mut shutdown, TICK).await {
                    return;
                }
            }
        }
    }
}

async fn save_cursor(store: &Store, cursor: u64) {
    if let Err(e) = store.git_set_push_cursor(cursor).await {
        tracing::warn!(error = %e, "the push trigger position was not stored");
    }
}

/// Is `event` a trigger? A completed turn, or the end of the session.
pub(in crate::manager) async fn on_event(store: &Store, event: &EventEnvelope) {
    let ended = match &event.event {
        EventBody::Known(Event::TurnCompleted { .. }) => false,
        EventBody::Known(Event::SessionEnded { .. }) => true,
        _ => return,
    };
    trigger(store, &event.session_id, ended).await;
}

/// Every session with a push row that is not stopped is looked at once.
async fn rescan(store: &Store) {
    let sessions = match store.git_push_live_sessions().await {
        Ok(sessions) => sessions,
        Err(e) => {
            tracing::warn!(error = %e, "the push rows are unreadable");
            return;
        }
    };
    for session in sessions {
        let ended = matches!(
            store.summary(session.clone()).await,
            Ok(Some(s)) if matches!(s.state, SessionState::Exited | SessionState::Lost)
        );
        trigger(store, &session, ended).await;
    }
}

/// One trigger (design §5.4): when the push mode of the project asks for it,
/// the tip of the session branch goes to `desired_oid`. It never commits.
pub(in crate::manager) async fn trigger(store: &Store, session_id: &SessionId, ended: bool) {
    let result = async {
        let Some(session) = store.git_session(session_id.clone()).await? else {
            return Ok(());
        };
        let Some(project) = store.git_project(session.project_id.clone()).await? else {
            return Ok(());
        };
        let wanted = match project.view.settings.push {
            PushMode::OnTurn => true,
            PushMode::OnExit => ended,
            PushMode::Off | PushMode::Unknown => false,
        };
        let (true, Some(branch), true) =
            (wanted, session.branch.as_deref(), session.created_branch)
        else {
            return Ok(());
        };
        if let Some(tip) = branch_tip(&session.repo_root, branch).await {
            store.git_push_trigger(session_id.clone(), tip).await?;
        }
        Ok::<(), crate::store::StoreError>(())
    }
    .await;
    if let Err(e) = result {
        tracing::warn!(session = %session_id, error = %e, "a push trigger failed");
    }
}

/// The commit of the local branch `refs/heads/<branch>` in the repository of
/// the home (design §5.4). `None` when the branch does not exist, or Git
/// cannot read it.
async fn branch_tip(repo: &str, branch: &str) -> Option<String> {
    let options = GitOptions {
        timeout: TIP_TIMEOUT,
        ..GitOptions::isolated()
    };
    let name = format!("refs/heads/{branch}");
    let (exit, out, _) = Git::new(std::path::Path::new(repo), options)
        .status(&["rev-parse", "--verify", "--quiet", &name], &[], None)
        .await
        .ok()?;
    let oid = String::from_utf8_lossy(&out).trim().to_owned();
    (exit == 0 && matches!(oid.len(), 40 | 64)).then_some(oid)
}
