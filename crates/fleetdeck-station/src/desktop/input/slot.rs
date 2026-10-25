//! The one input slot of the station (R13 P7 §7.3 step 3, §9.3): a second
//! input operation is refused at once with `busy`. Nothing queues.

use std::sync::atomic::{AtomicBool, Ordering};

use fleetdeck_proto::coordinator_desktop_text::code as p6_code;

use crate::desktop::Fail;
use crate::desktop::coordinator::p6_fail;

/// The slot. One per [`crate::desktop::Desktop`], so one per station.
#[derive(Debug, Default)]
pub struct InputSlot {
    held: AtomicBool,
}

/// Holds the slot until it is dropped.
#[derive(Debug)]
pub struct SlotGuard<'a> {
    slot: &'a InputSlot,
}

impl InputSlot {
    /// Takes the slot.
    ///
    /// # Errors
    /// `busy` (the P6 code and text) when an input operation holds it.
    pub fn try_take(&self) -> Result<SlotGuard<'_>, Fail> {
        if self
            .held
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            Ok(SlotGuard { slot: self })
        } else {
            Err(p6_fail(p6_code::BUSY))
        }
    }

    /// Whether an input operation holds the slot now.
    #[must_use]
    pub fn is_held(&self) -> bool {
        self.held.load(Ordering::Acquire)
    }
}

impl Drop for SlotGuard<'_> {
    fn drop(&mut self) {
        self.slot.held.store(false, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn a_held_slot_refuses_at_once_and_a_drop_frees_it() {
        let slot = InputSlot::default();
        let guard = slot.try_take().unwrap();
        assert!(slot.is_held());
        let e = slot.try_take().unwrap_err();
        assert_eq!(e.code, "busy");
        assert_eq!(
            e.message,
            "The desktop operation cannot start because its capacity limit is in use."
        );
        drop(guard);
        assert!(!slot.is_held());
        assert!(slot.try_take().is_ok());
    }
}
