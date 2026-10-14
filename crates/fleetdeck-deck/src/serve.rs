//! `fleetdeck deck serve` (M2 P3-6b): the deck process. It opens `deck.db`,
//! loads (or creates) the deck identity key, starts the link node
//! (listeners for stations that dial in, dialers to stations that listen,
//! [`crate::link::DeckNode`]) and serves `deck.sock` until the shutdown
//! signal. The board API (P3-8) listens on the `listen` addresses of
//! deck.toml.

use std::future::Future;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use fleetdeck_core::config::DeckConfig;
use fleetdeck_core::identity::{Identity, IdentityError};
use fleetdeck_core::localsock::BindError;
use fleetdeck_core::paths::{Env, NodeDirs, ensure_private_dir};
use fleetdeck_core::restart::Restart;
use fleetdeck_core::settings::{DeckHolder, SettingsWriter};
use fleetdeck_core::{SharedClock, SharedIds, SystemClock, UlidSource};
use fleetdeck_link::reconnect::BackoffPolicy;
use fleetdeck_proto::StationId;
use tokio::sync::watch;

use crate::http::tls::TlsError;
use crate::http::{Board, BoardServer, BoardTls, LoginLimits, Origins};
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

/// What the process that runs the deck tells it (DL-172).
#[derive(Debug, Clone, Default)]
pub struct StartOptions {
    /// `(key, flag)` of the settings that the process took from the command
    /// line, for example `("listen", "--listen")`.
    pub overrides: Vec<(String, String)>,
    /// Restarts the process. `None` in `fleetdeck up` mode (the local
    /// station restarts the process) and in tests.
    pub restart: Option<Arc<dyn Restart>>,
    /// The id of the local station in `fleetdeck up` mode; the caller sets
    /// it after the pairing. A restart of the deck then restarts that
    /// station.
    pub local_station: Arc<OnceLock<StationId>>,
}

/// Runs the deck until `stop` completes.
///
/// # Errors
/// [`ServeError`] at start; a running deck only logs errors.
pub async fn serve(
    dirs: &NodeDirs,
    config: &DeckConfig,
    backoff: BackoffPolicy,
    options: StartOptions,
    stop: impl Future<Output = ()>,
) -> Result<(), ServeError> {
    let deck = RunningDeck::start(dirs, config, backoff, options).await?;
    stop.await;
    deck.stop().await;
    Ok(())
}

/// A started deck: the link node, the board server and the local socket
/// (M2 P4-4: `fleetdeck up` runs it next to a station).
#[derive(Debug)]
pub struct RunningDeck {
    pub node: Arc<DeckNode>,
    pub identity: Arc<Identity>,
    pub store: DeckStore,
    /// The first board address, its certificate and whether it is
    /// self-signed (`None` without a board listener).
    pub endpoint: Option<BoardEndpoint>,
    board: BoardServer,
    server: tokio::task::JoinHandle<()>,
    /// The automations scheduler (R5, DL-168).
    scheduler: tokio::task::JoinHandle<()>,
    shutdown: watch::Sender<bool>,
    socket: PathBuf,
}

impl RunningDeck {
    /// Opens `deck.db`, starts the link node, the board and the local socket.
    ///
    /// # Errors
    /// [`ServeError`].
    pub async fn start(
        dirs: &NodeDirs,
        config: &DeckConfig,
        backoff: BackoffPolicy,
        options: StartOptions,
    ) -> Result<Self, ServeError> {
        let deck_dir = dirs.deck();
        ensure_private_dir(&deck_dir).map_err(|source| ServeError::Dir {
            path: deck_dir.clone(),
            source,
        })?;
        let clock: SharedClock = Arc::new(SystemClock);
        let ids: SharedIds = Arc::new(UlidSource::new(Arc::clone(&clock)));
        let store = DeckStore::open(&dirs.deck_db(), clock, ids).await?;
        store.set_conflict_mail(config.conflict_mail);
        store.set_fleet_control(config.fleet_control);
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
        let board_node = Arc::clone(&node);
        let tls_dir = deck_dir.clone();
        let elevations = Arc::new(crate::elevation::Elevations::default());
        let board_elevations = Arc::clone(&elevations);
        let live = crate::live::DeckLive::of(config);
        let settings_path = dirs.deck_config();
        let StartOptions {
            overrides,
            restart,
            local_station,
        } = options;
        let deck_settings = crate::http::settings_routes::DeckSettings {
            writer: Arc::new(SettingsWriter::<DeckHolder>::new(
                &settings_path,
                None,
                &Env::default(),
            )),
            path: settings_path,
            overrides,
            gate: Arc::default(),
        };
        let board_settings = deck_settings.clone();
        let live_cell = crate::live::LiveCell::new(live);
        let board_live = live_cell.clone();
        let board = BoardServer::bind(
            &config.listen,
            move |bound| {
                let tls =
                    BoardTls::load_or_create(&tls_dir, &cert_names(bound, tailnet.as_deref()))?;
                let board = Arc::new(Board {
                    store: board_store,
                    link: board_link,
                    origins: Origins::new(bound, tailnet.as_deref()),
                    limiter: LoginLimits::default(),
                    device_links: crate::http::DeviceLinkLimiter::default(),
                    certificate_sha256: tls.sha256.clone(),
                    self_signed: tls.self_signed,
                    tickets: crate::http::ws::Tickets::default(),
                    ws_recheck: crate::http::ws::RECHECK,
                    pairing: Some(board_node),
                    assets: crate::http::assets::BoardAssets::Embedded,
                    live: board_live,
                    elevations: board_elevations,
                    settings: Some(board_settings),
                    restart,
                    local_station,
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
            board: endpoint.clone(),
            elevations,
            settings: Some(deck_settings),
            live: live_cell,
        });
        let server = tokio::spawn(local::serve(listener, local, rx.clone()));
        let scheduler = crate::scheduler::spawn(store.clone(), Arc::clone(node.link()), rx);
        Ok(Self {
            node,
            identity,
            store,
            endpoint,
            board,
            server,
            scheduler,
            shutdown,
            socket,
        })
    }

    /// Stops the board, the local socket and the link node.
    pub async fn stop(self) {
        tracing::info!("deck stopping");
        let _ = self.shutdown.send(true);
        let _ = self.server.await;
        let _ = self.scheduler.await;
        self.board.join().await;
        self.node.shutdown();
        if let Err(e) = std::fs::remove_file(&self.socket) {
            tracing::debug!(error = %e, "socket file");
        }
    }
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
