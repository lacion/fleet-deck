//! The shim's stdin write records (ADR-005 §4.3).
//!
//! One record per write identity (`write_id`, `write_seq`), created before
//! the first byte goes to the pipe. Admission refuses, without I/O:
//! - an absent identity whose sequence is at or below `evicted_through`
//!   (`forgotten`);
//! - a sequence that another record or tombstone holds, and a known
//!   identity with another sequence (`mismatch`).
//!
//! A known identity with its own sequence is a duplicate: it is not written
//! again. One write is active at a time; at most [`MAX_PENDING_WRITES`]
//! writes and [`MAX_PENDING_BYTES`] bytes wait. A takeover makes the waiting
//! writes of older generations `failed_unwritten { superseded }`; the active
//! write continues. Terminal records are evicted after [`MAX_RECORDS`];
//! eviction raises `evicted_through` and never removes it.

use std::collections::{HashMap, VecDeque};

use tokio::io::{AsyncWrite, AsyncWriteExt};

use super::wire::{StatusAnswer, WriteState};

pub const MAX_PENDING_WRITES: usize = 256;
pub const MAX_PENDING_BYTES: usize = 4 * 1024 * 1024;
/// The terminal records and tombstones that the shim keeps.
pub const MAX_RECORDS: usize = 4096;

/// The error text of a capacity refusal.
pub const CAPACITY: &str = "capacity";
/// The error text of a waiting write cancelled by a takeover.
pub const SUPERSEDED: &str = "superseded";

#[derive(Debug)]
struct Record {
    write_seq: u64,
    generation: u64,
    state: WriteState,
    /// The bytes of a waiting write.
    bytes: Option<Vec<u8>>,
}

/// What admission did with a `write` frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Admission {
    /// A new record; the write waits for its turn.
    Queued,
    /// A known identity with its own sequence: no second write.
    Duplicate(WriteState),
    /// The shim refused it for capacity; no byte goes to the pipe.
    Refused(WriteState),
    Forgotten,
    Mismatch,
}

/// The write records of one shim.
#[derive(Debug, Default)]
pub struct WriteTable {
    records: HashMap<String, Record>,
    by_seq: HashMap<u64, String>,
    waiting: VecDeque<String>,
    active: Option<String>,
    pending_bytes: usize,
    /// The sequences of terminal records, oldest first.
    terminal: VecDeque<u64>,
    evicted_through: u64,
}

impl WriteTable {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The highest sequence that the shim ever evicted.
    #[must_use]
    pub fn evicted_through(&self) -> u64 {
        self.evicted_through
    }

    /// The writes that wait or run.
    #[must_use]
    pub fn pending(&self) -> usize {
        self.waiting.len() + usize::from(self.active.is_some())
    }

    /// The state of a known identity.
    #[must_use]
    pub fn state(&self, write_id: &str) -> Option<&WriteState> {
        self.records.get(write_id).map(|r| &r.state)
    }

    /// Admission of a complete `write` frame of the current generation.
    pub fn admit(
        &mut self,
        generation: u64,
        write_id: &str,
        write_seq: u64,
        bytes: Vec<u8>,
    ) -> Admission {
        if let Some(record) = self.records.get(write_id) {
            return if record.write_seq == write_seq {
                Admission::Duplicate(record.state.clone())
            } else {
                Admission::Mismatch
            };
        }
        if write_seq <= self.evicted_through {
            return Admission::Forgotten;
        }
        if self.by_seq.contains_key(&write_seq) {
            return Admission::Mismatch;
        }
        if self.waiting.len() >= MAX_PENDING_WRITES
            || self.pending_bytes.saturating_add(bytes.len()) > MAX_PENDING_BYTES
        {
            let state = WriteState::FailedUnwritten {
                error: CAPACITY.to_owned(),
            };
            self.insert(write_id, write_seq, generation, state.clone(), None);
            return Admission::Refused(state);
        }
        self.pending_bytes += bytes.len();
        self.insert(
            write_id,
            write_seq,
            generation,
            WriteState::Received,
            Some(bytes),
        );
        self.waiting.push_back(write_id.to_owned());
        Admission::Queued
    }

    /// The next write to run, if none runs. It becomes `writing { 0 }`.
    pub fn start_next(&mut self) -> Option<(String, Vec<u8>)> {
        if self.active.is_some() {
            return None;
        }
        while let Some(id) = self.waiting.pop_front() {
            let Some(record) = self.records.get_mut(&id) else {
                continue;
            };
            let Some(bytes) = record.bytes.take() else {
                continue;
            };
            self.pending_bytes = self.pending_bytes.saturating_sub(bytes.len());
            record.state = WriteState::Writing { bytes_done: 0 };
            self.active = Some(id.clone());
            return Some((id, bytes));
        }
        None
    }

    /// The bytes of the active write that are in the pipe.
    pub fn progress(&mut self, write_id: &str, bytes_done: u64) {
        if let Some(record) = self.records.get_mut(write_id)
            && matches!(record.state, WriteState::Writing { .. })
        {
            record.state = WriteState::Writing { bytes_done };
        }
    }

    /// The end of the active write. Returns its sequence for `write_done`.
    pub fn finish(&mut self, write_id: &str, state: WriteState) -> Option<u64> {
        debug_assert!(state.is_terminal());
        if self.active.as_deref() == Some(write_id) {
            self.active = None;
        }
        let record = self.records.get_mut(write_id)?;
        if record.state.is_terminal() {
            return None;
        }
        record.state = state;
        let seq = record.write_seq;
        self.terminal.push_back(seq);
        self.evict();
        Some(seq)
    }

    /// A takeover by `generation`: the waiting writes of older generations
    /// become `failed_unwritten { superseded }`. Returns their identities.
    pub fn takeover(&mut self, generation: u64) -> Vec<(String, u64)> {
        let mut cancelled = Vec::new();
        let waiting = std::mem::take(&mut self.waiting);
        for id in waiting {
            let Some(record) = self.records.get_mut(&id) else {
                continue;
            };
            if record.generation >= generation {
                self.waiting.push_back(id);
                continue;
            }
            if let Some(bytes) = record.bytes.take() {
                self.pending_bytes = self.pending_bytes.saturating_sub(bytes.len());
            }
            record.state = WriteState::FailedUnwritten {
                error: SUPERSEDED.to_owned(),
            };
            let seq = record.write_seq;
            self.terminal.push_back(seq);
            cancelled.push((id, seq));
        }
        self.evict();
        cancelled
    }

    /// The answer to `write_status` (ADR-005 §4.3). `not_received` stores a
    /// `closed_unwritten` tombstone in the same step, so the frame can never
    /// be written later.
    pub fn status(&mut self, write_id: &str, write_seq: u64) -> StatusAnswer {
        if let Some(record) = self.records.get(write_id) {
            return if record.write_seq == write_seq {
                StatusAnswer::Known {
                    state: record.state.clone(),
                }
            } else {
                StatusAnswer::Mismatch
            };
        }
        if write_seq <= self.evicted_through {
            return StatusAnswer::Forgotten;
        }
        if self.by_seq.contains_key(&write_seq) {
            return StatusAnswer::Mismatch;
        }
        self.insert(write_id, write_seq, 0, WriteState::ClosedUnwritten, None);
        StatusAnswer::NotReceived
    }

    fn insert(
        &mut self,
        write_id: &str,
        write_seq: u64,
        generation: u64,
        state: WriteState,
        bytes: Option<Vec<u8>>,
    ) {
        let terminal = state.is_terminal();
        self.records.insert(
            write_id.to_owned(),
            Record {
                write_seq,
                generation,
                state,
                bytes,
            },
        );
        self.by_seq.insert(write_seq, write_id.to_owned());
        if terminal {
            self.terminal.push_back(write_seq);
            self.evict();
        }
    }

    fn evict(&mut self) {
        while self.terminal.len() > MAX_RECORDS {
            let Some(seq) = self.terminal.pop_front() else {
                break;
            };
            if let Some(id) = self.by_seq.remove(&seq) {
                self.records.remove(&id);
            }
            self.evicted_through = self.evicted_through.max(seq);
        }
    }
}

/// Writes `bytes` to the provider's stdin and reports the bytes in the pipe
/// after each call. A pipe has no user-space buffer, so a successful
/// `write` call puts its bytes in the pipe.
pub async fn write_tracked<W>(w: &mut W, bytes: &[u8], mut progress: impl FnMut(u64)) -> WriteState
where
    W: AsyncWrite + Unpin,
{
    let mut done = 0usize;
    while done < bytes.len() {
        let error = match w.write(&bytes[done..]).await {
            Ok(0) => std::io::Error::from(std::io::ErrorKind::WriteZero),
            Ok(n) => {
                done += n;
                progress(done as u64);
                continue;
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => e,
        };
        return if done == 0 {
            WriteState::FailedUnwritten {
                error: error.to_string(),
            }
        } else {
            WriteState::Partial {
                bytes_done: done as u64,
                error: error.to_string(),
            }
        };
    }
    WriteState::Written
}
