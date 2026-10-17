//! The board listeners: TLS, HTTP/1.1 with upgrades, admission and the
//! accept loop. Split from `mod.rs` for the module size rule (R13 P2b/P3b
//! step 1); `http` re-exports [`BoardServer`] and [`BindError`].

use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use hyper_util::rt::{TokioIo, TokioTimer};
use hyper_util::service::TowerToHyperService;
use tokio::net::TcpListener;
use tokio::sync::watch;
use tokio::task::{JoinHandle, JoinSet};
use tokio_rustls::TlsAcceptor;

use super::{Board, BoardTls, admission, router};

/// Bound board listeners (HTTPS, HTTP/1.1 with upgrades for the WS route).
#[derive(Debug)]
pub struct BoardServer {
    /// The bound addresses (a port 0 in the config gets a real port).
    pub addrs: Vec<SocketAddr>,
    /// The certificate the listeners present.
    pub tls: BoardTls,
    /// The optional addresses that were not used, with the reason.
    pub skipped: Vec<(SocketAddr, String)>,
    admission: Arc<admission::Admission>,
    tasks: Vec<JoinHandle<()>>,
}

/// How long a client has for the TLS handshake.
const HANDSHAKE: Duration = Duration::from_secs(10);
/// How long a client has to send the request headers, also between two
/// requests of a keep-alive connection (hyper starts the clock again when it
/// waits for the next request).
const HEADER_READ: Duration = Duration::from_secs(10);

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
        Self::bind_with(addrs, &[], build, shutdown).await
    }

    /// [`BoardServer::bind`] with `optional` addresses (design 14.4): each
    /// one is bound after `addrs`, when it is assigned to a local interface;
    /// one that is not, or that cannot be bound, is skipped with a warning
    /// and listed in [`BoardServer::skipped`]. It never fails the bind.
    ///
    /// # Errors
    /// As [`BoardServer::bind`], for `addrs` only.
    pub async fn bind_with<E>(
        addrs: &[SocketAddr],
        optional: &[SocketAddr],
        build: impl FnOnce(&[SocketAddr]) -> Result<(Arc<Board>, BoardTls), E>,
        shutdown: watch::Receiver<bool>,
    ) -> Result<Self, BindError<E>> {
        let mut listeners = Vec::with_capacity(addrs.len() + optional.len());
        for addr in addrs {
            let l = TcpListener::bind(addr)
                .await
                .map_err(|source| BindError::Io {
                    addr: *addr,
                    source,
                })?;
            listeners.push(l);
        }
        let mut skipped = Vec::new();
        for addr in optional {
            let outcome = match crate::tailnet_auto::unusable(*addr) {
                Some(reason) => Err(reason),
                None => TcpListener::bind(addr)
                    .await
                    .map_err(|e| crate::tailnet_auto::bind_failed(e.kind())),
            };
            match outcome {
                Ok(l) => listeners.push(l),
                Err(reason) => {
                    tracing::warn!(%addr, %reason, "an automatic tailnet board address is not used");
                    skipped.push((*addr, reason));
                }
            }
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
        let admission = Arc::new(admission::Admission::default());
        let tasks = listeners
            .into_iter()
            .map(|listener| {
                tokio::spawn(serve_tls(
                    listener,
                    acceptor.clone(),
                    app.clone(),
                    Arc::clone(&admission),
                    shutdown.clone(),
                ))
            })
            .collect();
        Ok(Self {
            addrs: bound,
            tls,
            skipped,
            admission,
            tasks,
        })
    }

    /// The open board connections (live admission guards).
    #[doc(hidden)]
    #[must_use]
    pub fn admitted(&self) -> usize {
        self.admission.admitted()
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
    admission: Arc<admission::Admission>,
    mut shutdown: watch::Receiver<bool>,
) {
    let mut conns = JoinSet::new();
    loop {
        tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok((tcp, peer)) => {
                    // Admission comes first: a refused connection gets no
                    // task and no TLS work.
                    let Some(guard) = admission.try_admit(peer.ip()) else {
                        if admission.should_log_refusal() {
                            tracing::debug!(peer = %peer.ip(), "board connection refused: too many open connections");
                        }
                        drop(tcp);
                        continue;
                    };
                    let acceptor = acceptor.clone();
                    conns.spawn(serve_connection(tcp, peer, acceptor, app.clone(), guard));
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

/// One board connection: TLS, then HTTP/1.1 with upgrades. The guard lives
/// in the stream, so it lives as long as an upgraded WebSocket.
async fn serve_connection(
    tcp: tokio::net::TcpStream,
    peer: SocketAddr,
    acceptor: TlsAcceptor,
    app: Router,
    guard: admission::AdmitGuard,
) {
    let Ok(Ok(tls)) = tokio::time::timeout(HANDSHAKE, acceptor.accept(tcp)).await else {
        return;
    };
    let inner = TowerToHyperService::new(app);
    let service =
        hyper::service::service_fn(move |mut req: axum::http::Request<hyper::body::Incoming>| {
            req.extensions_mut().insert(admission::PeerAddr(peer));
            hyper::service::Service::call(&inner, req)
        });
    let conn = hyper::server::conn::http1::Builder::new()
        .timer(TokioTimer::new())
        .header_read_timeout(HEADER_READ)
        .keep_alive(true)
        .serve_connection(TokioIo::new(admission::Guarded::new(tls, guard)), service)
        .with_upgrades();
    if let Err(e) = conn.await {
        tracing::debug!(error = %e, "board connection");
    }
}
