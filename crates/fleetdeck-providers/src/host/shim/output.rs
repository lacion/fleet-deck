//! The shim's stdout buffer (ADR-005 §4.1, §4.2).
//!
//! Every stdout byte has an absolute offset from 0. The buffer holds every
//! byte at or above the acknowledged cursor (the *floor*), at most
//! `capacity` bytes. When it is full the shim stops reading the provider's
//! stdout ([`OutputBuffer::space`] is 0), so the provider blocks on its own
//! write and no byte is dropped. An `ack` frees the bytes below the cursor.
//!
//! A full buffer without a newline holds a line of the buffer size or more
//! ([`OutputBuffer::overlong_line`]): the session fails with
//! `output_frame_too_large` instead of waiting forever for an `ack`.

use std::collections::VecDeque;

use super::wire::GapReason;

/// The default buffer size (ADR-005 §4.1: 16 MiB).
pub const DEFAULT_CAPACITY: usize = 16 * 1024 * 1024;
/// The smallest and the largest buffer size that the station setting accepts.
pub const MIN_CAPACITY: usize = 1024 * 1024;
pub const MAX_CAPACITY: usize = 256 * 1024 * 1024;

/// What a `replay { from }` gives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Replay {
    /// A range below the floor that the shim no longer holds, reported
    /// before the bytes (`gap { from, to }`, ADR-005 §4.2).
    pub gap: Option<(u64, u64, GapReason)>,
    /// The offset of `bytes`.
    pub offset: u64,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum AckError {
    #[error("the cursor {cursor} is above the end of the output ({end})")]
    BeyondEnd { cursor: u64, end: u64 },
}

/// The stdout buffer of one provider.
#[derive(Debug)]
pub struct OutputBuffer {
    capacity: usize,
    /// The offset of `bytes[0]` (also the acknowledged cursor).
    floor: u64,
    bytes: VecDeque<u8>,
}

impl OutputBuffer {
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.clamp(1, MAX_CAPACITY),
            floor: 0,
            bytes: VecDeque::new(),
        }
    }

    /// The oldest byte that the buffer holds (`cursor_floor`).
    #[must_use]
    pub fn floor(&self) -> u64 {
        self.floor
    }

    /// The offset after the last byte.
    #[must_use]
    pub fn end(&self) -> u64 {
        self.floor + self.bytes.len() as u64
    }

    /// How many bytes the shim may read now. 0 means: do not read stdout.
    #[must_use]
    pub fn space(&self) -> usize {
        self.capacity.saturating_sub(self.bytes.len())
    }

    /// Stores bytes that the shim read. The caller reads at most
    /// [`Self::space`] bytes, so nothing is ever dropped; more is a defect,
    /// and only the part that fits is stored. Returns the offset of the
    /// first stored byte and the number of stored bytes.
    pub fn push(&mut self, bytes: &[u8]) -> (u64, usize) {
        let offset = self.end();
        let n = bytes.len().min(self.space());
        self.bytes.extend(&bytes[..n]);
        (offset, n)
    }

    /// Frees the bytes below `cursor`. A cursor at or below the floor
    /// changes nothing (a repeated or late `ack`).
    pub fn ack(&mut self, cursor: u64) -> Result<(), AckError> {
        let end = self.end();
        if cursor > end {
            return Err(AckError::BeyondEnd { cursor, end });
        }
        if cursor <= self.floor {
            return Ok(());
        }
        let n = usize::try_from(cursor - self.floor).unwrap_or(usize::MAX);
        self.bytes.drain(..n);
        self.floor = cursor;
        Ok(())
    }

    /// The bytes from `from` on. A `from` below the floor gives an explicit
    /// gap and the bytes from the floor; a `from` above the end gives no
    /// bytes. The shim never invents bytes.
    #[must_use]
    pub fn replay(&self, from: u64) -> Replay {
        let end = self.end();
        if from < self.floor {
            return Replay {
                gap: Some((from, self.floor, GapReason::BelowFloor)),
                offset: self.floor,
                bytes: self.bytes.iter().copied().collect(),
            };
        }
        let start = usize::try_from(from.min(end) - self.floor).unwrap_or(usize::MAX);
        Replay {
            gap: None,
            offset: from.min(end),
            bytes: self.bytes.iter().skip(start).copied().collect(),
        }
    }

    /// The range of a line that fills the whole buffer without a newline
    /// (ADR-005 §4.2), or `None`. The caller stops the provider and reports
    /// `gap { from, to, reason: output_frame_too_large }`.
    /// A full buffer that still holds a newline is ordinary backpressure: the
    /// station has complete lines to commit and acknowledge.
    #[must_use]
    pub fn overlong_line(&self) -> Option<(u64, u64)> {
        if self.space() > 0 || self.bytes.contains(&b'\n') {
            return None;
        }
        Some((self.floor, self.end()))
    }
}
