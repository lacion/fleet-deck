//! `fleetdeck deck serve` (M2 P3-6b): the deck process. It opens `deck.db`,
//! loads (or creates) the deck identity key, starts the link node
//! (listeners for stations that dial in, dialers to stations that listen,
//! [`crate::link::DeckNode`]) and serves `deck.sock` until the shutdown
//! signal. The board API (P3-8) listens on the `listen` addresses of
//! deck.toml.

use std::future::Future;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use fleetdeck_core::config::DeckConfig;
use fleetdeck_core::identity::{Identity, IdentityError};
use fleetdeck_core::localsock::BindError;
use fleetdeck_core::paths::{NodeDirs, ensure_private_dir};
use fleetdeck_core::{SharedClock, SharedIds, SystemClock, UlidSource};
use fleetdeck_link::reconnect::BackoffPolicy;
use tokio::sync::watch;

use crate::http::tls::TlsError;
use crate::http::{Board, BoardServer, BoardTls, LoginLimiter, Origins};
use crate::link::{DeckNode, NodeError};
use crate::local::{self, LocalDeck};
use crate::local_auth::BoardEndpoint;
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
    #[error(transparent)]
    Board(crate::http::BindError<TlsError>),
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
    let node = DeckNode::start(
        store.clone(),
        Arc::clone(&identity),
        &config.link_listen,
        backoff,
    )
    .await?;
    let (shutdown, rx) = watch::channel(false);
    let tailnet = config.tailnet_hostname.clone();
    let board_store = store.clone();
    let board_link = Arc::clone(node.link());
    let tls_dir = deck_dir.clone();
    let board = BoardServer::bind(
        &config.listen,
        move |bound| {
            let tls = BoardTls::load_or_create(&tls_dir, &cert_names(bound, tailnet.as_deref()))?;
            let board = Arc::new(Board {
                store: board_store,
                link: board_link,
                origins: Origins::new(bound, tailnet.as_deref()),
                limiter: LoginLimiter::default(),
                tickets: crate::http::ws::Tickets::default(),
                ws_recheck: crate::http::ws::RECHECK,
            });
            Ok((board, tls))
        },
        rx.clone(),
    )
    .await
    .map_err(ServeError::Board)?;
    let endpoint = board.addrs.first().map(|a| BoardEndpoint {
        url: format!("https://{a}"),
        certificate_sha256: board.tls.sha256.clone(),
        self_signed: board.tls.self_signed,
    });
    tracing::info!(
        deck = %config.name,
        key = %identity.fingerprint().short(),
        socket = %socket.display(),
        link_listen = ?node.listen_addrs(),
        board = ?board.addrs,
        board_certificate = %board.tls.sha256,
        "deck ready"
    );
    let local = Arc::new(LocalDeck {
        node: Arc::clone(&node),
        board: endpoint,
    });
    let server = tokio::spawn(local::serve(listener, local, rx));
    stop.await;
    tracing::info!("deck stopping");
    let _ = shutdown.send(true);
    let _ = server.await;
    board.join().await;
    node.shutdown();
    if let Err(e) = std::fs::remove_file(&socket) {
        tracing::debug!(error = %e, "socket file");
    }
    Ok(())
}

/// The names of the board certificate: every bound address (an unspecified
/// address names nothing useful and is left out), `localhost`, and the
/// tailnet host name.
fn cert_names(bound: &[SocketAddr], tailnet: Option<&str>) -> Vec<String> {
    let mut names: Vec<String> = bound
        .iter()
        .map(|a| a.ip())
        .filter(|ip| !ip.is_unspecified())
        .map(|ip| ip.to_string())
        .collect();
    names.push("localhost".to_owned());
    names.extend(tailnet.map(str::to_owned));
    names.sort();
    names.dedup();
    names
}
