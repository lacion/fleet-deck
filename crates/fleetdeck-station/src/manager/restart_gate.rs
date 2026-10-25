//! The restart freeze (DL-172 §1.6, PRE-PR r1 finding 3).
//!
//! A restart that the owner accepts without consent must not stop a session
//! that starts after the check. [`RestartState`] is the freeze:
//!
//! - While `restarting`, a `start`, `import` or `resume` is refused with
//!   `invalid_state` ([`guard`]), and the effect worker does not claim a
//!   queued start or resume effect (it stays queued and runs after the
//!   re-exec; it is never failed).
//! - A submit that creates a session (or may resume one: an input to a
//!   hibernated session, NV-27) and a claim of the worker count while
//!   they run ([`InFlight`]). Each takes its count before it reads the flag,
//!   and the admission sets the flag before it reads the counts, so one of
//!   the two always sees the other.
//!
//! - The admission holds [`RestartState::lock_handoff`] for its whole
//!   check. A start task with a setup command takes the same lock after its
//!   command ended, before it takes its claim and ends its setup row, so no
//!   handoff can come between the counts, the lists and the commit of one
//!   admission (PRE-PR r3 finding 2). A task that finds the freeze set waits
//!   for the end of the admission ([`RestartState::wait_resolved`]).
//!
//! The flag stays set after a restart is committed. A refused or failed
//! admission clears it.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};
use std::time::Duration;

use fleetdeck_proto::command::SessionCommand;
use fleetdeck_proto::event::ProviderKind;
use fleetdeck_proto::{Reject, RejectCode};

use super::SessionManager;
use crate::store::DecideError;

/// The poll interval of [`SessionManager::settle_starts`].
const POLL: Duration = Duration::from_millis(50);

/// The message of a refusal while the station restarts.
pub const RESTARTING: &str = "the station is restarting";

/// The freeze flag and the counts of the work that it waits for.
#[derive(Debug, Default)]
pub struct RestartState {
    restarting: AtomicBool,
    committed: AtomicBool,
    submits: AtomicUsize,
    claims: AtomicUsize,
    /// The provider kind under an update (B28): 0 none, 1 Claude, 2 Codex,
    /// 3 another kind.
    updating: AtomicU8,
    /// Held by the admission for its whole check, and by the handoff of a
    /// deferred setup start.
    handoff: tokio::sync::Mutex<()>,
    /// Wakes [`Self::wait_resolved`] when the flag changes.
    changed: tokio::sync::Notify,
}

impl RestartState {
    /// True from [`Self::begin`] until a refusal clears it, or for ever
    /// after a commit.
    #[must_use]
    pub fn restarting(&self) -> bool {
        self.restarting.load(Ordering::SeqCst)
    }

    /// True after a restart is committed.
    #[must_use]
    pub fn committed(&self) -> bool {
        self.committed.load(Ordering::SeqCst)
    }

    /// The lock of the handoff: the admission holds it for its whole check,
    /// and the task of a deferred setup start takes it for its handoff.
    pub async fn lock_handoff(&self) -> tokio::sync::MutexGuard<'_, ()> {
        self.handoff.lock().await
    }

    /// Waits until the restart is committed (true) or the flag is cleared
    /// (false).
    pub async fn wait_resolved(&self) -> bool {
        loop {
            let woken = self.changed.notified();
            tokio::pin!(woken);
            woken.as_mut().enable();
            if self.committed() {
                return true;
            }
            if !self.restarting() {
                return false;
            }
            woken.await;
        }
    }

    /// The provider kind under an update, if one holds the starts (B28
    /// §3.7.1).
    #[must_use]
    pub fn updating(&self) -> Option<ProviderKind> {
        match self.updating.load(Ordering::SeqCst) {
            0 => None,
            1 => Some(ProviderKind::Claude),
            2 => Some(ProviderKind::Codex),
            _ => Some(ProviderKind::Unknown),
        }
    }

    /// Starts the update hold of `kind`. False when the station restarts or
    /// another update holds: then nothing changed.
    pub(super) fn begin_update(&self, kind: ProviderKind) -> bool {
        let code = match kind {
            ProviderKind::Claude => 1,
            ProviderKind::Codex => 2,
            ProviderKind::Unknown => 3,
        };
        if self.restarting()
            || self
                .updating
                .compare_exchange(0, code, Ordering::SeqCst, Ordering::SeqCst)
                .is_err()
        {
            return false;
        }
        // The restart admission sets `restarting` before it reads `updating`.
        if self.restarting() {
            self.updating.store(0, Ordering::SeqCst);
            return false;
        }
        true
    }

    /// Ends the update hold and wakes the waiters.
    pub(super) fn end_update(&self) {
        self.updating.store(0, Ordering::SeqCst);
        self.changed.notify_waiters();
    }

    pub(super) fn begin(&self) {
        self.restarting.store(true, Ordering::SeqCst);
        self.changed.notify_waiters();
    }

    pub(super) fn commit(&self) {
        self.committed.store(true, Ordering::SeqCst);
        self.changed.notify_waiters();
    }

    /// Clears the flag, unless a restart is committed already.
    pub(super) fn end(&self) {
        if !self.committed.load(Ordering::SeqCst) {
            self.restarting.store(false, Ordering::SeqCst);
        }
        self.changed.notify_waiters();
    }

    fn idle(&self) -> bool {
        self.submits.load(Ordering::SeqCst) == 0 && self.claims.load(Ordering::SeqCst) == 0
    }

    /// The claim of the effect worker: the flag, read after the count.
    #[must_use]
    pub fn begin_claim(self: &Arc<Self>) -> crate::worker::Claim {
        let guard: Box<dyn std::any::Any + Send> = Box::new(self.enter_claim());
        crate::worker::Claim {
            hold_starts: self.restarting() || self.updating().is_some(),
            guard: Some(guard),
        }
    }

    /// A submit that creates a session starts; it ends when the guard drops.
    #[must_use]
    pub fn enter_submit(self: &Arc<Self>) -> InFlight {
        self.submits.fetch_add(1, Ordering::SeqCst);
        InFlight {
            state: Arc::clone(self),
            submit: true,
        }
    }

    /// The worker starts a claim; it ends when the guard drops.
    #[must_use]
    pub fn enter_claim(self: &Arc<Self>) -> InFlight {
        self.claims.fetch_add(1, Ordering::SeqCst);
        InFlight {
            state: Arc::clone(self),
            submit: false,
        }
    }
}

/// One counted unit of work. The count ends at drop.
#[derive(Debug)]
pub struct InFlight {
    state: Arc<RestartState>,
    submit: bool,
}

impl Drop for InFlight {
    fn drop(&mut self) {
        let count = if self.submit {
            &self.state.submits
        } else {
            &self.state.claims
        };
        count.fetch_sub(1, Ordering::SeqCst);
    }
}

/// True for the commands that create a session.
#[must_use]
pub fn creates_session(command: &SessionCommand) -> bool {
    matches!(
        command,
        SessionCommand::Start { .. }
            | SessionCommand::Import { .. }
            | SessionCommand::Resume { .. }
            | SessionCommand::MoveIn { .. }
    )
}

/// True for the inputs that resume a hibernated session: the decision
/// turns them into a resume (NV-27).
#[must_use]
pub fn may_resume(command: &SessionCommand) -> bool {
    matches!(
        command,
        SessionCommand::Prompt { .. }
            | SessionCommand::FollowUp { .. }
            | SessionCommand::Steer { .. }
            | SessionCommand::ShellCommand { .. }
    )
}

/// The refusal of a new session while the station restarts.
///
/// # Errors
/// [`DecideError::Reject`] with `invalid_state` when `restarting` and the
/// command creates a session.
pub fn guard(restarting: bool, command: &SessionCommand) -> Result<(), DecideError> {
    if restarting && creates_session(command) {
        return Err(DecideError::Reject(Reject::new(
            RejectCode::InvalidState,
            RESTARTING,
        )));
    }
    Ok(())
}

impl super::SessionTarget {
    /// The restart freeze that the manager shares with this target.
    #[must_use]
    pub fn with_restart(mut self, restart: Arc<RestartState>) -> Self {
        self.restart = restart;
        self
    }
}

impl SessionManager {
    /// The freeze of the restart admission.
    #[must_use]
    pub fn restart_state(&self) -> &Arc<RestartState> {
        &self.restart
    }

    /// Freezes new sessions (step a of the admission).
    pub fn begin_restart(&self) {
        self.restart.begin();
    }

    /// Ends the freeze after a refusal or a failure. The workers claim the
    /// queued starts again. No effect after [`Self::commit_restart`].
    pub fn end_restart(&self) {
        self.restart.end();
        self.store.nudge_worker();
    }

    /// Keeps the freeze for ever: the process restarts.
    pub fn commit_restart(&self) {
        self.restart.commit();
    }

    /// Waits until no submit that creates a session and no claim of a start
    /// or resume effect is in flight, at most `bound`. True when it is idle.
    pub async fn settle_starts(&self, bound: Duration) -> bool {
        let deadline = tokio::time::Instant::now() + bound;
        loop {
            if self.restart.idle() {
                return true;
            }
            if tokio::time::Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(POLL).await;
        }
    }
}
