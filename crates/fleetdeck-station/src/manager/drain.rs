//! Station drain before a self-upgrade (PLAN Phase 7, M3 7-3, DL-128).
//!
//! A draining station refuses commands that create new work (`start`,
//! `resume`, `fork`, `prompt`, `follow_up`, `steer`, and a revert) with
//! `invalid_state`. It still admits the commands that end or settle work:
//! answers, interrupts, stops, mode and model changes, rename, and the
//! auto-continue preference. [`SessionManager::drain`] waits until no
//! session has open work, or until the bound.

use std::sync::atomic::Ordering;
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
            | SessionCommand::Fork { .. }
            | SessionCommand::Prompt { .. }
            | SessionCommand::FollowUp { .. }
            | SessionCommand::Steer { .. }
            | SessionCommand::Revert { .. }
    )
}

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

    /// Refuses new work from now on and waits until no session has open
    /// work, at most `bound`. The drain never ends: a drained station is
    /// stopped and replaced.
    ///
    /// # Errors
    /// A database error while the sessions are read.
    pub async fn drain(&self, bound: Duration) -> crate::store::Result<DrainReport> {
        self.draining.store(true, Ordering::SeqCst);
        let deadline = tokio::time::Instant::now() + bound;
        loop {
            let busy: Vec<String> = self
                .store()
                .snapshot()
                .await?
                .sessions
                .iter()
                .filter(|s| busy(s))
                .map(|s| s.session_id.to_string())
                .collect();
            if busy.is_empty() || tokio::time::Instant::now() >= deadline {
                return Ok(DrainReport {
                    idle: busy.is_empty(),
                    busy,
                });
            }
            tokio::time::sleep(POLL).await;
        }
    }
}
