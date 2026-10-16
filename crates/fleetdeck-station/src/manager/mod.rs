//! The session manager (PLAN §6.4, M2 P2-6): every session command goes
//! `decide → commit → effect → adapter`.
//!
//! 1. [`SessionManager::submit`] decides the command in one transaction with
//!    the station policy ([`policy`]) and the session's projection
//!    ([`decide`]). The transaction stores the receipt and the effects.
//! 2. The effect worker claims each effect and runs its I/O through
//!    [`SessionTarget`] against the live adapter session.
//! 3. The pump of each session appends the adapter's events; provider
//!    evidence in those events confirms the effects.

pub mod auto_continue;
pub(crate) mod binding_rotate;
pub mod binding_start;
pub mod coordinator_command;
mod coordinator_start;
pub mod decide;
pub mod drain;
pub mod durable;
pub mod files;
pub mod fleet_control;
pub mod hibernate;
pub mod policy;
pub mod policy_hub;
mod restart_check;
pub mod restart_gate;
pub mod resume;
pub mod revert;
pub mod revert_run;
pub mod shell_advance;
mod shell_context;
pub mod shell_decide;
mod shells_slot;
pub mod shim_claude;
pub mod shim_codex;
pub mod shim_codex_poll;
pub mod shim_exit;
pub mod shim_station;
pub mod shim_stdin;
pub mod shims;
pub mod target;
pub mod trash;
pub mod turns;
pub mod workspaces;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use fleetdeck_core::config::{Driver, StationConfig};
use fleetdeck_core::{ChildEnv, SharedIds};
use fleetdeck_proto::command::{CommandEnvelope, SessionCommand, StopMode};
use fleetdeck_proto::link::StationProvider;
use fleetdeck_providers::ProviderAdapter;
use fleetdeck_providers::claude::{ClaudeAdapter, ClaudeAdapterConfig, RawLogSettings};
use fleetdeck_providers::codex::{CodexAdapter, CodexAdapterConfig};
use fleetdeck_providers::host::SessionHost;
use fleetdeck_providers::rawlog::RawLogLimits;
use tokio::sync::watch;
use tokio::task::JoinHandle;

pub use policy::{Policy, WorkspaceError};
pub use policy_hub::{Holders, PolicyHub, PrepareError, Prepared};
pub use target::{Adapters, SessionTarget};

#[path = "manager_execution.rs"]
mod execution_checks;
#[path = "manager_providers.rs"]
mod providers_info;

use crate::store::{Caller, Decided, Result, Store};
use crate::worker::EffectWorker;

/// The longest wait for one provider probe of `station.info`.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// How often the manager applies the offline approval policy.
pub const DENY_TICK: Duration = Duration::from_secs(1);

/// Builds the adapters of the configured provider instances: one Claude or
/// Codex adapter per instance, with the raw-frame log under `raw_root`.
///
/// # Errors
/// An instance `env` table that cannot go into a child environment.
pub fn adapters_from_config(
    config: &StationConfig,
    host: &Arc<dyn SessionHost>,
    raw_root: Option<PathBuf>,
) -> std::result::Result<Adapters, fleetdeck_core::childenv::ChildEnvError> {
    let mut out = Adapters::new();
    let raw_log = || {
        raw_root.as_ref().map(|root| RawLogSettings {
            root: root.clone(),
            limits: RawLogLimits::default(),
        })
    };
    for instance in &config.provider_instances {
        let env = ChildEnv::for_instance(ChildEnv::from_process(), instance)?;
        let adapter: Arc<dyn ProviderAdapter> = match instance.driver {
            Driver::Claude => {
                let mut cfg =
                    ClaudeAdapterConfig::new(instance.id.clone(), instance.binary.clone(), env);
                cfg.raw_log = raw_log();
                cfg.allow_bypass = config.allow_bypass_permissions;
                Arc::new(ClaudeAdapter::new(cfg, Arc::clone(host)))
            }
            Driver::Codex => {
                let mut cfg =
                    CodexAdapterConfig::new(instance.id.clone(), instance.binary.clone(), env);
                cfg.raw_log = raw_log();
                cfg.allow_full_access = config.allow_bypass_permissions;
                Arc::new(CodexAdapter::new(cfg, Arc::clone(host)))
            }
        };
        out.insert(instance.id.clone(), adapter);
    }
    Ok(out)
}

/// The environments of the setup commands: for each provider instance the
/// same environment that its adapter gets (DL-148).
///
/// # Errors
/// An instance `env` table that cannot go into a child environment.
pub fn setup_envs_from_config(
    config: &StationConfig,
) -> std::result::Result<policy::SetupEnvs, fleetdeck_core::childenv::ChildEnvError> {
    let mut out = policy::SetupEnvs::new();
    for instance in &config.provider_instances {
        let env = ChildEnv::for_instance(ChildEnv::from_process(), instance)?;
        out.insert(instance.id.clone(), env);
    }
    Ok(out)
}

/// One station's session manager.
pub struct SessionManager {
    store: Store,
    policy: Arc<PolicyHub>,
    /// The coordinator bindings of this station (ADR-006 §5): the ingress
    /// authenticates against them, and the owner's renew and revoke
    /// commands change them (DL-212). Built once, not configuration.
    coordinator: crate::coordinator::Bindings,
    /// GitHub, GitLab, Jira and Linear (R3, DL-164): a response cache and
    /// the recent writes. A publish replaces it when the trackers change.
    integrations: std::sync::RwLock<Arc<crate::integrations::Integrations>>,
    target: Arc<SessionTarget>,
    ids: SharedIds,
    shutdown: watch::Sender<bool>,
    tasks: std::sync::Mutex<Vec<JoinHandle<()>>>,
    /// The provider report of `station.info`, probed once per process.
    providers: tokio::sync::OnceCell<Vec<StationProvider>>,
    draining: std::sync::atomic::AtomicBool,
    /// The submits that create work and are not decided yet (NV-25).
    drain_submits: std::sync::atomic::AtomicUsize,
    /// The restart freeze and its counts, shared with the target (finding 3
    /// of PRE-PR r1).
    restart: Arc<restart_gate::RestartState>,
    /// The shells of the station, once the link node set them (DL-150).
    shells: target::ShellsSlot,
    /// The artifact service, once the station gave it a directory (DL-165).
    artifacts: std::sync::OnceLock<crate::artifacts::ArtifactService>,
    /// The model help of the station, once the station process set it
    /// (R10, DL-167).
    assist: std::sync::OnceLock<Arc<crate::assist::Assist>>,
    /// The private paths of the station (DL-173), given at the start. Built
    /// once; not part of the live policy snapshot.
    private_paths: Arc<crate::workspace::PrivatePaths>,
    /// The settings holder of the station, once the station process set it
    /// (DL-172). Without it the settings methods answer `unsupported`.
    settings: std::sync::OnceLock<Arc<crate::settings_svc::StationSettings>>,
    /// Wakes the shell advance task (B8): called after a settlement that the
    /// manager makes.
    shell_notify: Arc<tokio::sync::Notify>,
}

impl std::fmt::Debug for SessionManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionManager")
            .field("policy", &self.policy)
            .finish_non_exhaustive()
    }
}

impl SessionManager {
    /// Recovers the store (no provider process survives a station restart
    /// with the direct-child host), then starts the effect worker and the
    /// offline approval ticker.
    ///
    /// # Errors
    /// A database error.
    pub async fn start(
        store: Store,
        policy: Policy,
        adapters: Adapters,
        ids: SharedIds,
        private: Arc<crate::workspace::PrivatePaths>,
    ) -> Result<Self> {
        Self::start_inner(
            store,
            policy,
            Holders::default(),
            adapters,
            ids,
            None,
            private,
        )
        .await
    }

    /// [`Self::start`] with the runtime holders that the caller built (the
    /// coordinator bindings that the ingress shares, a service of tests).
    ///
    /// # Errors
    /// A database error.
    pub async fn start_with_holders(
        store: Store,
        policy: Policy,
        holders: Holders,
        adapters: Adapters,
        ids: SharedIds,
        private: Arc<crate::workspace::PrivatePaths>,
    ) -> Result<Self> {
        Self::start_inner(store, policy, holders, adapters, ids, None, private).await
    }

    /// [`Self::start`] for a station that hosts sessions under shims
    /// (ADR-005, DL-113): the station start first recovers every shim row
    /// and restores the durable drivers ([`shim_station::recover_station`]),
    /// then runs the generic recovery with the restored processes as live;
    /// the restored sessions stay live. New sessions whose adapter has a
    /// durable driver launch under a shim.
    ///
    /// # Errors
    /// A database error.
    pub async fn start_with_shims(
        store: Store,
        policy: Policy,
        adapters: Adapters,
        ids: SharedIds,
        shims: target::ShimSettings,
        private: Arc<crate::workspace::PrivatePaths>,
    ) -> Result<Self> {
        Self::start_inner(
            store,
            policy,
            Holders::default(),
            adapters,
            ids,
            Some(shims),
            private,
        )
        .await
    }

    async fn start_inner(
        store: Store,
        policy: Policy,
        holders: Holders,
        adapters: Adapters,
        ids: SharedIds,
        shims: Option<target::ShimSettings>,
        private: Arc<crate::workspace::PrivatePaths>,
    ) -> Result<Self> {
        // Turn checkpoint rows that a crash left open are settled from
        // their refs before any command (DL-082). With shims they are
        // settled before the restored drivers replay buffered output, so a
        // replayed turn never races its own row (DL-114).
        if shims.is_some() {
            settle_turn_rows(&store).await?;
        }
        // The observers of restored sessions start at the head before the
        // restoration: every turn and exit event that a restored driver
        // commits while it replays buffered output is observed (DL-114).
        let restore_from = *store.subscribe().borrow();
        let policy = if shims.is_some() {
            policy.on_shim_host()
        } else {
            policy
        };
        let Holders {
            coordinator,
            integrations,
        } = holders;
        // The fence and the binding rows come before the shim recovery, and
        // the binding sink before any driver restores (DL-194 §9.8).
        let held = match &shims {
            Some(settings) => {
                let run_dir = settings.host.config().station_dir.join("run");
                Some(binding_start::prepare(&store, &coordinator, run_dir).await?)
            }
            None => None,
        };
        let restored = match &shims {
            None => {
                store.recover(Vec::new()).await?;
                settle_turn_rows(&store).await?;
                Vec::new()
            }
            Some(settings) => {
                let start = shim_station::recover_station(
                    &store,
                    &settings.host,
                    &durable_configs(&adapters),
                    settings.in_flight_wait,
                )
                .await?;
                if !start.ended.is_empty() {
                    tracing::warn!(ended = ?start.ended, "shim rows ended at the station start");
                }
                binding_start::finish(
                    &store,
                    &coordinator,
                    &start.restored,
                    held.unwrap_or_default(),
                )
                .await?;
                start.restored
            }
        };
        // Reverts that a stop interrupted end from their stored step before
        // any command (DL-087); a claimed rollback is never repeated.
        let git = crate::workspace::GitOptions::isolated();
        let env = revert_run::RevertEnv {
            store: &store,
            git: &git,
            limits: crate::workspace::Limits::default(),
            private: &private,
        };
        let recovered = revert_run::recover(env).await?;
        if recovered > 0 {
            tracing::info!(recovered, "recovered interrupted reverts");
        }
        // A setup process that outlived the last station process is stopped
        // before any command is admitted (DL-148).
        let stopped = crate::workspace::setup_cleanup::cleanup_stale(&store).await?;
        if stopped > 0 {
            tracing::info!(stopped, "cleaned up setup runs");
        }
        // Auto-continue intents of the sessions that this start lost, before
        // any command is admitted (ADR-007 §2.2, DL-127).
        let continued = auto_continue::evaluate(&store, &policy, restore_from).await?;
        if continued > 0 {
            tracing::info!(continued, "stored auto-continue intents");
        }
        // A trash operation that a restart interrupted goes on from its row
        // (DL-150); the effect of the earlier process is retired.
        let requeued = store.requeue_worktree_ops().await?;
        if requeued > 0 {
            tracing::info!(requeued, "resumed interrupted worktree operations");
        }
        let shells = target::ShellsSlot::default();
        let integrations = integrations
            .unwrap_or_else(|| crate::integrations::Integrations::new(policy.trackers().to_vec()));
        let hub = Arc::new(PolicyHub::new(policy).with_fleet_control(coordinator.clone()));
        let restart = Arc::<restart_gate::RestartState>::default();
        let mut target = SessionTarget::new(store.clone(), adapters, Arc::clone(&ids))
            .with_restart(Arc::clone(&restart))
            .with_coordinator(coordinator.clone())
            .with_policy(Arc::clone(&hub))
            .with_shells(shells.clone())
            .with_private(Arc::clone(&private));
        if let Some(settings) = shims {
            target = target.with_shims(settings);
        }
        let target = target.shared();
        target
            .adopt(
                restored,
                fleetdeck_providers::host::StopPolicy::default(),
                restore_from,
            )
            .await;
        let worker = EffectWorker::register(store.clone(), Arc::clone(&target) as Arc<_>).await?;
        let (shutdown, rx) = watch::channel(false);
        let mut tasks = Vec::new();
        let worker_rx = rx.clone();
        tasks.push(tokio::spawn(async move {
            if let Err(e) = worker.run(worker_rx).await {
                tracing::error!(error = %e, "effect worker stopped");
            }
        }));
        // The ticker always runs: it reads `hibernate_after_min` from the
        // current snapshot at each tick, and 0 is off (DL-172 §1.4).
        tasks.push(tokio::spawn(hibernate::ticker(
            store.clone(),
            Arc::clone(&hub),
            coordinator.clone(),
            hibernate::TICK,
            rx.clone(),
        )));
        tasks.push(tokio::spawn(deny_ticker(store.clone(), rx.clone())));
        // B8: the shell rows of the last process are settled and advanced
        // before the shell admission opens; the advance task follows.
        let shell_notify = target.shell_notify();
        let fences = target.fences().clone();
        shell_advance::start_up(&store, &fences).await?;
        tasks.push(tokio::spawn(shell_advance::task(
            store.clone(),
            fences,
            Arc::clone(&shell_notify),
            rx,
        )));
        Ok(Self {
            store,
            policy: hub,
            coordinator,
            integrations: std::sync::RwLock::new(Arc::new(integrations)),
            target,
            ids,
            shutdown,
            tasks: std::sync::Mutex::new(tasks),
            providers: tokio::sync::OnceCell::new(),
            draining: std::sync::atomic::AtomicBool::new(false),
            drain_submits: std::sync::atomic::AtomicUsize::new(0),
            restart,
            shells,
            artifacts: std::sync::OnceLock::new(),
            assist: std::sync::OnceLock::new(),
            private_paths: private,
            settings: std::sync::OnceLock::new(),
            shell_notify,
        })
    }

    /// Gives the manager the shells of the station: the trash and purge
    /// decisions and the file work of the trash read their start folders
    /// (DL-150). The first call wins.
    pub fn set_shells(&self, shells: Arc<crate::shell::Shells>) {
        self.shells.set(shells);
    }

    /// Gives the manager the directory of the artifact bytes (DL-165). The
    /// first call wins; without it the artifact methods answer
    /// `unsupported`.
    pub fn set_artifacts_dir(&self, root: PathBuf, private: Arc<crate::workspace::PrivatePaths>) {
        let service = crate::artifacts::ArtifactService::new(
            self.store.clone(),
            Arc::clone(&self.policy),
            root,
            private,
        );
        let _ = self.artifacts.set(service);
    }

    /// The private paths of the station, given at the start.
    #[must_use]
    pub fn private_paths(&self) -> Arc<crate::workspace::PrivatePaths> {
        Arc::clone(&self.private_paths)
    }

    /// The artifact service, if the station has a directory for it.
    #[must_use]
    pub fn artifacts(&self) -> Option<&crate::artifacts::ArtifactService> {
        self.artifacts.get()
    }

    /// Gives the manager the model help of the station (R10, DL-167). The
    /// first call wins.
    pub fn set_assist(&self, assist: Arc<crate::assist::Assist>) {
        let _ = self.assist.set(assist);
    }

    /// The model help, if the station process set it.
    #[must_use]
    pub fn assist(&self) -> Option<Arc<crate::assist::Assist>> {
        self.assist.get().cloned()
    }

    /// Gives the manager the settings holder of the station (DL-172). The
    /// first call wins.
    pub fn set_settings(&self, settings: Arc<crate::settings_svc::StationSettings>) {
        let _ = self.settings.set(settings);
    }

    /// The settings holder, if the station process set it.
    #[must_use]
    pub fn settings(&self) -> Option<Arc<crate::settings_svc::StationSettings>> {
        self.settings.get().cloned()
    }

    #[must_use]
    pub fn store(&self) -> &Store {
        &self.store
    }

    /// The current policy snapshot. Read it for each request, decision or
    /// dispatch; never keep it longer (DL-172 §1.4).
    #[must_use]
    pub fn policy(&self) -> Arc<Policy> {
        self.policy.current()
    }

    /// The hub that holds the policy snapshot and its version.
    #[must_use]
    pub fn policy_hub(&self) -> &Arc<PolicyHub> {
        &self.policy
    }

    /// The coordinator bindings of this station.
    #[must_use]
    pub fn coordinator_bindings(&self) -> &crate::coordinator::Bindings {
        &self.coordinator
    }

    /// The integration service of this station (R3, DL-164). Read it for
    /// each request: a publish can replace it.
    #[must_use]
    pub fn integrations(&self) -> Arc<crate::integrations::Integrations> {
        Arc::clone(
            &self
                .integrations
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        )
    }

    /// Step 1 of a live config change (DL-172 §1.4): validates `new` and
    /// builds the snapshot, with a new integration service when the
    /// trackers changed. It publishes nothing. The fields that apply only at
    /// a restart (`listen`, `host`, `shim_buffer_mib`, `provider_instances`)
    /// are ignored.
    ///
    /// # Errors
    /// The key of the value that cannot apply, and a message without the
    /// value.
    pub fn prepare(&self, new: &StationConfig) -> std::result::Result<Prepared, PrepareError> {
        policy_hub::validate(new)?;
        let policy = self.policy.current();
        let integrations = (policy.trackers() != new.trackers.as_slice())
            .then(|| self.integrations().rebuilt(new.trackers.clone()));
        Ok(Prepared {
            policy: policy.reconfigured(new),
            integrations,
        })
    }

    /// Step 2 of a live config change: swaps the snapshot and the rebuilt
    /// integration service. It cannot fail. The caller runs one `prepare`
    /// and `publish` at a time.
    pub fn publish(&self, prepared: Prepared) {
        if let Some(service) = prepared.integrations {
            *self
                .integrations
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Arc::new(service);
        }
        self.policy.swap(prepared.policy);
    }

    /// A new id from the station's id source (command ids of the local CLI).
    #[must_use]
    pub fn new_id<I: From<String>>(&self) -> I {
        fleetdeck_core::new_id(self.ids.as_ref())
    }

    /// The number of sessions with a live provider process.
    #[must_use]
    pub fn live_sessions(&self) -> usize {
        self.target.live_count()
    }

    /// The live sessions and their provider pids (R13 §10).
    pub fn live_pids(&self) -> Vec<(fleetdeck_proto::SessionId, Option<u32>)> {
        self.target.live_pids()
    }

    /// Decides one command. The effects run after the commit.
    ///
    /// # Errors
    /// A database error: nothing is stored.
    pub async fn submit(&self, envelope: CommandEnvelope, caller: Caller) -> Result<Decided> {
        let policy = self.policy.current();
        let coordinator = self.coordinator.clone();
        let origin = envelope.origin;
        let command = envelope.command.clone();
        let is_release = matches!(command, SessionCommand::ReleaseShell { .. });
        let interrupted = matches!(command, SessionCommand::Interrupt { .. })
            .then(|| command.session_id().cloned())
            .flatten();
        // The count comes before the flag is read: the restart admission
        // sets the flag, then waits for the count (restart_gate). An input
        // may resume a hibernated session, so it counts too (NV-27).
        let root = shell_decide::resolve_root(&self.store, &command).await?;
        let _creating = (restart_gate::creates_session(&command)
            || restart_gate::may_resume(&command))
        .then(|| self.restart.enter_submit());
        // The same for the drain: it waits for the count (NV-25).
        let _deciding = drain::creates_work(&command).then(|| self.enter_drain_submit());
        let restarting = self.restart.restarting();
        let guard = auto_continue::guard(&command, &caller)
            .and_then(|()| coordinator_command::guard(&command, &caller))
            .and_then(|()| drain::guard(self.draining(), &command))
            .and_then(|()| restart_gate::guard(restarting, &command))
            .and_then(|()| {
                shell_decide::guard(&command, &caller, origin, envelope.authority.is_some())
            });
        let shells = self.shells.get();
        // The process check of a reconciliation runs here, outside the
        // writer transaction; the decision rechecks only the row state
        // (DL-182 §4.2).
        let check = self.reconcile_check(&command).await;
        let decided = self
            .store
            .decide(envelope, caller, move |d| {
                guard?;
                match command {
                    SessionCommand::ReconcileExecution {
                        session_id,
                        shim_id,
                    } => shims::decide_reconcile(d, origin, &session_id, &shim_id, check.as_ref()),
                    command @ (SessionCommand::WorktreeTrash { .. }
                    | SessionCommand::WorktreeRestore { .. }
                    | SessionCommand::WorktreePurge { .. }
                    | SessionCommand::WorktreeRelease { .. }) => {
                        trash::decide(d, &policy, origin, shells.as_deref(), command)
                    }
                    command @ (SessionCommand::ShellCommand { .. }
                    | SessionCommand::ReleaseShell { .. }) => {
                        shell_decide::decide(d, &policy, restarting, root.as_deref(), command)
                    }
                    command => {
                        shell_decide::check_root(d, root.as_deref(), &command)?;
                        decide::decide(d, &policy, &coordinator, origin, restarting, command)
                    }
                }
            })
            .await;
        // A revoke wrote a fence entry; after the commit it is removed
        // (DL-194 §2.3 step 4). A failure leaves it, which is harmless.
        if let Some(fence) = self.coordinator.fence() {
            fence.settle(&self.store).await;
        }
        // B8: a release settled a row; its advance follows.
        if is_release
            && decided
                .as_ref()
                .is_ok_and(|d| d.receipt.state == fleetdeck_proto::command::ReceiptState::Accepted)
        {
            self.shell_notify.notify_one();
        }
        self.target.cancel_shells_after(interrupted, &decided).await;
        decided
    }

    /// Stops the worker and the ticker, then every live session. Commands
    /// that arrive later are still decided, but no worker dispatches them
    /// until the next start (recovery retires them).
    pub async fn shutdown(&self, how: StopMode) {
        // B8: no shell row can be created after the cancel sweep.
        let open = self
            .store
            .shell_close_admission_and_cancel_all()
            .await
            .unwrap_or_else(|e| {
                tracing::error!(error = %e, "shell admission not closed at shutdown");
                Vec::new()
            });
        self.target.signal_all_shells();
        let _ = self.shutdown.send(true);
        let tasks: Vec<_> = self
            .tasks
            .lock()
            .map(|mut t| t.drain(..).collect())
            .unwrap_or_default();
        for task in tasks {
            let _ = task.await;
        }
        self.target
            .wait_shells_settled(&open, Duration::from_secs(10))
            .await;
        self.target.stop_all(how).await;
        let _ = tokio::task::spawn_blocking(crate::workspace::precheck::kill_all_marked).await;
    }
}

/// Settles the turn checkpoint rows that a crash left open (DL-082).
async fn settle_turn_rows(store: &Store) -> Result<()> {
    let settled =
        crate::workspace::settle_unsettled(store, crate::workspace::GitOptions::isolated()).await?;
    if !settled.finalized.is_empty() || !settled.abandoned.is_empty() {
        tracing::info!(
            finalized = settled.finalized.len(),
            abandoned = settled.abandoned.len(),
            "settled turn checkpoints after restart"
        );
    }
    Ok(())
}

/// The adapter configs of restored shim sessions: each session's adapter
/// builds its durable launch from the stored summary.
#[must_use]
pub fn durable_configs(adapters: &Adapters) -> shim_station::Configs {
    fn launch(
        adapters: &Adapters,
        summary: &fleetdeck_proto::SessionSummary,
    ) -> Option<fleetdeck_providers::durable::DurableConfig> {
        let instance = summary.provider_instance.clone()?;
        let adapter = adapters.get(&instance)?;
        let spec = fleetdeck_proto::command::SessionSpec {
            provider_instance: instance,
            cwd: summary.cwd.clone()?,
            mode: summary.mode.clone(),
            model: summary.model.clone(),
            title: summary.title.clone(),
            coordinator: None,
            launch: None,
        };
        adapter.durable_launch(&spec).ok().map(|l| l.core)
    }
    let (a, b) = (adapters.clone(), adapters.clone());
    shim_station::Configs {
        claude: Box::new(move |summary| match launch(&a, summary)? {
            fleetdeck_providers::durable::DurableConfig::Claude(config) => Some(config),
            fleetdeck_providers::durable::DurableConfig::Codex { .. } => None,
        }),
        codex: Box::new(move |summary| match launch(&b, summary)? {
            fleetdeck_providers::durable::DurableConfig::Codex { config, .. } => Some(config),
            fleetdeck_providers::durable::DurableConfig::Claude(_) => None,
        }),
    }
}

async fn deny_ticker(store: Store, mut shutdown: watch::Receiver<bool>) {
    let mut tick = tokio::time::interval(DENY_TICK);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = tick.tick() => {
                if let Err(e) = store.deny_overdue().await {
                    tracing::warn!(error = %e, "offline approval policy");
                }
            }
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    return;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
#[path = "fault_tests.rs"]
mod fault_tests;

#[cfg(test)]
#[path = "first_input_tests.rs"]
mod first_input_tests;

#[cfg(test)]
#[path = "auto_continue_tests.rs"]
mod auto_continue_tests;

#[cfg(test)]
#[path = "coordinator_spec_tests.rs"]
mod coordinator_spec_tests;

#[cfg(test)]
#[path = "coordinator_start_tests.rs"]
mod coordinator_start_tests;

#[cfg(test)]
#[path = "coordinator_live_tests.rs"]
mod coordinator_live_tests;

#[cfg(test)]
#[path = "version_tests.rs"]
mod version_tests;

#[cfg(test)]
#[path = "adapters_bypass_tests.rs"]
mod adapters_bypass_tests;

#[cfg(test)]
#[path = "shim_resume_rig.rs"]
mod shim_resume_rig;

#[cfg(test)]
#[path = "shim_resume_tests.rs"]
mod shim_resume_tests;

#[cfg(test)]
#[path = "shim_resume_check_tests.rs"]
mod shim_resume_check_tests;

#[cfg(test)]
#[path = "binding_revoke_tests.rs"]
mod binding_revoke_tests;

#[cfg(test)]
#[path = "shell_run_tests.rs"]
mod shell_run_tests;
