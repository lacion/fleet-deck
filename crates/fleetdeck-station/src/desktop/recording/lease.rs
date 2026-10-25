//! The continuation lease client of a recording (design §6.3, §6.4, §7.5,
//! decisions O12-10 items 12 and 13).
//!
//! The station asks the deck `desktop.capture_lease` on the link of the
//! start: once before the spawn (`start`), then every second while it records
//! (`record`), and once before the import (`import`). At most one request is
//! in flight; a request has a 5 s deadline. A successful answer moves the
//! recorder's lease expiry to `boot(request start) + 6 s`; the expiry itself
//! runs on the recorder thread, so an awaited request cannot postpone it.
//! The operation keeps its own copy of the expiry in [`LeaseGuard`], which
//! outlives the recorder (the import and the commit).

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;
use fleetdeck_link::control::Control;
use fleetdeck_proto::link::Outcome;
use fleetdeck_proto::recording::{
    CaptureLeaseAnswer, CaptureLeaseRequest, LeaseDecision, LeasePhase, StopReason,
};
use tokio::task::JoinHandle;

use super::Timing;
use super::guard::LeaseGuard;
use super::latch::Latch;
use crate::media::clock::{BootInstant, Clocks, SuspendWatch};
use crate::media::recorder::RecorderCtl;

/// The deck link of a recording, as the recording sees it. The production
/// value is [`ControlLink`]; a test replaces it.
#[async_trait]
pub trait DeckLink: Send + Sync {
    /// True while the link has not ended.
    fn open(&self) -> bool;
    /// True while the link is the current link of its peer.
    fn current(&self) -> bool;
    /// Waits until the link ended.
    async fn closed(&self);
    /// One `desktop.capture_lease` request on this link and its response. The
    /// error text is for the log only.
    async fn lease(&self, req: &CaptureLeaseRequest) -> Result<Outcome, String>;
}

/// The control channel of a deck link.
pub struct ControlLink(pub Control);

#[async_trait]
impl DeckLink for ControlLink {
    fn open(&self) -> bool {
        self.0.ended().is_none()
    }

    fn current(&self) -> bool {
        self.0.is_current()
    }

    async fn closed(&self) {
        self.0.closed().await;
    }

    async fn lease(&self, req: &CaptureLeaseRequest) -> Result<Outcome, String> {
        let params = serde_json::to_value(req).map_err(|e| e.to_string())?;
        self.0
            .request("desktop.capture_lease", None, params)
            .await
            .map_err(|e| e.to_string())
    }
}

/// What the deck decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Verdict {
    Continue,
    /// The owner stop: save.
    Save,
    /// Discard with this reason (a lease reason of §7.5, or `lease_denied`).
    Discard(StopReason),
}

/// The result of one request: the verdict (`None`: the answer is ignored)
/// and the boot instant at which the request started.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Answered {
    pub verdict: Option<Verdict>,
    pub started: BootInstant,
}

/// The verdict of a response to request `sequence` (design §7.5). A wrong
/// sequence is ignored. Every other answer that is not exactly `continue`
/// without a reason, `save` with `owner_stop`, or `discard` with an allowed
/// reason is `lease_denied`; so is an error, a wire error and a malformed
/// answer.
pub(crate) fn judge(sequence: u64, response: &Result<Outcome, String>) -> Option<Verdict> {
    let denied = Some(Verdict::Discard(StopReason::LeaseDenied));
    let Ok(Outcome::Ok(value)) = response else {
        return denied;
    };
    let Ok(answer) = serde_json::from_value::<CaptureLeaseAnswer>(value.clone()) else {
        return denied;
    };
    if answer.sequence != sequence {
        return None;
    }
    match (answer.decision, answer.reason) {
        (LeaseDecision::Continue, None) => Some(Verdict::Continue),
        (LeaseDecision::Save, Some(r)) if r.lease_allowed(LeaseDecision::Save) => {
            Some(Verdict::Save)
        }
        (LeaseDecision::Discard, Some(r)) if r.lease_allowed(LeaseDecision::Discard) => {
            Some(Verdict::Discard(r))
        }
        _ => denied,
    }
}

/// The lease of one recording.
pub(crate) struct Lease {
    link: Arc<dyn DeckLink>,
    capture_id: String,
    action_hash: String,
    clocks: Arc<dyn Clocks>,
    timing: Timing,
    sequence: AtomicU64,
    /// One request in flight: the renewals and the import request take turns.
    inflight: tokio::sync::Mutex<()>,
}

impl Lease {
    pub(crate) fn new(
        link: Arc<dyn DeckLink>,
        capture_id: &str,
        action_hash: &str,
        clocks: Arc<dyn Clocks>,
        timing: Timing,
    ) -> Arc<Self> {
        Arc::new(Self {
            link,
            capture_id: capture_id.to_owned(),
            action_hash: action_hash.to_owned(),
            clocks,
            timing,
            sequence: AtomicU64::new(0),
            inflight: tokio::sync::Mutex::new(()),
        })
    }

    /// When a lease that started at `started` ends.
    pub(crate) fn until(&self, started: BootInstant) -> BootInstant {
        started.plus(self.timing.validity)
    }

    /// One request for `phase`, bounded by the request deadline. A suspend
    /// during the request, or a link that is no longer the current one when
    /// the answer comes, turns a good answer into a discard. A failed request
    /// on a link that ended is `link_closed`.
    pub(crate) async fn request(&self, phase: LeasePhase) -> Answered {
        let _turn = self.inflight.lock().await;
        let sequence = self.sequence.fetch_add(1, Ordering::AcqRel) + 1;
        let started = self.clocks.boot();
        let watch = SuspendWatch::start(&*self.clocks);
        let req = CaptureLeaseRequest {
            capture_id: self.capture_id.clone(),
            action_hash: self.action_hash.clone(),
            sequence,
            phase,
        };
        let response = match tokio::time::timeout(self.timing.request, self.link.lease(&req)).await
        {
            Ok(response) => response,
            Err(_) => Err("the lease request timed out".to_owned()),
        };
        if let Err(e) = &response {
            tracing::debug!(error = %e, "the lease request failed");
        }
        let mut verdict = judge(sequence, &response);
        // A request that failed because the link ended is a link close, not
        // a denial: the supervisor and this renewal see the same end.
        if response.is_err() && (!self.link.open() || !self.link.current()) {
            verdict = Some(Verdict::Discard(StopReason::LinkClosed));
        }
        if matches!(verdict, Some(Verdict::Continue | Verdict::Save)) {
            if watch.suspended_with(&*self.clocks) {
                verdict = Some(Verdict::Discard(StopReason::LeaseExpired));
            } else if !self.link.open() || !self.link.current() {
                verdict = Some(Verdict::Discard(StopReason::LinkClosed));
            }
        }
        Answered { verdict, started }
    }

    /// The renewal task: a `record` request every period (never two at once),
    /// each answer applied at once, to the recorder and to the operation
    /// guard. It ends after a discard.
    pub(crate) fn spawn_renewals(
        self: &Arc<Self>,
        ctl: RecorderCtl,
        latch: Arc<Latch>,
        guard: Arc<LeaseGuard>,
    ) -> JoinHandle<()> {
        let lease = Arc::clone(self);
        tokio::spawn(async move {
            loop {
                let began = tokio::time::Instant::now();
                let answered = lease.request(LeasePhase::Record).await;
                match answered.verdict {
                    Some(Verdict::Continue) => {
                        let until = lease.until(answered.started);
                        guard.renew(until);
                        ctl.renew(until);
                    }
                    Some(Verdict::Save) => {
                        let until = lease.until(answered.started);
                        guard.renew(until);
                        ctl.renew(until);
                        latch.stop(true, StopReason::OwnerStop).await;
                    }
                    Some(Verdict::Discard(reason)) => {
                        latch.stop(false, reason).await;
                        return;
                    }
                    None => {}
                }
                tokio::time::sleep_until(began + lease.timing.tick).await;
            }
        })
    }
}
