//! Analytics (B29): the parsers of the provider transcripts and the scan.
//! The query is in `query` (design §3.7).

use std::sync::{Arc, Mutex, OnceLock};

use fleetdeck_proto::analytics::SourceState;
use fleetdeck_proto::event::ProviderKind;

pub mod claude;
pub mod codex;
pub mod query;
pub mod scan;
mod walk;

#[cfg(test)]
mod claude_tests;
#[cfg(test)]
mod codex_tests;
#[cfg(test)]
mod query_tests;
#[cfg(test)]
mod scan_tests;

/// What the last pass knows of one provider home (design section 3.5 step 7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HomeStatus {
    pub provider: ProviderKind,
    /// `AnalyticsSource.key`.
    pub key: String,
    pub state: SourceState,
    pub verified: bool,
    /// No byte is left to read and the walk was not truncated.
    pub complete: bool,
    /// Bytes on disk that no pass read yet.
    pub pending_bytes: u64,
    /// Unix ms of the end of the last pass that saw this home.
    pub scanned_at: Option<i64>,
    /// Claude only: `cleanupPeriodDays` of the home.
    pub keeps_days: Option<u32>,
}

/// The state of the scanner, in memory, written by the scan and read by the
/// query. Before the first pass ends it holds no home.
#[derive(Debug)]
pub struct ScanStatus {
    homes: Mutex<Vec<HomeStatus>>,
    /// One analytics query at a time per station (design section 3.7 step 2).
    query: tokio::sync::Semaphore,
}

impl Default for ScanStatus {
    fn default() -> Self {
        Self {
            homes: Mutex::new(Vec::new()),
            query: tokio::sync::Semaphore::new(1),
        }
    }
}

impl ScanStatus {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The scan status of this station process (one scanner per station).
    #[must_use]
    pub fn shared() -> Arc<ScanStatus> {
        static SHARED: OnceLock<Arc<ScanStatus>> = OnceLock::new();
        Arc::clone(SHARED.get_or_init(|| Arc::new(ScanStatus::new())))
    }

    /// The homes of the last pass.
    #[must_use]
    pub fn homes(&self) -> Vec<HomeStatus> {
        self.homes.lock().map(|h| h.clone()).unwrap_or_default()
    }

    /// The slot of the one running query.
    pub(crate) fn query_gate(&self) -> &tokio::sync::Semaphore {
        &self.query
    }

    pub(crate) fn set(&self, homes: Vec<HomeStatus>) {
        if let Ok(mut guard) = self.homes.lock() {
            *guard = homes;
        }
    }
}

/// One counted API response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageRecord {
    pub at: i64,
    pub model: String,
    /// Claude `sessionId`; Codex: the thread of the file (the store maps it
    /// to the root thread, §3.5 step 4).
    pub conversation: String,
    /// The text of the dedupe key; `None`: no key (counted without dedupe).
    pub key: Option<String>,
    pub input: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub output: u64,
    pub reasoning: u64,
}

/// What a parser makes of one line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Parsed {
    Record(UsageRecord),
    Nothing,
    Malformed,
}

/// The cheap gate of the parsers: does the line hold these bytes?
pub(crate) fn has_bytes(line: &[u8], needle: &[u8]) -> bool {
    let Some((&first, _)) = needle.split_first() else {
        return true;
    };
    let mut from = 0;
    while let Some(at) = line[from..].iter().position(|&b| b == first) {
        let start = from + at;
        if line[start..].starts_with(needle) {
            return true;
        }
        from = start + 1;
    }
    false
}
