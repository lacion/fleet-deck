//! MCP Apps wiring of the session target (B15, design
//! `docs/v2/design/mcp-apps.md` §4.3 items 1 and 2).
//!
//! - **Candidates.** A direct session hands its candidate receiver to a
//!   forwarder task per session; a session under a shim hands the candidates
//!   of its committed output to the app sink that [`SessionTarget`] attaches
//!   when the session enters the `Live` map (`insert_durable`). Both feed the
//!   capture service ([`crate::apps::Capture`]).
//! - **Bridge registry.** One [`AppHost`] per live session with a process: the
//!   bridge, the turns and the process incarnation. The entry leaves with its
//!   process: [`HostEnd::Exited`] where the process ends, [`HostEnd::Detached`]
//!   in the station-shutdown detach. A newer process keeps its entry.
//!   **The absence of an entry is never proof that a process or a tool call
//!   ended** (design §4.6.1, "End proof").

use std::collections::HashMap;
use std::sync::{Arc, Mutex as StdMutex, MutexGuard, OnceLock, PoisonError};

use fleetdeck_proto::{ProviderInstanceId, SessionId};
use fleetdeck_providers::LiveSession;
use fleetdeck_providers::apps::AppBridge;
use tokio::sync::Notify;
use tokio::task::JoinHandle;

use super::SessionTarget;
use crate::apps::Capture;
use crate::apps::calls::{self, AppEnv, CallLimits, ProcessExits};
use crate::manager::durable::{DurableCore, DurableDriver};
use crate::manager::shim_station::Durable;
use crate::manager::turns::SessionTurns;
use crate::store::{ProcessIncarnation, Store};

/// How an app host ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostEnd {
    /// The provider process ended (observed).
    Exited,
    /// The station detached from a session under a shim at its shutdown: the
    /// shim and the provider go on.
    Detached,
}

/// The app side of one live session.
#[derive(Clone)]
pub struct AppHost {
    pub bridge: Arc<dyn AppBridge>,
    pub turns: Arc<SessionTurns>,
    /// The process that the bridge talks to.
    pub process: ProcessIncarnation,
}

/// What the end of a process needs (§4.6.1, "Process end").
struct Ends {
    store: Store,
    exits: Arc<ProcessExits>,
    /// The wake of the advance task; bound when the first host registers.
    wake: OnceLock<Arc<Notify>>,
}

/// The registry of the app hosts, by session. Clones share one map.
#[derive(Clone)]
pub(super) struct AppHosts {
    map: Arc<StdMutex<HashMap<SessionId, AppHost>>>,
    ends: Arc<Ends>,
}

impl AppHosts {
    fn new(store: Store) -> Self {
        Self {
            map: Arc::default(),
            ends: Arc::new(Ends {
                store,
                exits: Arc::default(),
                wake: OnceLock::new(),
            }),
        }
    }

    fn map(&self) -> MutexGuard<'_, HashMap<SessionId, AppHost>> {
        self.map.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(super) fn register(&self, session: SessionId, host: AppHost) {
        self.map().insert(session, host);
    }

    pub(super) fn get(&self, session: &SessionId) -> Option<AppHost> {
        self.map().get(session).cloned()
    }

    /// Removes the entry of `session` when it belongs to `process`; a newer
    /// process keeps its entry. Returns whether an entry was removed.
    ///
    /// For [`HostEnd::Exited`] it then records the exit of that process
    /// (also when the entry belongs to a newer process) and settles the
    /// `uncertain` app calls of that process in a task, in this order:
    /// the record comes first, so a call task that marks its row later reads
    /// it. [`HostEnd::Detached`] records nothing and changes no row: the shim
    /// and its provider go on (§4.6.1, "End proof").
    pub(super) fn drop_host(
        &self,
        session: &SessionId,
        process: &ProcessIncarnation,
        end: HostEnd,
    ) -> bool {
        let removed = {
            let mut map = self.map();
            let own = map
                .get(session)
                .is_some_and(|host| &host.process == process);
            if own {
                map.remove(session);
            }
            own
        };
        if end == HostEnd::Exited {
            self.ends.exits.record(session, process.as_str());
            self.settle_exited(session, process);
        }
        tracing::debug!(session = %session, ?end, removed, "app host dropped");
        removed
    }

    fn settle_exited(&self, session: &SessionId, process: &ProcessIncarnation) {
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            tracing::warn!(session = %session, "app calls not settled: no runtime");
            return;
        };
        let (ends, session, process) = (Arc::clone(&self.ends), session.clone(), process.clone());
        runtime.spawn(async move {
            match calls::process_ended(&ends.store, &session, process.as_str()).await {
                Ok(0) => {}
                Ok(_) => {
                    if let Some(wake) = ends.wake.get() {
                        wake.notify_one();
                    }
                }
                Err(e) => tracing::error!(error = %e, session = %session, "app calls not settled"),
            }
        });
    }
}

/// The app state of a [`SessionTarget`].
pub(super) struct Apps {
    hosts: AppHosts,
    capture: Capture,
    limits: Arc<CallLimits>,
}

impl Apps {
    pub(super) fn new(store: Store) -> Self {
        Self {
            hosts: AppHosts::new(store.clone()),
            capture: Capture::new(store),
            limits: Arc::default(),
        }
    }

    /// The registry, for the tasks that outlive a call.
    pub(super) fn hosts(&self) -> AppHosts {
        self.hosts.clone()
    }
}

impl SessionTarget {
    /// The app host of a live session: a clone of its registry entry. `None`
    /// without a live process (or for a provider without an app bridge).
    #[must_use]
    pub fn app_host(&self, session: &SessionId) -> Option<AppHost> {
        self.apps.hosts.get(session)
    }

    /// Whether the adapter of `instance` offers the MCP Apps extension to a
    /// new session (its verified CLI version); `false` without an adapter.
    pub(super) async fn offers_mcp_apps(&self, instance: &ProviderInstanceId) -> bool {
        match self.adapters.get(instance) {
            Some(adapter) => adapter.offers_mcp_apps().await,
            None => false,
        }
    }

    /// The environment of the app methods (`session.app_call`,
    /// `session.app_calls`, `session.app_release`): the store, the exits that
    /// this station process observed, the call limits and the wake of the
    /// advance task.
    #[must_use]
    pub fn app_env(&self) -> AppEnv {
        let mut env = AppEnv::new(self.store.clone(), Arc::clone(&self.ids));
        env.exits = Arc::clone(&self.apps.hosts.ends.exits);
        env.limits = Arc::clone(&self.apps.limits);
        env.wake = self.shell_notify();
        env
    }

    /// Registers the app host of a session that entered the `Live` map.
    pub fn register_app_host(&self, session: &SessionId, host: AppHost) {
        // The advance task listens to the shell wake; an exit that settles
        // app calls wakes it too (the first host binds it).
        let _bound = self.apps.hosts.ends.wake.set(self.shell_notify());
        self.apps.hosts.register(session.clone(), host);
    }

    /// Removes the app host of `session` when it belongs to `process` (see
    /// the module documentation). Returns whether an entry was removed.
    pub fn drop_app_host(
        &self,
        session: &SessionId,
        process: &ProcessIncarnation,
        end: HostEnd,
    ) -> bool {
        self.apps.hosts.drop_host(session, process, end)
    }

    /// The app path of a direct session: the host in the registry, a
    /// forwarder from the session's candidate receiver to the capture
    /// service, and the removal of the host when the pump ends (the process
    /// is gone). A session without a bridge gets none of them and its
    /// receiver is dropped.
    pub(super) fn attach_direct_apps(
        &self,
        session_id: &SessionId,
        process: &ProcessIncarnation,
        turns: &Arc<SessionTurns>,
        session: &mut dyn LiveSession,
        pump: JoinHandle<()>,
    ) {
        let candidates = session.take_app_candidates();
        let Some(bridge) = session.app_bridge() else {
            return;
        };
        self.register_app_host(
            session_id,
            AppHost {
                bridge: Arc::clone(&bridge),
                turns: Arc::clone(turns),
                process: process.clone(),
            },
        );
        if let Some(mut candidates) = candidates {
            let (capture, session) = (self.apps.capture.clone(), session_id.clone());
            let bridge = Arc::clone(&bridge);
            tokio::spawn(async move {
                while let Some(candidate) = candidates.recv().await {
                    capture.offer(session.clone(), candidate, Arc::clone(&bridge));
                }
            });
        }
        let (hosts, session, process) = (self.apps.hosts(), session_id.clone(), process.clone());
        tokio::spawn(async move {
            // The pump ends when the process ends; a panic ends it too.
            let _ended = pump.await;
            hosts.drop_host(&session, &process, HostEnd::Exited);
        });
    }

    /// The app path of a session under a shim: the host in the registry and
    /// the app sink on the driver, which hands over the candidates that it
    /// kept. Called when the session enters the `Live` map.
    pub(super) fn attach_durable_apps(
        &self,
        session_id: &SessionId,
        process: &ProcessIncarnation,
        turns: &Arc<SessionTurns>,
        driver: &Durable,
    ) {
        match driver {
            Durable::Claude(d) => self.wire_durable(session_id, process, turns, d),
            Durable::Codex(d) => self.wire_durable(session_id, process, turns, d),
        }
    }

    fn wire_durable<C: DurableCore>(
        &self,
        session_id: &SessionId,
        process: &ProcessIncarnation,
        turns: &Arc<SessionTurns>,
        driver: &Arc<DurableDriver<C>>,
    ) where
        DurableDriver<C>: AppBridge,
    {
        let bridge: Arc<dyn AppBridge> = Arc::clone(driver) as Arc<dyn AppBridge>;
        self.register_app_host(
            session_id,
            AppHost {
                bridge,
                turns: Arc::clone(turns),
                process: process.clone(),
            },
        );
        // The sink holds the driver weakly: the driver holds the sink.
        let (weak, capture, session) = (
            Arc::downgrade(driver),
            self.apps.capture.clone(),
            session_id.clone(),
        );
        driver.attach_app_sink(Arc::new(move |candidate| {
            if let Some(driver) = weak.upgrade() {
                capture.offer(session.clone(), candidate, driver);
            }
        }));
    }

    /// A restored session whose exit is already committed gets no bridge and
    /// no sink: the candidates that its driver kept are dropped.
    pub(super) fn discard_app_candidates(session_id: &SessionId, driver: &Durable) {
        match driver {
            Durable::Claude(d) => d.close_app_sink(session_id),
            Durable::Codex(d) => d.close_app_sink(session_id),
        }
    }
}

impl crate::manager::SessionManager {
    /// The app host of a live session (see [`SessionTarget::app_host`]); the
    /// link methods of the apps use it.
    #[must_use]
    pub fn app_host(&self, session: &SessionId) -> Option<AppHost> {
        self.target.app_host(session)
    }

    /// The environment of the app methods (see [`SessionTarget::app_env`]).
    #[must_use]
    pub fn app_env(&self) -> AppEnv {
        self.target.app_env()
    }
}

#[cfg(test)]
#[path = "apps_wired_tests.rs"]
mod tests;
