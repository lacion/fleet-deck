//! `fleetdeck deck serve` (M2 P3-6b): the deck process. It opens `deck.db`,
//! loads (or creates) the deck identity key, starts the link node
//! (listeners for stations that dial in, dialers to stations that listen,
//! [`crate::link::DeckNode`]) and serves `deck.sock` until the shutdown
//! signal. The board API listener comes with P3-9.

use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;

use fleetdeck_core::config::DeckConfig;
use fleetdeck_core::identity::{Identity, IdentityError};
use fleetdeck_core::localsock::BindError;
use fleetdeck_core::paths::{NodeDirs, ensure_private_dir};
use fleetdeck_core::{SharedClock, SharedIds, SystemClock, UlidSource};
use fleetdeck_link::reconnect::BackoffPolicy;
use tokio::sync::watch;

use crate::link::{DeckNode, NodeError};
use crate::local;
use crate::store::{DeckError, DeckStore};

/// Why the deck cannot start.
#[derive(Debug, thiserror::Error)]
pub enum ServeError {
    #[error("deck directory `{path}`: {source}")]
    Dir {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error(transparent)]
    Store(#[from] DeckError),
    #[error(transparent)]
    Local(#[from] BindError),
    #[error("deck identity key: {0}")]
    Identity(#[from] IdentityError),
    #[error("deck link: {0}")]
    Link(#[from] NodeError),
}

/// Runs the deck until `stop` completes.
///
/// # Errors
/// [`ServeError`] at start; a running deck only logs errors.
pub async fn serve(
    dirs: &NodeDirs,
    config: &DeckConfig,
    backoff: BackoffPolicy,
    stop: impl Future<Output = ()>,
) -> Result<(), ServeError> {
    let deck_dir = dirs.deck();
    ensure_private_dir(&deck_dir).map_err(|source| ServeError::Dir {
        path: deck_dir.clone(),
        source,
    })?;
    let clock: SharedClock = Arc::new(SystemClock);
    let ids: SharedIds = Arc::new(UlidSource::new(Arc::clone(&clock)));
    let store = DeckStore::open(&dirs.deck_db(), clock, ids).await?;
    let socket = local::socket_path(&deck_dir);
    let listener = local::bind(&socket).await?;
    let identity = Arc::new(Identity::load_or_create(&deck_dir)?);
    let node = DeckNode::start(store, Arc::clone(&identity), &config.link_listen, backoff).await?;
    tracing::info!(
        deck = %config.name,
        key = %identity.fingerprint().short(),
        socket = %socket.display(),
        link_listen = ?node.listen_addrs(),
        "deck ready"
    );
    let (shutdown, rx) = watch::channel(false);
    let server = tokio::spawn(local::serve(listener, Arc::clone(&node), rx));
    stop.await;
    tracing::info!("deck stopping");
    let _ = shutdown.send(true);
    let _ = server.await;
    node.shutdown();
    if let Err(e) = std::fs::remove_file(&socket) {
        tracing::debug!(error = %e, "socket file");
    }
    Ok(())
}
