//! The replay ring of the runner: the newest 256 KiB of the PTY output.

use std::collections::VecDeque;

/// The size of the ring.
pub const RING_BYTES: usize = 256 * 1024;

/// A byte ring that keeps the newest bytes.
#[derive(Debug)]
pub struct Ring {
    cap: usize,
    buf: VecDeque<u8>,
}

impl Default for Ring {
    fn default() -> Self {
        Self::with_capacity(RING_BYTES)
    }
}

impl Ring {
    /// A ring of `cap` bytes.
    #[must_use]
    pub fn with_capacity(cap: usize) -> Self {
        Self {
            cap,
            buf: VecDeque::new(),
        }
    }

    /// Appends `bytes`. When the ring is full, the oldest bytes go.
    pub fn push(&mut self, bytes: &[u8]) {
        let keep = if bytes.len() > self.cap {
            &bytes[bytes.len() - self.cap..]
        } else {
            bytes
        };
        let over = (self.buf.len() + keep.len()).saturating_sub(self.cap);
        self.buf.drain(..over);
        self.buf.extend(keep);
    }

    /// The bytes of the ring, oldest first.
    #[must_use]
    pub fn snapshot(&self) -> Vec<u8> {
        self.buf.iter().copied().collect()
    }
}
