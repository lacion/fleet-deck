//! The effect target of the session manager (PLAN §6.4): the I/O of each
//! claimed effect against the live provider sessions, and the pump that
//! appends each session's events to the log.
//!
//! The dispatch result follows the effect rules: a refusal before any I/O is
//! `NotDispatched`, a write that may have reached the provider is `Unknown`,
//! a write that worked is `Sent` (confirmation waits for provider evidence),
//! and a spawn that returned a live session is `Confirmed`.
//!
//! Each live session has its turn checkpoints ([`SessionTurns`], DL-084): a
//! prompt or a follow-up is written under the session's workspace fence
//! after its start checkpoint, and the pump feeds the stored events to the
//! checkpoints after each append.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::{Arc, Mutex as StdMutex};

use async_trait::async_trait;
use fleetdeck_core::{SharedIds, new_id};
use fleetdeck_proto::command::{SessionSpec, StopMode};
use fleetdeck_proto::{Event, ProviderInstanceId, SessionId};
use fleetdeck_providers::host::HostError;
use fleetdeck_providers::{
    Answer, ControlId, EventSink, HostInput, LiveSession, ModelId, NativeSessionId, PermissionMode,
    ProviderAdapter, ProviderError,
};
use tokio::sync::{Mutex, mpsc, oneshot};

use super::turns::SessionTurns;
use crate::store::{
    Dispatch, Effect, EffectPayload, InputRecord, NewEvent, ProcessIncarnation, Store,
};
use crate::worker::EffectTarget;
use crate::workspace::{GitOptions, Limits};

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
    turns: Arc<SessionTurns>,
    /// A session under a shim (DL-113); `session` is then `None`.
    durable: Option<shim_target::DurableLive>,
}

type Sessions = Arc<StdMutex<HashMap<SessionId, Arc<Mutex<Live>>>>>;

/// The session manager's [`EffectTarget`].
pub struct SessionTarget {
    store: Store,
    adapters: Adapters,
    ids: SharedIds,
    sessions: Sessions,
    git: GitOptions,
    limits: Limits,
    fences: super::turns::Fences,
    shims: Option<shim_target::ShimSettings>,
    /// The coordinator bindings that a coordinator start adds to (DL-216).
    coordinator: crate::coordinator::Bindings,
    /// The station policy for the dispatch recheck of an auto-continue
    /// (ADR-007 §2.4 rules 2 and 10, DL-133).
    policy: Option<super::policy::Policy>,
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
            git: GitOptions::isolated(),
            limits: Limits::default(),
            fences: super::turns::Fences::default(),
            shims: None,
            coordinator: crate::coordinator::Bindings::default(),
            policy: None,
        }
    }

    /// The station policy that the dispatch recheck of an auto-continue
    /// uses (ADR-007 §2.4 rules 2 and 10, DL-133).
    #[must_use]
    pub fn with_policy(mut self, policy: super::policy::Policy) -> Self {
        self.policy = Some(policy);
        self
    }

    /// The coordinator bindings of the station (ADR-006 §5.4).
    #[must_use]
    pub fn with_coordinator(mut self, bindings: crate::coordinator::Bindings) -> Self {
        self.coordinator = bindings;
        self
    }

    /// The Git runner and the limits of the turn checkpoints.
    #[must_use]
    pub fn with_checkpoints(mut self, git: GitOptions, limits: Limits) -> Self {
        self.git = git;
        self.limits = limits;
        self
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
            Ok(s) => s
                .iter()
                .map(|(id, l)| (id.clone(), Arc::clone(l)))
                .collect(),
            Err(_) => return,
        };
        for (session_id, live) in all {
            let durable = live.lock().await.durable.is_some();
            if durable {
                // A session under a shim keeps running (ADR-005): detach its
                // driver and observer; the next station start restores it.
                Self::detach_durable(&mut *live.lock().await).await;
                if let Ok(mut sessions) = self.sessions.lock() {
                    sessions.remove(&session_id);
                }
                continue;
            }
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

    /// Starts a provider process for the session: a new native session, or
    /// with `resume` a native resume of that session (DL-118).
    async fn start(
        &self,
        effect: &Effect,
        spec: &SessionSpec,
        prompt: Option<&InputRecord>,
        resume: Option<&str>,
    ) -> Dispatch {
        let session_id = effect.session_id.clone();
        let Some(adapter) = self.adapters.get(&spec.provider_instance).cloned() else {
            self.start_failed(&session_id, "the provider instance has no adapter")
                .await;
            return Dispatch::NotDispatched("unknown_provider".into());
        };
        if resume.is_some() && self.shims.is_some() {
            // A shim-host resume needs the ADR-005 §6 termination evidence
            // and a durable resume launch; neither is built (DL-118).
            self.start_failed(&session_id, "resume is not supported on the shim host yet")
                .await;
            return Dispatch::NotDispatched(REASON_RESUME_UNSUPPORTED.into());
        }
        let coordinator = match spec.coordinator.as_deref() {
            None => None,
            Some(c) => match super::coordinator_start::prepare(
                &self.coordinator,
                adapter.kind(),
                &session_id,
                spec,
                c,
                resume.is_some() || self.shims.is_some(),
            ) {
                Ok(launch) => Some(launch),
                Err(why) => {
                    self.start_failed(&session_id, &why).await;
                    return Dispatch::NotDispatched(format!("coordinator: {why}"));
                }
            },
        };
        if let Some((shims, launch)) = self.durable_launch(&adapter, spec) {
            return self
                .start_durable(effect, spec, prompt, &shims, launch)
                .await;
        }
        let turns = SessionTurns::open(
            self.store.clone(),
            Arc::clone(&self.ids),
            self.git.clone(),
            self.limits,
            &self.fences,
            session_id.clone(),
            Path::new(&spec.cwd),
        )
        .await;
        let (sink, rx) = EventSink::channel(SINK_CAPACITY);
        let (ready_tx, ready_rx) = oneshot::channel();
        tokio::spawn(pump(
            self.store.clone(),
            Arc::clone(&self.sessions),
            session_id.clone(),
            Arc::clone(&turns),
            rx,
            ready_rx,
            self.coordinator.clone(),
        ));
        let started = match (resume, coordinator) {
            (Some(native), _) => {
                let native = NativeSessionId(native.to_owned());
                adapter.resume(&native, spec.clone(), sink).await
            }
            (None, Some(launch)) => adapter.start_coordinated(spec.clone(), sink, launch).await,
            (None, None) => adapter.start(spec.clone(), sink).await,
        };
        let mut session = match started {
            Ok(session) => session,
            Err(e) => {
                drop(ready_tx);
                self.coordinator.revoke_session(&session_id);
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
            // No process means no binding: its credential dies with the
            // child (review round 8, DL-217).
            self.coordinator.revoke_session(&session_id);
            let _ = session.stop(StopMode::Kill).await;
            return Dispatch::Unknown(format!("store: {e}"));
        }
        turns.set_process(process.clone());
        let mut sent_prompt = Ok(());
        if let Some(input) = prompt {
            let fence = turns.fence().await;
            turns.begin_input(&fence, &input.input_id).await;
            turns.input_sent(&input.input_id);
            sent_prompt = session.prompt(host_input(input)).await;
            if let Err(e) = &sent_prompt
                && matches!(classify(e), Dispatch::NotDispatched(_))
            {
                turns.input_not_sent(&input.input_id).await;
            }
        }
        if let Ok(mut map) = self.sessions.lock() {
            map.insert(
                session_id,
                Arc::new(Mutex::new(Live {
                    process: process.clone(),
                    session: Some(session),
                    turns,
                    durable: None,
                })),
            );
        }
        let _ = ready_tx.send(process);
        if let Err(e) = sent_prompt {
            // The spawn is the provider's acceptance of the start; the first
            // input gets its own durable evidence (GATE-M2 P2-a).
            tracing::warn!(error = %e, "the first input was not sent");
            if let Err(e) = self
                .store
                .first_input_failed(effect.effect_id.clone(), classify(&e))
                .await
            {
                tracing::error!(error = %e, "the first input failure was not stored");
            }
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
        if let Some(durable) = live.durable.as_ref() {
            let turns = Arc::clone(&live.turns);
            return self.operate_durable(effect, durable, &turns).await;
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
        let turns = Arc::clone(&live.turns);
        let process = live.process.clone();
        let Some(session) = live.session.as_mut() else {
            return Dispatch::NotDispatched("stopping".into());
        };
        let sent = match &effect.payload {
            EffectPayload::Prompt { input } | EffectPayload::FollowUp { input } => {
                // The start checkpoint is published before the write, under
                // the workspace fence (PLAN §6.6).
                let fence = turns.fence().await;
                turns.begin_input(&fence, &input.input_id).await;
                turns.input_sent(&input.input_id);
                let sent = if matches!(effect.payload, EffectPayload::Prompt { .. }) {
                    session.prompt(host_input(input)).await
                } else {
                    session.follow_up(host_input(input)).await
                };
                if let Err(e) = &sent
                    && matches!(classify(e), Dispatch::NotDispatched(_))
                {
                    turns.input_not_sent(&input.input_id).await;
                }
                sent
            }
            EffectPayload::Steer { input } => {
                // A due completion capture runs before the steer can write.
                let _fence = turns.fence().await;
                session.steer(host_input(input)).await
            }
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
            EffectPayload::Revert { revert_id, .. } => {
                let env = super::revert_run::RevertEnv {
                    store: &self.store,
                    git: &self.git,
                    limits: self.limits,
                };
                return super::revert_run::run(env, &turns, session.as_mut(), &process, revert_id)
                    .await;
            }
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
            EffectPayload::Start {
                spec,
                prompt,
                workspace,
            } => {
                // The Git work of the start (M3 6-1, DL-200) runs in the
                // effect, before the provider starts.
                if let Some(plan) = workspace
                    && let Err(e) =
                        crate::workspace::provision::provision(plan, &self.git.program).await
                {
                    let text = format!("the workspace was not created: {e}");
                    self.start_failed(&effect.session_id, &text).await;
                    return Dispatch::NotDispatched("workspace_failed".into());
                }
                self.start(effect, spec, prompt.as_ref(), None).await
            }
            EffectPayload::Resume {
                native_session_id,
                spec,
                prompt,
                ..
            } => {
                if let Some(code) =
                    super::auto_continue::recheck(&self.store, self.policy.as_ref(), effect).await
                {
                    return Dispatch::NotDispatched(code);
                }
                self.start(effect, spec, prompt.as_ref(), Some(native_session_id))
                    .await
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
    turns: Arc<SessionTurns>,
    mut rx: mpsc::Receiver<fleetdeck_providers::adapter::Emitted>,
    ready: oneshot::Receiver<ProcessIncarnation>,
    coordinator: crate::coordinator::Bindings,
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
        let observed: Vec<Event> = batch.iter().map(|e| e.event.clone()).collect();
        let events = batch.into_iter().map(NewEvent::from).collect();
        if let Err(e) = store.append(session_id.clone(), events).await {
            tracing::error!(error = %e, session = %session_id, "events not stored; closing the session");
            break;
        }
        // The events are in the log first: `turn.completed` never waits for
        // a checkpoint (L13).
        let bound = super::coordinator_start::bound_events(&coordinator, &session_id, &observed);
        if !bound.is_empty()
            && let Err(e) = store.append(session_id.clone(), bound).await
        {
            tracing::error!(error = %e, "coordinator binding facts not stored; closing the session");
            break;
        }
        if turns.observe(&observed).await {
            tokio::spawn(Arc::clone(&turns).capture_due());
        }
        if ended {
            break;
        }
    }
    drop(rx);
    // The process is gone: its coordinator credential ends with it (§5.5).
    let gone = super::coordinator_start::ended_events(&coordinator, &session_id);
    if !gone.is_empty()
        && let Err(e) = store.append(session_id.clone(), gone).await
    {
        tracing::error!(error = %e, "coordinator binding end not stored");
    }
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

#[path = "shim_target.rs"]
mod shim_target;

/// The dispatch reason of a resume on the shim host (DL-118).
pub const REASON_RESUME_UNSUPPORTED: &str = "resume_not_supported_on_the_shim_host";

pub use shim_target::{REASON_REVERT_UNSUPPORTED, ShimSettings};

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
