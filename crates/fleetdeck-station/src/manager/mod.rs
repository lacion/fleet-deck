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

pub mod decide;
pub mod durable;
pub mod files;
pub mod policy;
pub mod revert;
pub mod revert_run;
pub mod shim_claude;
pub mod shim_codex;
pub mod shim_station;
pub mod shim_stdin;
pub mod shims;
pub mod target;
pub mod turns;
pub mod workspaces;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use fleetdeck_core::config::{Driver, StationConfig};
use fleetdeck_core::{ChildEnv, SharedIds};
use fleetdeck_proto::command::{CommandEnvelope, StopMode};
use fleetdeck_proto::link::StationProvider;
use fleetdeck_providers::claude::{ClaudeAdapter, ClaudeAdapterConfig, RawLogSettings};
use fleetdeck_providers::codex::{CodexAdapter, CodexAdapterConfig};
use fleetdeck_providers::host::SessionHost;
use fleetdeck_providers::rawlog::RawLogLimits;
use fleetdeck_providers::{AuthState, ProviderAdapter};
use tokio::sync::watch;
use tokio::task::JoinHandle;

pub use policy::{Policy, WorkspaceError};
pub use target::{Adapters, SessionTarget};

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
                Arc::new(ClaudeAdapter::new(cfg, Arc::clone(host)))
            }
            Driver::Codex => {
                let mut cfg =
                    CodexAdapterConfig::new(instance.id.clone(), instance.binary.clone(), env);
                cfg.raw_log = raw_log();
                Arc::new(CodexAdapter::new(cfg, Arc::clone(host)))
            }
        };
        out.insert(instance.id.clone(), adapter);
    }
    Ok(out)
}

/// One station's session manager.
pub struct SessionManager {
    store: Store,
    policy: Arc<Policy>,
    target: Arc<SessionTarget>,
    ids: SharedIds,
    shutdown: watch::Sender<bool>,
    tasks: std::sync::Mutex<Vec<JoinHandle<()>>>,
    /// The provider report of `station.info`, probed once per process.
    providers: tokio::sync::OnceCell<Vec<StationProvider>>,
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
    ) -> Result<Self> {
        Self::start_inner(store, policy, adapters, ids, None).await
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
    ) -> Result<Self> {
        Self::start_inner(store, policy, adapters, ids, Some(shims)).await
    }

    async fn start_inner(
        store: Store,
        policy: Policy,
        adapters: Adapters,
        ids: SharedIds,
        shims: Option<target::ShimSettings>,
    ) -> Result<Self> {
        let restored = match &shims {
            None => {
                store.recover(Vec::new()).await?;
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
                start.restored
            }
        };
        // Turn checkpoint rows that a crash left open are settled from
        // their refs before any command (DL-082).
        let settled =
            crate::workspace::settle_unsettled(&store, crate::workspace::GitOptions::isolated())
                .await?;
        if !settled.finalized.is_empty() || !settled.abandoned.is_empty() {
            tracing::info!(
                finalized = settled.finalized.len(),
                abandoned = settled.abandoned.len(),
                "settled turn checkpoints after restart"
            );
        }
        // Reverts that a stop interrupted end from their stored step before
        // any command (DL-087); a claimed rollback is never repeated.
        let git = crate::workspace::GitOptions::isolated();
        let env = revert_run::RevertEnv {
            store: &store,
            git: &git,
            limits: crate::workspace::Limits::default(),
        };
        let recovered = revert_run::recover(env).await?;
        if recovered > 0 {
            tracing::info!(recovered, "recovered interrupted reverts");
        }
        let mut target = SessionTarget::new(store.clone(), adapters, Arc::clone(&ids));
        if let Some(settings) = shims {
            target = target.with_shims(settings);
        }
        let target = Arc::new(target);
        target
            .adopt(restored, fleetdeck_providers::host::StopPolicy::default())
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
        tasks.push(tokio::spawn(deny_ticker(store.clone(), rx)));
        Ok(Self {
            store,
            policy: Arc::new(policy),
            target,
            ids,
            shutdown,
            tasks: std::sync::Mutex::new(tasks),
            providers: tokio::sync::OnceCell::new(),
        })
    }

    #[must_use]
    pub fn store(&self) -> &Store {
        &self.store
    }

    #[must_use]
    pub fn policy(&self) -> &Policy {
        &self.policy
    }

    /// A new id from the station's id source (command ids of the local CLI).
    #[must_use]
    pub fn new_id<I: From<String>>(&self) -> I {
        fleetdeck_core::new_id(self.ids.as_ref())
    }

    /// The configured provider instances with the version and sign-in state
    /// of one probe each (`station.info`, M2 P4-2). The first call probes,
    /// each probe bounded by [`PROBE_TIMEOUT`]; later calls reuse the
    /// result for the life of the station process (DL-059). An instance
    /// without an adapter, or a failed probe, reports no version and
    /// `unknown` sign-in.
    pub async fn providers(&self) -> Vec<StationProvider> {
        self.providers
            .get_or_init(|| async {
                let mut out = Vec::new();
                for (instance, kind) in self.policy.instances() {
                    let probe = match self.target.adapter(instance) {
                        Some(adapter) => tokio::time::timeout(PROBE_TIMEOUT, adapter.probe())
                            .await
                            .ok()
                            .and_then(std::result::Result::ok),
                        None => None,
                    };
                    let auth = match probe.as_ref().map(|p| &p.auth) {
                        Some(AuthState::SignedIn { .. }) => "signed_in",
                        Some(AuthState::SignedOut) => "signed_out",
                        Some(AuthState::Unknown) | None => "unknown",
                    };
                    out.push(StationProvider {
                        instance: instance.clone(),
                        provider: *kind,
                        version: probe.and_then(|p| p.version),
                        auth: auth.to_owned(),
                        capabilities: self.target.adapter(instance).map(|a| a.capabilities()),
                    });
                }
                out
            })
            .await
            .clone()
    }

    /// The number of sessions with a live provider process.
    #[must_use]
    pub fn live_sessions(&self) -> usize {
        self.target.live_count()
    }

    /// Decides one command. The effects run after the commit.
    ///
    /// # Errors
    /// A database error: nothing is stored.
    pub async fn submit(&self, envelope: CommandEnvelope, caller: Caller) -> Result<Decided> {
        let policy = Arc::clone(&self.policy);
        let origin = envelope.origin;
        let command = envelope.command.clone();
        self.store
            .decide(envelope, caller, move |d| {
                decide::decide(d, &policy, origin, command)
            })
            .await
    }

    /// Stops the worker and the ticker, then every live session. Commands
    /// that arrive later are still decided, but no worker dispatches them
    /// until the next start (recovery retires them).
    pub async fn shutdown(&self, how: StopMode) {
        let _ = self.shutdown.send(true);
        let tasks: Vec<_> = self
            .tasks
            .lock()
            .map(|mut t| t.drain(..).collect())
            .unwrap_or_default();
        for task in tasks {
            let _ = task.await;
        }
        self.target.stop_all(how).await;
    }
}

/// The adapter configs of restored shim sessions: each session's adapter
/// builds its durable launch from the stored summary.
fn durable_configs(adapters: &Adapters) -> shim_station::Configs {
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
