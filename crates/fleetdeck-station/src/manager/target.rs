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
use std::sync::{Arc, Mutex as StdMutex, Weak};

use async_trait::async_trait;
use fleetdeck_core::{SharedIds, new_id};
use fleetdeck_proto::command::{SessionSpec, StopMode};
use fleetdeck_proto::event::ProviderKind;
use fleetdeck_proto::{Event, ProviderInstanceId, SessionId};
use fleetdeck_providers::host::HostError;
use fleetdeck_providers::{
    Answer, ControlId, EventSink, LiveSession, ModelId, PermissionMode, ProviderAdapter,
    ProviderError,
};
use tokio::sync::{Mutex, mpsc, oneshot, watch};

use super::durable::host_input;
pub use super::shells_slot::ShellsSlot;
use super::turns::SessionTurns;
use crate::store::{
    Dispatch, Effect, EffectPayload, InputRecord, NewEvent, ProcessIncarnation, Store,
};
use crate::worker::EffectTarget;
use crate::workspace::setup::STOPPED_MESSAGE;
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
    /// The private paths of the station, for the turn checkpoints and the
    /// reverts (FDV2-003). The default refuses every path (fail closed).
    private: Arc<crate::workspace::PrivatePaths>,
    fences: super::turns::Fences,
    shims: Option<shim_target::ShimSettings>,
    /// The coordinator bindings that a coordinator start adds to (DL-216).
    coordinator: crate::coordinator::Bindings,
    /// The station policy for the dispatch recheck of an auto-continue
    /// (ADR-007 §2.4 rules 2 and 10, DL-133).
    policy: Option<Arc<super::policy_hub::PolicyHub>>,
    /// The starts that wait for their setup command or workspace, by session
    /// (DL-148).
    setups: setup_task::SetupTasks,
    /// This target as an `Arc`, for the tasks that outlive one dispatch;
    /// empty until [`Self::shared`].
    this: Weak<SessionTarget>,
    /// The points where a test holds a start (tests only).
    #[cfg(test)]
    hooks: setup_task::Hooks,
    /// The instances whose probed version is on the adapter's deny list
    /// (R8.2, DL-162): their starts are refused.
    broken: Arc<StdMutex<BTreeMap<ProviderInstanceId, &'static str>>>,
    /// The shells of the station, for the file work of the worktree trash.
    shells: ShellsSlot,
    /// The restart freeze (PRE-PR r1).
    pub(super) restart: Arc<super::restart_gate::RestartState>,
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
            private: Arc::new(crate::workspace::PrivatePaths::deny_all()),
            fences: super::turns::Fences::default(),
            shims: None,
            coordinator: crate::coordinator::Bindings::default(),
            policy: None,
            setups: Arc::default(),
            this: Weak::new(),
            #[cfg(test)]
            hooks: setup_task::Hooks::default(),
            shells: ShellsSlot::default(),
            broken: Arc::default(),
            restart: Arc::default(),
        }
    }

    /// Refuses the starts of `instance`: its probed version is on the deny
    /// list of its adapter (R8.2, DL-162).
    pub fn mark_broken(&self, instance: &ProviderInstanceId, kind: ProviderKind) {
        let name: &'static str = match kind {
            ProviderKind::Claude => "claude",
            ProviderKind::Codex => "codex",
            ProviderKind::Unknown => "provider",
        };
        if let Ok(mut broken) = self.broken.lock() {
            broken.insert(instance.clone(), name);
        }
    }

    /// The refusal text of a start on a broken instance, if it is one.
    fn broken_reason(&self, instance: &ProviderInstanceId) -> Option<String> {
        let broken = self.broken.lock().ok()?;
        broken
            .get(instance)
            .map(|name| format!("this {name} version is known to break the protocol"))
    }

    /// The points where a test holds a start.
    #[cfg(test)]
    pub(super) fn hooks(&self) -> &setup_task::Hooks {
        &self.hooks
    }

    /// The target in an `Arc` that knows itself. A start with a setup command
    /// or a workspace needs it: the setup and the workspace work run in a
    /// task after the dispatch returned.
    #[must_use]
    pub fn shared(mut self) -> Arc<Self> {
        Arc::new_cyclic(|weak| {
            self.this = weak.clone();
            self
        })
    }

    /// The slot that holds the shells of the station (DL-150).
    #[must_use]
    pub fn with_shells(mut self, shells: ShellsSlot) -> Self {
        self.shells = shells;
        self
    }

    /// The live station policy that the dispatch rechecks use: a queued
    /// start, an auto-continue (ADR-007 §2.4 rules 2 and 10, DL-133) and
    /// the file work of a trash. Each dispatch reads the current snapshot
    /// (DL-172 §1.4).
    #[must_use]
    pub fn with_policy(mut self, hub: Arc<super::policy_hub::PolicyHub>) -> Self {
        self.policy = Some(hub);
        self
    }

    /// The current policy snapshot, once the target has a hub.
    pub(super) fn current_policy(&self) -> Option<Arc<super::policy::Policy>> {
        self.policy.as_ref().map(|hub| hub.current())
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

    /// Stops every live session (station shutdown). First it stops the setup
    /// commands that run and waits for their tasks, up to 15 s.
    pub async fn stop_all(&self, how: StopMode) {
        self.stop_setups(how).await;
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
        cancel: Option<&watch::Receiver<bool>>,
    ) -> Dispatch {
        let session_id = effect.session_id.clone();
        if let Some(reason) = self.broken_reason(&spec.provider_instance) {
            self.start_failed(&session_id, &reason).await;
            return Dispatch::NotDispatched("provider_broken".into());
        }
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
        let fleet_only = spec.coordinator.is_none();
        let coordinator = match spec.coordinator.as_deref() {
            // A session without a grant gets the session's own tools when
            // its provider can take the handoff (DL-165).
            None => match super::coordinator_start::prepare_fleet(
                &self.coordinator,
                adapter.kind(),
                &session_id,
                spec,
                self.shims.is_some(),
            ) {
                Ok(launch) => Some(launch),
                Err(why) => {
                    super::coordinator_start::log_no_tools(&session_id, adapter.kind(), &why);
                    None
                }
            },
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
                .start_durable(effect, spec, prompt, &shims, launch, cancel)
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
            Arc::clone(&self.private),
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
        if setup_task::cancelled(cancel) {
            // A stop came during the setup: no provider is spawned.
            drop(ready_tx);
            self.coordinator.revoke_session(&session_id);
            self.start_failed(&session_id, STOPPED_MESSAGE).await;
            return Dispatch::NotDispatched(REASON_SETUP_STOPPED.into());
        }
        let started = target_spawn::Spawn {
            bindings: &self.coordinator,
            session_id: &session_id,
            spec,
            resume,
            launch: coordinator,
            fleet_only,
        }
        .run(&adapter, sink)
        .await;
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
        #[cfg(test)]
        self.hooks.input.hold().await;
        // A stop that came during the setup: the first input is not sent.
        let mut cancelled_input = prompt.is_some() && setup_task::cancelled(cancel);
        if let Some(input) = prompt.filter(|_| !cancelled_input) {
            let fence = turns.fence().await;
            turns.begin_input(&fence, &input.input_id).await;
            #[cfg(test)]
            self.hooks.checkpoint.hold().await;
            // The fence and the checkpoint work can wait for long: the last
            // check is right before the delivery, with no await between.
            if setup_task::cancelled(cancel) {
                turns.input_not_sent(&input.input_id).await;
                cancelled_input = true;
            } else {
                turns.input_sent(&input.input_id);
                sent_prompt = session.prompt(host_input(input)).await;
                if let Err(e) = &sent_prompt
                    && matches!(classify(e), Dispatch::NotDispatched(_))
                {
                    turns.input_not_sent(&input.input_id).await;
                }
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
        if cancelled_input {
            let not_sent = Dispatch::NotDispatched(REASON_SETUP_STOPPED.into());
            if let Err(e) = self
                .store
                .first_input_failed(effect.effect_id.clone(), not_sent)
                .await
            {
                tracing::error!(error = %e, "the first input failure was not stored");
            }
        } else if let Err(e) = sent_prompt {
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

    async fn operate(&self, effect: &Effect) -> Dispatch {
        let Some(live) = self.live(&effect.session_id) else {
            return Dispatch::NotDispatched("no_process".into());
        };
        let mut live = live.lock().await;
        // A stop of a start that waited for its setup command has no
        // incarnation: it stops the process that is live now (DL-148).
        let unbound_stop = effect.process_incarnation.is_none()
            && matches!(effect.payload, EffectPayload::Stop { .. });
        if !unbound_stop && effect.process_incarnation.as_ref() != Some(&live.process) {
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
                    private: &self.private,
                };
                return super::revert_run::run(env, &turns, session.as_mut(), &process, revert_id)
                    .await;
            }
            EffectPayload::Start { .. }
            | EffectPayload::Resume { .. }
            | EffectPayload::Worktree { .. }
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
    fn begin_claim(&self) -> crate::worker::Claim {
        self.restart.begin_claim()
    }

    async fn dispatch(&self, effect: &Effect) -> Dispatch {
        match &effect.payload {
            EffectPayload::Start {
                spec,
                prompt,
                workspace,
                setup_cmd,
            } => {
                // The queued start is checked again against the current
                // policy (DL-172 §1.4).
                if let Some(why) = self.current_policy().and_then(|p| {
                    p.start_refusal(&spec.cwd, workspace.is_some(), setup_cmd.is_some())
                }) {
                    self.start_failed(&effect.session_id, why).await;
                    return Dispatch::NotDispatched("policy_changed".into());
                }
                // The base commit is read before the worktree is made (R1.1).
                let wants_base = !self.has_base(&effect.session_id).await;
                let planned = if wants_base {
                    crate::workspace::base::planned(&self.git, workspace.as_ref(), &spec.cwd).await
                } else {
                    None
                };
                // The Git work of the start (M3 6-1, DL-200) and its setup
                // command run in a task of their own: the effect worker
                // goes on with the other sessions.
                if workspace.is_some() || setup_cmd.is_some() {
                    return self
                        .defer_start(
                            effect,
                            spec,
                            prompt.as_ref(),
                            workspace.as_ref(),
                            setup_cmd.as_deref(),
                            wants_base,
                            planned,
                        )
                        .await;
                }
                let started = self.start(effect, spec, prompt.as_ref(), None, None).await;
                if matches!(started, Dispatch::Confirmed | Dispatch::Deferred) {
                    self.record_base(&effect.session_id, planned).await;
                }
                started
            }
            EffectPayload::Resume {
                native_session_id,
                spec,
                prompt,
                ..
            } => {
                let policy = self.current_policy();
                if let Some(code) =
                    super::auto_continue::recheck(&self.store, policy.as_deref(), effect).await
                {
                    return Dispatch::NotDispatched(code);
                }
                // An import or a resume of a session without a base (R1.1).
                let base = if self.has_base(&effect.session_id).await {
                    None
                } else {
                    crate::workspace::base::inferred(&self.git, Path::new(&spec.cwd)).await
                };
                let started = self
                    .start(effect, spec, prompt.as_ref(), Some(native_session_id), None)
                    .await;
                if matches!(started, Dispatch::Confirmed | Dispatch::Deferred) {
                    self.record_base(&effect.session_id, base).await;
                }
                started
            }
            // A stop during the setup command or the workspace work cancels it; the task ends
            // the session.
            EffectPayload::Stop { how } if self.cancel_setup(&effect.session_id, *how) => {
                Dispatch::Confirmed
            }
            // The file work of a worktree trash needs no provider process
            // (DL-150): it runs before the target lookup.
            EffectPayload::Worktree { op_id, action } => match self.current_policy() {
                Some(policy) => {
                    let shells = self.shells.get();
                    crate::workspace::trash::run(
                        &self.store,
                        &policy,
                        &self.git,
                        shells.as_deref(),
                        op_id,
                        *action,
                    )
                    .await
                }
                None => Dispatch::NotDispatched("no_policy".into()),
            },
            _ => self.operate(effect).await,
        }
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
        crate::artifacts::suggest::observe(&store, &session_id, &observed).await;
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

#[path = "setup_task.rs"]
mod setup_task;

#[path = "target_base.rs"]
mod target_base;

#[path = "target_spawn.rs"]
mod target_spawn;

#[cfg(test)]
#[path = "target_spawn_tests.rs"]
mod target_spawn_tests;

/// The dispatch reason of a start that a stop cancelled during its setup.
pub const REASON_SETUP_STOPPED: &str = "setup_stopped";

/// The dispatch reason of a resume on the shim host (DL-118).
pub const REASON_RESUME_UNSUPPORTED: &str = "resume_not_supported_on_the_shim_host";

pub use shim_target::{REASON_REVERT_UNSUPPORTED, ShimSettings};

#[cfg(test)]
#[path = "target_classify_tests.rs"]
mod classify_tests;
