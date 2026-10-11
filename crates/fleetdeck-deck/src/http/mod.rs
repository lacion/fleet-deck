//! The board HTTP API (M2 P3-8; PLAN §9). Every route goes through the
//! [`guard`] with the scope of the [`methods::API`] table. The listener
//! accepts only the deck's board addresses (D13; checked in `DeckConfig`).
//! Plain HTTP is served; HTTPS for LAN and tailnet browsers comes from a
//! TLS proxy such as `tailscale serve` (DL-054).

pub mod auth_routes;
pub mod guard;
pub mod methods;
pub mod read_routes;

use std::io;
use std::net::SocketAddr;
use std::sync::Arc;

use axum::Router;
use axum::middleware;
use axum::routing::{get, post};
use tokio::net::TcpListener;
use tokio::sync::watch;
use tokio::task::JoinHandle;

pub use auth_routes::{LoginLimiter, SessionView};
pub use guard::{ApiError, Origins};

use crate::store::DeckStore;

/// The shared state of the board API.
#[derive(Debug)]
pub struct Board {
    pub store: DeckStore,
    pub origins: Origins,
    pub limiter: LoginLimiter,
}

/// The board API router.
pub fn router(board: Arc<Board>) -> Router {
    Router::new()
        .route("/api/auth/login", post(auth_routes::password))
        .route("/api/auth/link", post(auth_routes::link))
        .route("/api/auth/logout", post(auth_routes::logout))
        .route("/api/auth/session", get(auth_routes::session))
        .route("/api/stations", get(read_routes::stations))
        .route("/api/sessions", get(read_routes::sessions))
        .route("/api/commands", get(read_routes::commands))
        .route("/api/audit", get(read_routes::audit))
        .route_layer(middleware::from_fn_with_state(
            Arc::clone(&board),
            guard::guard,
        ))
        .with_state(board)
}

/// Bound board listeners.
#[derive(Debug)]
pub struct BoardServer {
    /// The bound addresses (a port 0 in the config gets a real port).
    pub addrs: Vec<SocketAddr>,
    tasks: Vec<JoinHandle<()>>,
}

impl BoardServer {
    /// Binds every address. The board's [`Origins`] come from the bound
    /// addresses, so `build` gets them.
    ///
    /// # Errors
    /// An address cannot be bound.
    pub async fn bind(
        addrs: &[SocketAddr],
        build: impl FnOnce(&[SocketAddr]) -> Arc<Board>,
        shutdown: watch::Receiver<bool>,
    ) -> io::Result<Self> {
        let mut listeners = Vec::with_capacity(addrs.len());
        for addr in addrs {
            listeners.push(TcpListener::bind(addr).await?);
        }
        let bound = listeners
            .iter()
            .map(TcpListener::local_addr)
            .collect::<io::Result<Vec<_>>>()?;
        let app = router(build(&bound));
        let tasks = listeners
            .into_iter()
            .map(|listener| {
                let app = app.clone();
                let mut rx = shutdown.clone();
                tokio::spawn(async move {
                    let stop = async move {
                        while !*rx.borrow() {
                            if rx.changed().await.is_err() {
                                return;
                            }
                        }
                    };
                    if let Err(e) = axum::serve(listener, app)
                        .with_graceful_shutdown(stop)
                        .await
                    {
                        tracing::error!(error = %e, "board listener");
                    }
                })
            })
            .collect();
        Ok(Self {
            addrs: bound,
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
