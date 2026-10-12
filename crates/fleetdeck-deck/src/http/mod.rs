//! The board HTTP API (M2 P3-8; PLAN §9). Every route goes through the
//! [`guard`] with the scope of the [`methods::API`] table. The listener
//! accepts only the deck's board addresses (D13; checked in `DeckConfig`).
//! The deck serves HTTPS itself ([`tls`]; D13: no reverse proxy, DL-054).

pub mod assets;
pub mod auth_routes;
pub mod command_routes;
pub mod file_routes;
pub mod guard;
pub mod methods;
pub mod plan_routes;
pub mod read_routes;
pub mod station_routes;
pub mod tls;
pub mod workspace_routes;
pub mod ws;

use std::io;
use std::net::SocketAddr;
use std::sync::Arc;

use axum::Router;
use axum::middleware;
use axum::routing::{get, post};
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::watch;

use hyper_util::rt::TokioIo;
use hyper_util::service::TowerToHyperService;
use tokio::task::{JoinHandle, JoinSet};
use tokio_rustls::TlsAcceptor;

pub use auth_routes::{LoginLimiter, SessionView};
pub use guard::{ApiError, Origins};
pub use tls::BoardTls;

use crate::link::DeckLink;
use crate::store::DeckStore;

/// The shared state of the board API.
#[derive(Debug)]
pub struct Board {
    pub store: DeckStore,
    /// The command router (P3-9a).
    pub link: Arc<DeckLink>,
    pub origins: Origins,
    pub limiter: LoginLimiter,
    /// Single-use WebSocket tickets of bearer clients.
    pub tickets: ws::Tickets,
    /// How often an open WebSocket checks its login ([`ws::RECHECK`]).
    pub ws_recheck: std::time::Duration,
    /// The link node for the pairing methods (`None`: they answer 503).
    pub pairing: Option<Arc<crate::link::DeckNode>>,
    /// The board web client (P4-4).
    pub assets: assets::BoardAssets,
}

/// The board API router.
pub fn router(board: Arc<Board>) -> Router {
    let mut r = Router::new();
    for api in methods::command_methods() {
        r = r.route(&methods::route_of(api), post(command_routes::command));
    }
    r.route("/api/auth/login", post(auth_routes::password))
        .route("/api/auth/link", post(auth_routes::link))
        .route("/api/auth/logout", post(auth_routes::logout))
        .route("/api/auth/session", get(auth_routes::session))
        .route(
            "/api/stations",
            get(read_routes::stations).post(station_routes::add),
        )
        .route("/api/stations/invite", post(station_routes::invite))
        .route(
            "/api/stations/{station}/remove",
            post(station_routes::remove),
        )
        .route(
            "/api/stations/{station}/workspaces",
            get(workspace_routes::list),
        )
        .route("/api/stations/{station}/files/list", get(file_routes::list))
        .route("/api/stations/{station}/files/read", get(file_routes::read))
        .route(
            "/api/stations/{station}/files/search",
            get(file_routes::search),
        )
        .route(
            "/api/stations/{station}/sessions/{session}/turns/{turn}/diff",
            get(file_routes::diff),
        )
        .route("/api/plans", get(plan_routes::list))
        .route("/api/plans/mark", post(plan_routes::mark))
        .route("/api/sessions", get(read_routes::sessions))
        .route("/api/commands", get(read_routes::commands))
        .route(
            "/api/commands/{command_id}",
            get(command_routes::get_command),
        )
        .route("/api/audit", get(read_routes::audit))
        .route("/api/ws", get(ws::upgrade))
        .route("/api/ws/ticket", post(ws::ticket))
        .route_layer(middleware::from_fn_with_state(
            Arc::clone(&board),
            guard::guard,
        ))
        .fallback(board_files)
        .with_state(board)
}

/// Every path outside the API table: the board web client (P4-4).
async fn board_files(
    axum::extract::State(board): axum::extract::State<Arc<Board>>,
    method: axum::http::Method,
    uri: axum::http::Uri,
) -> axum::response::Response {
    assets::serve(&board.assets, &method, &uri).await
}

/// Bound board listeners (HTTPS, HTTP/1.1 with upgrades for the WS route).
#[derive(Debug)]
pub struct BoardServer {
    /// The bound addresses (a port 0 in the config gets a real port).
    pub addrs: Vec<SocketAddr>,
    /// The certificate the listeners present.
    pub tls: BoardTls,
    tasks: Vec<JoinHandle<()>>,
}

/// How long a client has for the TLS handshake.
const HANDSHAKE: Duration = Duration::from_secs(10);

impl BoardServer {
    /// Binds every address. The board's [`Origins`] and the certificate
    /// names come from the bound addresses, so `build` gets them and
    /// returns the board state and its TLS configuration.
    ///
    /// # Errors
    /// An address cannot be bound, or `build` fails.
    pub async fn bind<E>(
        addrs: &[SocketAddr],
        build: impl FnOnce(&[SocketAddr]) -> Result<(Arc<Board>, BoardTls), E>,
        shutdown: watch::Receiver<bool>,
    ) -> Result<Self, BindError<E>> {
        let mut listeners = Vec::with_capacity(addrs.len());
        for addr in addrs {
            let l = TcpListener::bind(addr)
                .await
                .map_err(|source| BindError::Io {
                    addr: *addr,
                    source,
                })?;
            listeners.push(l);
        }
        let bound = listeners
            .iter()
            .map(TcpListener::local_addr)
            .collect::<io::Result<Vec<_>>>()
            .map_err(|source| BindError::Io {
                addr: addrs
                    .first()
                    .copied()
                    .unwrap_or(SocketAddr::from(([127, 0, 0, 1], 0))),
                source,
            })?;
        let (board, tls) = build(&bound).map_err(BindError::Build)?;
        let app = router(board);
        let acceptor = TlsAcceptor::from(Arc::clone(&tls.config));
        let tasks = listeners
            .into_iter()
            .map(|listener| {
                tokio::spawn(serve_tls(
                    listener,
                    acceptor.clone(),
                    app.clone(),
                    shutdown.clone(),
                ))
            })
            .collect();
        Ok(Self {
            addrs: bound,
            tls,
            tasks,
        })
    }

    /// Waits for the listeners after the shutdown signal.
    pub async fn join(self) {
        for t in self.tasks {
            let _ = t.await;
        }
    }
}

/// Why [`BoardServer::bind`] failed.
#[derive(Debug, thiserror::Error)]
pub enum BindError<E> {
    #[error("board listener {addr}: {source}")]
    Io {
        addr: SocketAddr,
        #[source]
        source: io::Error,
    },
    #[error(transparent)]
    Build(E),
}

async fn stopped(rx: &mut watch::Receiver<bool>) {
    while !*rx.borrow() {
        if rx.changed().await.is_err() {
            return;
        }
    }
}

async fn serve_tls(
    listener: TcpListener,
    acceptor: TlsAcceptor,
    app: Router,
    mut shutdown: watch::Receiver<bool>,
) {
    let mut conns = JoinSet::new();
    loop {
        tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok((tcp, _)) => {
                    let acceptor = acceptor.clone();
                    let service = TowerToHyperService::new(app.clone());
                    conns.spawn(async move {
                        let Ok(Ok(tls)) = tokio::time::timeout(HANDSHAKE, acceptor.accept(tcp)).await
                        else {
                            return;
                        };
                        let conn = hyper::server::conn::http1::Builder::new()
                            .serve_connection(TokioIo::new(tls), service)
                            .with_upgrades();
                        if let Err(e) = conn.await {
                            tracing::debug!(error = %e, "board connection");
                        }
                    });
                }
                Err(e) => {
                    tracing::warn!(error = %e, "board accept");
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            },
            Some(_) = conns.join_next(), if !conns.is_empty() => {}
            () = stopped(&mut shutdown) => break,
        }
    }
    conns.abort_all();
}
