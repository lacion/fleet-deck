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
use std::time::Instant;

use fleetdeck_core::config::DeckConfig;
use fleetdeck_core::identity::{Identity, IdentityError};
use fleetdeck_core::localsock::BindError;
use fleetdeck_core::paths::{Env, NodeDirs, ensure_private_dir};
use fleetdeck_core::restart::Restart;
use fleetdeck_core::settings::{DeckHolder, SettingsWriter};
use fleetdeck_core::tailscale;
use fleetdeck_core::tailscale_wsl::{self, HostFacts};
use fleetdeck_core::{SharedClock, SharedIds, SystemClock, UlidSource};
use fleetdeck_link::reconnect::BackoffPolicy;
use fleetdeck_proto::StationId;
use tokio::sync::watch;

use crate::cert_task::CertState;
use crate::http::endpoints::Endpoints;
use crate::http::tls::TlsError;
use crate::http::{Board, BoardServer, BoardTls, LoginLimits, Origins};
use crate::link::{DeckNode, NodeError, NodeOptions};
use crate::local::{self, LocalDeck};
use crate::local_auth::BoardEndpoint;
use crate::store::{DeckError, DeckStore};
use crate::tailnet_auto::{self, SharedStart, TailnetStart};

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

/// The time between two sweeps of the coordinator grants.
pub const GRANT_SWEEP_EVERY: std::time::Duration = std::time::Duration::from_secs(30);

/// Starts the task that ends every grant past its lifetime or idle and
/// rebuilds the derived `coordinate` scopes (grant stations design §2.3 item
/// 7). The link cache follows for each station that changed. It stops when
/// `shutdown` turns true.
pub fn spawn_grant_sweep(
    store: DeckStore,
    peers: Arc<crate::peers::DeckPeers>,
    every: std::time::Duration,
    mut shutdown: watch::Receiver<bool>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval_at(tokio::time::Instant::now() + every, every);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                _ = tick.tick() => {
                    match store.sweep_grants().await {
                        Ok(changed) => tracing::debug!(stations = changed.len(), "the grant sweep ran"),
                        Err(e) => tracing::warn!(error = %e, "the grant sweep failed"),
                    }
                    if let Err(e) = peers.refresh_changed().await {
                        tracing::warn!(error = %e, "refresh of the link scopes");
                    }
                }
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() {
                        return;
                    }
                }
            }
        }
    })
}

/// A test hook of the board certificate (see [`StartOptions::certificate_check`]).
pub type CertificateCheck = fn(&[SocketAddr]) -> Result<(), TlsError>;

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
    /// A test hook: runs with the bound board addresses before the board
    /// certificate is prepared; an error is a certificate failure.
    #[doc(hidden)]
    pub certificate_check: Option<CertificateCheck>,
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
    /// What the automatic tailnet start did (the Tailnet card shows it).
    tailnet_start: SharedStart,
    server: tokio::task::JoinHandle<()>,
    /// The automations scheduler (R5, DL-168).
    scheduler: tokio::task::JoinHandle<()>,
    /// The wake rule of the wall (R13 §6.6).
    wake: tokio::task::JoinHandle<()>,
    /// The fleet summary sender for the desktop shell (R13 P4).
    fleet_summary: tokio::task::JoinHandle<()>,
    /// The station deletion retries of the artifacts (R13 P5a).
    artifact_deletions: tokio::task::JoinHandle<()>,
    /// The recovery of the owner screenshots (R13 P5a).
    desktop_captures: tokio::task::JoinHandle<()>,
    /// The audit log prune (NV-11).
    maintenance: tokio::task::JoinHandle<()>,
    /// The sender and poller of the home attempts (FG-5).
    homes: tokio::task::JoinHandle<()>,
    /// The land tasks and the radar task (FG-7, FG-8).
    land: [tokio::task::JoinHandle<()>; 4],
    /// The move task (FG-9).
    moves: tokio::task::JoinHandle<()>,
    /// The sweep of the grants that ended (grant stations design §2.3).
    grant_sweep: tokio::task::JoinHandle<()>,
    /// The Tailscale certificate of the board (DL-190).
    cert: crate::cert_task::CertTask,
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
        let settings_path = dirs.deck_config();
        let StartOptions {
            overrides,
            restart,
            local_station,
            certificate_check,
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
        // "Use Tailscale automatically" (tailnet design 14.4) runs before
        // any listener binds.
        let (config, mut start) = automatic_start(&store, &deck_settings, config).await;
        let config = &config;
        store.set_conflict_mail(config.conflict_mail);
        store.set_fleet_control(config.fleet_control);
        let socket = local::socket_path(&deck_dir);
        let listener = local::bind(&socket).await?;
        let identity = Arc::new(Identity::load_or_create(&deck_dir)?);
        let overridden = |key: &str| deck_settings.overrides.iter().any(|(k, _)| k == key);
        // With the mode off every entry is manual: the provenance lists are
        // ignored.
        let (link_manual, link_auto) = if overridden("link_listen") || !config.tailnet_auto {
            (config.link_listen.clone(), Vec::new())
        } else {
            split(&config.link_listen, &config.tailnet_auto_link_listen)
        };
        let (board_manual, board_auto) = if overridden("listen") || !config.tailnet_auto {
            (config.listen.clone(), Vec::new())
        } else {
            split(&config.listen, &config.tailnet_auto_listen)
        };
        let node = DeckNode::start_with(
            store.clone(),
            Arc::clone(&identity),
            &link_manual,
            backoff,
            NodeOptions {
                optional_listen: link_auto,
                ..NodeOptions::default()
            },
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
        let board_settings = deck_settings.clone();
        let live_cell = crate::live::LiveCell::new(live);
        let board_live = live_cell.clone();
        let tailnet_start: SharedStart = Arc::default();
        // The state cell of the certificate task: the Board and the task
        // share it (DL-190).
        let cert_state = Arc::new(std::sync::RwLock::new(CertState::off()));
        // The board state and its certificate, built from the bound
        // addresses. A new closure per try: the certificate fallback binds
        // again. `kept` is the stored pair for the fallback (design 15.3).
        let build = |kept: Option<BoardTls>| {
            let tailnet = tailnet.clone();
            let board_store = board_store.clone();
            let board_link = Arc::clone(&board_link);
            let board_node = Arc::clone(&board_node);
            let tls_dir = tls_dir.clone();
            let board_elevations = Arc::clone(&board_elevations);
            let board_settings = board_settings.clone();
            let board_live = board_live.clone();
            let board_start = Arc::clone(&tailnet_start);
            let restart = restart.clone();
            let local_station = Arc::clone(&local_station);
            let cert_state = Arc::clone(&cert_state);
            move |bound: &[SocketAddr]| {
                let tls = match kept.clone() {
                    // The fallback (design 15.3): the stored pair, never made again.
                    Some(tls) => tls,
                    None => {
                        if let Some(check) = certificate_check {
                            check(bound)?;
                        }
                        BoardTls::load_or_create(&tls_dir, &cert_names(bound, tailnet.as_deref()))?
                    }
                };
                // The resolver names the host that may get the Tailscale
                // certificate (DL-190).
                let tls = tls.with_name(tailnet.as_deref())?;
                let origins = Origins::new(bound, tailnet.as_deref());
                let endpoints = Arc::new(Endpoints::new(
                    origins.list(),
                    Arc::clone(&tls.resolver),
                    tls.self_signed,
                ));
                let board = Arc::new(Board {
                    store: board_store,
                    link: board_link,
                    origins,
                    limiter: LoginLimits::default(),
                    device_links: crate::http::DeviceLinkLimiter::default(),
                    certificate_sha256: tls.sha256.clone(),
                    self_signed: tls.self_signed,
                    endpoints,
                    cert: cert_state,
                    tickets: crate::http::ws::Tickets::default(),
                    ws_recheck: crate::http::ws::RECHECK,
                    ws_display_send: crate::http::ws::DISPLAY_SEND,
                    pairing: Some(board_node),
                    assets: crate::http::assets::BoardAssets::Embedded,
                    live: board_live,
                    elevations: board_elevations,
                    settings: Some(board_settings),
                    restart,
                    local_station,
                    tailnet_start: board_start,
                    host: tailscale_wsl::detect,
                });
                Ok((board, tls))
            }
        };
        // Read before the first try can write (design 15.3).
        let kept = if board_auto.is_empty() {
            None
        } else {
            BoardTls::load_kept(&deck_dir)
        };
        let board = match BoardServer::bind_with(
            &board_manual,
            &board_auto,
            build(None),
            rx.clone(),
        )
        .await
        {
            Ok(board) => board,
            // The certificate for the automatic entries cannot be made
            // (design 15.3): bind the manual entries only, with the stored
            // pair that was read before the first try (never made again).
            // With no stored pair, the pair is made for the manual entries;
            // a failure then fails the start.
            Err(crate::http::BindError::Build(e)) if !board_auto.is_empty() => {
                tracing::warn!(error = %e, "the board certificate for the automatic tailnet entries could not be made");
                start.start_reason = Some(certificate_reason(&e));
                BoardServer::bind_with(&board_manual, &[], build(kept), rx.clone())
                    .await
                    .map_err(ServeError::Board)?
            }
            Err(e) => return Err(ServeError::Board(e)),
        };
        start.skipped = node
            .skipped()
            .iter()
            .chain(&board.skipped)
            .cloned()
            .collect();
        *tailnet_start
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = start;
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
        let cert_live = live_cell.clone();
        let wake_live = live_cell.clone();
        // The same origins and resolver as the board's endpoints.
        let local_endpoints = Arc::new(Endpoints::new(
            Origins::new(&board.addrs, tailnet.as_deref()).list(),
            Arc::clone(&board.tls.resolver),
            board.tls.self_signed,
        ));
        let local = Arc::new(LocalDeck {
            node: Arc::clone(&node),
            board: endpoint.clone(),
            endpoints: local_endpoints,
            elevations,
            settings: Some(deck_settings),
            live: live_cell,
        });
        let summary_endpoints = Arc::clone(&local.endpoints);
        let server = tokio::spawn(local::serve(listener, local, rx.clone()));
        let maintenance = crate::maintenance::spawn(store.clone(), rx.clone());
        let grant_sweep = spawn_grant_sweep(
            store.clone(),
            Arc::clone(&node.link().peers),
            GRANT_SWEEP_EVERY,
            rx.clone(),
        );
        let cert = crate::cert_task::spawn(
            crate::cert_task::CertDeps {
                live: cert_live.clone(),
                resolver: Arc::clone(&board.tls.resolver),
                dir: deck_dir.clone(),
                hostname: config.tailnet_hostname.clone(),
                issuer: Arc::new(crate::cert_task::TailscaleIssuer::new(cert_live)),
                clock: Arc::new(SystemClock),
                anchors: crate::http::tls_tailscale::isrg_anchors(),
                intervals: crate::cert_task::Intervals::PRODUCTION,
                state: cert_state,
            },
            rx.clone(),
        );
        let homes = tokio::spawn(crate::fleet_git_homes::run(
            store.clone(),
            Arc::clone(node.link()),
            Arc::clone(&node.link().homes_kick),
            rx.clone(),
            crate::fleet_git_homes::HomesRunOptions { send: true },
        ));
        // The land tasks: admission and status poll, outbox, fence sender (FG-8).
        let link = node.link();
        let kick = Arc::clone(&link.land_kick);
        let land = [
            tokio::spawn(crate::fleet_git_land::run(
                store.clone(),
                Arc::clone(link),
                Arc::clone(&kick),
                rx.clone(),
            )),
            tokio::spawn(crate::fleet_git_land_outbox::run(
                store.clone(),
                Arc::clone(link),
                Arc::clone(&kick),
                rx.clone(),
            )),
            tokio::spawn(crate::fleet_git_land_abandon::run(
                store.clone(),
                Arc::clone(link),
                kick,
                rx.clone(),
            )),
            tokio::spawn(crate::fleet_git_radar::run(
                store.clone(),
                Arc::clone(link),
                Arc::clone(&link.radar_state),
                Arc::clone(&link.radar_kick),
                rx.clone(),
                crate::fleet_git_radar::RadarOptions::PRODUCTION,
            )),
        ];
        // The move task: the steps of each session move (FG-9).
        let moves = tokio::spawn(crate::fleet_git_move::run(
            store.clone(),
            Arc::clone(link),
            Arc::clone(&link.move_kick),
            crate::fleet_git_move::MoveOptions::PRODUCTION,
            rx.clone(),
        ));
        let wake = crate::wake::spawn(
            store.clone(),
            Arc::clone(node.link()),
            wake_live,
            rx.clone(),
        );
        let fleet_summary = crate::fleet_summary::spawn(
            store.clone(),
            Arc::clone(node.link()),
            summary_endpoints,
            rx.clone(),
        );
        let artifact_deletions =
            crate::artifact_deletions::spawn(store.clone(), Arc::clone(node.link()), rx.clone());
        let desktop_captures =
            crate::desktop_capture::spawn(store.clone(), Arc::clone(node.link()), rx.clone());
        let scheduler = crate::scheduler::spawn(store.clone(), Arc::clone(node.link()), rx);
        Ok(Self {
            node,
            identity,
            store,
            endpoint,
            board,
            tailnet_start,
            server,
            scheduler,
            wake,
            fleet_summary,
            artifact_deletions,
            desktop_captures,
            maintenance,
            homes,
            land,
            moves,
            grant_sweep,
            cert,
            shutdown,
            socket,
        })
    }

    /// The Tailscale certificate task: its state is for the Tailnet card.
    #[must_use]
    pub fn cert(&self) -> &crate::cert_task::CertTask {
        &self.cert
    }

    /// What the automatic tailnet start did, as the Tailnet card shows it.
    #[must_use]
    pub fn tailnet_start(&self) -> TailnetStart {
        self.tailnet_start
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Stops the board, the local socket and the link node.
    pub async fn stop(self) {
        tracing::info!("deck stopping");
        let _ = self.shutdown.send(true);
        let _ = self.server.await;
        let _ = self.scheduler.await;
        let _ = self.wake.await;
        let _ = self.fleet_summary.await;
        let _ = self.artifact_deletions.await;
        let _ = self.desktop_captures.await;
        let _ = self.maintenance.await;
        let _ = self.homes.await;
        for task in self.land {
            let _ = task.await;
        }
        let _ = self.moves.await;
        let _ = self.grant_sweep.await;
        self.cert.join().await;
        self.board.join().await;
        self.node.shutdown();
        if let Err(e) = std::fs::remove_file(&self.socket) {
            tracing::debug!(error = %e, "socket file");
        }
    }
}

/// The reason on the Tailnet card when the board certificate for the
/// automatic entries could not be made (design 15.3).
fn certificate_reason(error: &TlsError) -> String {
    let kind = match error {
        TlsError::File { .. } => "file".to_owned(),
        TlsError::Generate(_) => "generate".to_owned(),
        TlsError::Rustls(_) => "tls".to_owned(),
        TlsError::Io(e) => format!("{:?}", e.kind()),
    };
    format!("the board certificate could not be updated ({kind})")
}

/// Splits `list` into the entries that the automatic mode added (`auto`) and
/// the manual ones.
fn split(list: &[SocketAddr], auto: &[SocketAddr]) -> (Vec<SocketAddr>, Vec<SocketAddr>) {
    list.iter().partition(|a| !auto.contains(a))
}

/// Runs the automatic tailnet start when the setting is on. The host checks
/// run in a blocking task inside their own deadline of 3 s. Writes the audit
/// row `tailnet.auto` when a save happened.
async fn automatic_start(
    store: &DeckStore,
    settings: &crate::http::settings_routes::DeckSettings,
    config: &DeckConfig,
) -> (DeckConfig, TailnetStart) {
    if !config.tailnet_auto {
        return (config.clone(), TailnetStart::default());
    }
    // The one deadline of the program lookup, the status call and the save.
    // The host checks below keep their own deadline.
    let deadline = Instant::now() + tailnet_auto::DEADLINE;
    let facts = tokio::time::timeout(
        tailnet_auto::DEADLINE,
        tokio::task::spawn_blocking(tailscale_wsl::detect),
    )
    .await;
    let facts: HostFacts = match facts {
        Ok(Ok(facts)) => facts,
        Ok(Err(_)) | Err(_) => {
            tracing::warn!("the host checks of the automatic tailnet start did not finish");
            let start = TailnetStart {
                enabled: true,
                start_reason: Some(tailnet_auto::HOST_TIMEOUT.to_owned()),
                skipped: Vec::new(),
            };
            return (config.clone(), start);
        }
    };
    let (next, start) = tailnet_auto::apply_at_start(
        settings,
        config.clone(),
        &settings.overrides,
        &facts,
        &tailscale::is_usable,
        deadline,
    )
    .await;
    tailnet_auto::audit(store, config, &next).await;
    (next, start)
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
