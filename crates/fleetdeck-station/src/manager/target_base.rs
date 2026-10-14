//! The base commit and the start failure of a session (R1.1): small writes
//! of the station into the log that the effect target makes around a start.

use fleetdeck_proto::session::SessionBase;
use fleetdeck_proto::{Event, SessionId};

use super::SessionTarget;
use crate::store::NewEvent;

impl SessionTarget {
    /// True when the session has a base already, or when the summary cannot
    /// be read (no base is then recorded).
    pub(super) async fn has_base(&self, session_id: &SessionId) -> bool {
        match self.store.summary(session_id.clone()).await {
            Ok(summary) => summary.is_some_and(|s| s.base.is_some()),
            Err(e) => {
                tracing::warn!(error = %e, "the summary is unreadable; no base is recorded");
                true
            }
        }
    }

    /// Appends `session.base` (R1.1). The station emits it, not the provider.
    pub(super) async fn record_base(&self, session_id: &SessionId, base: Option<SessionBase>) {
        let Some(base) = base else { return };
        let event = Event::SessionBase {
            git_ref: base.git_ref,
            oid: base.oid,
            inferred: base.inferred,
        };
        if let Err(e) = self
            .store
            .append(session_id.clone(), vec![NewEvent::from(event)])
            .await
        {
            tracing::warn!(error = %e, "the session base was not stored");
        }
    }

    /// A start that never got a process ends the session (`exited`).
    pub(super) async fn start_failed(&self, session_id: &SessionId, message: &str) {
        let events = vec![
            NewEvent::from(Event::Error {
                code: "start_failed".into(),
                message: message.to_owned(),
                retry_at: None,
            }),
            NewEvent::from(Event::SessionEnded {
                reason: "start_failed".into(),
                exit_code: None,
            }),
        ];
        if let Err(e) = self.store.append(session_id.clone(), events).await {
            tracing::error!(error = %e, "start failure not stored");
        }
    }
}
