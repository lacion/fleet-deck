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
use fleetdeck_core::paths::{NodeDirs, ensure_private_dir};
use fleetdeck_core::{SharedClock, SharedIds, SystemClock, UlidSource};
use fleetdeck_link::reconnect::BackoffPolicy;
use fleetdeck_proto::command::StopMode;
use fleetdeck_providers::host::shim::host::{ShimHost, ShimHostConfig};
use fleetdeck_providers::host::{DirectChildHost, SessionHost};
use tokio::sync::watch;

use crate::link::{NodeError, StationNode};
use crate::local;
use crate::manager::target::ShimSettings;
use crate::manager::{Policy, SessionManager, adapters_from_config};
use crate::store::{Store, StoreConfig};

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
    stop: impl Future<Output = ()>,
) -> Result<(), ServeError> {
    let station = RunningStation::start(dirs, config).await?;
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
}

impl RunningStation {
    /// Opens and recovers `station.db`, starts the manager, the link node
    /// and the local socket.
    ///
    /// # Errors
    /// [`ServeError`].
    pub async fn start(dirs: &NodeDirs, config: &StationConfig) -> Result<Self, ServeError> {
        let station_dir = dirs.station();
        ensure_private_dir(&station_dir).map_err(|source| ServeError::Dir {
            path: station_dir.clone(),
            source,
        })?;
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
        let policy = Policy::from_config(config);
        let manager = Arc::new(match config.host {
            HostChoice::Direct => SessionManager::start(store, policy, adapters, ids).await?,
            HostChoice::Shim => {
                let shims = shim_settings(config, &station_dir, &store)?;
                SessionManager::start_with_shims(store, policy, adapters, ids, shims).await?
            }
        });
        let identity = Arc::new(Identity::load_or_create(&station_dir)?);
        // A station restart ends every shell (PLAN §6.8, A10): the PTYs
        // close with the process.
        let shells = Arc::new(crate::shell::Shells::new(crate::shell::ShellConfig {
            allow: config.allow_shell,
            max_shells: config.max_shells,
            program: None,
        }));
        let node = StationNode::start_with_shells(
            Arc::clone(&manager),
            Arc::clone(&identity),
            config.name.clone(),
            &config.listen,
            BackoffPolicy::default(),
            Arc::clone(&shells),
        )
        .await?;
        tracing::info!(
            station = %config.name,
            key = %identity.fingerprint().short(),
            socket = %socket.display(),
            listen = ?node.listen_addrs().await,
            "station ready"
        );
        let (shutdown, rx) = watch::channel(false);
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
        })
    }

    /// Stops the local socket and the link node, then every live session
    /// gracefully.
    pub async fn stop(self) {
        tracing::info!("station stopping");
        let _ = self.shutdown.send(true);
        let _ = self.server.await;
        self.node.shutdown();
        self.shells.close_all();
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
