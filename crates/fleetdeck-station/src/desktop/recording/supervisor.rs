//! The watchdog of a running recording (design §7.4, §8): every second it
//! reads the live gates, the link, the lock, the monitor, the size of the
//! file and the free space. Each trigger is latched through [`Latch`], which
//! signals the recorder before it writes the store, so a store or a probe
//! that blocks never delays a kill. The timers of the recorder itself (the
//! duration, the lease, the total deadline) run on its own thread.

use std::fs::File;
use std::os::fd::OwnedFd;
use std::sync::Arc;
use std::time::{Duration, Instant};

use fleetdeck_proto::recording::{MAX_RECORDING_BYTES, StopReason};
use tokio::task::JoinHandle;

use super::latch::Latch;
use super::lease::DeckLink;
use super::probe::{self, Snapshot};
use super::{RoomCheck, Timing, check_gates};
use crate::desktop::Desktop;
use crate::desktop::capture::Allowed;

/// What the watchdog needs.
pub(crate) struct Watch {
    pub desktop: Arc<Desktop>,
    pub latch: Arc<Latch>,
    pub allowed: Arc<dyn Fn() -> Allowed + Send + Sync>,
    pub monitor: String,
    /// The first probe: the monitor as the recording started.
    pub base: Snapshot,
    /// A duplicate of the staging file (same inode) for the size.
    pub file: File,
    /// A duplicate of the staging folder for the free space.
    pub dir: OwnedFd,
    pub room: RoomCheck,
    pub link: Arc<dyn DeckLink>,
    pub timing: Timing,
    pub seconds: u32,
    pub started: Instant,
}

/// What one pass found.
pub(crate) enum Found {
    Nothing,
    Stop { reason: StopReason, ceiling: bool },
}

impl Watch {
    /// The checks that need no await: gates, link, size, free space.
    fn quick(&self) -> Found {
        if check_gates(&(self.allowed)(), &self.monitor).is_err() {
            return Found::Stop {
                reason: StopReason::PolicyOff,
                ceiling: false,
            };
        }
        if !self.link.open() || !self.link.current() {
            return Found::Stop {
                reason: StopReason::LinkClosed,
                ceiling: false,
            };
        }
        match self.file.metadata() {
            Ok(meta) if meta.len() >= MAX_RECORDING_BYTES => {
                return Found::Stop {
                    reason: StopReason::OutputLimit,
                    ceiling: true,
                };
            }
            Ok(_) => {}
            // A file that cannot be read is a write failure, not the ceiling.
            Err(_) => {
                return Found::Stop {
                    reason: StopReason::OutputLimit,
                    ceiling: false,
                };
            }
        }
        if (self.room)(&self.dir, 0).is_err() {
            return Found::Stop {
                reason: StopReason::DiskReserve,
                ceiling: false,
            };
        }
        Found::Nothing
    }

    /// One pass.
    pub(crate) async fn pass(&self) -> Found {
        if let found @ Found::Stop { .. } = self.quick() {
            return found;
        }
        let now = probe::snapshot(&self.desktop, &self.monitor, self.timing.probe).await;
        match probe::verdict(&self.base, &now) {
            Some(reason) => Found::Stop {
                reason,
                ceiling: false,
            },
            None => Found::Nothing,
        }
    }

    /// Latches one stop.
    async fn stop(&self, reason: StopReason, ceiling: bool) {
        if ceiling {
            self.latch.mark_ceiling();
        }
        self.latch.stop(false, reason).await;
    }

    /// The task: a pass every period, and an immediate stop when the link
    /// ends. It ends after the first discard.
    pub(crate) fn spawn(self) -> JoinHandle<()> {
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(self.timing.tick);
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            let mut marked = false;
            loop {
                tokio::select! {
                    () = self.link.closed() => {
                        self.stop(StopReason::LinkClosed, false).await;
                        return;
                    }
                    _ = tick.tick() => {}
                }
                match self.pass().await {
                    Found::Stop { reason, ceiling } => {
                        self.stop(reason, ceiling).await;
                        return;
                    }
                    Found::Nothing => {}
                }
                // The duration end is the recorder's own timer; the row shows
                // the phase from here.
                if !marked && self.started.elapsed() >= Duration::from_secs(u64::from(self.seconds))
                {
                    marked = true;
                    self.latch.mark_stopping().await;
                }
            }
        })
    }
}
