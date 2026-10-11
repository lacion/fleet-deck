//! Reconnect by the dialing side (PLAN §8.1): exponential backoff with jitter,
//! capped at 5 minutes, reset only after a link stayed up for a while
//! (t3code-lessons: a link that drops at once must not reset the backoff).
//!
//! Every dial uses a new, larger link epoch, so the listener replaces a link
//! that it still thinks is alive (§8.2: higher `link_epoch` wins).

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use ring::rand::{SecureRandom, SystemRandom};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::link::{Link, NodeConfig, dial};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BackoffPolicy {
    /// The ceiling of the first retry.
    pub base: Duration,
    /// The largest ceiling (5 minutes in the product).
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

/// "Equal jitter": the delay of attempt `n` is uniform in
/// `[c/2, c]` with `c = min(cap, base * 2^n)`. Never zero, so a crowd of
/// stations never redials in a tight loop; spread, so they do not redial
/// together after a deck restart.
#[derive(Debug)]
pub struct Backoff {
    policy: BackoffPolicy,
    attempt: u32,
}

impl Backoff {
    pub fn new(policy: BackoffPolicy) -> Self {
        Self { policy, attempt: 0 }
    }

    pub fn attempt(&self) -> u32 {
        self.attempt
    }

    /// The ceiling of the next delay.
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

/// Link epochs from the wall clock in milliseconds, strictly growing inside
/// one process. A restarted dialer starts above its old epochs unless the
/// clock went back (DL-021).
#[derive(Debug, Default)]
pub struct EpochClock {
    last: AtomicU64,
}

impl EpochClock {
    pub fn next(&self) -> u64 {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
            .unwrap_or(0);
        let mut prev = self.last.load(Ordering::Relaxed);
        loop {
            let next = now.max(prev.saturating_add(1));
            match self
                .last
                .compare_exchange(prev, next, Ordering::Relaxed, Ordering::Relaxed)
            {
                Ok(_) => return next,
                Err(p) => prev = p,
            }
        }
    }
}

#[derive(Debug)]
pub enum DialerEvent {
    /// A new link. The supervisor keeps a control handle and redials when the
    /// control channel ends.
    Linked(Link),
    /// A dial failed; the supervisor retries after `retry_in`.
    Failed {
        attempt: u32,
        reason: String,
        retry_in: Duration,
    },
    /// The link ended after `uptime`; the supervisor redials after `retry_in`.
    Lost {
        uptime: Duration,
        retry_in: Duration,
    },
}

/// Dials `addr` until the receiver is dropped: dial, hand over the link, wait
/// for its end, back off, dial again with a larger epoch.
pub fn spawn_dialer(
    node: NodeConfig,
    addr: SocketAddr,
    policy: BackoffPolicy,
) -> (mpsc::Receiver<DialerEvent>, JoinHandle<()>) {
    let (tx, rx) = mpsc::channel(16);
    let task = tokio::spawn(async move {
        let clock = EpochClock::default();
        let mut backoff = Backoff::new(policy);
        loop {
            match dial(&node, addr, clock.next()).await {
                Ok(link) => {
                    let control = link.control.clone();
                    let up = Instant::now();
                    if tx.send(DialerEvent::Linked(link)).await.is_err() {
                        return;
                    }
                    tokio::select! {
                        () = control.closed() => {}
                        () = tx.closed() => return,
                    }
                    let uptime = up.elapsed();
                    backoff.link_ended(uptime);
                    let retry_in = backoff.next_delay();
                    if tx
                        .send(DialerEvent::Lost { uptime, retry_in })
                        .await
                        .is_err()
                    {
                        return;
                    }
                    tokio::time::sleep(retry_in).await;
                }
                Err(e) => {
                    let attempt = backoff.attempt();
                    let retry_in = backoff.next_delay();
                    let reason = e.reason().map_or_else(|| e.to_string(), str::to_owned);
                    let event = DialerEvent::Failed {
                        attempt,
                        reason,
                        retry_in,
                    };
                    if tx.send(event).await.is_err() {
                        return;
                    }
                    tokio::time::sleep(retry_in).await;
                }
            }
        }
    });
    (rx, task)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy() -> BackoffPolicy {
        BackoffPolicy {
            base: Duration::from_millis(500),
            cap: Duration::from_secs(300),
            stable: Duration::from_secs(60),
        }
    }

    #[test]
    fn delays_grow_with_jitter_up_to_the_cap() {
        let mut b = Backoff::new(policy());
        assert_eq!(b.next_with(0.0), Duration::from_millis(250));
        assert_eq!(b.next_with(1.0), Duration::from_millis(1000));
        assert_eq!(b.next_with(0.5), Duration::from_millis(1500));
        for _ in 0..40 {
            let d = b.next_delay();
            assert!(d >= Duration::from_secs(150) || b.attempt() < 12, "{d:?}");
            assert!(d <= Duration::from_secs(300), "{d:?}");
        }
        assert_eq!(b.ceiling(), Duration::from_secs(300));
    }

    #[test]
    fn only_a_stable_link_resets_the_backoff() {
        let mut b = Backoff::new(policy());
        for _ in 0..5 {
            b.next_delay();
        }
        b.link_ended(Duration::from_secs(2));
        assert_eq!(
            b.attempt(),
            5,
            "a link that dropped at once keeps the backoff"
        );
        b.link_ended(Duration::from_secs(61));
        assert_eq!(b.attempt(), 0);
    }

    #[test]
    fn epochs_always_grow() {
        let c = EpochClock::default();
        let mut last = 0;
        for _ in 0..1000 {
            let e = c.next();
            assert!(e > last);
            last = e;
        }
    }
}
