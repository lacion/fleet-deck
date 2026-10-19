//! The deck's setup reports of unpaired keys (BG-3, design
//! `docs/v2/design/board-gaps.md` section 4): the reporting window and the
//! sink that the link node calls for each `ProvisionReport`.
//!
//! The reporting window is in memory only. It lasts one hour, is off by
//! default, and is separate from the pairing window: opening one never opens
//! the other. The rows are in the store (`store/provision_reports.rs`). This
//! module adds the global limit of 120 accepted updates in a sliding hour.
//!
//! The order of the checks is: the window, the global limit, the rate of a
//! known key, the capacity. A store error is logged and answered as
//! `rate_limited` with 600 seconds; the report is not stored then.

use std::collections::VecDeque;
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use fleetdeck_core::identity::Fingerprint;
use fleetdeck_link::hello::ReportStatus;
use fleetdeck_link::report_accept::{ReportReply, ReportSink, ValidReport};
use ring::rand::SecureRandom;

use crate::store::DeckStore;
use crate::store::provision_reports::{Applied, Outcome};

/// The reporting window lasts one hour.
pub const WINDOW_TTL: Duration = Duration::from_secs(60 * 60);

/// The most accepted report updates in a sliding hour, all keys together.
pub const MAX_UPDATES_PER_HOUR: usize = 120;

/// The sliding hour of the global limit, in milliseconds.
const HOUR_MS: i64 = 60 * 60 * 1000;

/// The seconds that `stored` and `disabled` tell the reporter to wait, and
/// that a store error answers.
pub const RETRY_AFTER_DEFAULT: u64 = 600;

/// The seconds that `capacity` tells the reporter to wait.
pub const RETRY_AFTER_CAPACITY: u64 = 3600;

/// The codes that a retry can repair (`board-gaps.md` section 4.5). The
/// other codes have no board repair.
#[must_use]
pub fn retry_allowed(code: &str) -> bool {
    matches!(
        code,
        "denied"
            | "cancelled"
            | "revoked"
            | "pending"
            | "pairing_failed"
            | "provider_probe_failed"
            | "integration_failed"
            | "unknown_error"
    )
}

/// A new retry id: 32 lowercase hex digits (128 random bits). `None` when
/// the system random generator fails.
#[must_use]
pub fn new_retry_id() -> Option<String> {
    let mut bytes = [0u8; 16];
    ring::rand::SystemRandom::new().fill(&mut bytes).ok()?;
    Some(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// The reporting window as the board sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowView {
    pub open: bool,
    pub generation: u64,
    /// Unix milliseconds; `None` while the window is closed.
    pub deadline_ms: Option<i64>,
}

#[derive(Debug, Clone, Copy, Default)]
struct Window {
    /// Unix milliseconds of the deck store clock; 0 while closed.
    deadline_ms: i64,
    generation: u64,
}

/// The reporting window, the global limit and the store of the deck.
pub struct ProvisionReports {
    store: DeckStore,
    window: Mutex<Window>,
    /// The Unix times of the accepted updates in the sliding hour. The lock
    /// is held from the limit check to the count, so two reports never pass
    /// the limit together.
    updates: tokio::sync::Mutex<VecDeque<i64>>,
}

impl std::fmt::Debug for ProvisionReports {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProvisionReports").finish_non_exhaustive()
    }
}

impl ProvisionReports {
    /// A closed window over `store`.
    #[must_use]
    pub fn new(store: DeckStore) -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self {
            store,
            window: Mutex::new(Window::default()),
            updates: tokio::sync::Mutex::default(),
        })
    }

    fn window_lock(&self) -> std::sync::MutexGuard<'_, Window> {
        self.window.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The window now.
    #[must_use]
    pub fn window(&self) -> WindowView {
        let now = self.store.now_ms();
        let w = *self.window_lock();
        let open = w.deadline_ms > now;
        WindowView {
            open,
            generation: w.generation,
            deadline_ms: open.then_some(w.deadline_ms),
        }
    }

    /// Opens the window for one hour (a new generation).
    pub fn open_window(&self) -> WindowView {
        let ttl = i64::try_from(WINDOW_TTL.as_millis()).unwrap_or(HOUR_MS);
        {
            let mut w = self.window_lock();
            w.generation += 1;
            w.deadline_ms = self.store.now_ms().saturating_add(ttl);
        }
        self.window()
    }

    /// Closes the window.
    pub fn close_window(&self) -> WindowView {
        self.window_lock().deadline_ms = 0;
        self.window()
    }
}

fn plain(status: ReportStatus, seconds: u64) -> ReportReply {
    ReportReply::plain(status, seconds)
}

#[async_trait::async_trait]
impl ReportSink for ProvisionReports {
    fn is_open(&self) -> bool {
        self.window().open
    }

    async fn report(&self, peer: Fingerprint, report: ValidReport, now_ms: i64) -> ReportReply {
        if !self.is_open() {
            return plain(ReportStatus::Disabled, RETRY_AFTER_DEFAULT);
        }
        let mut updates = self.updates.lock().await;
        while updates.front().is_some_and(|t| *t <= now_ms - HOUR_MS) {
            updates.pop_front();
        }
        if updates.len() >= MAX_UPDATES_PER_HOUR {
            let oldest = updates.front().copied().unwrap_or(now_ms);
            let left_ms = u64::try_from((oldest + HOUR_MS - now_ms).max(0)).unwrap_or(0);
            let seconds = left_ms.div_ceil(1000).max(1);
            return plain(ReportStatus::RateLimited, seconds);
        }
        let (terminal_id, state_digest) = (report.terminal_id.clone(), report.state_digest.clone());
        let applied = self
            .store
            .apply_provision_report(peer.to_string(), report, now_ms)
            .await;
        let Applied { outcome, counted } = match applied {
            Ok(a) => a,
            Err(e) => {
                tracing::warn!(error = %e, "a setup report could not be stored");
                return plain(ReportStatus::RateLimited, RETRY_AFTER_DEFAULT);
            }
        };
        if counted {
            updates.push_back(now_ms);
        }
        match outcome {
            Outcome::Stored => plain(ReportStatus::Stored, RETRY_AFTER_DEFAULT),
            Outcome::RateLimited(seconds) => plain(ReportStatus::RateLimited, seconds),
            Outcome::Capacity => plain(ReportStatus::Capacity, RETRY_AFTER_CAPACITY),
            Outcome::Retry { retry_id } => ReportReply::retry(retry_id, terminal_id, state_digest),
        }
    }
}

#[cfg(test)]
#[path = "provision_reports_tests.rs"]
mod tests;
