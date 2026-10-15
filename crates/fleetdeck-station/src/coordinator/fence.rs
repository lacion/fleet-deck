//! The write-ahead invalidation fence of the shim host (DL-194 §2.3, §9.13).
//!
//! `<station dir>/run/bindings.fence` names the `(shim_id, digest)` pairs
//! whose invalidation is not yet in a `shim_bindings` row: an owner revoke
//! or a rotation by resume. The station writes the entry before the row
//! change, so a crash between the two leaves the binding refused at the next
//! start. Every write publishes the complete content with an atomic rename.
//! The file holds digests only, never a token.

use std::collections::BTreeSet;
use std::fs::OpenOptions;
use std::io::{ErrorKind, Write as _};
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};
use std::path::PathBuf;
use std::sync::Mutex;

use crate::store::Store;
use crate::store::shim_bindings::RowState;

/// The first line of the fence file.
pub const FENCE_HEADER: &str = "fleetdeck-fence 1";
/// The most entries that the fence holds.
pub const MAX_FENCE_ENTRIES: usize = 4096;

const FILE: &str = "bindings.fence";
const TMP: &str = "bindings.fence.tmp";

/// One entry: the shim id and the credential digest.
pub type Entry = (String, String);

/// What `read` found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FenceState {
    Missing,
    Entries(BTreeSet<Entry>),
    Unreadable,
}

/// Why a fence write failed.
#[derive(Debug, thiserror::Error)]
pub enum FenceError {
    #[error("the fence is full")]
    Full,
    #[error("the fence write failed: {0}")]
    Io(#[from] std::io::Error),
}

/// The fence file of one station.
#[derive(Debug)]
pub struct Fence {
    dir: PathBuf,
    /// One writer at a time (DL-194 §2.3 step 2).
    lock: Mutex<()>,
    /// Entries of revokes whose row change has to be confirmed before the
    /// entry is removed ([`Fence::settle`]).
    pending: Mutex<Vec<Entry>>,
}

fn valid_shim_id(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

fn valid_digest(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

fn parse(text: &str) -> Option<BTreeSet<Entry>> {
    let mut lines = text.lines();
    if lines.next()? != FENCE_HEADER {
        return None;
    }
    let mut out = BTreeSet::new();
    for line in lines {
        let (shim, digest) = line.split_once(' ')?;
        if !valid_shim_id(shim) || !valid_digest(digest) {
            return None;
        }
        out.insert((shim.to_owned(), digest.to_owned()));
        if out.len() > MAX_FENCE_ENTRIES {
            return None;
        }
    }
    Some(out)
}

impl Fence {
    /// The fence in `run_dir` (`<station dir>/run`).
    #[must_use]
    pub fn new(run_dir: PathBuf) -> Self {
        Self {
            dir: run_dir,
            lock: Mutex::new(()),
            pending: Mutex::new(Vec::new()),
        }
    }

    fn path(&self) -> PathBuf {
        self.dir.join(FILE)
    }

    /// The content of the file: `Missing` when it does not exist,
    /// `Unreadable` for any other read error, a bad header, a bad line or
    /// more than [`MAX_FENCE_ENTRIES`] entries.
    #[must_use]
    pub fn read(&self) -> FenceState {
        match std::fs::read(self.path()) {
            Ok(bytes) => String::from_utf8(bytes)
                .ok()
                .and_then(|text| parse(&text))
                .map_or(FenceState::Unreadable, FenceState::Entries),
            Err(e) if e.kind() == ErrorKind::NotFound => FenceState::Missing,
            Err(e) => {
                tracing::warn!(kind = ?e.kind(), "the binding fence cannot be read");
                FenceState::Unreadable
            }
        }
    }

    fn guard(&self) -> std::sync::MutexGuard<'_, ()> {
        self.lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Creates the fence with the header only when it is missing.
    ///
    /// # Errors
    /// A write error.
    pub fn ensure(&self) -> Result<(), FenceError> {
        let _guard = self.guard();
        if self.read() == FenceState::Missing {
            self.publish(&BTreeSet::new(), None)?;
        }
        Ok(())
    }

    /// Adds `entries` to the file. `op` (`revoke`, `rotate`) is the label
    /// of the `after_fence` fault point.
    ///
    /// # Errors
    /// [`FenceError::Full`] above the limit (nothing written), or a write
    /// error. An unreadable file is an `InvalidData` error: the old entries
    /// would be lost, so nothing is written.
    pub fn add(&self, entries: &[Entry], op: &str) -> Result<(), FenceError> {
        if entries
            .iter()
            .any(|(shim, digest)| !valid_shim_id(shim) || !valid_digest(digest))
        {
            // Such a line would make the whole file unreadable.
            return Err(FenceError::Io(std::io::Error::new(
                ErrorKind::InvalidData,
                "the fence entry is malformed",
            )));
        }
        let _guard = self.guard();
        let mut all = self.current()?;
        all.extend(entries.iter().cloned());
        if all.len() > MAX_FENCE_ENTRIES {
            return Err(FenceError::Full);
        }
        self.publish(&all, Some(op))
    }

    /// Removes `entries` from the file. A missing file stays missing.
    ///
    /// # Errors
    /// A write error, or an unreadable file (`InvalidData`).
    pub fn remove(&self, entries: &[Entry]) -> Result<(), FenceError> {
        let _guard = self.guard();
        if self.read() == FenceState::Missing {
            return Ok(());
        }
        let mut all = self.current()?;
        let before = all.len();
        for e in entries {
            all.remove(e);
        }
        if all.len() != before {
            self.publish(&all, None)?;
        }
        Ok(())
    }

    /// Rewrites the file with the header only. The start order calls it
    /// after it suspended every row of an unreadable fence; a rewrite is
    /// not a delete.
    ///
    /// # Errors
    /// A write error.
    pub fn reset(&self) -> Result<(), FenceError> {
        let _guard = self.guard();
        self.publish(&BTreeSet::new(), None)
    }

    /// Deletes a leftover `bindings.fence.tmp`: its rename did not happen,
    /// so its content was never in effect.
    pub fn clean_tmp(&self) {
        if let Err(e) = std::fs::remove_file(self.dir.join(TMP))
            && e.kind() != ErrorKind::NotFound
        {
            tracing::warn!(kind = ?e.kind(), "the binding fence tmp file was not removed");
        }
    }

    /// Notes an entry whose row change has to be confirmed.
    pub fn mark_settle(&self, entry: Entry) {
        let mut pending = self
            .pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !pending.contains(&entry) {
            pending.push(entry);
        }
    }

    /// Removes the pending entries whose row is `ended`, `suspended` or
    /// missing (step 4 of §2.3). An entry whose row is still open stays in
    /// the file and in the list. A failed removal leaves the entry, which
    /// is harmless.
    pub async fn settle(&self, store: &Store) {
        let pending: Vec<Entry> = self
            .pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        if pending.is_empty() {
            return;
        }
        let Some(done) = settled(store, pending).await else {
            return;
        };
        if done.is_empty() {
            return;
        }
        if let Err(e) = self.remove(&done) {
            tracing::warn!(error = %e, "the binding fence entry was not removed");
            return;
        }
        self.pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retain(|e| !done.contains(e));
    }

    /// Removes, in one write, every entry of the file whose row is
    /// `ended`, `suspended` or missing. The start order calls it after the
    /// rows were suspended and the rows of other shims deleted. A failure
    /// is logged.
    pub async fn prune(&self, store: &Store) {
        let FenceState::Entries(entries) = self.read() else {
            return;
        };
        if entries.is_empty() {
            return;
        }
        let Some(done) = settled(store, entries.into_iter().collect()).await else {
            return;
        };
        if done.is_empty() {
            return;
        }
        if let Err(e) = self.remove(&done) {
            tracing::warn!(error = %e, "binding fence entries were not removed");
        }
    }

    /// The entries that a write starts from.
    fn current(&self) -> Result<BTreeSet<Entry>, FenceError> {
        match self.read() {
            FenceState::Missing => Ok(BTreeSet::new()),
            FenceState::Entries(entries) => Ok(entries),
            FenceState::Unreadable => Err(FenceError::Io(std::io::Error::new(
                ErrorKind::InvalidData,
                "the fence cannot be read",
            ))),
        }
    }

    /// The atomic write: tmp file (0600), fsync, rename, directory fsync.
    /// `fault` is the label of the `after_fence` point, right after the
    /// rename.
    fn publish(&self, entries: &BTreeSet<Entry>, fault: Option<&str>) -> Result<(), FenceError> {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&self.dir)?;
        let mut text = String::from(FENCE_HEADER);
        text.push('\n');
        for (shim, digest) in entries {
            text.push_str(shim);
            text.push(' ');
            text.push_str(digest);
            text.push('\n');
        }
        let tmp = self.dir.join(TMP);
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .mode(0o600)
            .open(&tmp)?;
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&tmp, self.path())?;
        if let Some(op) = fault {
            crate::fault::hit(crate::fault::FaultPoint::AfterFence, op);
        }
        std::fs::File::open(&self.dir)?.sync_all()?;
        Ok(())
    }
}

/// The entries whose row is `ended`, `suspended` or missing. `None` when
/// the rows cannot be read (logged).
async fn settled(store: &Store, entries: Vec<Entry>) -> Option<Vec<Entry>> {
    let rows = match store.shim_bindings_load().await {
        Ok(rows) => rows,
        Err(e) => {
            tracing::warn!(error = %e, "the binding fence cannot be settled");
            return None;
        }
    };
    Some(
        entries
            .into_iter()
            .filter(|(_, digest)| {
                rows.iter()
                    .find(|r| &r.digest == digest)
                    .is_none_or(|r| matches!(r.state, RowState::Ended | RowState::Suspended))
            })
            .collect(),
    )
}

#[cfg(test)]
#[path = "fence_tests.rs"]
mod tests;
