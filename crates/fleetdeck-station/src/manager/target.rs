//! The effect target of the session manager (PLAN §6.4): the I/O of each
//! claimed effect against the live provider sessions, and the pump that
//! appends each session's events to the log.
//!
//! The dispatch result follows the effect rules: a refusal before any I/O is
//! `NotDispatched`, a write that may have reached the provider is `Unknown`,
//! a write that worked is `Sent` (confirmation waits for provider evidence),
//! and a spawn that returned a live session is `Confirmed`.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex as StdMutex};

use async_trait::async_trait;
use fleetdeck_core::{SharedIds, new_id};
use fleetdeck_proto::command::{SessionSpec, StopMode};
use fleetdeck_proto::{Event, ProviderInstanceId, SessionId};
use fleetdeck_providers::host::HostError;
use fleetdeck_providers::{
    Answer, ControlId, EventSink, HostInput, LiveSession, ModelId, PermissionMode, ProviderAdapter,
    ProviderError,
};
use tokio::sync::{Mutex, mpsc, oneshot};

use crate::store::{
    Dispatch, Effect, EffectPayload, InputRecord, NewEvent, ProcessIncarnation, Store,
};
use crate::worker::EffectTarget;

/// Events that the pump appends in one transaction at most.
const PUMP_BATCH: usize = 64;
/// The event channel between one adapter session and its pump.
const SINK_CAPACITY: usize = 256;
/// The `session.ended` reason when a process ends without one.
pub const REASON_PROCESS_EXIT: &str = "process_exit";

/// The provider adapters of a station, by instance id.
pub type Adapters = BTreeMap<ProviderInstanceId, Arc<dyn ProviderAdapter>>;

struct Live {
    process: ProcessIncarnation,
    /// `None` after a stop took the session.
    session: Option<Box<dyn LiveSession>>,
}

type Sessions = Arc<StdMutex<HashMap<SessionId, Arc<Mutex<Live>>>>>;

/// The session manager's [`EffectTarget`].
pub struct SessionTarget {
    store: Store,
    adapters: Adapters,
    ids: SharedIds,
    sessions: Sessions,
}

impl std::fmt::Debug for SessionTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionTarget")
            .field("adapters", &self.adapters.keys().collect::<Vec<_>>())
            .finish_non_exhaustive()
    }
}

impl SessionTarget {
    #[must_use]
    pub fn new(store: Store, adapters: Adapters, ids: SharedIds) -> Self {
        Self {
            store,
            adapters,
            ids,
            sessions: Arc::default(),
        }
    }

    /// The adapter of a provider instance, if this station has one.
    #[must_use]
    pub fn adapter(&self, instance: &ProviderInstanceId) -> Option<Arc<dyn ProviderAdapter>> {
        self.adapters.get(instance).cloned()
    }

    /// The number of sessions with a live process in this station process.
    #[must_use]
    pub fn live_count(&self) -> usize {
        self.sessions.lock().map(|s| s.len()).unwrap_or(0)
    }

    /// Stops every live session (station shutdown).
    pub async fn stop_all(&self, how: StopMode) {
        let all: Vec<_> = match self.sessions.lock() {
            Ok(s) => s.values().cloned().collect(),
            Err(_) => return,
        };
        for live in all {
            let session = live.lock().await.session.take();
            if let Some(mut session) = session
                && let Err(e) = session.stop(how).await
            {
                tracing::warn!(error = %e, "stop at shutdown");
            }
        }
    }

    fn live(&self, session_id: &SessionId) -> Option<Arc<Mutex<Live>>> {
        self.sessions.lock().ok()?.get(session_id).cloned()
    }

    async fn start(
        &self,
        effect: &Effect,
        spec: &SessionSpec,
        prompt: Option<&InputRecord>,
    ) -> Dispatch {
        let session_id = effect.session_id.clone();
        let Some(adapter) = self.adapters.get(&spec.provider_instance).cloned() else {
            self.start_failed(&session_id, "the provider instance has no adapter")
                .await;
            return Dispatch::NotDispatched("unknown_provider".into());
        };
        let (sink, rx) = EventSink::channel(SINK_CAPACITY);
        let (ready_tx, ready_rx) = oneshot::channel();
        tokio::spawn(pump(
            self.store.clone(),
            Arc::clone(&self.sessions),
            session_id.clone(),
            rx,
            ready_rx,
        ));
        let mut session = match adapter.start(spec.clone(), sink).await {
            Ok(session) => session,
            Err(e) => {
                drop(ready_tx);
                self.start_failed(&session_id, &e.to_string()).await;
                return Dispatch::NotDispatched(format!("start_failed: {e}"));
            }
        };
        let process: ProcessIncarnation = new_id(self.ids.as_ref());
        if let Err(e) = self
            .store
            .process_started(session_id.clone(), process.clone(), None)
            .await
        {
            tracing::error!(error = %e, "the process could not be recorded; stopping it");
            drop(ready_tx);
            let _ = session.stop(StopMode::Kill).await;
            return Dispatch::Unknown(format!("store: {e}"));
        }
        let mut sent_prompt = Ok(());
        if let Some(input) = prompt {
            sent_prompt = session.prompt(host_input(input)).await;
        }
        if let Ok(mut map) = self.sessions.lock() {
            map.insert(
                session_id,
                Arc::new(Mutex::new(Live {
                    process: process.clone(),
                    session: Some(session),
                })),
            );
        }
        let _ = ready_tx.send(process);
        if let Err(e) = sent_prompt {
            tracing::warn!(error = %e, "the first prompt was not sent");
        }
        Dispatch::Confirmed
    }

    /// A start that never got a process ends the session (`exited`).
    async fn start_failed(&self, session_id: &SessionId, message: &str) {
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

    async fn operate(&self, effect: &Effect) -> Dispatch {
        let Some(live) = self.live(&effect.session_id) else {
            return Dispatch::NotDispatched("no_process".into());
        };
        let mut live = live.lock().await;
        if effect.process_incarnation.as_ref() != Some(&live.process) {
            return Dispatch::NotDispatched("process_changed".into());
        }
        if let EffectPayload::Stop { how } = &effect.payload {
            let Some(mut session) = live.session.take() else {
                return Dispatch::NotDispatched("already_stopping".into());
            };
            let how = *how;
            // A graceful stop can take the whole grace period; the worker
            // goes on. `session.ended` confirms the stop.
            tokio::spawn(async move {
                if let Err(e) = session.stop(how).await {
                    tracing::warn!(error = %e, "stop");
                }
            });
            return Dispatch::Sent;
        }
        let Some(session) = live.session.as_mut() else {
            return Dispatch::NotDispatched("stopping".into());
        };
        let sent = match &effect.payload {
            EffectPayload::Prompt { input } => session.prompt(host_input(input)).await,
            EffectPayload::Steer { input } => session.steer(host_input(input)).await,
            EffectPayload::FollowUp { input } => session.follow_up(host_input(input)).await,
            EffectPayload::Interrupt => {
                session
                    .interrupt(Some(ControlId(effect.effect_id.as_str().to_owned())))
                    .await
            }
            EffectPayload::Answer {
                request_id,
                decision,
                by,
            } => {
                session
                    .answer(Answer {
                        request: request_id.clone(),
                        decision: decision.clone(),
                        by: *by,
                    })
                    .await
            }
            EffectPayload::SetMode { mode } => session.set_mode(PermissionMode(mode.clone())).await,
            EffectPayload::SetModel { model } => session.set_model(ModelId(model.clone())).await,
            EffectPayload::Start { .. }
            | EffectPayload::Resume { .. }
            | EffectPayload::Stop { .. } => {
                return Dispatch::NotDispatched("not_a_live_operation".into());
            }
        };
        match sent {
            Ok(()) => Dispatch::Sent,
            Err(e) => classify(&e),
        }
    }
}

#[async_trait]
impl EffectTarget for SessionTarget {
    async fn dispatch(&self, effect: &Effect) -> Dispatch {
        match &effect.payload {
            EffectPayload::Start { spec, prompt } => {
                self.start(effect, spec, prompt.as_ref()).await
            }
            EffectPayload::Resume { .. } => {
                Dispatch::NotDispatched("resume is not built yet (DL-037)".into())
            }
            _ => self.operate(effect).await,
        }
    }
}

fn host_input(record: &InputRecord) -> HostInput {
    HostInput {
        id: record.input_id.clone(),
        origin: record.origin,
        input: record.input.clone(),
    }
}

/// The dispatch result of an adapter error. Only an error that can follow a
/// partial write is `Unknown`; every other error happens before the write.
#[must_use]
pub fn classify(e: &ProviderError) -> Dispatch {
    match e {
        ProviderError::Host(HostError::Write(_))
        | ProviderError::SinkClosed
        | ProviderError::EventsUndelivered => Dispatch::Unknown(e.to_string()),
        _ => Dispatch::NotDispatched(e.to_string()),
    }
}

/// Appends one session's events in order. After the process registered, a
/// `session.ended` (or a closed channel) ends the process in the store. If
/// the log fails, the pump stops reading: the adapter's sink closes and the
/// adapter kills the process (fail closed, DL-033).
async fn pump(
    store: Store,
    sessions: Sessions,
    session_id: SessionId,
    mut rx: mpsc::Receiver<fleetdeck_providers::adapter::Emitted>,
    ready: oneshot::Receiver<ProcessIncarnation>,
) {
    // Hold the events until the process is recorded, so that its end cannot
    // come first. A failed start drops `ready`: the pump still stores what
    // the adapter said.
    let process = ready.await.ok();
    let mut ended = false;
    while let Some(first) = rx.recv().await {
        let mut batch = vec![first];
        while batch.len() < PUMP_BATCH {
            match rx.try_recv() {
                Ok(next) => batch.push(next),
                Err(_) => break,
            }
        }
        ended |= batch.iter().any(|e| e.event.is_terminal_for_session());
        let events = batch.into_iter().map(NewEvent::from).collect();
        if let Err(e) = store.append(session_id.clone(), events).await {
            tracing::error!(error = %e, session = %session_id, "events not stored; closing the session");
            break;
        }
        if ended {
            break;
        }
    }
    drop(rx);
    if let Some(process) = process {
        if let Ok(mut map) = sessions.lock() {
            map.remove(&session_id);
        }
        if let Err(e) = store
            .process_ended(process, REASON_PROCESS_EXIT.to_owned())
            .await
        {
            tracing::error!(error = %e, "process end not stored");
        }
    }
}

#[cfg(test)]
mod classify_tests {
    use std::io;

    use fleetdeck_providers::ProviderError;
    use fleetdeck_providers::host::HostError;

    use super::classify;
    use crate::store::Dispatch;

    #[test]
    fn a_write_that_can_be_partial_is_unknown_and_an_ended_session_is_not_dispatched() {
        let partial = ProviderError::Host(HostError::Write(io::Error::new(
            io::ErrorKind::BrokenPipe,
            "the stdin writer stopped",
        )));
        assert!(matches!(classify(&partial), Dispatch::Unknown(_)));
        assert!(matches!(
            classify(&ProviderError::SessionEnded),
            Dispatch::NotDispatched(_)
        ));
    }
}
