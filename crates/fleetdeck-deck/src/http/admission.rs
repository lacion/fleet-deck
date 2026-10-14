//! Admission limits of the board before any login (FDV2-001, FDV2-002).
//!
//! - [`Admission`]: a global cap and a per-source cap on the open board
//!   connections. A connection takes its [`AdmitGuard`] at `accept`, before
//!   any TLS work, and holds it until the connection ends (also an upgraded
//!   WebSocket: [`Guarded`] owns the guard).
//! - [`LoginLimits`]: one failure limiter for each login flow
//!   ([`FlowLimiter`]), so a flood on one flow never locks the others, and
//!   one cap on the concurrent Argon2 verifications.
//!
//! A source is an IP address: IPv4-mapped IPv6 is the IPv4 address, and an
//! IPv6 address is its /64 prefix ([`source_key`]). Every map of sources has
//! at most [`MAX_KEYS`] keys.

use std::collections::HashMap;
use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use axum::extract::Extension;
use axum::http::StatusCode;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use super::guard::ApiError;

/// Most open board connections.
pub const BOARD_MAX_CONNECTIONS: usize = 256;
/// Most open board connections of one source.
pub const BOARD_MAX_PER_IP: usize = 32;
/// Most keys (sources) in one map.
pub const MAX_KEYS: usize = 4096;
/// Failures of one source in one window, for one login flow.
pub const MAX_FAILURES_PER_IP: usize = 5;
/// Failures of all sources in one window, for one login flow.
pub const MAX_FAILURES_GLOBAL: usize = 50;
/// The window of the failure limiters.
const WINDOW_MS: i64 = 60_000;
/// Argon2 verifications at the same time.
pub const HASH_PERMITS: usize = 2;
/// How long a login waits for a verification slot.
pub const HASH_WAIT: Duration = Duration::from_secs(2);
/// The wait that a refusal at the key cap reports, in seconds.
const CAP_WAIT_S: i64 = 60;
/// At most one log line for refused connections in this time.
const REFUSAL_LOG_EVERY: Duration = Duration::from_secs(10);

/// The peer address of a board connection, an extension of each request.
#[derive(Debug, Clone, Copy)]
pub struct PeerAddr(pub SocketAddr);

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

/// The source key of a request. A request without [`PeerAddr`] (a test that
/// calls the router directly) is the unspecified IPv4 address.
#[must_use]
pub fn request_source(peer: Option<Extension<PeerAddr>>) -> IpAddr {
    peer.map_or(IpAddr::V4(Ipv4Addr::UNSPECIFIED), |Extension(p)| {
        source_key(p.0.ip())
    })
}

/// The admission of board connections.
#[derive(Debug)]
pub struct Admission {
    global: Arc<Semaphore>,
    per_ip: Arc<Mutex<HashMap<IpAddr, usize>>>,
    last_refusal_log: Mutex<Option<Instant>>,
}

impl Default for Admission {
    fn default() -> Self {
        Self {
            global: Arc::new(Semaphore::new(BOARD_MAX_CONNECTIONS)),
            per_ip: Arc::default(),
            last_refusal_log: Mutex::default(),
        }
    }
}

impl Admission {
    /// Admits a connection from `ip`, or `None` when the global cap, the cap
    /// of the source or the cap of the keys is full.
    #[must_use]
    pub fn try_admit(&self, ip: IpAddr) -> Option<AdmitGuard> {
        let key = source_key(ip);
        let mut map = self.per_ip.lock().unwrap_or_else(PoisonError::into_inner);
        let count = map.get(&key).copied();
        if count.is_some_and(|c| c >= BOARD_MAX_PER_IP) {
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

    /// The number of live guards (open connections).
    #[doc(hidden)]
    #[must_use]
    pub fn admitted(&self) -> usize {
        BOARD_MAX_CONNECTIONS - self.global.available_permits()
    }

    /// True at most once in [`REFUSAL_LOG_EVERY`]: the caller may log.
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

/// One admitted connection. It frees its place when it drops.
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

/// A stream that owns an [`AdmitGuard`]. After a WebSocket upgrade hyper
/// ends the connection task and hands the stream to the upgrade, so the
/// guard lives in the stream and not in the task.
#[derive(Debug)]
pub struct Guarded<S> {
    inner: S,
    _guard: AdmitGuard,
}

impl<S> Guarded<S> {
    pub fn new(inner: S, guard: AdmitGuard) -> Self {
        Self {
            inner,
            _guard: guard,
        }
    }
}

impl<S: AsyncRead + Unpin> AsyncRead for Guarded<S> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for Guarded<S> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }

    fn poll_write_vectored(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write_vectored(cx, bufs)
    }

    fn is_write_vectored(&self) -> bool {
        self.inner.is_write_vectored()
    }
}

/// One failure record of a window.
#[derive(Debug)]
struct Entry {
    id: u64,
    at: i64,
}

#[derive(Debug, Default)]
struct Windows {
    next_id: u64,
    per_ip: HashMap<IpAddr, Vec<Entry>>,
    global: Vec<Entry>,
}

impl Windows {
    /// Drops the records that left the window, and the sources without any.
    fn prune(&mut self, now: i64) {
        let live = |e: &Entry| e.at > now - WINDOW_MS;
        self.global.retain(live);
        for v in self.per_ip.values_mut() {
            v.retain(live);
        }
        self.per_ip.retain(|_, v| !v.is_empty());
    }

    /// The seconds to wait when a window of `entries` is full.
    fn wait_of(entries: &[Entry], max: usize, now: i64) -> Option<i64> {
        if entries.len() < max {
            return None;
        }
        let oldest = entries.iter().map(|e| e.at).min()?;
        Some((oldest + WINDOW_MS - now) / 1000 + 1)
    }

    /// The wait when a window of the source or the global one is full.
    fn locked(&self, key: IpAddr, now: i64) -> Option<i64> {
        let per_ip = self
            .per_ip
            .get(&key)
            .and_then(|v| Self::wait_of(v, MAX_FAILURES_PER_IP, now));
        let global = Self::wait_of(&self.global, MAX_FAILURES_GLOBAL, now);
        per_ip.max(global)
    }

    fn remove(&mut self, key: IpAddr, id: u64) {
        self.global.retain(|e| e.id != id);
        if let Some(v) = self.per_ip.get_mut(&key) {
            v.retain(|e| e.id != id);
            if v.is_empty() {
                self.per_ip.remove(&key);
            }
        }
    }

    /// Moves the record `id` to the time `at`.
    fn retime(&mut self, key: IpAddr, id: u64, at: i64) {
        let entries = self
            .global
            .iter_mut()
            .chain(self.per_ip.get_mut(&key).into_iter().flatten())
            .filter(|e| e.id == id);
        for e in entries {
            e.at = at;
        }
    }
}

/// The failure limiter of one login flow: per source at most
/// [`MAX_FAILURES_PER_IP`] failures in a minute, and for all sources at most
/// [`MAX_FAILURES_GLOBAL`]. An attempt that is still running counts as a
/// failure, so concurrent requests never start more verifications than the
/// window allows.
#[derive(Debug, Default, Clone)]
pub struct FlowLimiter(Arc<Mutex<Windows>>);

impl FlowLimiter {
    fn windows(&self) -> std::sync::MutexGuard<'_, Windows> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The seconds to wait, or `None` when an attempt may start.
    #[must_use]
    pub fn locked(&self, ip: IpAddr, now: i64) -> Option<i64> {
        let key = source_key(ip);
        let mut w = self.windows();
        w.prune(now);
        w.locked(key, now)
    }

    /// Starts an attempt of `ip`: checks the windows (running attempts
    /// count) and reserves one place in them.
    ///
    /// # Errors
    /// The seconds to wait: a window is full, or the map holds [`MAX_KEYS`]
    /// other sources.
    pub fn begin(&self, ip: IpAddr, now: i64) -> Result<Attempt, i64> {
        let key = source_key(ip);
        let mut w = self.windows();
        w.prune(now);
        if let Some(wait) = w.locked(key, now) {
            return Err(wait);
        }
        if !w.per_ip.contains_key(&key) && w.per_ip.len() >= MAX_KEYS {
            return Err(CAP_WAIT_S);
        }
        let id = w.next_id;
        w.next_id += 1;
        let entry = || Entry { id, at: now };
        w.global.push(entry());
        w.per_ip.entry(key).or_default().push(entry());
        drop(w);
        Ok(Attempt {
            limiter: Arc::clone(&self.0),
            key,
            id,
        })
    }

    /// The failure records of `ip` and of all sources (tests).
    #[doc(hidden)]
    #[must_use]
    pub fn counts(&self, ip: IpAddr, now: i64) -> (usize, usize) {
        let key = source_key(ip);
        let mut w = self.windows();
        w.prune(now);
        (w.per_ip.get(&key).map_or(0, Vec::len), w.global.len())
    }

    /// The sources that the limiter holds (tests).
    #[doc(hidden)]
    #[must_use]
    pub fn keys(&self) -> usize {
        self.windows().per_ip.len()
    }
}

/// One login attempt that [`FlowLimiter::begin`] reserved. The reservation
/// is a failure record from the start, so an attempt that is dropped without
/// an outcome (a request that the client cancelled) stays a failure.
#[derive(Debug)]
#[must_use = "an attempt that is dropped counts as a failure"]
pub struct Attempt {
    limiter: Arc<Mutex<Windows>>,
    key: IpAddr,
    id: u64,
}

impl Attempt {
    fn windows(&self) -> std::sync::MutexGuard<'_, Windows> {
        self.limiter.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The credentials were valid: the reservation goes away.
    pub fn succeeded(self) {
        self.windows().remove(self.key, self.id);
    }

    /// Nothing was verified (the deck was busy, an internal error): the
    /// reservation goes away without a failure.
    pub fn release(self) {
        self.windows().remove(self.key, self.id);
    }

    /// The credentials were not valid: the reservation becomes a failure at
    /// `now`.
    pub fn failed(self, now: i64) {
        self.windows().retime(self.key, self.id, now);
    }
}

/// The limits of the login flows (FDV2-002). The device link keeps its own
/// limiter ([`super::DeviceLinkLimiter`]).
#[derive(Debug)]
pub struct LoginLimits {
    /// The password login and the elevation (a password or a code).
    pub password: FlowLimiter,
    pub login_link: FlowLimiter,
    pub member_join: FlowLimiter,
    hashing: Arc<Semaphore>,
    /// Permits taken so far (tests count these, not Argon2 calls).
    #[doc(hidden)]
    pub verifications_started: AtomicUsize,
    /// The most permits held at once (tests).
    #[doc(hidden)]
    pub verifications_peak: AtomicUsize,
}

impl Default for LoginLimits {
    fn default() -> Self {
        Self {
            password: FlowLimiter::default(),
            login_link: FlowLimiter::default(),
            member_join: FlowLimiter::default(),
            hashing: Arc::new(Semaphore::new(HASH_PERMITS)),
            verifications_started: AtomicUsize::new(0),
            verifications_peak: AtomicUsize::new(0),
        }
    }
}

impl LoginLimits {
    /// Takes a verification permit; `None` when none is free within
    /// [`HASH_WAIT`].
    pub async fn hash_permit(&self) -> Option<OwnedSemaphorePermit> {
        let permit = tokio::time::timeout(HASH_WAIT, Arc::clone(&self.hashing).acquire_owned())
            .await
            .ok()?
            .ok()?;
        self.verifications_started.fetch_add(1, Ordering::SeqCst);
        let held = HASH_PERMITS - self.hashing.available_permits();
        self.verifications_peak.fetch_max(held, Ordering::SeqCst);
        Some(permit)
    }

    /// The verification semaphore (a test holds permits to make a slow
    /// verification).
    #[doc(hidden)]
    #[must_use]
    pub fn hash_permits(&self) -> Arc<Semaphore> {
        Arc::clone(&self.hashing)
    }
}

/// `429 too_many_attempts` with the wait in seconds.
#[must_use]
pub fn too_many_attempts(wait: i64) -> ApiError {
    ApiError::new(
        StatusCode::TOO_MANY_REQUESTS,
        "too_many_attempts",
        format!("too many failed logins; try again in {wait} s"),
    )
}

/// `429 busy`: no verification slot was free in time.
#[must_use]
pub fn busy() -> ApiError {
    ApiError::new(
        StatusCode::TOO_MANY_REQUESTS,
        "busy",
        "the deck is busy; try again in a moment",
    )
}
