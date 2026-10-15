//! The last `status` frame of a connection (DL-194 §9.7): the end of the
//! shim's buffered output, for the replay gate of the station.

use std::sync::Mutex;

/// The `end` of the last `status` frame received, `None` before the first.
#[derive(Debug, Default)]
pub(super) struct StatusEnd(Mutex<Option<u64>>);

impl StatusEnd {
    pub(super) fn set(&self, end: u64) {
        let mut slot = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *slot = Some(end);
    }

    pub(super) fn get(&self) -> Option<u64> {
        *self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
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
}
