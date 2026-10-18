//! The two guards of the recording operation (R13 P5b, design
//! `omarchy-p5b.md` §5.3, §6.1, §6.4, §7.4, §7.6).
//!
//! - [`LeaseGuard`] is the lease expiry of the operation. The recorder thread
//!   has its own expiry, but it ends with the recorder; the import and the
//!   commit come after. The guard lives from the first successful lease until
//!   the commit returns, checks the boot clock and the suspend watch every
//!   250 ms, and latches `lease_expired` (a discard) when the lease ran out.
//!   The operation also asks it, with no await, right before the commit.
//! - The launch guard runs on the thread of the recorder, immediately before
//!   `Command::spawn`. It reads memory and clocks only (no await, no remote
//!   call): the stop latch, the start permit, the live local gates and the
//!   lease.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use fleetdeck_proto::recording::StopReason;
use tokio::task::JoinHandle;

use super::check_gates;
use super::latch::Latch;
use super::op::{Run, Stage};
use crate::desktop::authority::Start;
use crate::media::clock::{BootInstant, Clocks, SuspendWatch};
use crate::media::recorder::{SpawnGuard, StopCause};

/// How often the lease guard looks at the clocks.
const PERIOD: Duration = Duration::from_millis(250);

/// What the one lock of the guard holds.
struct State {
    until: BootInstant,
    /// A decision found the lease out: it stays out.
    failed: bool,
}

/// The lease expiry of one recording operation. A renewal and a validity
/// check are one serialized decision under one lock.
pub(super) struct LeaseGuard {
    state: Mutex<State>,
    watch: SuspendWatch,
    clocks: Arc<dyn Clocks>,
    latch: Arc<Latch>,
}

impl LeaseGuard {
    /// A guard for a lease that ends at `until`; the suspend watch starts
    /// now. A decision that finds the lease out latches `lease_expired` in
    /// `latch`.
    pub(super) fn new(until: BootInstant, clocks: Arc<dyn Clocks>, latch: Arc<Latch>) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(State {
                until,
                failed: false,
            }),
            watch: SuspendWatch::start(&*clocks),
            clocks,
            latch,
        })
    }

    /// The one decision. It reads the boot clock under the lock; a lease
    /// that is out (or after a suspend, or found out before) is out for
    /// good, and the discard `lease_expired` is latched in memory. Else the
    /// expiry moves to `extend` when that is later.
    fn decide(&self, extend: Option<BootInstant>) -> bool {
        let valid = {
            let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            let now = self.clocks.boot();
            if state.failed || now >= state.until || self.watch.suspended_with(&*self.clocks) {
                state.failed = true;
                false
            } else {
                if let Some(until) = extend {
                    state.until = state.until.max(until);
                }
                true
            }
        };
        if !valid {
            self.latch.discard_now(StopReason::LeaseExpired);
        }
        valid
    }

    /// A successful renewal or import lease. It extends the expiry only
    /// when the lease is still in force; a renewal that arrives after the
    /// deadline (or after a suspend) does not extend it: the lease is out.
    pub(super) fn renew(&self, until: BootInstant) {
        self.decide(Some(until));
    }

    /// True while the lease has not run out and no suspend was seen. It
    /// reads the clocks only. A lease that was found out stays out.
    pub(super) fn valid(&self) -> bool {
        self.decide(None)
    }

    /// Starts the task of the guard. It ends after it latched the expiry;
    /// the returned handle ends it too when it is dropped.
    pub(super) fn arm(self: &Arc<Self>) -> Armed {
        let guard = Arc::clone(self);
        let task = tokio::spawn(async move {
            let mut tick = tokio::time::interval(PERIOD);
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                tick.tick().await;
                if !guard.valid() {
                    // The signal to a running recorder goes out first.
                    guard.latch.stop(false, StopReason::LeaseExpired).await;
                    return;
                }
            }
        });
        Armed {
            guard: Arc::clone(self),
            task,
        }
    }
}

/// A running [`LeaseGuard`] task: it ends when this value is dropped.
pub(super) struct Armed {
    guard: Arc<LeaseGuard>,
    task: JoinHandle<()>,
}

impl Armed {
    pub(super) fn guard(&self) -> &Arc<LeaseGuard> {
        &self.guard
    }
}

impl Drop for Armed {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Why the launch guard refused: the failure of the operation. It is set
/// before the guard answers, and read after the spawn returned.
pub(super) type Refusal = Arc<Mutex<Option<Stage>>>;

impl Run<'_> {
    /// The guard for the spawn (see the module text), and the slot that
    /// holds the failure when it refuses.
    ///
    /// - A stop (in the latch of memory): `recording_cancelled` with the
    ///   reason of the stop.
    /// - A start permit that is no longer valid: `lease_denied`.
    /// - A live local gate that is off: `policy_off`.
    /// - A lease that ran out: `lease_expired`.
    pub(super) fn launch_guard(
        &self,
        start: Arc<Start>,
        lease: Arc<LeaseGuard>,
    ) -> (SpawnGuard, Refusal) {
        let req = self.req;
        let refusal: Refusal = Arc::default();
        let slot = Arc::clone(&refusal);
        let (latch, admitted, allowed, monitor) = (
            Arc::clone(self.latch),
            req.admitted.clone(),
            Arc::clone(&req.allowed),
            req.monitor.clone(),
        );
        #[cfg(test)]
        let hold = self.desktop.recordings.hooks.launch();
        let guard: SpawnGuard = Box::new(move || {
            #[cfg(test)]
            if let Some(block) = hold {
                block.wait();
            }
            let refuse = |stage: Stage, cause: StopCause| {
                *slot.lock().unwrap_or_else(PoisonError::into_inner) = Some(stage);
                Err(cause)
            };
            if let Some(reason) = latch.first_stop() {
                return refuse(Stage::cancelled(reason), StopCause::Discard);
            }
            if let Err(fail) = start.still_valid() {
                return refuse(
                    Stage::new(fail, StopReason::LeaseDenied),
                    StopCause::Discard,
                );
            }
            if let Err(fail) = admitted
                .local_gates()
                .and_then(|()| check_gates(&allowed(), &monitor))
            {
                return refuse(Stage::new(fail, StopReason::PolicyOff), StopCause::Discard);
            }
            if !lease.valid() {
                return refuse(
                    Stage::discard(StopReason::LeaseExpired, false),
                    StopCause::LeaseExpired,
                );
            }
            Ok(())
        });
        (guard, refusal)
    }
}
