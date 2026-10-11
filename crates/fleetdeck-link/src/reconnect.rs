//! Reconnect by the dialing side (PLAN §8.1, ADR-004 §2.10): exponential
//! backoff with equal jitter, capped at 5 minutes, reset only after a link
//! stayed up for 60 s (t3code-lessons: a link that drops at once must not
//! reset the backoff).
//!
//! Only the dialer reconnects. Every attempt commits a new, higher link
//! epoch ([`crate::dial`]), so the listener replaces a link that it still
//! thinks is alive. Failures are reported with their stable reason code.

use std::net::SocketAddr;
use std::time::Duration;

use fleetdeck_core::identity::PeerKey;
use ring::rand::{SecureRandom, SystemRandom};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::Instant;

use crate::hello::HandshakeError;
use crate::live::{LiveLink, start};
use crate::node::{NodeConfig, dial};
use crate::ws::LinkError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BackoffPolicy {
    /// The ceiling of the first retry.
    pub base: Duration,
    /// The largest ceiling (5 minutes).
    pub cap: Duration,
    /// A link that stays up this long resets the backoff.
    pub stable: Duration,
}

impl Default for BackoffPolicy {
    fn default() -> Self {
        Self {
            base: Duration::from_millis(500),
            cap: Duration::from_secs(300),
            stable: Duration::from_secs(60),
        }
    }
}

/// "Equal jitter": the delay of attempt `n` is uniform in `[c/2, c]` with
/// `c = min(cap, base * 2^n)`. It is never zero, so a crowd of stations
/// never redials in a tight loop, and it is spread, so they do not redial
/// together after a deck restart.
#[derive(Debug)]
pub struct Backoff {
    policy: BackoffPolicy,
    attempt: u32,
}

impl Backoff {
    #[must_use]
    pub fn new(policy: BackoffPolicy) -> Self {
        Self { policy, attempt: 0 }
    }

    #[must_use]
    pub fn attempt(&self) -> u32 {
        self.attempt
    }

    /// The ceiling of the next delay.
    #[must_use]
    pub fn ceiling(&self) -> Duration {
        let factor = 1u32.checked_shl(self.attempt.min(31)).unwrap_or(u32::MAX);
        self.policy
            .base
            .checked_mul(factor)
            .unwrap_or(self.policy.cap)
            .min(self.policy.cap)
    }

    /// The next delay; `unit` is a uniform value in `[0, 1]`.
    pub fn next_with(&mut self, unit: f64) -> Duration {
        let c = self.ceiling();
        self.attempt = self.attempt.saturating_add(1);
        c / 2 + c.mul_f64(unit.clamp(0.0, 1.0) / 2.0)
    }

    /// The next delay with a random jitter.
    pub fn next_delay(&mut self) -> Duration {
        let mut b = [0u8; 4];
        let unit = match SystemRandom::new().fill(&mut b) {
            Ok(()) => f64::from(u32::from_le_bytes(b)) / f64::from(u32::MAX),
            Err(_) => 1.0,
        };
        self.next_with(unit)
    }

    /// Reports how long the last link stayed up.
    pub fn link_ended(&mut self, uptime: Duration) {
        if uptime >= self.policy.stable {
            self.attempt = 0;
        }
    }
}

/// What the dialer task reports.
#[derive(Debug)]
pub enum DialerEvent {
    /// A new running link. The dialer waits for its control channel to end.
    Linked(Box<LiveLink>),
    /// A dial failed with the stable `reason`; the next try is in `retry_in`.
    Failed {
        attempt: u32,
        reason: String,
        retry_in: Duration,
    },
    /// The link ended after `uptime` with `reason`; redial in `retry_in`.
    Lost {
        uptime: Duration,
        reason: String,
        retry_in: Duration,
    },
    /// The peer is no longer pinned here (revoked): the dialer stops.
    Stopped { reason: String },
}

fn reason_of(e: &LinkError) -> String {
    e.reason().map_or_else(|| e.to_string(), str::to_owned)
}

/// Dials `peer` at `addr` until the receiver is dropped: dial, hand over the
/// link, wait for its end, back off, dial again with a higher epoch.
pub fn spawn_dialer(
    node: NodeConfig,
    addr: SocketAddr,
    peer: PeerKey,
    policy: BackoffPolicy,
) -> (mpsc::Receiver<DialerEvent>, JoinHandle<()>) {
    let (tx, rx) = mpsc::channel(16);
    let task = tokio::spawn(async move {
        let mut backoff = Backoff::new(policy);
        loop {
            let event = match dial(&node, addr, peer).await {
                Ok(open) => {
                    let link = start(&node, open);
                    let control = link.control.clone();
                    let up = Instant::now();
                    if tx.send(DialerEvent::Linked(Box::new(link))).await.is_err() {
                        return;
                    }
                    let closing = tokio::select! {
                        c = control.closed() => c,
                        () = tx.closed() => return,
                    };
                    let uptime = up.elapsed();
                    backoff.link_ended(uptime);
                    DialerEvent::Lost {
                        uptime,
                        reason: closing.reason,
                        retry_in: backoff.next_delay(),
                    }
                }
                Err(LinkError::Handshake(HandshakeError::NotPaired)) => {
                    let _ = tx
                        .send(DialerEvent::Stopped {
                            reason: HandshakeError::NotPaired.reason().to_owned(),
                        })
                        .await;
                    return;
                }
                Err(e) => DialerEvent::Failed {
                    attempt: backoff.attempt(),
                    reason: reason_of(&e),
                    retry_in: backoff.next_delay(),
                },
            };
            let retry_in = match &event {
                DialerEvent::Lost { retry_in, .. } | DialerEvent::Failed { retry_in, .. } => {
                    *retry_in
                }
                _ => Duration::ZERO,
            };
            tracing::info!(?retry_in, "link dialer: {}", describe(&event));
            if tx.send(event).await.is_err() {
                return;
            }
            tokio::select! {
                () = tokio::time::sleep(retry_in) => {}
                () = tx.closed() => return,
            }
        }
    });
    (rx, task)
}

fn describe(e: &DialerEvent) -> String {
    match e {
        DialerEvent::Linked(_) => "linked".into(),
        DialerEvent::Failed {
            attempt, reason, ..
        } => format!("dial attempt {attempt} failed: {reason}"),
        DialerEvent::Lost { uptime, reason, .. } => {
            format!("link lost after {uptime:?}: {reason}")
        }
        DialerEvent::Stopped { reason } => format!("stopped: {reason}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delays_grow_with_jitter_up_to_the_cap() {
        let mut b = Backoff::new(BackoffPolicy::default());
        assert_eq!(b.next_with(0.0), Duration::from_millis(250));
        assert_eq!(b.next_with(1.0), Duration::from_millis(1000));
        assert_eq!(b.next_with(0.5), Duration::from_millis(1500));
        for _ in 0..40 {
            let d = b.next_delay();
            assert!(d <= Duration::from_secs(300), "{d:?}");
        }
        assert_eq!(b.ceiling(), Duration::from_secs(300));
        let d = b.next_delay();
        assert!(
            d >= Duration::from_secs(150),
            "never below half the ceiling"
        );
    }

    #[test]
    fn only_a_stable_link_resets_the_backoff() {
        let mut b = Backoff::new(BackoffPolicy::default());
        for _ in 0..5 {
            b.next_delay();
        }
        b.link_ended(Duration::from_secs(59));
        assert_eq!(b.attempt(), 5, "a short link keeps the backoff");
        b.link_ended(Duration::from_secs(60));
        assert_eq!(b.attempt(), 0);
    }

    #[test]
    fn a_huge_attempt_count_does_not_overflow() {
        let mut b = Backoff::new(BackoffPolicy::default());
        for _ in 0..1000 {
            let d = b.next_delay();
            assert!(d > Duration::ZERO);
        }
        assert_eq!(b.ceiling(), Duration::from_secs(300));
    }
}
