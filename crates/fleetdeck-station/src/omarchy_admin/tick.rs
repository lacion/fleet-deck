//! The screensaver tick (design `omarchy-admin.md` §3.4): every 60 s the
//! station runs `omarchy screensaver-tick` as a short child of its own
//! executable, while Omarchy is present and an install record exists. The
//! output of the child is discarded.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::watch;
use tokio::time::{Instant, MissedTickBehavior};

use super::child::RunResult;
use super::{Holder, OmarchyAdmin};

/// The time between two ticks.
pub const EVERY: Duration = Duration::from_secs(60);

/// Runs the scheduler until the shutdown signal. The first tick comes one
/// interval after the start; missed intervals are skipped.
pub async fn run(admin: Arc<OmarchyAdmin>, every: Duration, mut stop: watch::Receiver<bool>) {
    let mut ticks = tokio::time::interval_at(Instant::now() + every, every);
    ticks.set_missed_tick_behavior(MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            () = wait_stop(&mut stop) => return,
            _ = ticks.tick() => {}
        }
        // A tick that runs when the station stops is dropped: its group is
        // killed with it.
        tokio::select! {
            () = wait_stop(&mut stop) => return,
            () = admin.tick_once() => {}
        }
    }
}

/// Completes when the shutdown is set or its sender is gone.
async fn wait_stop(stop: &mut watch::Receiver<bool>) {
    while !*stop.borrow() {
        if stop.changed().await.is_err() {
            return;
        }
    }
}

impl OmarchyAdmin {
    /// One tick: skipped without Omarchy, without a config home, without an
    /// install record, or while an action or a tick holds the slot.
    pub(super) async fn tick_once(&self) {
        if crate::desktop::detect::omarchy_root(&self.cfg.omarchy_env).is_none() {
            return;
        }
        let Some(record) = self.cfg.install_record() else {
            return;
        };
        if std::fs::symlink_metadata(&record).is_err() {
            return;
        }
        let Some(permit) = self.try_hold(Holder::Tick) else {
            return;
        };
        let ended = self
            .run_tail(&["omarchy", "screensaver-tick"], self.cfg.limits.tick)
            .await;
        drop(permit);
        match ended {
            Ok(RunResult::Exit { code: Some(0), .. }) => {}
            Ok(RunResult::Exit { code, .. }) => {
                tracing::debug!(?code, "the screensaver tick ended with a failure");
            }
            Ok(RunResult::TimedOut) => tracing::debug!("the screensaver tick timed out"),
            Ok(RunResult::TooLarge) => tracing::debug!("the screensaver tick wrote too much"),
            Err(e) => tracing::debug!(error = %e, "the screensaver tick did not start"),
        }
    }
}
