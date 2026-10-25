//! The takeover of the sessions that a station start restored (moved from
//! `shim_target.rs` in B15 T5a to make room; ADR-005, DL-113).

use std::path::Path;
use std::sync::Arc;

use fleetdeck_proto::event::SessionState;
use fleetdeck_providers::host::StopPolicy;

use super::{Drain, SessionTarget, observe};
use crate::manager::shim_station::Restored;

impl SessionTarget {
    /// Takes over the sessions that a station start restored.
    pub async fn adopt(&self, restored: Vec<Restored>, stop: StopPolicy, from: u64) {
        for r in restored {
            let summary = self
                .store
                .summary(r.session_id.clone())
                .await
                .ok()
                .flatten();
            let cwd = summary
                .as_ref()
                .and_then(|s| s.cwd.clone())
                .unwrap_or_default();
            let facts =
                crate::workflows::launch::adopted(&self.store, &r.session_id, &r.process).await;
            // A restore starts no new process: no identity (B9, out of scope).
            let turns = self.turns(&r.session_id, Path::new(&cwd)).await;
            turns.set_process(r.process.clone());
            // A session whose exit a restored driver already committed:
            // its observer still reads every event from `from` (turns that
            // the provider completed while the station was down), then ends
            // the session when it has caught up. It takes no commands. It
            // gets no app bridge and no app sink: the candidates that the
            // driver kept are dropped (B15 §4.3 item 1).
            if summary.is_some_and(|s| matches!(s.state, SessionState::Exited | SessionState::Lost))
            {
                Self::discard_app_candidates(&r.session_id, &r.driver);
                tokio::spawn(observe(
                    self.store.clone(),
                    Arc::clone(&self.sessions),
                    self.apps.hosts(),
                    r.session_id.clone(),
                    r.process,
                    r.driver.process(),
                    turns,
                    from,
                    Drain::ToHead,
                    facts,
                ));
                continue;
            }
            // `from` is the head before the restoration: the observer sees
            // every event that the driver committed while it replayed.
            // An adopted process gets the launch facts that its launch stored
            // (B12).
            self.insert_durable(&r.session_id, r.process, turns, r.driver, stop, from, facts);
        }
    }
}
