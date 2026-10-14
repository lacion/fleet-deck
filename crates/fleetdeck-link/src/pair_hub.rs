//! The pairing request window and the pending requests of a deck
//! (DL-178, tailnet design 5.1 and 5.4).
//!
//! The hub lives in the memory of the deck process. A restart closes the
//! window and ends every waiting request.
//!
//! - **Window.** The owner opens it for 10 minutes. Each opening is a new
//!   *generation* with a fixed deadline. It applies only to the tailnet
//!   link listeners of the deck.
//! - **Pending requests.** Each admitted request is bound to the generation
//!   that admitted it, and it expires at the earlier of 10 minutes and the
//!   deadline of that window. At most 16 wait together, 1 per proven key,
//!   2 per source, and 30 new ones in a minute. A denied key waits 10
//!   minutes before it can ask again.
//! - **One decision at a time.** Approve, deny, cancel and the window
//!   changes run under one async lock. An approval re-checks everything
//!   under that lock, commits through the [`PairApprover`], pins the key and
//!   only then wakes the connection that sends the answer (commit, then
//!   answer).

use std::collections::{BTreeSet, HashMap, VecDeque};
use std::fmt;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use fleetdeck_core::identity::{Fingerprint, PeerKey};
use fleetdeck_proto::link::Scope;
use tokio::sync::{Mutex as AsyncMutex, oneshot};
use tokio::time::Instant;

use crate::admission::source_key;
use crate::pair_request::{CanonicalRequest, pending_digest, reason};
use crate::pairing::hex;
use crate::tls::{InvitationGate, PinSet, Role};

/// The window and each request last 10 minutes.
pub const WINDOW_TTL: Duration = Duration::from_secs(10 * 60);
/// Requests that wait together.
pub const MAX_PENDING: usize = 16;
/// Pending requests of one proven key.
pub const MAX_PER_KEY: usize = 1;
/// Pending requests of one source.
pub const MAX_PER_SOURCE: usize = 2;
/// New requests in [`RATE_WINDOW`], all sources together.
pub const MAX_PER_MINUTE: usize = 30;
pub const RATE_WINDOW: Duration = Duration::from_secs(60);
/// A denied key can not ask again for this time.
pub const COOLDOWN: Duration = Duration::from_secs(10 * 60);
/// The deck pings a waiting connection this often.
pub const PING_EVERY: Duration = Duration::from_secs(15);
/// A waiting connection that sent no pong for this time closes.
pub const DEAD_AFTER: Duration = Duration::from_secs(45);

/// The limits of the hub. The defaults are the values of the design; tests
/// use shorter times.
#[derive(Debug, Clone)]
pub struct HubConfig {
    pub window_ttl: Duration,
    pub request_ttl: Duration,
    pub max_pending: usize,
    pub max_per_key: usize,
    pub max_per_source: usize,
    pub max_per_minute: usize,
    pub rate_window: Duration,
    pub cooldown: Duration,
    pub ping_every: Duration,
    pub dead_after: Duration,
}

impl Default for HubConfig {
    fn default() -> Self {
        Self {
            window_ttl: WINDOW_TTL,
            request_ttl: WINDOW_TTL,
            max_pending: MAX_PENDING,
            max_per_key: MAX_PER_KEY,
            max_per_source: MAX_PER_SOURCE,
            max_per_minute: MAX_PER_MINUTE,
            rate_window: RATE_WINDOW,
            cooldown: COOLDOWN,
            ping_every: PING_EVERY,
            dead_after: DEAD_AFTER,
        }
    }
}

/// The state of the window, in memory only.
#[derive(Debug, Clone, Copy)]
pub struct PairingWindow {
    pub generation: u64,
    pub deadline: Instant,
    pub open: bool,
}

/// The window as the board sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowView {
    pub open: bool,
    pub generation: u64,
    /// Unix milliseconds; `None` while the window is closed.
    pub deadline_ms: Option<i64>,
}

/// The record of one approval, kept by the deck for the retry of a lost
/// answer (`PairResume`). It never changes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Completion {
    pub request_id: String,
    pub station_key: PeerKey,
    pub deck_key: PeerKey,
    pub request_digest: [u8; 32],
    pub nonce: [u8; 32],
    pub requested_name: String,
    pub offered_scopes: BTreeSet<Scope>,
    pub granted_scopes: BTreeSet<Scope>,
    pub window_generation: u64,
    pub approved_at: i64,
}

/// Why a commit failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CommitError {
    #[error("the station key is already paired")]
    AlreadyPaired,
    #[error("a station is already named `{0}`")]
    NameTaken(String),
    #[error("the request id belongs to another approval")]
    RequestIdConflict,
    #[error("the pairing could not be stored: {0}")]
    Store(String),
}

/// The deck store of the hub: the pairing commit and the completion records.
#[async_trait::async_trait]
pub trait PairApprover: fmt::Debug + Send + Sync {
    /// In one transaction: the station key (pin and grants) and the
    /// completion record. A key that is already paired is refused and never
    /// changed.
    async fn commit(&self, completion: &Completion) -> Result<(), CommitError>;
    /// The completion record of `request_id`.
    async fn completion(&self, request_id: &str) -> Result<Option<Completion>, String>;
}

/// What a waiting connection learns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// The approval is committed; send `PairApproved`.
    Approved,
    /// Close with this reason (`denied`, `window_closed`, `cancelled`).
    Closed(&'static str),
}

/// Why a request is refused at admission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    WindowClosed,
    AlreadyPaired,
    Denied,
    RequestIdConflict,
    Busy,
    Store(String),
}

impl Refusal {
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::WindowClosed => reason::WINDOW_CLOSED,
            Self::AlreadyPaired => reason::ALREADY_PAIRED,
            Self::Denied => reason::DENIED,
            Self::RequestIdConflict => reason::REQUEST_ID_CONFLICT,
            Self::Busy => reason::BUSY,
            Self::Store(_) => "pairing_failed",
        }
    }
}

/// Why an approval or a denial is refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ApproveError {
    #[error("no such pending request")]
    UnknownRequest,
    #[error("the window that admitted the request is closed")]
    WindowClosed,
    #[error("the digest is not the digest of the request")]
    DigestMismatch,
    #[error("the fingerprint is not the key of the request")]
    FingerprintMismatch,
    #[error("the station key is already paired")]
    AlreadyPaired,
    #[error("a station is already named `{0}`")]
    NameTaken(String),
    #[error("the pairing could not be stored: {0}")]
    Store(String),
}

/// The window can not open without a tailnet link listener.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("the deck has no tailnet link listener running")]
pub struct NoTailnetListener;

/// A pending request as the board lists it.
#[derive(Debug, Clone, PartialEq)]
pub struct PendingView {
    pub request_id: String,
    pub requested_name: String,
    pub station_fingerprint: String,
    pub offered_scopes: Vec<Scope>,
    pub source_ip: IpAddr,
    pub admitted_at: i64,
    pub expires_at: i64,
    pub digest: String,
    /// The informational Tailscale identity, once fetched.
    pub identity: Option<serde_json::Value>,
}

/// An admitted request: the connection waits on `decision`.
#[derive(Debug)]
pub struct Admitted {
    pub id: String,
    pub decision: oneshot::Receiver<Decision>,
    pub expires: Instant,
}

/// The result of an approval.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Approved {
    pub station: PeerKey,
}

struct Pending {
    request: CanonicalRequest,
    key: PeerKey,
    ip: IpAddr,
    generation: u64,
    admitted_ms: i64,
    expires: Instant,
    expires_ms: i64,
    digest: [u8; 32],
    decide: Option<oneshot::Sender<Decision>>,
    identity: Option<serde_json::Value>,
}

impl Pending {
    fn is_gone(&self) -> bool {
        self.decide.as_ref().is_none_or(oneshot::Sender::is_closed)
    }
}

#[derive(Default)]
struct State {
    pending: HashMap<String, Pending>,
    recent: VecDeque<Instant>,
    cooldown: HashMap<Fingerprint, Instant>,
    /// The ids of requests that a window change closed, so that a late
    /// approval says `window_closed` and not `unknown_request`.
    closed: VecDeque<(String, Instant)>,
}

/// The most ids of closed requests that the hub remembers.
const MAX_CLOSED: usize = 64;

/// The window, the pending requests and the limits of one deck.
pub struct PairRequests {
    deck: PeerKey,
    granted: BTreeSet<Scope>,
    pins: Arc<PinSet>,
    approver: Arc<dyn PairApprover>,
    cfg: HubConfig,
    window: Mutex<PairingWindow>,
    listeners: Mutex<Vec<SocketAddr>>,
    state: AsyncMutex<State>,
}

impl fmt::Debug for PairRequests {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PairRequests")
            .field("window", &self.window())
            .finish_non_exhaustive()
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

fn ms_in(from_now: Duration) -> i64 {
    now_ms().saturating_add(i64::try_from(from_now.as_millis()).unwrap_or(i64::MAX))
}

impl PairRequests {
    /// A hub for the deck with key `deck`. `granted` is what an approved
    /// station gets on the deck; `pins` receives the approved key.
    #[must_use]
    pub fn new(
        deck: PeerKey,
        granted: BTreeSet<Scope>,
        pins: Arc<PinSet>,
        approver: Arc<dyn PairApprover>,
        cfg: HubConfig,
    ) -> Arc<Self> {
        Arc::new(Self {
            deck,
            granted,
            pins,
            approver,
            window: Mutex::new(PairingWindow {
                generation: 0,
                deadline: Instant::now(),
                open: false,
            }),
            listeners: Mutex::default(),
            state: AsyncMutex::new(State::default()),
            cfg,
        })
    }

    #[must_use]
    pub fn config(&self) -> &HubConfig {
        &self.cfg
    }

    #[must_use]
    pub fn deck_key(&self) -> PeerKey {
        self.deck
    }

    /// What the deck grants to an approved station.
    #[must_use]
    pub fn granted_scopes(&self) -> &BTreeSet<Scope> {
        &self.granted
    }

    #[must_use]
    pub fn approver(&self) -> &Arc<dyn PairApprover> {
        &self.approver
    }

    fn window_lock(&self) -> std::sync::MutexGuard<'_, PairingWindow> {
        self.window.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// True while the window is open and its deadline has not passed.
    #[must_use]
    pub fn is_open(&self) -> bool {
        let w = self.window_lock();
        w.open && Instant::now() < w.deadline
    }

    /// The deadline of the current window (tests).
    #[doc(hidden)]
    #[must_use]
    pub fn window_deadline(&self) -> Instant {
        self.window_lock().deadline
    }

    /// The window as the board sees it.
    #[must_use]
    pub fn window(&self) -> WindowView {
        let w = *self.window_lock();
        let now = Instant::now();
        if w.open && now < w.deadline {
            WindowView {
                open: true,
                generation: w.generation,
                deadline_ms: Some(ms_in(w.deadline - now)),
            }
        } else {
            WindowView {
                open: false,
                generation: w.generation,
                deadline_ms: None,
            }
        }
    }

    fn listeners_lock(&self) -> std::sync::MutexGuard<'_, Vec<SocketAddr>> {
        self.listeners.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Counts a tailnet link listener of the deck, bound to `addr`. It
    /// counts until the guard drops.
    #[must_use]
    pub fn add_listener(self: &Arc<Self>, addr: SocketAddr) -> ListenerGuard {
        self.listeners_lock().push(addr);
        ListenerGuard(Arc::clone(self), addr)
    }

    /// The running tailnet link listeners.
    #[must_use]
    pub fn tailnet_listeners(&self) -> usize {
        self.listeners_lock().len()
    }

    /// The bound addresses of the running tailnet link listeners, in the
    /// order they started.
    #[must_use]
    pub fn tailnet_addrs(&self) -> Vec<SocketAddr> {
        self.listeners_lock().clone()
    }

    /// Opens the window: a new generation with a new deadline. The requests
    /// of the old generation close with `window_closed`.
    ///
    /// # Errors
    /// [`NoTailnetListener`] when no tailnet link listener runs.
    pub async fn open_window(&self) -> Result<WindowView, NoTailnetListener> {
        if self.tailnet_listeners() == 0 {
            return Err(NoTailnetListener);
        }
        let mut st = self.state.lock().await;
        {
            let mut w = self.window_lock();
            w.generation += 1;
            w.deadline = Instant::now() + self.cfg.window_ttl;
            w.open = true;
        }
        Self::close_all(&mut st, Decision::Closed(reason::WINDOW_CLOSED));
        drop(st);
        Ok(self.window())
    }

    /// Closes the window. Every waiting request closes with
    /// `window_closed`. Returns whether the window was open.
    pub async fn close_window(&self) -> bool {
        let mut st = self.state.lock().await;
        let was_open = {
            let mut w = self.window_lock();
            let was = w.open && Instant::now() < w.deadline;
            w.open = false;
            was
        };
        Self::close_all(&mut st, Decision::Closed(reason::WINDOW_CLOSED));
        was_open
    }

    fn close_all(st: &mut State, decision: Decision) {
        let now = Instant::now();
        let window_change = decision == Decision::Closed(reason::WINDOW_CLOSED);
        for (id, mut p) in st.pending.drain().collect::<Vec<_>>() {
            if window_change {
                if st.closed.len() >= MAX_CLOSED {
                    st.closed.pop_front();
                }
                st.closed.push_back((id, now));
            }
            if let Some(tx) = p.decide.take() {
                let _ = tx.send(decision);
            }
        }
    }

    /// Ends every waiting request with `cancelled` (the deck shuts down).
    pub async fn cancel_all(&self) {
        let mut st = self.state.lock().await;
        Self::close_all(&mut st, Decision::Closed(reason::CANCELLED));
    }

    /// [`Self::cancel_all`] for a caller that can not wait; it skips the
    /// work when the lock is busy.
    pub fn cancel_all_now(&self) {
        if let Ok(mut st) = self.state.try_lock() {
            Self::close_all(&mut st, Decision::Closed(reason::CANCELLED));
        }
    }

    /// Ends the waiting request of `key` with `cancelled` (its station was
    /// removed).
    pub async fn cancel_key(&self, key: &PeerKey) {
        let mut st = self.state.lock().await;
        let ids: Vec<String> = st
            .pending
            .iter()
            .filter(|(_, p)| p.key == *key)
            .map(|(id, _)| id.clone())
            .collect();
        for id in ids {
            if let Some(mut p) = st.pending.remove(&id)
                && let Some(tx) = p.decide.take()
            {
                let _ = tx.send(Decision::Closed(reason::CANCELLED));
            }
        }
    }

    /// Admits a request or refuses it. `already_paired` tells that the
    /// proven key is a paired station.
    ///
    /// # Errors
    /// [`Refusal`], in this order: window closed, key already paired,
    /// cooldown, request id conflict, limits.
    pub async fn admit(
        &self,
        request: CanonicalRequest,
        key: PeerKey,
        ip: IpAddr,
        already_paired: bool,
    ) -> Result<Admitted, Refusal> {
        let id = request.id_hex();
        let digest = request.digest();
        let fp = key.fingerprint();
        let mut st = self.state.lock().await;
        let now = Instant::now();
        let (generation, deadline) = {
            let w = self.window_lock();
            if !(w.open && now < w.deadline) {
                return Err(Refusal::WindowClosed);
            }
            (w.generation, w.deadline)
        };
        if already_paired {
            return Err(Refusal::AlreadyPaired);
        }
        st.cooldown.retain(|_, until| *until > now);
        if st.cooldown.contains_key(&fp) {
            return Err(Refusal::Denied);
        }
        Self::sweep(&mut st, now);
        // A request id that this deck holds with another key or another
        // request is refused, also when only the stored approval holds it.
        if let Some(other) = st.pending.get(&id)
            && (other.key != key || other.request != request)
        {
            return Err(Refusal::RequestIdConflict);
        }
        match self.approver.completion(&id).await {
            Ok(Some(done)) if done.station_key != key || done.request_digest != digest => {
                return Err(Refusal::RequestIdConflict);
            }
            Ok(Some(_)) => return Err(Refusal::AlreadyPaired),
            Ok(None) => {}
            Err(e) => return Err(Refusal::Store(e)),
        }
        let per_key = st.pending.values().filter(|p| p.key == key).count();
        let source = source_key(ip);
        let per_source = st
            .pending
            .values()
            .filter(|p| source_key(p.ip) == source)
            .count();
        while st
            .recent
            .front()
            .is_some_and(|t| now.duration_since(*t) >= self.cfg.rate_window)
        {
            st.recent.pop_front();
        }
        if per_key >= self.cfg.max_per_key
            || per_source >= self.cfg.max_per_source
            || st.pending.len() >= self.cfg.max_pending
            || st.recent.len() >= self.cfg.max_per_minute
        {
            return Err(Refusal::Busy);
        }
        st.recent.push_back(now);
        let expires = (now + self.cfg.request_ttl).min(deadline);
        let (tx, rx) = oneshot::channel();
        let admitted_ms = now_ms();
        let record_digest = pending_digest(&digest, &fp, ip, admitted_ms);
        st.pending.insert(
            id.clone(),
            Pending {
                request,
                key,
                ip,
                generation,
                admitted_ms,
                expires,
                expires_ms: ms_in(expires - now),
                digest: record_digest,
                decide: Some(tx),
                identity: None,
            },
        );
        Ok(Admitted {
            id,
            decision: rx,
            expires,
        })
    }

    /// Removes `id` without a decision (the connection ended or expired).
    /// Returns whether the request was still waiting; `false`: a decision
    /// was made first.
    pub async fn finish(&self, id: &str) -> bool {
        self.state.lock().await.pending.remove(id).is_some()
    }

    /// Removes the requests that expired, and the ones whose connection is
    /// gone. An expired request closes with `expired`.
    fn sweep(st: &mut State, now: Instant) {
        let ids: Vec<String> = st
            .pending
            .iter()
            .filter(|(_, p)| p.is_gone() || p.expires <= now)
            .map(|(id, _)| id.clone())
            .collect();
        for id in ids {
            if let Some(mut p) = st.pending.remove(&id)
                && let Some(tx) = p.decide.take()
            {
                let _ = tx.send(Decision::Closed(reason::EXPIRED));
            }
        }
    }

    /// The waiting requests, oldest first.
    pub async fn list(&self) -> Vec<PendingView> {
        let mut st = self.state.lock().await;
        Self::sweep(&mut st, Instant::now());
        let mut out: Vec<PendingView> = st
            .pending
            .values()
            .map(|p| PendingView {
                request_id: p.request.id_hex(),
                requested_name: p.request.name.clone(),
                station_fingerprint: p.key.fingerprint().to_string(),
                offered_scopes: p.request.scopes_vec(),
                source_ip: p.ip.to_canonical(),
                admitted_at: p.admitted_ms,
                expires_at: p.expires_ms,
                digest: hex(&p.digest),
                identity: p.identity.clone(),
            })
            .collect();
        out.sort_by(|a, b| (a.admitted_at, &a.request_id).cmp(&(b.admitted_at, &b.request_id)));
        out
    }

    /// Keeps the informational identity of a pending request.
    pub async fn set_identity(&self, id: &str, identity: serde_json::Value) {
        if let Some(p) = self.state.lock().await.pending.get_mut(id) {
            p.identity = Some(identity);
        }
    }

    /// True when a pending request waits from `ip` (the devices list).
    pub async fn has_request_from(&self, ip: IpAddr) -> bool {
        let mut st = self.state.lock().await;
        Self::sweep(&mut st, Instant::now());
        let ip = ip.to_canonical();
        st.pending.values().any(|p| p.ip.to_canonical() == ip)
    }

    /// Approves request `id` for the key `fingerprint` and the digest of the
    /// pending record. All checks and the commit run under one lock.
    ///
    /// # Errors
    /// [`ApproveError`]. A failed commit leaves the request pending.
    pub async fn approve(
        &self,
        id: &str,
        fingerprint: &Fingerprint,
        digest: &[u8; 32],
    ) -> Result<Approved, ApproveError> {
        let mut st = self.state.lock().await;
        let now = Instant::now();
        Self::sweep(&mut st, now);
        let Some(p) = st.pending.get(id) else {
            let closed = st
                .closed
                .iter()
                .any(|(c, at)| c == id && now.duration_since(*at) < self.cfg.window_ttl);
            return Err(if closed {
                ApproveError::WindowClosed
            } else {
                ApproveError::UnknownRequest
            });
        };
        {
            let w = self.window_lock();
            if !(w.open && now < w.deadline && w.generation == p.generation) {
                return Err(ApproveError::WindowClosed);
            }
        }
        if !p.key.fingerprint().matches(fingerprint) {
            return Err(ApproveError::FingerprintMismatch);
        }
        if p.digest != *digest {
            return Err(ApproveError::DigestMismatch);
        }
        let completion = Completion {
            request_id: id.to_owned(),
            station_key: p.key,
            deck_key: self.deck,
            request_digest: p.request.digest(),
            nonce: p.request.nonce,
            requested_name: p.request.name.clone(),
            offered_scopes: p.request.offered_scopes.clone(),
            granted_scopes: self.granted.clone(),
            window_generation: p.generation,
            approved_at: now_ms(),
        };
        self.approver
            .commit(&completion)
            .await
            .map_err(|e| match e {
                CommitError::AlreadyPaired => ApproveError::AlreadyPaired,
                CommitError::NameTaken(n) => ApproveError::NameTaken(n),
                CommitError::RequestIdConflict => ApproveError::UnknownRequest,
                CommitError::Store(s) => ApproveError::Store(s),
            })?;
        self.pins.pin(completion.station_key, Role::Station);
        let station = completion.station_key;
        if let Some(mut p) = st.pending.remove(id)
            && let Some(tx) = p.decide.take()
        {
            // A connection that is gone misses the answer; the station
            // resumes with `PairResume`.
            let _ = tx.send(Decision::Approved);
        }
        Ok(Approved { station })
    }

    /// Denies request `id`; the key waits for the cooldown. Returns the key.
    ///
    /// # Errors
    /// [`ApproveError::UnknownRequest`].
    pub async fn deny(&self, id: &str) -> Result<PeerKey, ApproveError> {
        let mut st = self.state.lock().await;
        Self::sweep(&mut st, Instant::now());
        let Some(mut p) = st.pending.remove(id) else {
            return Err(ApproveError::UnknownRequest);
        };
        st.cooldown
            .insert(p.key.fingerprint(), Instant::now() + self.cfg.cooldown);
        if let Some(tx) = p.decide.take() {
            let _ = tx.send(Decision::Closed(reason::DENIED));
        }
        Ok(p.key)
    }
}

impl InvitationGate for PairRequests {
    fn has_open(&self) -> bool {
        self.is_open()
    }
}

/// Counts one tailnet link listener while it lives.
pub struct ListenerGuard(Arc<PairRequests>, SocketAddr);

impl Drop for ListenerGuard {
    fn drop(&mut self) {
        let mut all = self.0.listeners_lock();
        if let Some(at) = all.iter().position(|a| *a == self.1) {
            all.remove(at);
        }
    }
}

impl fmt::Debug for ListenerGuard {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ListenerGuard")
    }
}

#[cfg(test)]
#[path = "pair_hub_tests.rs"]
mod tests;
