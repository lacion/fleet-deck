//! The board HTTP API (M2 P3-8; PLAN §9). Every route goes through the
//! [`guard`] with the scope of the [`methods::api`] table. The listener
//! accepts only the deck's board addresses (D13; checked in `DeckConfig`).
//! The deck serves HTTPS itself ([`tls`]; D13: no reverse proxy, DL-054).

pub mod access;
pub mod account_routes;
pub mod account_secrets;
pub mod activity_routes;
pub mod admission;
pub mod archive_routes;
pub mod artifact_routes;
pub mod assets;
pub mod assign_routes;
pub mod assist_routes;
pub mod auth_routes;
pub mod command_routes;
pub mod file_routes;
pub mod fleet_routes;
pub mod grant_routes;
pub mod guard;
pub mod methods;
pub mod paste_routes;
pub mod people_routes;
pub mod plan_routes;
pub mod project_session_routes;
pub mod project_writes;
pub mod projects_routes;
pub mod read_routes;
pub mod schedule_routes;
pub mod secrets_routes;
pub mod settings_routes;
pub mod share_routes;
pub mod shell_routes;
pub mod station_routes;
pub mod tls;
pub mod workspace_routes;
pub mod worktree_routes;
pub mod ws;

use std::io;
use std::net::SocketAddr;
use std::sync::Arc;

use axum::Router;
use axum::extract::Extension;
use axum::middleware;
use axum::routing::{delete, get, post};
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::watch;

use hyper_util::rt::{TokioIo, TokioTimer};
use hyper_util::service::TowerToHyperService;
use tokio::task::{JoinHandle, JoinSet};
use tokio_rustls::TlsAcceptor;

pub use admission::LoginLimits;
pub use auth_routes::{DeviceLinkLimiter, SessionView};
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
    pub limiter: LoginLimits,
    /// The rate limit of `POST /api/auth/device-link` (DL-146).
    pub device_links: DeviceLinkLimiter,
    /// The SHA-256 of the board certificate, for a device link.
    pub certificate_sha256: String,
    /// The board certificate is the deck's self-signed one.
    pub self_signed: bool,
    /// Single-use WebSocket tickets of bearer clients.
    pub tickets: ws::Tickets,
    /// How often an open WebSocket checks its login ([`ws::RECHECK`]).
    pub ws_recheck: std::time::Duration,
    /// The link node for the pairing methods (`None`: they answer 503).
    pub pairing: Option<Arc<crate::link::DeckNode>>,
    /// The board web client (P4-4).
    pub assets: assets::BoardAssets,
    /// The live settings of the deck, read per use (DL-172, design 1.4).
    pub live: crate::live::LiveCell,
    /// The elevation codes and the elevations of the owner (DL-172).
    pub elevations: Arc<crate::elevation::Elevations>,
    /// The settings file of the deck (`None`: the settings routes answer 503).
    pub settings: Option<settings_routes::DeckSettings>,
    /// Restarts the deck process (`None`: the restart route answers 501).
    pub restart: Option<Arc<dyn fleetdeck_core::restart::Restart>>,
    /// The local station of `fleetdeck up`; set after the pairing.
    pub local_station: Arc<std::sync::OnceLock<fleetdeck_proto::StationId>>,
}

/// The board API router.
pub fn router(board: Arc<Board>) -> Router {
    // The open OAuth transactions (DL-173): in memory, one set for this
    // router and its clones.
    let oauth = Arc::new(crate::oauth::Transactions::default());
    let mut r = Router::new();
    for api in methods::command_methods() {
        r = r.route(&methods::route_of(api), post(command_routes::command));
    }
    r.route("/api/auth/login", post(auth_routes::password))
        .route("/api/auth/elevate", post(auth_routes::elevate))
        .route(
            "/api/settings/deck",
            get(settings_routes::deck_get).post(settings_routes::deck_set),
        )
        .route("/api/settings/restart", post(settings_routes::restart))
        .route(
            "/api/stations/{station}/settings",
            get(settings_routes::station_get).post(settings_routes::station_set),
        )
        .route(
            "/api/secrets/stations/{station}/settings",
            post(secrets_routes::station_settings).layer(axum::extract::DefaultBodyLimit::max(
                secrets_routes::MAX_BODY,
            )),
        )
        .route("/api/auth/link", post(auth_routes::link))
        .route("/api/auth/join", post(auth_routes::join))
        .route("/api/auth/device-link", post(auth_routes::device_link))
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
        .route(
            "/api/stations/{station}/workspace/preflight",
            post(workspace_routes::preflight),
        )
        .route(
            "/api/stations/{station}/worktrees",
            get(worktree_routes::list),
        )
        .route(
            "/api/stations/{station}/worktrees/trash",
            post(worktree_routes::trash),
        )
        .route(
            "/api/stations/{station}/worktrees/restore",
            post(worktree_routes::restore),
        )
        .route(
            "/api/stations/{station}/worktrees/purge",
            post(worktree_routes::purge),
        )
        .route(
            "/api/stations/{station}/worktrees/release",
            post(worktree_routes::release),
        )
        .route(
            "/api/stations/{station}/shells",
            get(shell_routes::list).post(shell_routes::command),
        )
        .route(
            "/api/stations/{station}/shells/{shell}/attach",
            get(shell_routes::attach),
        )
        .route(
            "/api/stations/{station}/paste",
            post(paste_routes::paste)
                .layer(axum::extract::DefaultBodyLimit::max(paste_routes::MAX_BODY)),
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
        .route(
            "/api/stations/{station}/sessions/{session}/diff",
            get(file_routes::session_diff),
        )
        .route(
            "/api/archive",
            get(archive_routes::list).post(archive_routes::change),
        )
        .route("/api/assist/{kind}", post(assist_routes::generate))
        .route(
            "/api/stations/{station}/sessions/{session}/digest",
            post(assist_routes::digest),
        )
        .route("/api/conflicts", get(plan_routes::conflicts))
        .route(
            "/api/schedules",
            get(schedule_routes::list).post(schedule_routes::create),
        )
        .route(
            "/api/schedules/{schedule_id}",
            axum::routing::put(schedule_routes::update).delete(schedule_routes::delete),
        )
        .route(
            "/api/schedules/{schedule_id}/update",
            post(schedule_routes::update),
        )
        .route(
            "/api/schedules/{schedule_id}/delete",
            post(schedule_routes::delete),
        )
        .route(
            "/api/schedules/{schedule_id}/run",
            post(schedule_routes::run),
        )
        .route(
            "/api/schedules/{schedule_id}/runs",
            get(schedule_routes::runs),
        )
        .route("/api/plans", get(plan_routes::list))
        .route("/api/plans/mark", post(plan_routes::mark))
        .route("/api/sessions", get(read_routes::sessions))
        .route(
            "/api/sessions/{station}/{session}/read",
            post(activity_routes::mark_read),
        )
        .route("/api/activity", get(activity_routes::activity))
        .route("/api/fleet/extras", get(read_routes::fleet_extras))
        .route("/api/assign", post(assign_routes::assign))
        .route(
            "/api/mail",
            get(fleet_routes::mails).post(fleet_routes::send_mail),
        )
        .route("/api/mail/group", post(fleet_routes::send_group_mail))
        .route(
            "/api/tasks",
            get(fleet_routes::tasks).post(fleet_routes::create_task),
        )
        .route(
            "/api/tasks/{task_id}/complete",
            post(fleet_routes::complete_task),
        )
        .route("/api/tasks/{task_id}/wait", get(fleet_routes::wait_task))
        .route("/api/tasks/wait", get(fleet_routes::wait_any_task))
        .route("/api/coordinator/grants", get(grant_routes::list))
        .route(
            "/api/coordinator/grants/{grant_id}/revoke",
            post(grant_routes::revoke),
        )
        .route(
            "/api/coordinator/grants/{grant_id}/renew",
            post(grant_routes::renew),
        )
        .route("/api/artifacts", get(artifact_routes::list))
        .route("/api/artifacts/publish", post(artifact_routes::publish))
        .route("/api/artifacts/{station}/{id}", get(artifact_routes::get))
        .route(
            "/api/artifacts/{station}/{id}/shares",
            post(artifact_routes::share),
        )
        .route(
            "/api/artifacts/{station}/{id}/shares/{token}",
            delete(artifact_routes::revoke),
        )
        .route(
            "/api/artifacts/{station}/{id}/view",
            post(artifact_routes::view),
        )
        .route(
            "/api/artifacts/{station}/{id}/{version}/raw/{*path}",
            get(artifact_routes::raw),
        )
        .route(
            "/api/projects",
            get(projects_routes::list).post(projects_routes::create),
        )
        .route(
            "/api/projects/{project}",
            axum::routing::put(projects_routes::update).delete(projects_routes::delete),
        )
        .route(
            "/api/projects/{project}/preflight",
            post(projects_routes::preflight),
        )
        .route(
            "/api/projects/{project}/issues",
            get(projects_routes::issues),
        )
        .route(
            "/api/projects/{project}/issues/{key}",
            get(projects_routes::issue),
        )
        .route(
            "/api/projects/{project}/issues/{key}/comments",
            post(project_writes::issue_comment),
        )
        .route(
            "/api/projects/{project}/issues/{key}/transition",
            post(project_writes::issue_transition),
        )
        .route(
            "/api/projects/{project}/assets",
            get(projects_routes::asset),
        )
        .route(
            "/api/projects/{project}/prs",
            get(projects_routes::prs).post(project_writes::pr_create),
        )
        .route(
            "/api/projects/{project}/prs/{number}",
            get(projects_routes::pr),
        )
        .route(
            "/api/projects/{project}/prs/{number}/checks",
            get(projects_routes::pr_checks),
        )
        .route(
            "/api/projects/{project}/prs/{number}/comments",
            post(project_writes::pr_comment),
        )
        .route(
            "/api/projects/{project}/prs/{number}/reviews",
            post(project_writes::pr_review),
        )
        .route(
            "/api/projects/{project}/sessions/assign",
            post(project_session_routes::assign),
        )
        .route(
            "/api/sessions/{station}/{session}/project/clear",
            post(project_session_routes::clear),
        )
        .route(
            "/api/sessions/{station}/{session}/project/auto",
            post(project_session_routes::auto),
        )
        .route(
            "/api/projects/{project}/sessions",
            post(project_writes::sessions),
        )
        // The accounts of a station (DL-173). The static `forge` segment wins
        // over `{account_id}`.
        .route(
            "/api/stations/{station}/accounts",
            get(account_routes::list),
        )
        .route(
            "/api/stations/{station}/accounts/forge/login",
            post(account_routes::forge_login),
        )
        .route(
            "/api/stations/{station}/accounts/forge/login/{login_id}",
            get(account_routes::forge_login_status),
        )
        .route(
            "/api/stations/{station}/accounts/{account_id}/logout",
            post(account_routes::logout),
        )
        .route(
            "/api/stations/{station}/accounts/{account_id}/containers",
            get(account_routes::containers),
        )
        // The credential relay (DL-173, design section 0): 16 KiB bodies.
        .route(
            "/api/secrets/stations/{station}/accounts/apps",
            post(account_secrets::app_set).layer(axum::extract::DefaultBodyLimit::max(
                account_secrets::MAX_BODY,
            )),
        )
        .route(
            "/api/secrets/stations/{station}/accounts/token",
            post(account_secrets::token_set).layer(axum::extract::DefaultBodyLimit::max(
                account_secrets::MAX_BODY,
            )),
        )
        .route(
            "/api/accounts/oauth/start",
            post(account_routes::oauth_start).layer(Extension(Arc::clone(&oauth))),
        )
        .route(
            "/api/accounts/oauth/finish",
            post(account_routes::oauth_finish).layer(Extension(Arc::clone(&oauth))),
        )
        .route("/api/commands", get(read_routes::commands))
        .route(
            "/api/commands/{command_id}",
            get(command_routes::get_command),
        )
        .route("/api/audit", get(read_routes::audit))
        .route("/api/ws", get(ws::upgrade))
        .route("/api/ws/ticket", post(ws::ticket))
        .route(
            "/api/shares",
            get(share_routes::list_shares).post(share_routes::create_share),
        )
        .route(
            "/api/shares/{share_id}/revoke",
            post(share_routes::revoke_share),
        )
        .route(
            "/api/spaces",
            get(share_routes::list_spaces).post(share_routes::create_space),
        )
        .route(
            "/api/spaces/{space_id}/sessions",
            post(share_routes::add_space_session),
        )
        .route(
            "/api/sessions/{station}/{session}/chat",
            get(people_routes::chat).post(people_routes::post_chat),
        )
        .route(
            "/api/sessions/{station}/{session}/drive/request",
            post(people_routes::drive_request),
        )
        .route(
            "/api/sessions/{station}/{session}/drive/grant",
            post(people_routes::drive_grant),
        )
        .route(
            "/api/sessions/{station}/{session}/drive/release",
            post(people_routes::drive_release),
        )
        .route_layer(middleware::from_fn_with_state(
            Arc::clone(&board),
            guard::guard,
        ))
        // The public share route is outside the guard: the token is its
        // credential (DL-165).
        .route("/a/{token}", get(artifact_routes::public_slash))
        .route("/a/{token}/", get(artifact_routes::public_main))
        .route("/a/{token}/{*path}", get(artifact_routes::public))
        // The OAuth return of Jira and Linear (DL-173, design section 2.3).
        // It is outside the guard on purpose: the provider redirect is a
        // cross-site navigation, so the SameSite=Strict login cookie is not
        // sent and no CSRF header can be added. The one-use `state` of an
        // open transaction is its only credential, and the answer is a 303
        // to a page that checks the cookie (`oauth_finish`).
        .route(
            "/oauth/callback",
            get(crate::oauth::callback).layer(Extension(oauth)),
        )
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
