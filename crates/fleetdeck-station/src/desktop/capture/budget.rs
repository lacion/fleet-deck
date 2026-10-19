//! The budgets of one capture (design §4): one capture at a time, the free
//! space of the staging disk, the monitor size.

use std::os::fd::OwnedFd;
use std::sync::{Mutex, PoisonError};

use fleetdeck_proto::capture::{code, text};
use fleetdeck_proto::desktop::MonitorInfo;

use super::{MAX_DIMENSION, MAX_PIXELS};
use crate::desktop::Fail;
use crate::desktop::recording::Holder;

/// The one capture slot of a station. A second capture does not wait.
#[derive(Debug, Default)]
pub(crate) struct CaptureSlot {
    held: Mutex<Option<Holder>>,
}

/// Holds the slot until it is dropped.
#[derive(Debug)]
pub(crate) struct SlotGuard<'a> {
    slot: &'a CaptureSlot,
}

impl CaptureSlot {
    /// Takes the slot for a screenshot.
    ///
    /// # Errors
    /// `busy` when a capture holds it.
    pub(crate) fn try_take(&self) -> Result<SlotGuard<'_>, Fail> {
        self.try_take_for(Holder::Screenshot)
    }

    /// Takes the slot for `holder`.
    ///
    /// # Errors
    /// `busy` when a capture holds it.
    pub(crate) fn try_take_for(&self, holder: Holder) -> Result<SlotGuard<'_>, Fail> {
        let mut held = self.held.lock().unwrap_or_else(PoisonError::into_inner);
        if held.is_some() {
            return Err(Fail::new(code::BUSY, text::BUSY));
        }
        *held = Some(holder);
        Ok(SlotGuard { slot: self })
    }

    /// Who holds the slot now.
    pub(crate) fn holder(&self) -> Option<Holder> {
        self.held
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

impl Drop for SlotGuard<'_> {
    fn drop(&mut self) {
        *self
            .slot
            .held
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = None;
    }
}

fn disk_full() -> Fail {
    Fail::new(code::DISK_FULL, text::DISK_FULL)
}

/// Checks that the disk of the staging directory `dir` keeps `reserve`
/// bytes free after `need` more bytes.
///
/// # Errors
/// `disk_full` when it does not, or when the free space is unknown.
pub(crate) fn room(dir: &OwnedFd, need: u64, reserve: u64) -> Result<(), Fail> {
    let stat = rustix::fs::fstatvfs(dir).map_err(|e| {
        tracing::warn!(error = %e, "cannot read the free space of the capture disk");
        disk_full()
    })?;
    let available = stat.f_bavail.saturating_mul(stat.f_frsize);
    if available >= reserve.saturating_add(need) {
        Ok(())
    } else {
        Err(disk_full())
    }
}

/// Checks the size that the monitor reports.
///
/// # Errors
/// `capture_dimensions` when a side or the pixel count is over the limit,
/// or a side is 0.
pub(crate) fn check_monitor(info: &MonitorInfo) -> Result<(), Fail> {
    check_dimensions(info.width, info.height)
}

/// The dimension rule of a monitor and of a PNG.
pub(super) fn check_dimensions(width: u32, height: u32) -> Result<(), Fail> {
    let side = 1..=MAX_DIMENSION;
    if side.contains(&width)
        && side.contains(&height)
        && u64::from(width) * u64::from(height) <= MAX_PIXELS
    {
        Ok(())
    } else {
        Err(Fail::new(
            code::CAPTURE_DIMENSIONS,
            text::CAPTURE_DIMENSIONS,
        ))
    }
}
