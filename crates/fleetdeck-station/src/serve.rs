//! `fleetdeck station serve` (M2 P2-7, P3-5c): the station process. It
//! opens `station.db`, recovers it, starts the session manager with the
//! adapters of `station.toml`, loads (or creates) the station identity key,
//! starts the link node (listeners and dialers of the paired deck,
//! [`crate::link::StationNode`]) and serves the local socket until the
//! shutdown signal. At shutdown it stops every live session gracefully.

use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;

use fleetdeck_core::config::{HostChoice, StationConfig};
use fleetdeck_core::identity::{Identity, IdentityError};
use fleetdeck_core::paths::{Env, NodeDirs, ensure_private_dir};
use fleetdeck_core::restart::Restart;
use fleetdeck_core::{SharedClock, SharedIds, SystemClock, UlidSource};
use fleetdeck_link::reconnect::BackoffPolicy;
use fleetdeck_proto::command::StopMode;
use fleetdeck_providers::host::shim::host::{ShimHost, ShimHostConfig};
use fleetdeck_providers::host::{DirectChildHost, SessionHost};
use tokio::sync::watch;

use crate::link::{NodeError, StationNode};
use crate::local;
use crate::manager::target::ShimSettings;
use crate::manager::{Policy, SessionManager, adapters_from_config, setup_envs_from_config};
use crate::store::{Store, StoreConfig};
use crate::workspace::{PrivateEnv, PrivatePaths};

/// How long the shutdown waits for the pumps to store the last events.
pub const PUMP_GRACE: std::time::Duration = std::time::Duration::from_secs(5);

/// Why the station cannot start.
#[derive(Debug, thiserror::Error)]
pub enum ServeError {
    #[error("station directory `{path}`: {source}")]
    Dir {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error(transparent)]
    Store(#[from] crate::store::StoreError),
    #[error("provider instance environment: {0}")]
    ChildEnv(#[from] fleetdeck_core::childenv::ChildEnvError),
    #[error(transparent)]
    Local(#[from] local::LocalError),
    #[error("station identity key: {0}")]
    Identity(#[from] IdentityError),
    #[error("station link: {0}")]
    Link(#[from] NodeError),
    #[error("shim host: the station executable is unknown: {0}")]
    ShimExe(std::io::Error),
}

/// The shim host of `host = "shim"` (ADR-005 §1, §5.1): this executable
/// is the shim (`fleetdeck shim`), the journals live in the station
/// directory, the sockets in `$XDG_RUNTIME_DIR` (or the private temporary
/// directory), and the output buffer is `shim_buffer_mib`.
fn shim_settings(
    config: &StationConfig,
    station_dir: &std::path::Path,
    store: &Store,
) -> Result<ShimSettings, ServeError> {
    let exe = std::env::current_exe().map_err(ServeError::ShimExe)?;
    let mut host = ShimHostConfig::new(
        exe,
        station_dir.to_owned(),
        store.incarnation().as_str().to_owned(),
    );
    host.buffer_bytes = usize::try_from(config.shim_buffer_mib)
        .unwrap_or(usize::MAX)
        .saturating_mul(1024 * 1024);
    Ok(ShimSettings::new(Arc::new(ShimHost::new(host))))
}

/// Runs the station until `stop` completes.
///
/// # Errors
/// [`ServeError`] at start; a running station only logs errors.
pub async fn serve(
    dirs: &NodeDirs,
    config: &StationConfig,
    restart: Option<Arc<dyn Restart>>,
    stop: impl Future<Output = ()>,
) -> Result<(), ServeError> {
    let station = RunningStation::start(dirs, config, None, restart).await?;
    stop.await;
    station.stop().await;
    Ok(())
}

/// A started station: the session manager, the link node and the local
/// socket (M2 P4-4: `fleetdeck up` runs it next to the deck).
#[derive(Debug)]
pub struct RunningStation {
    pub manager: Arc<SessionManager>,
    pub node: Arc<StationNode>,
    pub identity: Arc<Identity>,
    server: tokio::task::JoinHandle<()>,
    shutdown: watch::Sender<bool>,
    socket: PathBuf,
    shells: Arc<crate::shell::Shells>,
    /// The model-help watcher (R10, DL-167).
    assist: tokio::task::JoinHandle<()>,
}

impl RunningStation {
    /// Opens and recovers `station.db`, starts the manager, the link node
    /// and the local socket. `deck` is the deck config file and the deck
    /// data directory when the deck runs in this process (`fleetdeck up`);
    /// they are private paths of the station too (DL-173). `restart` is the
    /// restart request of the process (DL-172); without it `station.restart`
    /// answers `unsupported`.
    ///
    /// # Errors
    /// [`ServeError`].
    pub async fn start(
        dirs: &NodeDirs,
        config: &StationConfig,
        deck: Option<(PathBuf, PathBuf)>,
        restart: Option<Arc<dyn Restart>>,
    ) -> Result<Self, ServeError> {
        let station_dir = dirs.station();
        ensure_private_dir(&station_dir).map_err(|source| ServeError::Dir {
            path: station_dir.clone(),
            source,
        })?;
        // From here the live database advances: a rollback of an activated
        // update is binary-only (ADR-005 §3.3, DL-128).
        if let Err(e) = crate::upgrade::mark_live(&station_dir.join("upgrades")) {
            tracing::warn!(error = %e, "could not mark the activated update live");
        }
        let clock: SharedClock = Arc::new(SystemClock);
        let ids: SharedIds = Arc::new(UlidSource::new(Arc::clone(&clock)));
        let store = Store::open(
            &dirs.station_db(),
            clock,
            Arc::clone(&ids),
            StoreConfig::default(),
        )
        .await?;
        let host: Arc<dyn SessionHost> = Arc::new(DirectChildHost::new());
        let adapters = adapters_from_config(config, &host, Some(station_dir.join("raw")))?;
        let socket = local::socket_path(&station_dir);
        let listener = local::bind(&socket).await?;
        let policy = Policy::from_config(config).with_setup_envs(setup_envs_from_config(config)?);
        let manager = Arc::new(match config.host {
            HostChoice::Direct => SessionManager::start(store, policy, adapters, ids).await?,
            HostChoice::Shim => {
                let shims = shim_settings(config, &station_dir, &store)?;
                SessionManager::start_with_shims(store, policy, adapters, ids, shims).await?
            }
        });
        let assist = Arc::new(crate::assist::Assist::new(
            Arc::clone(manager.policy_hub()),
            manager.store().clone(),
            assist_instances(config),
        ));
        manager.set_assist(Arc::clone(&assist));
        // Before the link starts: the file and artifact methods refuse the
        // station's own credential and config locations (DL-173).
        manager.set_private_paths(Arc::new(PrivatePaths::new(
            dirs,
            Env::from_process().home.as_deref(),
            deck.as_ref().map(|(c, d)| (c.as_path(), d.as_path())),
            &PrivateEnv::from_process(),
        )));
        manager.set_settings(Arc::new(crate::settings_svc::StationSettings::new(
            &dirs.station_config(),
            &station_dir.join("secrets"),
            Env::from_process(),
            restart,
        )));
        let (shutdown, rx) = watch::channel(false);
        let assist_task = tokio::spawn(crate::assist::watch::run(
            assist,
            Arc::clone(&manager),
            rx.clone(),
        ));
        let identity = Arc::new(Identity::load_or_create(&station_dir)?);
        // A station restart ends every shell (PLAN §6.8, A10): the PTYs
        // close with the process.
        let shells = Arc::new(crate::shell::Shells::new(crate::shell::ShellConfig {
            program: None,
            instances: terminal_instances(config),
        }));
        let node = StationNode::start_with_dirs(
            Arc::clone(&manager),
            Arc::clone(&identity),
            config.name.clone(),
            &config.listen,
            BackoffPolicy::default(),
            Arc::clone(&shells),
            Some(station_dir.join("paste")),
            Some(station_dir.join("artifacts")),
        )
        .await?;
        tracing::info!(
            station = %config.name,
            key = %identity.fingerprint().short(),
            socket = %socket.display(),
            listen = ?node.listen_addrs().await,
            "station ready"
        );
        let server = tokio::spawn(local::serve(
            listener,
            Arc::clone(&manager),
            Some(Arc::clone(&node)),
            rx,
        ));
        Ok(Self {
            manager,
            node,
            identity,
            server,
            shutdown,
            socket,
            shells,
            assist: assist_task,
        })
    }

    /// Stops the local socket and the link node, then every live session
    /// gracefully.
    pub async fn stop(self) {
        tracing::info!("station stopping");
        let _ = self.shutdown.send(true);
        let _ = self.server.await;
        let _ = self.assist.await;
        self.node.shutdown();
        // The stop waits for the terminal processes (up to the hangup grace).
        let shells = Arc::clone(&self.shells);
        if let Err(e) = tokio::task::spawn_blocking(move || shells.close_all()).await {
            tracing::warn!(error = %e, "shell close");
        }
        self.manager.shutdown(StopMode::Graceful).await;
        // The pumps store the last events of each process (`session.ended`).
        let deadline = tokio::time::Instant::now() + PUMP_GRACE;
        while self.manager.live_sessions() > 0 && tokio::time::Instant::now() < deadline {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        if let Err(e) = std::fs::remove_file(&self.socket) {
            tracing::debug!(error = %e, "socket file");
        }
    }
}

/// The provider instances that the model help can call (R10, DL-167): each
/// with its binary and its scrubbed environment. An instance whose
/// environment does not build is left out.
fn assist_instances(config: &StationConfig) -> Vec<crate::assist::AssistInstance> {
    let base = fleetdeck_core::childenv::ChildEnv::from_process();
    let mut out = Vec::new();
    for instance in &config.provider_instances {
        match fleetdeck_core::childenv::ChildEnv::for_instance(base.clone(), instance) {
            Ok(env) => out.push(crate::assist::AssistInstance {
                id: instance.id.clone(),
                driver: instance.driver,
                binary: instance.binary.clone(),
                env,
            }),
            Err(e) => tracing::warn!(
                instance = %instance.id,
                error = %e,
                "no model help for this provider instance"
            ),
        }
    }
    out
}

/// The provider instances that a terminal handover can start (DL-140): each
/// with its binary and its scrubbed environment. An instance whose
/// environment does not build has no terminal.
fn terminal_instances(
    config: &StationConfig,
) -> std::collections::BTreeMap<fleetdeck_proto::ProviderInstanceId, crate::shell::TerminalInstance>
{
    let base = fleetdeck_core::childenv::ChildEnv::from_process();
    let mut out = std::collections::BTreeMap::new();
    for instance in &config.provider_instances {
        match fleetdeck_core::childenv::ChildEnv::for_instance(base.clone(), instance) {
            Ok(env) => {
                out.insert(
                    instance.id.clone(),
                    crate::shell::TerminalInstance {
                        driver: instance.driver,
                        binary: instance.binary.clone(),
                        env,
                    },
                );
            }
            Err(e) => tracing::warn!(
                instance = %instance.id,
                error = %e,
                "no terminal handover for this provider instance"
            ),
        }
    }
    out
}
