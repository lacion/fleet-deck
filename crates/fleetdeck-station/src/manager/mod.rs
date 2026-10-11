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
pub mod policy;
pub mod target;
pub mod workspaces;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use fleetdeck_core::config::{Driver, StationConfig};
use fleetdeck_core::{ChildEnv, SharedIds};
use fleetdeck_proto::command::{CommandEnvelope, StopMode};
use fleetdeck_proto::link::StationProvider;
use fleetdeck_providers::AuthState;
use fleetdeck_providers::claude::{ClaudeAdapter, ClaudeAdapterConfig, RawLogSettings};
use fleetdeck_providers::host::SessionHost;
use fleetdeck_providers::rawlog::RawLogLimits;
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

/// Builds the adapters of the configured provider instances. Codex comes in
/// Phase 5; its instances get no adapter yet, so a start for them retires
/// as not dispatched.
///
/// # Errors
/// An instance `env` table that cannot go into a child environment.
pub fn adapters_from_config(
    config: &StationConfig,
    host: &Arc<dyn SessionHost>,
    raw_root: Option<PathBuf>,
) -> std::result::Result<Adapters, fleetdeck_core::childenv::ChildEnvError> {
    let mut out = Adapters::new();
    for instance in &config.provider_instances {
        if instance.driver != Driver::Claude {
            continue;
        }
        let env = ChildEnv::for_instance(ChildEnv::from_process(), instance)?;
        let mut cfg = ClaudeAdapterConfig::new(instance.id.clone(), instance.binary.clone(), env);
        cfg.raw_log = raw_root.as_ref().map(|root| RawLogSettings {
            root: root.clone(),
            limits: RawLogLimits::default(),
        });
        out.insert(
            instance.id.clone(),
            Arc::new(ClaudeAdapter::new(cfg, Arc::clone(host))),
        );
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
        store.recover(Vec::new()).await?;
        let target = Arc::new(SessionTarget::new(
            store.clone(),
            adapters,
            Arc::clone(&ids),
        ));
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
