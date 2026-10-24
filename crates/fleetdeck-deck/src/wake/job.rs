//! One wake job (R13 §6.6, §6D items 4, 5, 9 and 10).
//!
//! A worker runs the steps in order and stops at the first one that fails.
//! A stop before the send is a counter: no audit row and no mutation. The
//! send goes through [`crate::desktop_relay::act`], which writes the audit
//! row and never retries.

use std::sync::PoisonError;

use fleetdeck_proto::desktop::DesktopAction;
use fleetdeck_proto::{SessionId, StationId};

use super::scan::repo_ok;
use super::{Shared, WakeCounters, last_component, quiet};
use crate::desktop_relay::{Actor, act, info};
use crate::store::SnapshotMark;

/// A request that the job may wake for.
#[derive(Debug, Clone)]
pub(super) struct ReqRef {
    pub session: SessionId,
    pub request_id: String,
    pub requested_at: i64,
}

/// A wake for one bound monitor.
#[derive(Debug, Clone)]
pub(super) struct Job {
    pub station: StationId,
    pub monitor: String,
    /// The display of the audit row and of the quiet hours.
    pub display_id: String,
    pub requests: Vec<ReqRef>,
    /// The mark of the station when the scan found the requests.
    pub mark: Option<SnapshotMark>,
}

fn fresh(shared: &Shared, request: &ReqRef, now: i64) -> bool {
    let age = now - request.requested_at;
    -shared.limits.skew_ms <= age && age <= shared.limits.window_ms
}

pub(super) async fn run(shared: &Shared, job: Job) {
    let counters = &shared.counters;
    let ctx = &shared.ctx;
    let store = &ctx.store;
    // 1. The job is not expired.
    if !job
        .requests
        .iter()
        .any(|r| fresh(shared, r, store.now_ms()))
    {
        WakeCounters::bump(&counters.expired);
        return;
    }
    // 2. The deck switch is on.
    if !ctx.live.get().desktop_control {
        WakeCounters::bump(&counters.skip_control);
        return;
    }
    // 3. The display still takes the wake.
    let display = match store.display_get(job.display_id.clone()).await {
        Ok(Some(d)) => d,
        Ok(None) => {
            WakeCounters::bump(&counters.skip_display);
            return;
        }
        Err(e) => {
            tracing::warn!(error = %e, "wake job: display");
            WakeCounters::bump(&counters.skip_display);
            return;
        }
    };
    let bound = display
        .binding
        .as_ref()
        .is_some_and(|b| b.station_id == job.station && b.monitor == job.monitor);
    let scope = match store.display_scope(job.display_id.clone()).await {
        Ok(Some(scope)) => scope,
        Ok(None) => {
            WakeCounters::bump(&counters.skip_display);
            return;
        }
        Err(e) => {
            tracing::warn!(error = %e, "wake job: scope");
            WakeCounters::bump(&counters.skip_display);
            return;
        }
    };
    if !display.wake_on_needs_you || !bound || !scope.stations.contains(&job.station) {
        WakeCounters::bump(&counters.skip_display);
        return;
    }
    // 4. A fresh read of the desktop (it bypasses the station cache).
    let desktop = match info(ctx, job.station.clone(), true).await {
        Ok(desktop) => desktop,
        Err(e) => {
            tracing::debug!(station = %job.station, code = e.http().1, "wake job: desktop info");
            WakeCounters::bump(&counters.skip_info);
            return;
        }
    };
    // 5. Only the exact bound monitor, only when it is known to be off.
    let off = desktop
        .monitors
        .iter()
        .find(|m| m.name == job.monitor)
        .is_some_and(|m| m.dpms_on == Some(false));
    if !off {
        WakeCounters::bump(&counters.skip_monitor);
        return;
    }
    // 6. Quiet hours, with an offset that is known and fresh.
    if !quiet::allows_wake(display.quiet_hours.as_deref(), &desktop, store.now_ms()) {
        WakeCounters::bump(&counters.skip_quiet);
        return;
    }
    // 7. A snapshot of the station after the scan can have made the request
    //    part of the baseline: fail closed. Then the last check before the
    //    send: a request of the job is still open and in the filter.
    if store.snapshot_mark(&job.station) != job.mark {
        WakeCounters::bump(&counters.skip_raced);
        return;
    }
    let mut open = false;
    for request in &job.requests {
        if !fresh(shared, request, store.now_ms()) {
            continue;
        }
        let summary = match store
            .session_summary(job.station.clone(), request.session.clone())
            .await
        {
            Ok(Some(s)) => s,
            Ok(None) => continue,
            Err(e) => {
                tracing::warn!(error = %e, "wake job: session");
                continue;
            }
        };
        let basename = summary.repo.as_ref().map(|r| last_component(&r.root));
        if summary
            .open_requests
            .iter()
            .any(|o| o.request_id.as_str() == request.request_id)
            && repo_ok(scope.repo.as_deref(), basename.as_deref())
        {
            open = true;
            break;
        }
    }
    if !open {
        WakeCounters::bump(&counters.skip_closed);
        return;
    }
    // 8. The cooldown of the monitor counts from the last wake attempt. The
    //    check and the record are one step under the lock. A failed or timed
    //    out send keeps its entry (no retry).
    {
        let now = store.now_ms();
        let key = (job.station.clone(), job.monitor.clone());
        let mut last = shared
            .last_wake
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if last
            .get(&key)
            .is_some_and(|at| now - *at < shared.limits.cooldown_ms)
        {
            WakeCounters::bump(&counters.skip_cooldown);
            return;
        }
        last.insert(key, now);
    }
    // 9. The send. The relay audits it. An error or a timeout is final.
    let action = DesktopAction::ScreenPower {
        monitor: Some(job.monitor.clone()),
        on: true,
    };
    let actor = Actor::Wake {
        display_id: job.display_id.clone(),
    };
    match act(ctx, job.station.clone(), action, actor).await {
        Ok(_) => WakeCounters::bump(&counters.sent_ok),
        Err(e) => {
            tracing::info!(
                station = %job.station,
                display = %job.display_id,
                monitor = %job.monitor,
                code = e.http().1,
                "wake not sent"
            );
            WakeCounters::bump(&counters.sent_failed);
        }
    }
}
