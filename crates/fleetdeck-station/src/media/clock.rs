//! The suspend-aware clock of the recording lease (R13 P5b, design
//! `omarchy-p5b.md` §6.4).
//!
//! `Instant` stops while the machine sleeps; `CLOCK_BOOTTIME` does not. A
//! lease that expires on the boot clock cannot stay fresh across a suspend,
//! and [`SuspendWatch`] tells a suspend from the gap between the two clocks.

use std::time::{Duration, Instant};

use rustix::time::{ClockId, clock_gettime};

/// The boot clock may run this much ahead of the monotonic clock before the
/// gap counts as a suspend.
const SUSPEND_GAP: Duration = Duration::from_secs(1);

/// A point on the boot clock (`CLOCK_BOOTTIME`: it counts the time of a
/// suspend).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct BootInstant(Duration);

/// The boot clock now. On a system without `CLOCK_BOOTTIME` it is the
/// monotonic clock.
#[must_use]
pub fn boot_now() -> BootInstant {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    let id = ClockId::Boottime;
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    let id = ClockId::Monotonic;
    let ts = clock_gettime(id);
    BootInstant(Duration::new(
        u64::try_from(ts.tv_sec).unwrap_or(0),
        u32::try_from(ts.tv_nsec).unwrap_or(0),
    ))
}

impl BootInstant {
    /// This instant plus `d` (saturating).
    #[must_use]
    pub fn plus(self, d: Duration) -> Self {
        Self(self.0.saturating_add(d))
    }

    /// The time from `earlier` to this instant; zero when `earlier` is
    /// later.
    #[must_use]
    pub fn saturating_since(self, earlier: Self) -> Duration {
        self.0.saturating_sub(earlier.0)
    }
}

/// The two clocks of the supervisor. A test replaces them to move one.
pub trait Clocks: Send + Sync {
    /// The boot clock.
    fn boot(&self) -> BootInstant;
    /// The monotonic clock.
    fn mono(&self) -> Instant;
}

/// The real clocks.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClocks;

impl Clocks for SystemClocks {
    fn boot(&self) -> BootInstant {
        boot_now()
    }

    fn mono(&self) -> Instant {
        Instant::now()
    }
}

/// Detects a suspend: the boot clock advanced more than one second more
/// than the monotonic clock since the watch started.
#[derive(Debug, Clone, Copy)]
pub struct SuspendWatch {
    boot: BootInstant,
    mono: Instant,
}

impl SuspendWatch {
    /// Starts a watch on the real clocks.
    #[must_use]
    pub fn new() -> Self {
        Self::start(&SystemClocks)
    }

    /// Starts a watch on `clocks`.
    #[must_use]
    pub fn start(clocks: &dyn Clocks) -> Self {
        Self {
            boot: clocks.boot(),
            mono: clocks.mono(),
        }
    }

    /// `true` when a suspend happened since the start (real clocks).
    #[must_use]
    pub fn suspended(&self) -> bool {
        self.suspended_with(&SystemClocks)
    }

    /// `true` when a suspend happened since the start (`clocks`).
    #[must_use]
    pub fn suspended_with(&self, clocks: &dyn Clocks) -> bool {
        let boot = clocks.boot().saturating_since(self.boot);
        let mono = clocks.mono().saturating_duration_since(self.mono);
        boot.saturating_sub(mono) > SUSPEND_GAP
    }
}

impl Default for SuspendWatch {
    fn default() -> Self {
        Self::new()
    }
}
