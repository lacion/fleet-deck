//! The base commit and the start failure of a session (R1.1): small writes
//! of the station into the log that the effect target makes around a start.

use fleetdeck_proto::session::SessionBase;
use fleetdeck_proto::{Event, SessionId};

use super::SessionTarget;
use crate::manager::fleet_control::{self, Refusal};
use crate::store::{Dispatch, Effect, EffectPayload, NewEvent};

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

    /// The dispatch recheck of an effect that carries the authority of a
    /// fleet-control command (ADR-008 §6.2): `None` to go on, or the refusal
    /// to settle the effect with. A refused effect writes an `error` event
    /// (`authority_lost`, `authority_unknown`); a start or a resume that is
    /// refused ends the session as a failed start. Nothing is retried.
    pub(super) async fn authority_gate(&self, effect: &Effect) -> Option<Dispatch> {
        effect.payload.authority()?;
        // The live setting: the snapshot of the policy hub, else the value
        // that the bindings hold.
        let live = self.current_policy().map_or_else(
            || self.coordinator.fleet_control(),
            |policy| policy.allow_fleet_control(),
        );
        let Err(refusal) = fleet_control::recheck(&self.coordinator, live, effect).await else {
            return None;
        };
        self.authority_refused(effect, refusal).await;
        Some(Dispatch::NotDispatched(refusal.code().into()))
    }

    async fn authority_refused(&self, effect: &Effect, refusal: Refusal) {
        let mut events = vec![NewEvent::from(Event::Error {
            code: refusal.code().into(),
            message: refusal.message().into(),
            retry_at: None,
        })];
        if matches!(
            effect.payload,
            EffectPayload::Start { .. } | EffectPayload::Resume { .. }
        ) {
            events.push(NewEvent::from(Event::SessionEnded {
                reason: "start_failed".into(),
                exit_code: None,
            }));
        }
        if let Err(e) = self.store.append(effect.session_id.clone(), events).await {
            tracing::error!(error = %e, "the authority refusal was not stored");
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
