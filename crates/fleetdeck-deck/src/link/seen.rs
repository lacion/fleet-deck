//! The last-seen time of a live station link (B22, design `last-seen.md`
//! §3.4): written at the start, every `SEEN_REFRESH` while the link lives,
//! and at its end, as the time of the last frame from the station.

use std::sync::Arc;
use std::time::Duration;

use fleetdeck_link::control::Control;
use fleetdeck_proto::StationId;

use super::DeckLink;
use crate::store::DeckStore;

/// How often the deck writes the last-seen time of a live link.
pub(super) const SEEN_REFRESH: Duration = Duration::from_secs(30);

/// The deck time of the last frame: `now_ms` minus the idle time, never
/// below 0.
pub(super) fn heard_at_ms(now_ms: i64, idle: Duration) -> i64 {
    let idle = i64::try_from(idle.as_millis()).unwrap_or(i64::MAX);
    now_ms.saturating_sub(idle).max(0)
}

fn heard_at(store: &DeckStore, control: &Control) -> i64 {
    heard_at_ms(store.now_ms(), control.since_last_frame())
}

/// Starts the refresh of `id` for the life of `control`.
pub(super) fn keep_seen(ctx: &Arc<DeckLink>, control: &Control, id: &StationId) {
    let (store, control, id) = (ctx.store.clone(), control.clone(), id.clone());
    tokio::spawn(async move {
        let at = || heard_at(&store, &control);
        seen_loop(&store, &id, SEEN_REFRESH, at, control.closed()).await;
    });
}

/// Writes `at()` every `every` until `closed` resolves. The first write
/// comes after one interval (the link start wrote the time already).
pub(super) async fn seen_loop<F>(
    store: &DeckStore,
    id: &StationId,
    every: Duration,
    mut at: F,
    closed: impl Future,
) where
    F: FnMut() -> i64,
{
    tokio::pin!(closed);
    loop {
        tokio::select! {
            _ = &mut closed => return,
            () = tokio::time::sleep(every) => {}
        }
        if let Err(e) = store.station_heard(id.clone(), at()).await {
            tracing::warn!(error = %e, station = %id, "station last_seen");
        }
    }
}

/// The last write of a link that ended. `serve` awaits it before the
/// guard of the link marks the station unreachable.
pub(super) async fn record_end(store: &DeckStore, id: &StationId, control: &Control) {
    if let Err(e) = store
        .station_heard(id.clone(), heard_at(store, control))
        .await
    {
        tracing::warn!(error = %e, station = %id, "station last_seen");
    }
}

#[cfg(test)]
#[path = "seen_tests.rs"]
mod tests;
