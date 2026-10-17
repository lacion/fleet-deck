//! The last `status` frame of a connection (DL-194 §9.7): the end of the
//! shim's buffered output, for the replay gate of the station.

use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

/// The `end` of the last `status` frame received, `None` before the first,
/// and the end of the last complete line of the delivered output.
#[derive(Debug, Default)]
pub(super) struct StatusEnd {
    end: Mutex<Option<u64>>,
    line_end: AtomicU64,
}

impl StatusEnd {
    pub(super) fn set(&self, end: u64) {
        let mut slot = self
            .end
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *slot = Some(end);
    }

    pub(super) fn get(&self) -> Option<u64> {
        *self
            .end
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Records the offset after the last newline of the delivered output
    /// (DL-194 §9.7). The sink calls it before it publishes `delivered`.
    pub(super) fn note_line_end(&self, line_end: u64) {
        self.line_end.fetch_max(line_end, Ordering::SeqCst);
    }

    pub(super) fn line_end(&self) -> u64 {
        self.line_end.load(Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests {
    use super::StatusEnd;

    #[test]
    fn the_status_end_is_none_before_the_first_frame_then_the_last_end() {
        let s = StatusEnd::default();
        assert_eq!(s.get(), None);
        s.set(10);
        s.set(25);
        assert_eq!(s.get(), Some(25));
    }

    #[test]
    fn the_line_end_only_grows() {
        let s = StatusEnd::default();
        assert_eq!(s.line_end(), 0);
        s.note_line_end(12);
        s.note_line_end(7);
        assert_eq!(s.line_end(), 12);
    }
}
