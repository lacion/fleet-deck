//! Admission of pending link handshakes (FDV2-001). A listener takes a
//! permit at `accept`, before it spawns a task or does TLS work, and the
//! permit lives until `accept_one` returns. An established link holds none.
//! This is a small copy of the board's admission in the deck crate; the link
//! crate does not depend on the deck.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv6Addr};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use tokio::sync::{OwnedSemaphorePermit, Semaphore};

/// Most pending handshakes on one listener.
pub const LINK_MAX_PREAUTH: usize = 64;
/// Most pending handshakes of one source.
pub const LINK_MAX_PREAUTH_PER_IP: usize = 16;
/// Most sources in the map.
pub const MAX_KEYS: usize = 4096;
/// At most one log line for refused connections in this time.
const REFUSAL_LOG_EVERY: Duration = Duration::from_secs(10);

/// The key of a source: IPv4-mapped IPv6 is the IPv4 address, and an IPv6
/// address is its /64 prefix.
#[must_use]
pub fn source_key(ip: IpAddr) -> IpAddr {
    match ip.to_canonical() {
        IpAddr::V6(v6) => {
            let s = v6.segments();
            IpAddr::V6(Ipv6Addr::new(s[0], s[1], s[2], s[3], 0, 0, 0, 0))
        }
        v4 => v4,
    }
}

/// The admission of pending handshakes of one listener.
#[derive(Debug)]
pub struct Admission {
    global: Arc<Semaphore>,
    per_ip: Arc<Mutex<HashMap<IpAddr, usize>>>,
    last_refusal_log: Mutex<Option<Instant>>,
}

impl Default for Admission {
    fn default() -> Self {
        Self {
            global: Arc::new(Semaphore::new(LINK_MAX_PREAUTH)),
            per_ip: Arc::default(),
            last_refusal_log: Mutex::default(),
        }
    }
}

impl Admission {
    /// Admits a handshake from `ip`, or `None` when the listener cap, the cap
    /// of the source or the cap of the keys is full.
    #[must_use]
    pub fn try_admit(&self, ip: IpAddr) -> Option<AdmitGuard> {
        let key = source_key(ip);
        let mut map = self.per_ip.lock().unwrap_or_else(PoisonError::into_inner);
        let count = map.get(&key).copied();
        if count.is_some_and(|c| c >= LINK_MAX_PREAUTH_PER_IP) {
            return None;
        }
        if count.is_none() && map.len() >= MAX_KEYS {
            return None;
        }
        let permit = Arc::clone(&self.global).try_acquire_owned().ok()?;
        *map.entry(key).or_insert(0) += 1;
        Some(AdmitGuard {
            _permit: permit,
            per_ip: Arc::clone(&self.per_ip),
            key,
        })
    }

    /// The number of live guards (pending handshakes).
    #[must_use]
    pub fn admitted(&self) -> usize {
        LINK_MAX_PREAUTH - self.global.available_permits()
    }

    /// True at most once in 10 s: the caller may log a refusal.
    #[must_use]
    pub fn should_log_refusal(&self) -> bool {
        let mut last = self
            .last_refusal_log
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let now = Instant::now();
        if last.is_some_and(|t| now.duration_since(t) < REFUSAL_LOG_EVERY) {
            return false;
        }
        *last = Some(now);
        true
    }
}

/// One admitted handshake. It frees its place when it drops.
#[derive(Debug)]
pub struct AdmitGuard {
    _permit: OwnedSemaphorePermit,
    per_ip: Arc<Mutex<HashMap<IpAddr, usize>>>,
    key: IpAddr,
}

impl Drop for AdmitGuard {
    fn drop(&mut self) {
        let mut map = self.per_ip.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(count) = map.get_mut(&self.key) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                map.remove(&self.key);
            }
        }
    }
}
