//! The light desktop probe of a recording (design §7.4, §6.2, decision O12-10
//! item 15): the compositor signature, the geometry and the transform of the
//! recorded monitor, and the lock state. A probe that cannot read something is
//! a safety failure, never permission to keep an old answer.

use std::time::Duration;

use fleetdeck_proto::recording::StopReason;

use crate::desktop::info::{self, RawMonitor};
use crate::desktop::run::Runner;
use crate::desktop::{Ctx, Desktop, Fail, code};

/// What one probe read.
#[derive(Debug, Clone)]
pub(crate) struct Snapshot {
    /// The signature of the Hyprland instance (a compositor restart changes
    /// it).
    pub signature: String,
    pub monitor: RawMonitor,
    /// `None` when the lock state is unknown.
    pub locked: Option<bool>,
}

impl Snapshot {
    /// The size that the recording of this monitor must have: the mode, with
    /// the sides swapped for the transforms 1, 3, 5 and 7.
    pub(crate) fn recorded_size(&self) -> (u32, u32) {
        let (w, h) = (self.monitor.info.width, self.monitor.info.height);
        if self.monitor.transform % 2 == 1 {
            (h, w)
        } else {
            (w, h)
        }
    }

    fn same_geometry(&self, other: &Self) -> bool {
        let (a, b) = (&self.monitor, &other.monitor);
        a.x == b.x
            && a.y == b.y
            && a.transform == b.transform
            && a.info.width == b.info.width
            && a.info.height == b.info.height
    }
}

/// Reads the monitor and the lock with the environment of `ctx`.
///
/// # Errors
/// `not_found` when the monitor is gone, `failed` when `hyprctl` fails.
pub(crate) async fn snapshot_in(ctx: &Ctx, monitor: &str) -> Result<Snapshot, Fail> {
    let runner = Runner {
        env: &ctx.env,
        gate: None,
        start: None,
    };
    let monitor = info::monitors(&runner)
        .await?
        .into_iter()
        .find(|m| m.info.name == monitor)
        .ok_or_else(|| Fail::new(code::NOT_FOUND, "The monitor is gone."))?;
    let locked = info::locked(&runner).await;
    Ok(Snapshot {
        signature: ctx.found.signature.clone(),
        monitor,
        locked,
    })
}

/// A fresh probe: it looks for the desktop again (the signature), then reads.
/// `limit` is the deadline of the whole probe.
///
/// # Errors
/// As [`snapshot_in`], `unsupported` without a desktop, and `failed` when the
/// deadline passes.
pub(crate) async fn snapshot(
    desktop: &Desktop,
    monitor: &str,
    limit: Duration,
) -> Result<Snapshot, Fail> {
    let read = async {
        let ctx = desktop.context().await?;
        snapshot_in(&ctx, monitor).await
    };
    tokio::time::timeout(limit, read)
        .await
        .unwrap_or_else(|_| Err(Fail::new(code::FAILED, "The desktop probe timed out.")))
}

/// The safety verdict of a probe against the first one: `None` when nothing
/// changed and the screen is unlocked.
pub(crate) fn verdict(base: &Snapshot, now: &Result<Snapshot, Fail>) -> Option<StopReason> {
    let Ok(now) = now else {
        return Some(StopReason::MonitorChanged);
    };
    if now.signature != base.signature || !now.same_geometry(base) {
        return Some(StopReason::MonitorChanged);
    }
    // An unknown lock state is not unlocked.
    (now.locked != Some(false)).then_some(StopReason::ScreenLocked)
}

impl Desktop {
    /// The probe of `desktop.capture_policy_now`: whether the monitor is
    /// present and the screen is unlocked now, in at most `limit`. A probe
    /// that fails answers `(false, false)`.
    pub(crate) async fn policy_probe(&self, monitor: &str, limit: Duration) -> (bool, bool) {
        match snapshot(self, monitor, limit).await {
            Ok(s) => (true, s.locked == Some(false)),
            Err(e) => {
                tracing::debug!(code = e.code, "the policy probe found no monitor");
                (false, false)
            }
        }
    }
}
