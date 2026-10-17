//! Station drain before a self-upgrade (PLAN Phase 7, M3 7-3, DL-128).
//!
//! A draining station refuses commands that create new work (`start`,
//! `resume`, `fork`, `prompt`, `follow_up`, `steer`, and a revert) with
//! `invalid_state`. It still admits the commands that end or settle work:
//! answers, interrupts, stops, mode and model changes, rename, and the
//! auto-continue preference. [`SessionManager::drain`] waits until no
//! session has open work, or until the bound.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use fleetdeck_proto::command::SessionCommand;
use fleetdeck_proto::event::SessionState;
use fleetdeck_proto::{Reject, RejectCode, SessionSummary};
use serde::{Deserialize, Serialize};

use super::SessionManager;
use crate::store::DecideError;

/// The poll interval of [`SessionManager::drain`].
const POLL: Duration = Duration::from_millis(50);

/// The result of a drain.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DrainReport {
    /// True when no session had open work at the end.
    pub idle: bool,
    /// The sessions that still had open work at the end.
    pub busy: Vec<String>,
}

/// True when the command creates new provider work.
#[must_use]
pub fn creates_work(command: &SessionCommand) -> bool {
    matches!(
        command,
        SessionCommand::Start { .. }
            | SessionCommand::Resume { .. }
            | SessionCommand::Import { .. }
            | SessionCommand::MoveIn { .. }
            | SessionCommand::Fork { .. }
            | SessionCommand::Prompt { .. }
            | SessionCommand::FollowUp { .. }
            | SessionCommand::Steer { .. }
            | SessionCommand::Revert { .. }
            | SessionCommand::ShellCommand { .. }
    )
}

/// One submit that creates work and is not decided yet (NV-25). The count
/// ends at drop.
#[derive(Debug)]
pub(super) struct DrainSubmit<'a>(&'a AtomicUsize);

impl<'a> DrainSubmit<'a> {
    /// Counts one submit.
    pub(super) fn enter(count: &'a AtomicUsize) -> Self {
        count.fetch_add(1, Ordering::SeqCst);
        Self(count)
    }
}

impl Drop for DrainSubmit<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// The text in `busy` when only a submit that is not decided yet holds the
/// drain.
const DECIDING: &str = "(a command was being decided)";

/// The refusal of new work while the station drains.
///
/// # Errors
/// [`DecideError::Reject`] with `invalid_state` when `draining` and the
/// command creates new work.
pub fn guard(draining: bool, command: &SessionCommand) -> Result<(), DecideError> {
    if draining && creates_work(command) {
        return Err(DecideError::Reject(Reject::new(
            RejectCode::InvalidState,
            "the station is draining for an upgrade; new work is refused",
        )));
    }
    Ok(())
}

/// True when the session has open work: a start, a stop, a run or a turn.
#[must_use]
pub fn busy(summary: &SessionSummary) -> bool {
    match summary.state {
        SessionState::Starting | SessionState::Stopping => true,
        SessionState::Running => summary.current_run.is_some() || summary.current_turn.is_some(),
        SessionState::Exited | SessionState::Lost => false,
    }
}

impl SessionManager {
    /// True after [`Self::drain`] started.
    #[must_use]
    pub fn draining(&self) -> bool {
        self.draining.load(Ordering::SeqCst)
    }

    /// Counts a submit that creates work, until the guard drops. The count
    /// comes before the flag is read: [`Self::drain`] sets the flag, then
    /// reads the count, so one of the two always sees the other (NV-25).
    pub(super) fn enter_drain_submit(&self) -> DrainSubmit<'_> {
        DrainSubmit::enter(&self.drain_submits)
    }

    /// Refuses new work from now on and waits until no session has open
    /// work and no submit that creates work is being decided, at most
    /// `bound`. The drain never ends: a drained station is stopped and
    /// replaced.
    ///
    /// # Errors
    /// A database error while the sessions are read.
    pub async fn drain(&self, bound: Duration) -> crate::store::Result<DrainReport> {
        self.draining.store(true, Ordering::SeqCst);
        let deadline = tokio::time::Instant::now() + bound;
        loop {
            // The count is read before the snapshot: a submit that ended
            // before this read has committed its work (NV-25).
            let deciding = self.drain_submits.load(Ordering::SeqCst) > 0;
            let mut busy: Vec<String> = self
                .store()
                .snapshot()
                .await?
                .sessions
                .iter()
                .filter(|s| busy(s))
                .map(|s| s.session_id.to_string())
                .collect();
            // B8: a session with an unsettled shell command is busy.
            for row in self.store().shell_runs_unsettled().await? {
                let id = row.session_id.to_string();
                if !busy.contains(&id) {
                    busy.push(id);
                }
            }
            if (busy.is_empty() && !deciding) || tokio::time::Instant::now() >= deadline {
                if deciding && busy.is_empty() {
                    busy.push(DECIDING.to_owned());
                }
                return Ok(DrainReport {
                    idle: busy.is_empty(),
                    busy,
                });
            }
            tokio::time::sleep(POLL).await;
        }
    }
}
