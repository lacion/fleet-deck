//! The lifecycle of the Tailscale certificate of the board (DL-190,
//! certificate design section 2.5).
//!
//! One task does every issuance, renewal, storage and publication:
//!
//! - It follows the live setting `tailscale_cert`. Off: nothing runs.
//! - At start a stored file that passes the checks of use is served at once,
//!   before any call of `tailscale`.
//! - An attempt asks `tailscale status` for the name of this machine, asks
//!   `tailscale cert` for a certificate, checks it (admission), stores it and
//!   swaps it into the resolver.
//! - A **generation** counter ends a run that the setting or the shutdown
//!   made stale. A run checks it under one lock before the file rename and
//!   again before the swap; a stale run drops its result.
//!
//! No text here holds output of `tailscale`: only reason codes.

use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, PoisonError, RwLock};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use fleetdeck_core::clock::SharedClock;
use fleetdeck_core::tailscale::{CertError, TailscaleCli, TailscaleError};
use rustls::pki_types::UnixTime;
use tokio::sync::{Mutex, watch};
use tokio::task::JoinHandle;

use crate::http::tls_tailscale::{
    ADMISSION_REMAINING, Anchors, BoardResolver, Stored, TailscaleCert, check, parse_pem,
    read_stored, remove_staging, remove_tailscale, write_tailscale,
};
use crate::live::LiveCell;

/// The longest run of `tailscale cert`.
pub const ISSUE_DEADLINE: Duration = Duration::from_secs(90);

/// The waits of the loop.
#[derive(Debug, Clone, Copy)]
pub struct Intervals {
    /// After an attempt that worked.
    pub normal: Duration,
    /// After a failed attempt, and while the certificate is near its end.
    pub retry: Duration,
    /// The time left under which the wait is `retry`.
    pub near_end: Duration,
}

impl Intervals {
    /// Every 12 h; every 1 h after a failure or with fewer than 14 days left.
    pub const PRODUCTION: Self = Self {
        normal: Duration::from_secs(12 * 60 * 60),
        retry: Duration::from_secs(60 * 60),
        near_end: Duration::from_secs(14 * 24 * 60 * 60),
    };
}

/// The wait after an attempt. `remaining` is the time left of the served
/// certificate. The loop also wakes when that time ends.
#[must_use]
pub fn next_wait(failed: bool, remaining: Option<Duration>, iv: &Intervals) -> Duration {
    let near = remaining.is_some_and(|r| r < iv.near_end);
    let mut wait = if failed || near { iv.retry } else { iv.normal };
    if let Some(left) = remaining.filter(|r| !r.is_zero()) {
        wait = wait.min(left + Duration::from_millis(1));
    }
    wait
}

/// What the task shows about the certificate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CertStateKind {
    Off,
    Valid,
    Renewing,
    Failed,
    Expired,
    NameMismatch,
}

impl CertStateKind {
    /// The name on the wire.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Valid => "valid",
            Self::Renewing => "renewing",
            Self::Failed => "failed",
            Self::Expired => "expired",
            Self::NameMismatch => "name_mismatch",
        }
    }
}

/// The state of the certificate. A `valid` certificate can have a
/// `reason_code`: the last renewal failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CertState {
    pub enabled: bool,
    pub state: CertStateKind,
    pub name: Option<String>,
    pub not_after_ms: Option<i64>,
    pub reason_code: Option<&'static str>,
}

impl CertState {
    #[must_use]
    pub fn off() -> Self {
        Self {
            enabled: false,
            state: CertStateKind::Off,
            name: None,
            not_after_ms: None,
            reason_code: None,
        }
    }
}

/// The calls of `tailscale` that the task needs (a test gives its own).
#[async_trait]
pub trait Issuer: Send + Sync {
    /// The PEM output of `tailscale cert` for `domain` (chain and key).
    async fn issue(&self, domain: &str) -> Result<Vec<u8>, CertError>;

    /// The DNS name of this machine in `tailscale status`.
    async fn self_name(&self) -> Result<String, CertError>;
}

/// The real `tailscale`, chosen like the other calls of the deck.
#[derive(Debug, Clone)]
pub struct TailscaleIssuer {
    live: LiveCell,
}

impl TailscaleIssuer {
    /// `live` holds the deck setting `tailscale_bin`, read at each call.
    #[must_use]
    pub fn new(live: LiveCell) -> Self {
        Self { live }
    }

    async fn cli(&self) -> Result<TailscaleCli, CertError> {
        let configured = self.live.get().tailscale_bin.clone();
        // The lookup reads files: off the async threads.
        tokio::task::spawn_blocking(move || TailscaleCli::locate(configured.as_deref()))
            .await
            .map_err(|_| CertError::Failed)
    }
}

#[async_trait]
impl Issuer for TailscaleIssuer {
    async fn issue(&self, domain: &str) -> Result<Vec<u8>, CertError> {
        let cli = self.cli().await?;
        cli.cert(domain, Instant::now() + ISSUE_DEADLINE).await
    }

    async fn self_name(&self) -> Result<String, CertError> {
        let cli = self.cli().await?;
        match cli.status().await {
            Ok(status) => Ok(status.this.dns_name),
            Err(TailscaleError::NotInstalled) => Err(CertError::NotInstalled),
            Err(TailscaleError::Timeout) => Err(CertError::Timeout),
            Err(TailscaleError::TooLarge) => Err(CertError::TooLarge),
            Err(_) => Err(CertError::Failed),
        }
    }
}

/// Everything the task needs.
pub struct CertDeps {
    pub live: LiveCell,
    pub resolver: Arc<BoardResolver>,
    /// The deck data directory (the file `board-tailscale.pem`).
    pub dir: PathBuf,
    /// The deck setting `tailnet_hostname`: the name of the certificate.
    pub hostname: Option<String>,
    pub issuer: Arc<dyn Issuer>,
    /// The time of the validity checks and of the state.
    pub clock: SharedClock,
    pub anchors: Anchors,
    pub intervals: Intervals,
    /// The state cell: the creator keeps a clone to read it.
    pub state: Arc<RwLock<CertState>>,
}

/// The running task and its state.
#[derive(Debug)]
pub struct CertTask {
    handle: JoinHandle<()>,
    state: Arc<RwLock<CertState>>,
}

impl CertTask {
    /// The state now.
    #[must_use]
    pub fn state(&self) -> CertState {
        self.state
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// The shared state, for a reader that outlives this handle.
    #[must_use]
    pub fn state_cell(&self) -> Arc<RwLock<CertState>> {
        Arc::clone(&self.state)
    }

    /// Waits for the task to end (after the shutdown signal).
    pub async fn join(self) {
        let _ = self.handle.await;
    }
}

/// Starts the task. When the setting is on and a stored file passes the
/// checks of use, it is served before this function returns. The task stops
/// when `shutdown` turns true.
pub fn spawn(deps: CertDeps, shutdown: watch::Receiver<bool>) -> CertTask {
    let enabled = deps.live.get().tailscale_cert;
    // A temporary file from a write that did not end holds a key (DL-190).
    if let Err(e) = remove_staging(&deps.dir) {
        tracing::warn!(error = %e, "the temporary Tailscale file was not removed");
    }
    let core = Arc::new(Core {
        state: Arc::clone(&deps.state),
        generation: AtomicU64::new(0),
        served: Mutex::new(None),
        deps,
    });
    if enabled {
        core.set(|s| {
            s.enabled = true;
            s.state = CertStateKind::Renewing;
        });
        match core.served.try_lock() {
            Ok(mut served) => core.start_serving(&mut served),
            Err(_) => tracing::warn!("the stored Tailscale certificate was not read"),
        }
    }
    let state = Arc::clone(&core.state);
    let handle = tokio::spawn(run(core, enabled, shutdown));
    CertTask { handle, state }
}

struct Core {
    deps: CertDeps,
    state: Arc<RwLock<CertState>>,
    generation: AtomicU64,
    /// The one lock of the publication. It holds the end (Unix ms) of the
    /// certificate that this task knows: served, or over.
    served: Mutex<Option<i64>>,
}

/// Why an attempt did not give a certificate.
enum Fail {
    Mismatch,
    Code(&'static str),
    /// The setting or the shutdown made the run stale: change nothing.
    Stale,
}

enum Commit {
    Done,
    Stale,
    StoreFailed,
}

type Attempt = Pin<Box<dyn std::future::Future<Output = Duration> + Send>>;

async fn run(core: Arc<Core>, enabled: bool, mut shutdown: watch::Receiver<bool>) {
    let notify = core.deps.live.changed();
    let mut enabled = enabled;
    let mut program = core.deps.live.get().tailscale_bin.clone();
    let mut next: Option<tokio::time::Instant> = enabled.then(tokio::time::Instant::now);
    let mut running: Option<Attempt> = None;
    loop {
        let due = next.unwrap_or_else(tokio::time::Instant::now);
        tokio::select! {
            wait = async {
                match running.as_mut() {
                    Some(attempt) => attempt.await,
                    None => std::future::pending().await,
                }
            }, if running.is_some() => {
                running = None;
                next = Some(tokio::time::Instant::now() + wait);
            }
            () = tokio::time::sleep_until(due), if running.is_none() && next.is_some() => {
                next = None;
                let generation = core.generation.load(Ordering::SeqCst);
                running = Some(Box::pin(Arc::clone(&core).attempt(generation)));
            }
            () = notify.notified() => {
                let live = core.deps.live.get();
                let want = live.tailscale_cert;
                let changed = want != enabled;
                if changed {
                    enabled = want;
                    // A run that is going is cancelled; a result that
                    // still arrives is stale by the generation.
                    running = None;
                    if want {
                        core.enable().await;
                        next = Some(tokio::time::Instant::now());
                    } else {
                        core.turn_off().await;
                        next = None;
                    }
                }
                // A new program ends the run that uses the old one. The
                // served certificate stays until the next attempt.
                if live.tailscale_bin != program {
                    program.clone_from(&live.tailscale_bin);
                    if want && !changed {
                        core.generation.fetch_add(1, Ordering::SeqCst);
                        running = None;
                        next = Some(tokio::time::Instant::now());
                    }
                }
            }
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    core.generation.fetch_add(1, Ordering::SeqCst);
                    return;
                }
            }
        }
    }
}

impl Core {
    fn now_ms(&self) -> i64 {
        i64::try_from(self.deps.clock.now_ms()).unwrap_or(i64::MAX)
    }

    fn now(&self) -> UnixTime {
        UnixTime::since_unix_epoch(Duration::from_millis(self.deps.clock.now_ms()))
    }

    fn set(&self, f: impl FnOnce(&mut CertState)) {
        f(&mut self.state.write().unwrap_or_else(PoisonError::into_inner));
    }

    fn stale(&self, generation: u64) -> bool {
        self.generation.load(Ordering::SeqCst) != generation
    }

    /// The certificate name: the deck's `tailnet_hostname`, lower case.
    fn hostname(&self) -> Option<String> {
        self.deps
            .hostname
            .as_deref()
            .map(str::to_ascii_lowercase)
            .filter(|h| h.ends_with(".ts.net"))
    }

    /// Serves the stored file when it passes the checks of use. Makes no
    /// call of `tailscale`.
    fn start_serving(&self, served: &mut Option<i64>) {
        let Some(host) = self.hostname() else {
            return;
        };
        match read_stored(&self.deps.dir, &host, self.now(), &self.deps.anchors) {
            Stored::Valid(cert) => match cert.served() {
                Ok(pair) => {
                    self.deps.resolver.set_tailscale(Some(pair));
                    *served = Some(cert.not_after_ms);
                    self.set(|s| {
                        s.state = CertStateKind::Valid;
                        s.name = Some(cert.name.clone());
                        s.not_after_ms = Some(cert.not_after_ms);
                    });
                }
                Err(e) => tracing::warn!(code = e.code(), "the stored Tailscale certificate"),
            },
            Stored::Expired(end) => {
                *served = Some(end);
                self.set(|s| {
                    s.state = CertStateKind::Expired;
                    s.name = Some(host);
                    s.not_after_ms = Some(end);
                });
            }
            Stored::Missing => {}
        }
    }

    /// The setting turned on while the deck runs.
    async fn enable(&self) {
        self.generation.fetch_add(1, Ordering::SeqCst);
        let mut served = self.served.lock().await;
        *served = None;
        self.set(|s| {
            *s = CertState::off();
            s.enabled = true;
            s.state = CertStateKind::Renewing;
        });
        self.start_serving(&mut served);
    }

    /// The setting turned off: the generation first, then the resolver, then
    /// the file.
    async fn turn_off(&self) {
        self.generation.fetch_add(1, Ordering::SeqCst);
        let mut served = self.served.lock().await;
        self.deps.resolver.set_tailscale(None);
        *served = None;
        if let Err(e) = remove_tailscale(&self.deps.dir) {
            tracing::warn!(error = %e, "the stored Tailscale certificate was not removed");
        }
        self.set(|s| *s = CertState::off());
    }

    /// One attempt. Returns the wait before the next one.
    async fn attempt(self: Arc<Self>, generation: u64) -> Duration {
        self.refresh_expiry().await;
        self.mark_renewing().await;
        let result = self.try_issue(generation).await;
        let failed = match result {
            Ok(()) => false,
            Err(Fail::Stale) => return self.deps.intervals.retry,
            Err(Fail::Mismatch) => {
                self.set(|s| {
                    s.state = CertStateKind::NameMismatch;
                    s.name = self.deps.hostname.as_deref().map(str::to_ascii_lowercase);
                    s.reason_code = None;
                });
                true
            }
            Err(Fail::Code(code)) => {
                self.record_failure(code).await;
                true
            }
        };
        let remaining = self
            .served
            .lock()
            .await
            .map(|end| Duration::from_millis(u64::try_from(end - self.now_ms()).unwrap_or(0)));
        next_wait(failed, remaining, &self.deps.intervals)
    }

    async fn try_issue(&self, generation: u64) -> Result<(), Fail> {
        let host = self.hostname().ok_or(Fail::Mismatch)?;
        let name = self
            .deps
            .issuer
            .self_name()
            .await
            .map_err(|e| Fail::Code(e.code()))?;
        if name.trim_end_matches('.').to_ascii_lowercase() != host {
            return Err(Fail::Mismatch);
        }
        let pem = self
            .deps
            .issuer
            .issue(&host)
            .await
            .map_err(|e| Fail::Code(e.code()))?;
        let (chain, key) = parse_pem(&pem).map_err(|e| Fail::Code(e.code()))?;
        let cert = check(
            chain,
            key,
            &host,
            self.now(),
            ADMISSION_REMAINING,
            &self.deps.anchors,
        )
        .map_err(|e| Fail::Code(e.code()))?;
        match self.commit(generation, &cert).await {
            Commit::Done => Ok(()),
            Commit::Stale => Err(Fail::Stale),
            Commit::StoreFailed => Err(Fail::Code("store_failed")),
        }
    }

    /// Stores and serves a checked certificate. Under one lock, the
    /// generation is checked before the rename and again before the swap.
    async fn commit(&self, generation: u64, cert: &TailscaleCert) -> Commit {
        let mut served = self.served.lock().await;
        if self.stale(generation) {
            return Commit::Stale;
        }
        if let Err(e) = write_tailscale(&self.deps.dir, cert) {
            tracing::warn!(error = %e, "the Tailscale certificate was not stored");
            return Commit::StoreFailed;
        }
        if self.stale(generation) {
            return Commit::Stale;
        }
        let Ok(pair) = cert.served() else {
            return Commit::StoreFailed;
        };
        self.deps.resolver.set_tailscale(Some(pair));
        *served = Some(cert.not_after_ms);
        self.set(|s| {
            s.state = CertStateKind::Valid;
            s.name = Some(cert.name.clone());
            s.not_after_ms = Some(cert.not_after_ms);
            s.reason_code = None;
        });
        Commit::Done
    }

    /// A certificate that reached its end is `expired`.
    async fn refresh_expiry(&self) {
        let served = self.served.lock().await;
        if served.is_some_and(|end| end <= self.now_ms()) {
            self.set(|s| s.state = CertStateKind::Expired);
        }
    }

    /// A certificate in use is `renewing` while an attempt runs.
    async fn mark_renewing(&self) {
        let served = self.served.lock().await;
        let live = served.is_some_and(|end| end > self.now_ms());
        self.set(|s| {
            if live || s.state != CertStateKind::Expired {
                s.state = CertStateKind::Renewing;
            }
        });
    }

    /// The attempt failed: the certificate in use stays, with the reason.
    async fn record_failure(&self, code: &'static str) {
        let served = self.served.lock().await;
        let now = self.now_ms();
        self.set(|s| {
            s.state = match *served {
                Some(end) if end > now => CertStateKind::Valid,
                Some(_) => CertStateKind::Expired,
                None => CertStateKind::Failed,
            };
            s.reason_code = Some(code);
        });
        tracing::warn!(code, "the Tailscale certificate was not renewed");
    }
}

#[cfg(test)]
#[path = "cert_task_tests.rs"]
mod tests;
