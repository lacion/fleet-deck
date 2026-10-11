//! `fleetdeck station serve` (M2 P2-7): the station process without a deck
//! link (Phase 3 adds the link). It opens `station.db`, recovers it, starts
//! the session manager with the adapters of `station.toml`, and serves the
//! local socket until the shutdown signal. At shutdown it stops every live
//! session gracefully.

use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;

use fleetdeck_core::config::StationConfig;
use fleetdeck_core::paths::{NodeDirs, ensure_private_dir};
use fleetdeck_core::{SharedClock, SharedIds, SystemClock, UlidSource};
use fleetdeck_proto::command::StopMode;
use fleetdeck_providers::host::{DirectChildHost, SessionHost};
use tokio::sync::watch;

use crate::local;
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
    let manager =
        Arc::new(SessionManager::start(store, Policy::from_config(config), adapters, ids).await?);
    tracing::info!(
        station = %config.name,
        socket = %socket.display(),
        "station ready"
    );
    let (shutdown, rx) = watch::channel(false);
    let server = tokio::spawn(local::serve(listener, Arc::clone(&manager), rx));
    stop.await;
    tracing::info!("station stopping");
    let _ = shutdown.send(true);
    let _ = server.await;
    manager.shutdown(StopMode::Graceful).await;
    // The pumps store the last events of each process (`session.ended`).
    let deadline = tokio::time::Instant::now() + PUMP_GRACE;
    while manager.live_sessions() > 0 && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    if let Err(e) = std::fs::remove_file(&socket) {
        tracing::debug!(error = %e, "socket file");
    }
    Ok(())
}
