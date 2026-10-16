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
use std::sync::{Arc, Mutex as StdMutex, Weak};

use async_trait::async_trait;
use fleetdeck_core::{SharedIds, new_id};
use fleetdeck_proto::command::{SessionSpec, StopMode};
use fleetdeck_proto::event::ProviderKind;
use fleetdeck_proto::{ProviderInstanceId, SessionId};
use fleetdeck_providers::host::HostError;
use fleetdeck_providers::{EventSink, LiveSession, ProviderAdapter, ProviderError};
use tokio::sync::{Mutex, oneshot, watch};

use super::auto_continue::recheck;
use super::coordinator_start::{Tools, tools_for};
use super::durable::host_input;
pub use super::shells_slot::ShellsSlot;
use super::turns::SessionTurns;
use crate::history::subagent::LaunchFacts;
use crate::store::{Dispatch, Effect, EffectPayload, InputRecord, ProcessIncarnation, Store};
use crate::worker::EffectTarget;
use crate::workspace::setup::STOPPED_MESSAGE;
use crate::workspace::{GitOptions, Limits};

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
    /// Right before the dispatch recheck of a resume on the shim host
    /// (tests only, DL-182 §3).
    #[cfg(test)]
    resume_dispatch: setup_task::StartHook,
    /// Right before the authority gate of a start, and after the awaited
    /// preparation of a durable launch or write (tests and fault-injection
    /// builds only).
    #[cfg(any(test, feature = "fault-injection"))]
    gate_hook: GateHook,
    /// Before the launch identity of a start (tests and fault-injection, B9).
    #[cfg(any(test, feature = "fault-injection"))]
    launch_hook: GateHook,
    /// The `fleetdeck` executable that confines provisioning Git (B9).
    confine_exe: std::sync::OnceLock<std::path::PathBuf>,
    /// The instances whose probed version is on the adapter's deny list
    /// (R8.2, DL-162): their starts are refused.
    broken: Arc<StdMutex<BTreeMap<ProviderInstanceId, &'static str>>>,
    /// The shells of the station, for the file work of the worktree trash.
    shells: ShellsSlot,
    /// The restart freeze (PRE-PR r1).
    pub(super) restart: Arc<super::restart_gate::RestartState>,
    /// The running `!` shell tasks of Claude sessions (B8).
    shell: shell_run::ShellRunState,
}

impl SessionTarget {
    /// The workspace fences of the station (B8: the shell advance takes the
    /// fence of a root like a turn does).
    pub(super) fn fences(&self) -> &super::turns::Fences {
        &self.fences
    }

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
            #[cfg(test)]
            resume_dispatch: setup_task::StartHook::default(),
            #[cfg(any(test, feature = "fault-injection"))]
            gate_hook: GateHook::default(),
            #[cfg(any(test, feature = "fault-injection"))]
            launch_hook: GateHook::default(),
            confine_exe: std::sync::OnceLock::new(),
            shells: ShellsSlot::default(),
            broken: Arc::default(),
            restart: Arc::default(),
            shell: shell_run::ShellRunState::default(),
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

    /// The point where a test holds a resume on the shim host, right
    /// before its recheck of the termination evidence.
    #[cfg(test)]
    pub(super) fn resume_dispatch_hook(&self) -> &setup_task::StartHook {
        &self.resume_dispatch
    }

    /// The point where a test holds an effect right before its authority
    /// gate.
    #[cfg(any(test, feature = "fault-injection"))]
    pub(in crate::manager) fn gate_hook(&self) -> &GateHook {
        &self.gate_hook
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

    /// The live sessions with their provider pid (R13 §10). A busy lock or
    /// a session with no process gives `None`.
    #[must_use]
    pub fn live_pids(&self) -> Vec<(SessionId, Option<u32>)> {
        use fleetdeck_providers::host::ProcessControl as _;
        let Ok(map) = self.sessions.lock() else {
            return Vec::new();
        };
        let pid = |live: &Live| {
            let direct = live.session.as_ref().and_then(|s| s.pid());
            direct.or_else(|| live.durable.as_ref().map(|d| d.driver.process().pid()))
        };
        map.iter()
            .map(|(id, live)| (id.clone(), live.try_lock().ok().and_then(|l| pid(&l))))
            .collect()
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

    /// Where the provider of a launch writes its files, from the current
    /// policy snapshot (DL-191); `None` without an absolute provider home.
    pub(super) fn launch_facts(&self, spec: &SessionSpec) -> Option<LaunchFacts> {
        let kind = self.adapters.get(&spec.provider_instance)?.kind();
        let policy = self.current_policy();
        LaunchFacts::of(policy.as_deref(), &spec.provider_instance, kind, &spec.cwd)
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
        // No provider launch without an identity (B9).
        if let Some((code, text)) = launch::launch_refusal(spec) {
            self.start_failed(&session_id, text).await;
            return Dispatch::NotDispatched(code.into());
        }
        if let Some(reason) = self.broken_reason(&spec.provider_instance) {
            self.start_failed(&session_id, &reason).await;
            return Dispatch::NotDispatched("provider_broken".into());
        }
        let Some(adapter) = self.adapters.get(&spec.provider_instance).cloned() else {
            self.start_failed(&session_id, "the provider instance has no adapter")
                .await;
            return Dispatch::NotDispatched("unknown_provider".into());
        };
        let fleet_only = spec.coordinator.is_none();
        // The durable launch comes first: it says which host the tools are
        // for (DL-194).
        let durable = resume
            .is_none()
            .then(|| self.durable_launch(&adapter, spec))
            .flatten();
        let shim_host = (resume.is_some() && self.shims.is_some()) || durable.is_some();
        let (bridge, coordinator) = match tools_for(
            &self.coordinator,
            adapter.kind(),
            &session_id,
            spec,
            shim_host,
            resume.is_some(),
        ) {
            Ok(Tools::Direct(launch)) => (None, Some(launch)),
            Ok(Tools::Bridge(start)) => (Some(start), None),
            Ok(Tools::None) => (None, None),
            Err(why) => {
                self.start_failed(&session_id, &why).await;
                return Dispatch::NotDispatched(format!("coordinator: {why}"));
            }
        };
        if let (Some(native), Some(shims)) = (resume, self.shims.clone()) {
            // A resume on the shim host (DL-182): a new shim and a new
            // provider process, never a reattachment.
            return self
                .resume_durable(
                    effect, spec, prompt, native, &adapter, &shims, bridge, cancel,
                )
                .await;
        }
        if let Some((shims, launch)) = durable {
            return self
                .start_durable(effect, spec, prompt, &shims, launch, None, bridge, cancel)
                .await;
        }
        let turns = self.open_turns(&session_id, spec).await;
        let (sink, rx) = EventSink::channel(SINK_CAPACITY);
        let (ready_tx, ready_rx) = oneshot::channel();
        tokio::spawn(pump::pump(
            self.store.clone(),
            Arc::clone(&self.sessions),
            session_id.clone(),
            Arc::clone(&turns),
            rx,
            ready_rx,
            self.coordinator.clone(),
            self.launch_facts(spec),
        ));
        if setup_task::cancelled(cancel) {
            // A stop came during the setup: no provider is spawned.
            drop(ready_tx);
            self.coordinator.revoke_session(&session_id);
            self.start_failed(&session_id, STOPPED_MESSAGE).await;
            return Dispatch::NotDispatched(REASON_SETUP_STOPPED.into());
        }
        // ADR-008 §6.2: after every awaited step, right before the spawn.
        #[cfg(any(test, feature = "fault-injection"))]
        self.gate_hook.hold().await;
        if let Some(refused) = self.gate_now(effect).await {
            drop(ready_tx);
            self.coordinator.revoke_session(&session_id);
            return refused;
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
        // A folder change after `session.started` stops Codex (B9 §3.5).
        if adapter.kind() == ProviderKind::Codex
            && let Err(e) = self.launch_still_checked(spec).await
        {
            drop(ready_tx);
            self.coordinator.revoke_session(&session_id);
            let _ = session.stop(StopMode::Kill).await;
            return self.launch_changed(&session_id, e).await;
        }
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
        let mut refused_input = None;
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
            } else if let Some(refused) = self.gate_input(effect).await {
                // ADR-008 §6.2: the authority ended before the first input.
                turns.input_not_sent(&input.input_id).await;
                refused_input = Some(refused);
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
        } else if let Some(refused) = refused_input {
            if let Err(e) = self
                .store
                .first_input_failed(effect.effect_id.clone(), refused)
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
        // B8: a Claude shell command is station work; a Codex one goes
        // through the provider after the station checks of its own.
        match &effect.payload {
            EffectPayload::ShellCommand {
                provider: ProviderKind::Codex,
                ..
            } => self.dispatch_codex_shell(effect).await,
            EffectPayload::ShellCommand { .. } => self.dispatch_shell(effect),
            _ => self.operate_provider(effect).await,
        }
    }

    /// One claimed effect against the live provider session: the process
    /// check, the gate, then the durable or the direct write.
    pub(super) async fn operate_provider(&self, effect: &Effect) -> Dispatch {
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
        // ADR-008 §6.2: with the session lock held, before the write. A
        // direct input gates itself after its fence and checkpoint work. A
        // durable effect is gated here too (the first of its gates): the
        // driver then gates again under its core lock, where the state
        // change is made (RE-REVIEW round 3).
        let durable = live.durable.is_some();
        if (durable || !target_base::gates_itself(&effect.payload))
            && let Some(refused) = self.gate_now(effect).await
        {
            if durable
                && let EffectPayload::Prompt { input, .. } | EffectPayload::FollowUp { input, .. } =
                    &effect.payload
            {
                live.turns.input_not_sent(&input.input_id).await;
            }
            return refused;
        }
        if let Some(durable) = live.durable.as_ref() {
            let turns = Arc::clone(&live.turns);
            return self.operate_durable(effect, durable, &turns).await;
        }
        self.operate_direct(effect, &mut live).await
    }
}

#[async_trait]
impl EffectTarget for SessionTarget {
    fn begin_claim(&self) -> crate::worker::Claim {
        self.restart.begin_claim()
    }

    async fn dispatch(&self, effect: &Effect) -> Dispatch {
        // A stop of a start that waits for its setup command has no process
        // incarnation; it cancels the task and never reaches `operate`
        // (DL-148). Its authority is checked here (ADR-008 §6.2).
        if matches!(effect.payload, EffectPayload::Stop { .. })
            && effect.process_incarnation.is_none()
            && let Some(refused) = self.gate_now(effect).await
        {
            return refused;
        }
        match &effect.payload {
            EffectPayload::Start {
                spec,
                prompt,
                workspace,
                setup_cmd,
                ..
            } => {
                // The queued start is checked again against the current
                // policy (DL-172 §1.4).
                if let Some(why) = self.current_policy().and_then(|p| {
                    p.start_refusal(&spec.cwd, workspace.is_some(), setup_cmd.is_some())
                }) {
                    self.start_failed(&effect.session_id, why).await;
                    return Dispatch::NotDispatched("policy_changed".into());
                }
                // A worktree or a clone takes its identity after the Git work (B9).
                let mut launch = None;
                if workspace.is_none() {
                    match self.take_launch(effect, spec).await {
                        Ok(l) => launch = Some(l),
                        Err(refused) => return refused,
                    }
                }
                let spec = launch.as_ref().map_or(spec, |l| &l.spec);
                // The base commit is read before the worktree is made (R1.1).
                let wants_base = !self.has_base(&effect.session_id).await;
                let (plan, held) = (workspace.as_ref(), launch.as_ref());
                let planned = match self.start_base(effect, plan, spec, held, wants_base).await {
                    Ok(planned) => planned,
                    Err(refused) => return refused,
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
                import,
                transcript,
                ..
            } => {
                let (policy, boot) = (self.current_policy(), self.boot_reader());
                if let Some(code) =
                    recheck(&self.store, policy.as_deref(), boot.as_ref(), effect).await
                {
                    return Dispatch::NotDispatched(code);
                }
                let launch = match self.take_launch(effect, spec).await {
                    Ok(l) => l,
                    Err(refused) => return refused,
                };
                // The transcript is copied before the provider can write to it
                // again. A failure is recorded and never stops the import
                // (DL-189).
                if *import {
                    // Before any launch: a pinned import verifies its
                    // transcript, and a Codex child thread fails the start
                    // (B7).
                    if let Err(refusal) = crate::history::import_check::before_import(
                        &self.store,
                        policy.as_deref(),
                        &self.private,
                        &effect.session_id,
                        native_session_id,
                        &spec.cwd,
                        transcript.as_ref(),
                    )
                    .await
                    {
                        self.start_failed(&effect.session_id, refusal.message())
                            .await;
                        return Dispatch::NotDispatched(refusal.code().into());
                    }
                    crate::history::at_import(
                        &self.store,
                        policy.as_deref(),
                        &self.private,
                        &effect.session_id,
                        native_session_id,
                    )
                    .await;
                }
                // An import takes the `HEAD` now; a resume of a session
                // without a base guesses it (R1.1).
                let (cwd, spec) = (&launch.pinned, &launch.spec);
                let base = if self.has_base(&effect.session_id).await {
                    None
                } else if *import {
                    crate::workspace::base::at_head(&self.git, cwd).await
                } else {
                    crate::workspace::base::inferred(&self.git, cwd).await
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
            EffectPayload::Stop { how, .. } if self.cancel_setup(&effect.session_id, *how) => {
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

#[path = "pump.rs"]
mod pump;

#[path = "shim_target.rs"]
mod shim_target;

#[path = "setup_task.rs"]
mod setup_task;

#[path = "shell_run.rs"]
mod shell_run;

#[path = "shell_codex.rs"]
mod shell_codex;

#[path = "target_base.rs"]
mod target_base;

#[cfg(any(test, feature = "fault-injection"))]
#[path = "target_gate.rs"]
mod gate;
#[cfg(any(test, feature = "fault-injection"))]
pub use gate::GateHook;
#[path = "target_launch.rs"]
mod launch;
#[cfg(test)]
#[path = "launch_tests.rs"]
mod launch_tests;

#[path = "target_spawn.rs"]
mod target_spawn;

#[cfg(test)]
#[path = "target_spawn_tests.rs"]
mod target_spawn_tests;

/// The dispatch reason of a start that a stop cancelled during its setup.
pub const REASON_SETUP_STOPPED: &str = "setup_stopped";

pub use shim_target::{REASON_REVERT_UNSUPPORTED, ShimSettings};

#[cfg(test)]
#[path = "target_classify_tests.rs"]
mod classify_tests;
