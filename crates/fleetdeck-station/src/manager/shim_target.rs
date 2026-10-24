//! Sessions under shims in the session manager's effect target (ADR-005;
//! DL-113).
//!
//! With [`ShimSettings`] the target launches a session whose adapter has a
//! durable driver through a shim ([`super::super::shims::launch_shim`]) and
//! drives it with the durable driver: the driver commits the provider's
//! events itself (with the adapter state and the cursor), so an observer
//! task feeds the committed events of the session to its turn checkpoints
//! ([`SessionTurns`]) and ends the process when the session ends. Inputs
//! are written under the same workspace fence as with the direct host, so
//! the start checkpoint is published before the input (PLAN §6.6).
//!
//! A session under a shim survives a station stop: [`SessionTarget::stop_all`]
//! does not stop it, and the next station start restores it
//! ([`super::super::shim_station::recover_station`], then
//! [`SessionTarget::adopt`]).
//!
//! Revert (DL-116): a Codex session under a shim runs the same revert
//! executor as with the direct host ([`super::super::revert_run::run`]),
//! with the durable driver's `thread/rollback` as its conversation
//! rollback. A provider without a durable rollback (Claude has no
//! conversation rollback, so its revert is refused at admission) fails
//! the revert with [`REASON_REVERT_UNSUPPORTED`] before any I/O.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use fleetdeck_core::new_id;
use fleetdeck_proto::command::{SessionSpec, StopMode};
use fleetdeck_proto::{Event, EventBody, SessionId};
use fleetdeck_providers::ProviderAdapter;
use fleetdeck_providers::durable::{DurableConfig, DurableLaunch};
use fleetdeck_providers::host::shim::host::{AckMode, ShimHost, ShimProcess};
use fleetdeck_providers::host::{DEFAULT_IDLE_GRACE, ProcessControl as _, StopPolicy};
use tokio::sync::Mutex;

use super::super::binding_rotate::rotate_out;
use super::super::coordinator_start::BridgeStart;
use super::super::shim_claude::DurableClaude;
use super::super::shim_codex::DurableCodex;
use super::super::shim_codex_poll::{BACKGROUND_POLL, spawn_background_poll};
use super::super::shim_station::Durable;
use super::super::shims::{self, LaunchError, launch_shim_for, reason};
use super::super::turns::SessionTurns;
use super::apps::{AppHosts, HostEnd};
use super::{Live, REASON_PROCESS_EXIT, SessionTarget, Sessions};
use crate::history::subagent::LaunchFacts;
use crate::store::resumes::Purpose;
use crate::store::{
    Dispatch, Effect, EffectPayload, InputRecord, ProcessIncarnation, ReadResult, RevertStep,
    ShimState, Store,
};

/// The reason of a revert effect on a session under a shim.
pub const REASON_REVERT_UNSUPPORTED: &str = "revert_not_supported_on_the_shim_host";
/// Events that the observer reads in one call.
const OBSERVE_BATCH: usize = 256;

/// The shim host of a station and its recovery wait.
#[derive(Debug, Clone)]
pub struct ShimSettings {
    pub host: Arc<ShimHost>,
    /// How long recovery asks a shim about an in-flight stdin write.
    pub in_flight_wait: Duration,
    /// How long a revert waits for the provider's rollback answer.
    pub rollback_timeout: Duration,
}

impl ShimSettings {
    /// The settings with the default waits.
    #[must_use]
    pub fn new(host: Arc<ShimHost>) -> Self {
        Self {
            host,
            in_flight_wait: super::super::shims::IN_FLIGHT_WAIT,
            rollback_timeout: fleetdeck_providers::codex::ROLLBACK_TIMEOUT,
        }
    }
}

/// The durable Codex driver's `thread/rollback` as a revert's
/// conversation rollback.
struct CodexRollback<'a> {
    driver: &'a DurableCodex,
    timeout: Duration,
}

#[async_trait::async_trait]
impl super::super::revert_run::Rollback for CodexRollback<'_> {
    async fn roll_back(&mut self, turns: u32) -> Result<(), fleetdeck_providers::ProviderError> {
        self.driver.rollback(turns, self.timeout).await
    }
}

/// The durable side of a live session.
pub(super) struct DurableLive {
    pub(super) driver: Durable,
    pub(super) stop: StopPolicy,
    /// The session's event observer; a station stop aborts it.
    pub(super) observer: tokio::task::AbortHandle,
}

impl SessionTarget {
    /// Launches new sessions through shims when their adapter has a durable
    /// driver (see the module documentation).
    #[must_use]
    pub fn with_shims(mut self, shims: ShimSettings) -> Self {
        self.shims = Some(shims);
        self
    }

    /// The private paths of the station (FDV2-003): a turn checkpoint and a
    /// revert never read or write a private file.
    #[must_use]
    pub fn with_private(mut self, private: Arc<crate::workspace::PrivatePaths>) -> Self {
        self.private = private;
        self
    }

    /// The durable launch of `spec`, if this target hosts shims and the
    /// adapter has a durable driver.
    pub(super) fn durable_launch(
        &self,
        adapter: &Arc<dyn ProviderAdapter>,
        spec: &SessionSpec,
    ) -> Option<(ShimSettings, DurableLaunch)> {
        let shims = self.shims.clone()?;
        adapter.durable_launch(spec).ok().map(|l| (shims, l))
    }

    async fn turns(&self, session_id: &SessionId, cwd: &Path) -> Arc<SessionTurns> {
        SessionTurns::open(
            self.store.clone(),
            Arc::clone(&self.ids),
            self.git.clone(),
            self.limits,
            &self.fences,
            session_id.clone(),
            Some(cwd),
            None,
            Arc::clone(&self.private),
        )
        .await
    }

    // The session, the process, the turns, the driver, the stop policy, the
    // cursor and the launch facts are all that the observer needs.
    #[allow(clippy::too_many_arguments)]
    fn insert_durable(
        &self,
        session_id: &SessionId,
        process: ProcessIncarnation,
        turns: Arc<SessionTurns>,
        driver: Durable,
        stop: StopPolicy,
        from: u64,
        launch: Option<LaunchFacts>,
    ) {
        // The observer is spawned while this lock is held: its removal of
        // the entry at the session's end takes the same lock, so it never
        // runs before the entry exists.
        let Ok(mut map) = self.sessions.lock() else {
            return;
        };
        // The app host and the sink are installed under the same lock: the
        // observer drops the host only after it took the lock (B15 §4.3).
        self.attach_durable_apps(session_id, &process, &turns, &driver);
        let observer = tokio::spawn(observe(
            self.store.clone(),
            Arc::clone(&self.sessions),
            self.apps.hosts(),
            session_id.clone(),
            process.clone(),
            driver.process(),
            Arc::clone(&turns),
            from,
            Drain::ToTerminal,
            launch,
        ))
        .abort_handle();
        map.insert(
            session_id.clone(),
            Arc::new(Mutex::new(Live {
                process,
                session: None,
                turns,
                durable: Some(DurableLive {
                    driver,
                    stop,
                    observer,
                }),
            })),
        );
    }

    /// The sessions with a live process that a station restart ends: the
    /// ones that are not under a shim (DL-172 §1.6), sorted by id.
    pub(in crate::manager) async fn direct_sessions(&self) -> Vec<SessionId> {
        let all: Vec<_> = match self.sessions.lock() {
            Ok(s) => s
                .iter()
                .map(|(id, l)| (id.clone(), Arc::clone(l)))
                .collect(),
            Err(_) => return Vec::new(),
        };
        let mut out = Vec::new();
        for (id, live) in all {
            if live.lock().await.durable.is_none() {
                out.push(id);
            }
        }
        out.sort();
        out
    }

    /// A station stop leaves every session under a shim running: its
    /// driver and observer stop, its shim and provider do not (ADR-005).
    pub(super) async fn detach_durable(live: &mut Live) {
        if let Some(durable) = live.durable.as_ref() {
            durable.observer.abort();
            durable.driver.abandon().await;
        }
    }

    /// The boot id reader of the shim host, once the target hosts shims.
    #[must_use]
    pub fn boot_reader(&self) -> Option<fleetdeck_providers::host::shim::identity::BootIdReader> {
        self.shims
            .as_ref()
            .map(|s| Arc::clone(&s.host.config().boot_id_reader))
    }

    /// Why a resume or an import of `native` must not start a provider:
    /// another execution of the conversation may still run, or a reboot
    /// proof does not hold any more (DL-182 §3, §4.1). A store error
    /// blocks too.
    async fn resume_blocked(
        &self,
        session_id: &SessionId,
        kind: fleetdeck_proto::event::ProviderKind,
        native: &str,
        purpose: Purpose,
        shims: &ShimSettings,
    ) -> Option<String> {
        let found = self
            .store
            .execution_exclusion(
                Some(session_id.clone()),
                kind,
                Some(native.to_owned()),
                Some(session_id.clone()),
                purpose,
            )
            .await;
        let found = match found {
            Ok(found) => found,
            Err(e) => return Some(format!("store: {e}")),
        };
        if found.blocked.is_some() {
            return found.blocked;
        }
        shims::revalidate(&shims.host.config().boot_id_reader, &found.reboot_rows)
    }

    /// Resumes a conversation under a new shim (DL-182 §3): the termination
    /// evidence is read again at the dispatch, the adapter builds the
    /// durable resume launch, and the session starts like a new durable
    /// one, with the resume's prompt as its first input. There is no
    /// fallback to the direct host.
    // The dispatch needs the effect, the spec, the prompt, the native id,
    // the adapter, the settings and the cancel signal.
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn resume_durable(
        &self,
        effect: &Effect,
        spec: &SessionSpec,
        prompt: Option<&InputRecord>,
        native: &str,
        adapter: &Arc<dyn ProviderAdapter>,
        shims: &ShimSettings,
        tools: Option<BridgeStart>,
        cancel: Option<&tokio::sync::watch::Receiver<bool>>,
    ) -> Dispatch {
        let session_id = effect.session_id.clone();
        #[cfg(test)]
        self.resume_dispatch.hold().await;
        // An import fails closed for runs with an unknown conversation id.
        let purpose = match &effect.payload {
            EffectPayload::Resume { import: true, .. } => Purpose::Import,
            _ => Purpose::Resume,
        };
        if let Some(why) = self
            .resume_blocked(&session_id, adapter.kind(), native, purpose, shims)
            .await
        {
            self.start_failed(
                &session_id,
                &format!("the previous execution is not proven ended: {why}"),
            )
            .await;
            return Dispatch::NotDispatched("execution_not_ended".into());
        }
        let id = fleetdeck_providers::adapter::NativeSessionId(native.to_owned());
        let launch = match adapter.durable_resume_launch(spec, &id) {
            Ok(launch) => launch,
            Err(e) => {
                self.start_failed(&session_id, &e.to_string()).await;
                return Dispatch::NotDispatched(format!("start_failed: {e}"));
            }
        };
        self.start_durable(
            effect,
            spec,
            prompt,
            shims,
            launch,
            Some(native),
            tools,
            cancel,
        )
        .await
    }

    /// Starts a session under a shim and writes its first input. With
    /// `native`, the launch continues that conversation (a resume or an
    /// import); the row holds it before the spawn (DL-182 §3).
    // The launch needs the effect, the spec, the prompt, the settings, the
    // launch, the native target and the cancel signal.
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn start_durable(
        &self,
        effect: &Effect,
        spec: &SessionSpec,
        prompt: Option<&InputRecord>,
        shims: &ShimSettings,
        mut launch: DurableLaunch,
        native: Option<&str>,
        tools: Option<BridgeStart>,
        cancel: Option<&tokio::sync::watch::Receiver<bool>>,
    ) -> Dispatch {
        let session_id = effect.session_id.clone();
        // The skill root of a Codex session with tools (MCP P3 §4.2): the
        // station chose it when it prepared the launch (version gate).
        if let (DurableConfig::Codex { skill_dir, .. }, Some(t)) = (&mut launch.core, &tools) {
            *skill_dir = t.launch.skill_dir.clone();
        }
        if super::setup_task::cancelled(cancel) {
            // A stop came during the setup: no shim is launched.
            self.start_failed(&session_id, crate::workspace::setup::STOPPED_MESSAGE)
                .await;
            return Dispatch::NotDispatched(super::REASON_SETUP_STOPPED.into());
        }
        let turns = self.open_turns(&session_id, spec).await;
        let from = *self.store.subscribe().borrow();
        if let (Some(t), Some(_)) = (&tools, native)
            && let Err(text) =
                rotate_out(&self.store, &self.coordinator, &t.binding.binding_id).await
        {
            // The old token must stop first (DL-194 §2.3): no shim starts.
            self.start_failed(&session_id, &text).await;
            return Dispatch::NotDispatched("credentials".into());
        }
        // Whether the Codex `initialize` offers the MCP Apps extension (B15):
        // asked before the shim starts, so the cached probe never delays the
        // handshake.
        let offer_apps = matches!(launch.core, DurableConfig::Codex { .. })
            && self.offers_mcp_apps(&spec.provider_instance).await;
        // ADR-008 §6.2: the gate runs inside the launch, after the `starting`
        // row and right before the shim starts.
        let attached = match launch_shim_for(
            &self.store,
            &shims.host,
            &session_id,
            &launch.process,
            AckMode::Manual,
            native,
            &|| Box::pin(self.gate_durable(effect)),
            &self.coordinator,
            tools,
        )
        .await
        {
            Err(LaunchError::Refused(refused)) => return refused,
            Ok(attached) => attached,
            Err(LaunchError::Prepare(e)) => {
                self.start_failed(&session_id, &e.to_string()).await;
                return Dispatch::NotDispatched(format!("start_failed: {e}"));
            }
            Err(LaunchError::Settled { error, settled })
                if settled == reason::SHIM_NEVER_STARTED =>
            {
                self.start_failed(&session_id, &error).await;
                return Dispatch::NotDispatched(format!("start_failed: {error}"));
            }
            // The provider may have run: the start stays uncertain.
            Err(e) => return Dispatch::Unknown(e.to_string()),
        };
        let control = Arc::clone(&attached.process);
        let pid = attached.record.provider_identity.as_ref().map(|i| i.pid);
        let process: ProcessIncarnation = new_id(self.ids.as_ref());
        if let Err(e) = self
            .store
            .process_started(session_id.clone(), process.clone(), pid)
            .await
        {
            tracing::error!(error = %e, "the process could not be recorded; stopping it");
            let _report = control.kill().await;
            return Dispatch::Unknown(format!("store: {e}"));
        }
        turns.set_process(process.clone());
        let driver = match launch.core {
            DurableConfig::Claude(config) => DurableClaude::open(&self.store, attached, config)
                .await
                .map(Durable::Claude),
            DurableConfig::Codex {
                config,
                thread,
                skill_dir,
            } => {
                DurableCodex::open(
                    &self.store,
                    attached,
                    config,
                    &thread,
                    skill_dir.as_deref(),
                    offer_apps,
                )
                .await
                .map(|d| {
                    // The shim host polls the background work too
                    // (DL-024, DL-199).
                    spawn_background_poll(&d, BACKGROUND_POLL);
                    Durable::Codex(d)
                })
            }
        };
        let driver = match driver {
            Ok(driver) => driver,
            Err(e) => {
                // The driver's pump commits the exit of the killed provider;
                // the row ends with the shim's own evidence, so that the
                // failed launch does not block the next resume (DL-182 §6).
                let _report = control.kill().await;
                self.observe_end(&session_id, &process).await;
                let _ended = tokio::time::timeout(EXIT_REPORT_WAIT, control.wait()).await;
                end_rows(&self.store, &session_id, control.exit_state().as_ref()).await;
                return Dispatch::NotDispatched(format!("start_failed: {e}"));
            }
        };
        // A folder change after `session.started` stops Codex (B9 §3.5).
        if matches!(driver, Durable::Codex(_))
            && let Err(e) = self.launch_still_checked(spec).await
        {
            self.start_failed(&session_id, &e.to_string()).await;
            let _report = control.kill().await;
            self.observe_end(&session_id, &process).await;
            let _ended = tokio::time::timeout(EXIT_REPORT_WAIT, control.wait()).await;
            end_rows(&self.store, &session_id, control.exit_state().as_ref()).await;
            return Dispatch::NotDispatched(e.code().into());
        }
        self.insert_durable(
            &session_id,
            process,
            Arc::clone(&turns),
            driver.clone(),
            launch.stop,
            from,
            self.launch_facts(spec),
        );
        if prompt.is_some() && super::setup_task::cancelled(cancel) {
            // A stop came during the setup: the first input is not sent.
            let not_sent = Dispatch::NotDispatched(super::REASON_SETUP_STOPPED.into());
            if let Err(e) = self
                .store
                .first_input_failed(effect.effect_id.clone(), not_sent)
                .await
            {
                tracing::error!(error = %e, "the first input failure was not stored");
            }
            return Dispatch::Confirmed;
        }
        if let Some(input) = prompt {
            let fence = turns.fence().await;
            turns.begin_input(&fence, &input.input_id).await;
            // The fence and the checkpoint work can wait for long: the
            // authority gate runs inside `first_input`, right before the
            // commit of the input (ADR-008 §6.2).
            let sent = if super::setup_task::cancelled(cancel) {
                Dispatch::NotDispatched(super::REASON_SETUP_STOPPED.into())
            } else {
                turns.input_sent(&input.input_id);
                match driver
                    .first_input(effect, &|| Box::pin(self.gate_first_input(effect)))
                    .await
                {
                    Ok(None) => Dispatch::Sent,
                    Ok(Some(refused)) => refused,
                    Err(e) => Dispatch::NotDispatched(e.to_string()),
                }
            };
            drop(fence);
            if matches!(sent, Dispatch::NotDispatched(_)) {
                turns.input_not_sent(&input.input_id).await;
            }
            if !matches!(sent, Dispatch::Sent) {
                // The spawn is the provider's acceptance of the start; the
                // first input gets its own durable evidence (GATE-M2 P2-a).
                if let Err(e) = self
                    .store
                    .first_input_failed(effect.effect_id.clone(), sent)
                    .await
                {
                    tracing::error!(error = %e, "the first input failure was not stored");
                }
            }
        }
        Dispatch::Confirmed
    }

    /// The authority gate of a durable launch or write. A test holds it
    /// first, after the awaited preparation (`shim_starting`, the
    /// transition and the frame claim).
    async fn gate_durable(&self, effect: &Effect) -> Option<Dispatch> {
        #[cfg(any(test, feature = "fault-injection"))]
        self.gate_hook().hold().await;
        self.gate_now(effect).await
    }

    /// The gate of the first input of a durable launch (held like
    /// [`Self::gate_durable`]).
    async fn gate_first_input(&self, effect: &Effect) -> Option<Dispatch> {
        #[cfg(any(test, feature = "fault-injection"))]
        self.gate_hook().hold().await;
        self.gate_input(effect).await
    }

    async fn observe_end(&self, session_id: &SessionId, process: &ProcessIncarnation) {
        if let Err(e) = self
            .store
            .process_ended(process.clone(), "start_failed".to_owned())
            .await
        {
            tracing::error!(error = %e, session = %session_id, "process end not stored");
        }
    }

    /// One claimed effect of a session under a shim.
    pub(super) async fn operate_durable(
        &self,
        effect: &Effect,
        durable: &DurableLive,
        turns: &Arc<SessionTurns>,
    ) -> Dispatch {
        match &effect.payload {
            EffectPayload::Stop { how, .. } => {
                let process = durable.driver.process();
                let (how, mut policy) = (*how, durable.stop);
                if how == StopMode::Graceful && durable.driver.idle() {
                    // A user's async hook can keep an idle CLI alive after
                    // stdin EOF (DL-141).
                    policy.grace = DEFAULT_IDLE_GRACE.min(policy.grace);
                }
                // `session.ended` confirms the stop (the driver commits it).
                tokio::spawn(async move {
                    match how {
                        StopMode::Graceful => drop(process.stop(policy).await),
                        StopMode::Kill => drop(process.kill().await),
                    }
                });
                Dispatch::Sent
            }
            EffectPayload::Prompt { input, .. } | EffectPayload::FollowUp { input, .. } => {
                let fence = turns.fence().await;
                turns.begin_input(&fence, &input.input_id).await;
                turns.input_sent(&input.input_id);
                // The gate runs inside the driver, right before the first
                // byte (ADR-008 §6.2); a refusal is not sent.
                let sent = durable
                    .driver
                    .apply_gated(effect, &|| Box::pin(self.gate_durable(effect)))
                    .await;
                if matches!(sent, Dispatch::NotDispatched(_)) {
                    turns.input_not_sent(&input.input_id).await;
                }
                sent
            }
            EffectPayload::Steer { .. } => {
                let _fence = turns.fence().await;
                durable
                    .driver
                    .apply_gated(effect, &|| Box::pin(self.gate_durable(effect)))
                    .await
            }
            EffectPayload::Revert { revert_id, .. } => {
                if let (Durable::Codex(driver), Some(process)) =
                    (&durable.driver, effect.process_incarnation.as_ref())
                {
                    let env = super::super::revert_run::RevertEnv {
                        store: &self.store,
                        git: &self.git,
                        limits: self.limits,
                        private: &self.private,
                    };
                    let timeout = self
                        .shims
                        .as_ref()
                        .map_or(fleetdeck_providers::codex::ROLLBACK_TIMEOUT, |s| {
                            s.rollback_timeout
                        });
                    let mut rollback = CodexRollback { driver, timeout };
                    return super::super::revert_run::run(
                        env,
                        turns,
                        &mut rollback,
                        process,
                        revert_id,
                    )
                    .await;
                }
                if let Err(e) = self
                    .store
                    .advance_revert(
                        revert_id.clone(),
                        &[RevertStep::Admitted],
                        RevertStep::Failed,
                        Some(REASON_REVERT_UNSUPPORTED.to_owned()),
                    )
                    .await
                {
                    return Dispatch::Unknown(format!("store: {e}"));
                }
                Dispatch::NotDispatched(REASON_REVERT_UNSUPPORTED.into())
            }
            EffectPayload::Start { .. } | EffectPayload::Resume { .. } => {
                Dispatch::NotDispatched("not_a_live_operation".into())
            }
            _ => {
                durable
                    .driver
                    .apply_gated(effect, &|| Box::pin(self.gate_durable(effect)))
                    .await
            }
        }
    }
}

/// Feeds the committed events of a session under a shim to its turn
/// checkpoints, from the store sequence `from`. When the session ends, the
/// session leaves the live set, its process ends, and its shim row is
/// `exited`.
/// When an observer stops reading.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Drain {
    /// At the session's terminal event (a live session).
    ToTerminal,
    /// Also when it has read up to the head (a session that ended before
    /// its adoption; its terminal event can be older than `from`).
    ToHead,
}

/// How long the end of a session waits for the shim's exit report.
const EXIT_REPORT_WAIT: Duration = Duration::from_secs(10);

// The observer task owns every value it needs; a struct would only rename them.
#[allow(clippy::too_many_arguments)]
async fn observe(
    store: Store,
    sessions: Sessions,
    apps: AppHosts,
    session_id: SessionId,
    process: ProcessIncarnation,
    shim: Arc<ShimProcess>,
    turns: Arc<SessionTurns>,
    from: u64,
    drain: Drain,
    launch: Option<LaunchFacts>,
) {
    let mut head = store.subscribe();
    let mut cursor = from;
    loop {
        let read = store.read_after(cursor, OBSERVE_BATCH).await;
        let (events, last) = match read {
            Ok(ReadResult::Events { events, head }) => {
                let last = events.last().map_or(head, |e| e.station_seq);
                (events, last)
            }
            Ok(ReadResult::SnapshotRequired { head }) => (Vec::new(), head),
            Err(e) => {
                tracing::error!(error = %e, session = %session_id, "observer stopped");
                return;
            }
        };
        let fetched = cursor != last;
        cursor = last.max(cursor);
        let observed: Vec<Event> = events
            .into_iter()
            .filter(|e| e.session_id == session_id)
            .filter_map(|e| match e.event {
                EventBody::Known(event) => Some(event),
                EventBody::Unknown { .. } => None,
            })
            .collect();
        let ended = observed.iter().any(Event::is_terminal_for_session);
        crate::artifacts::suggest::observe(&store, &session_id, &observed).await;
        crate::history::subagent::observe(&store, &session_id, launch.as_ref(), &observed).await;
        if !observed.is_empty() && turns.observe(&observed).await {
            tokio::spawn(Arc::clone(&turns).capture_due());
        }
        if ended || (!fetched && drain == Drain::ToHead) {
            break;
        }
        if !fetched && head.changed().await.is_err() {
            return;
        }
    }
    end_observed(&store, &sessions, &apps, &session_id, process, &shim).await;
}

/// The end of a session under a shim: it leaves the live set, its process
/// ends, and its open shim rows are `exited`. The row reason keeps the
/// shim's evidence (DL-120): `exited` only when the shim's exit report
/// shows no surviving member of the provider's process group or session;
/// a surviving member or a missing report is not a proven end, so a later
/// `session.resume` stays refused.
async fn end_observed(
    store: &Store,
    sessions: &Sessions,
    apps: &AppHosts,
    session_id: &SessionId,
    process: ProcessIncarnation,
    shim: &ShimProcess,
) {
    // The terminal event can be committed before the shim's report is
    // read; wait for the attachment's end (bounded).
    let _ended = tokio::time::timeout(EXIT_REPORT_WAIT, shim.wait()).await;
    let exit = shim.exit_state();
    if let Ok(mut map) = sessions.lock() {
        map.remove(session_id);
    }
    // The app host leaves (B15 §4.3 item 2). Only a shim report that proves
    // the end (no surviving member of the provider's group) is an observed
    // exit; else the provider's tools may still run (`Lost`).
    let end = if shims::exit_reason(exit.as_ref()) == reason::EXITED {
        HostEnd::Exited
    } else {
        HostEnd::Lost
    };
    apps.drop_host(session_id, &process, end);
    if let Err(e) = store
        .process_ended(process, REASON_PROCESS_EXIT.to_owned())
        .await
    {
        tracing::error!(error = %e, "process end not stored");
    }
    end_rows(store, session_id, exit.as_ref()).await;
}

/// Ends the open shim rows of `session_id` as `exited` with the reason and
/// the inspection version of the shim's `exit` report (DL-120, DL-182).
async fn end_rows(
    store: &Store,
    session_id: &SessionId,
    exit: Option<&fleetdeck_providers::host::shim::wire::ExitState>,
) {
    let why = shims::exit_reason(exit);
    let inspection = shims::inspection_of(exit);
    if let Ok(rows) = store.shims_of_session(session_id.clone()).await {
        for shim_id in rows {
            let _ended = store
                .shim_end(shim_id, ShimState::Exited, why.to_owned(), inspection)
                .await;
        }
    }
}

#[path = "shim_adopt.rs"]
mod adopt;
