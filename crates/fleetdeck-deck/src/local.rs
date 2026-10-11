//! The deck's owner-only local socket `deck.sock` (M2 P3-6b; PLAN §6.5):
//! the pairing commands and the registry and read model views of the local
//! CLI, `deck command` through the command router, and the owner-auth
//! requests of [`crate::local_auth`] (P3-8). The wire is that of
//! [`fleetdeck_core::localsock`]. Every mutation is audited with the actor
//! `local`.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use fleetdeck_core::Secret;
use fleetdeck_core::localsock::{self, BindError, LineClient, read_line, send};
use fleetdeck_proto::StationId;
use fleetdeck_proto::command::CommandOrigin;
use serde::{Deserialize, Serialize};
use tokio::io::BufReader;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::watch;

pub use fleetdeck_core::localsock::InvitationText;

use crate::link::{CommandKind, DeckNode};
use crate::local_auth::{self, AuthRequest, AuthResponse, LOCAL_ACTOR};
use crate::store::{
    AuditResult, DeckCommand, DeckError, DeckSession, Direction, NewAudit, StationRecord,
};

/// What the socket serves.
#[derive(Debug)]
pub struct LocalDeck {
    pub node: Arc<DeckNode>,
    /// The first board listener (login links), if any.
    pub board: Option<crate::local_auth::BoardEndpoint>,
}

/// The socket's file name in the deck directory.
pub const SOCKET_NAME: &str = "deck.sock";

/// The socket path of a deck directory.
#[must_use]
pub fn socket_path(deck_dir: &Path) -> PathBuf {
    deck_dir.join(SOCKET_NAME)
}

/// One request of the local CLI.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum DeckRequest {
    /// `deck invite`: an invitation for a station that dials the deck.
    Invite,
    /// `deck add-station`: pair with the listening station of the
    /// invitation and dial it.
    AddStation {
        invitation: InvitationText,
        name: Option<String>,
    },
    /// `deck stations`.
    Stations,
    /// `deck remove-station`.
    RemoveStation { station: String },
    /// `deck sessions`: every card of the read model.
    Sessions,
    /// `deck command`: route one command to a station and wait up to
    /// [`COMMAND_WAIT`] for its outcome (P3-7a).
    Command {
        station: String,
        command: CommandKind,
    },
    /// `deck commands`: the router's commands, oldest first.
    Commands,
    /// `fleetdeck open`, `deck password`, `deck token`, `deck audit`.
    Auth { auth: AuthRequest },
}

/// How long `deck command` waits for a receipt before it answers with the
/// `pending` command; the router keeps resending until `retry_until`.
pub const COMMAND_WAIT: Duration = Duration::from_secs(15);

/// A paired station as the CLI shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StationView {
    pub station_id: StationId,
    pub name: String,
    /// The short fingerprint of the station key.
    pub key: String,
    /// `deck_dials` or `station_dials`.
    pub direction: String,
    pub dial_url: Option<String>,
    pub reachable: bool,
    pub last_seen: Option<i64>,
    pub cursor: u64,
}

impl StationView {
    #[must_use]
    pub fn new(st: &StationRecord, reachable: bool) -> Self {
        Self {
            station_id: st.station_id.clone(),
            name: st.name.clone(),
            key: st.key.fingerprint().short(),
            direction: match st.direction {
                Direction::DeckDials => "deck_dials",
                Direction::StationDials => "station_dials",
            }
            .to_owned(),
            dial_url: st.dial_url.clone(),
            reachable,
            last_seen: st.last_seen,
            cursor: st.cursor,
        }
    }
}

/// The answer to one [`DeckRequest`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum DeckResponse {
    Invitation {
        invitation: InvitationText,
    },
    /// The station that [`DeckRequest::AddStation`] paired.
    Paired {
        station: StationView,
    },
    Stations {
        stations: Vec<StationView>,
    },
    Removed {
        station: StationView,
    },
    Sessions {
        sessions: Vec<DeckSession>,
    },
    Command {
        command: DeckCommand,
    },
    Commands {
        commands: Vec<DeckCommand>,
    },
    Auth {
        auth: AuthResponse,
    },
    Error {
        message: String,
    },
}

/// Binds `deck.sock` (0600).
///
/// # Errors
/// [`BindError`].
pub async fn bind(path: &Path) -> Result<UnixListener, BindError> {
    localsock::bind(path).await
}

/// Serves the socket until `shutdown` turns true.
pub async fn serve(
    listener: UnixListener,
    deck: Arc<LocalDeck>,
    mut shutdown: watch::Receiver<bool>,
) {
    loop {
        tokio::select! {
            accepted = localsock::accept_own(&listener) => match accepted {
                Ok(Some(stream)) => {
                    tokio::spawn(connection(stream, Arc::clone(&deck)));
                }
                Ok(None) => {}
                Err(e) => tracing::warn!(error = %e, "local socket accept"),
            },
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    return;
                }
            }
        }
    }
}

async fn connection(stream: UnixStream, deck: Arc<LocalDeck>) {
    let (read, mut write) = stream.into_split();
    let mut reader = BufReader::new(read);
    loop {
        let line = match read_line(&mut reader).await {
            Ok(Some(line)) => line,
            Ok(None) => return,
            Err(e) => {
                let _ = send(&mut write, &error(e.to_string())).await;
                return;
            }
        };
        let response = match serde_json::from_slice::<DeckRequest>(&line) {
            Ok(request) => handle(&deck, request).await,
            Err(e) => error(localsock::bad_request(&e)),
        };
        if send(&mut write, &response).await.is_err() {
            return;
        }
    }
}

fn error(message: String) -> DeckResponse {
    DeckResponse::Error { message }
}

/// Audits a `deck.sock` mutation.
async fn audit<T, E: std::fmt::Display>(
    node: &DeckNode,
    action: &str,
    target: &str,
    r: &Result<T, E>,
) {
    let row = match r {
        Ok(_) => NewAudit::new(LOCAL_ACTOR, action, AuditResult::Ok),
        Err(e) => NewAudit::new(LOCAL_ACTOR, action, AuditResult::Failed).detail(e.to_string()),
    };
    node.link().store.audit_or_log(row.target(target)).await;
}

async fn handle(deck: &LocalDeck, request: DeckRequest) -> DeckResponse {
    let node = &*deck.node;
    let store = &node.link().store;
    match request {
        DeckRequest::Auth { auth } => {
            local_auth::respond(local_auth::handle(store, deck.board.as_ref(), auth).await)
        }
        DeckRequest::Invite => match audited(node, "station.invite", "", node.invite()).await {
            Ok(text) => DeckResponse::Invitation {
                invitation: InvitationText(text.expose().clone()),
            },
            Err(e) => error(e.to_string()),
        },
        DeckRequest::AddStation { invitation, name } => {
            let r = node.add_station(&Secret::new(invitation.0), name).await;
            audit(node, "station.add", "", &r).await;
            match r {
                Ok(st) => DeckResponse::Paired {
                    station: StationView::new(&st, store.is_reachable(&st.station_id)),
                },
                Err(e) => error(e.to_string()),
            }
        }
        DeckRequest::Stations => match store.stations().await {
            Ok(list) => DeckResponse::Stations {
                stations: list
                    .iter()
                    .map(|st| StationView::new(st, store.is_reachable(&st.station_id)))
                    .collect(),
            },
            Err(e) => error(e.to_string()),
        },
        DeckRequest::RemoveStation { station } => match audited(
            node,
            "station.remove",
            &station.clone(),
            node.remove_station(&station).await,
        )
        .await
        {
            Ok(st) => DeckResponse::Removed {
                station: StationView::new(&st, false),
            },
            Err(e) => error(e.to_string()),
        },
        DeckRequest::Sessions => match store.sessions().await {
            Ok(sessions) => DeckResponse::Sessions { sessions },
            Err(e) => error(e.to_string()),
        },
        DeckRequest::Command { station, command } => match route(node, station, command).await {
            Ok(command) => DeckResponse::Command { command },
            Err(e) => error(e.to_string()),
        },
        DeckRequest::Commands => match store.commands().await {
            Ok(commands) => DeckResponse::Commands { commands },
            Err(e) => error(e.to_string()),
        },
    }
}

async fn audited<T, E: std::fmt::Display>(
    node: &DeckNode,
    action: &str,
    target: &str,
    r: Result<T, E>,
) -> Result<T, E> {
    audit(node, action, target, &r).await;
    r
}

async fn route(
    node: &DeckNode,
    station: String,
    kind: CommandKind,
) -> Result<DeckCommand, DeckError> {
    let link = node.link();
    let st = link
        .store
        .find_station(station.clone())
        .await?
        .ok_or(DeckError::UnknownStation(station))?;
    let method = kind.method().to_owned();
    let submitted = link
        .submit(st.station_id.clone(), CommandOrigin::Board, kind)
        .await;
    let mut row = match &submitted {
        Ok(cmd) => {
            NewAudit::new(LOCAL_ACTOR, method, AuditResult::Ok).command(cmd.command_id.clone())
        }
        Err(e) => NewAudit::new(LOCAL_ACTOR, method, AuditResult::Failed).detail(e.to_string()),
    };
    row = row.target(st.station_id.0.clone());
    link.store.audit_or_log(row).await;
    link.wait_command(submitted?.command_id, COMMAND_WAIT).await
}

/// The client side, for the CLI.
#[derive(Debug)]
pub struct DeckClient(LineClient);

impl DeckClient {
    /// Connects to the deck's socket.
    ///
    /// # Errors
    /// No deck serves `path`.
    pub async fn connect(path: &Path) -> io::Result<Self> {
        LineClient::connect(path, "deck").await.map(Self)
    }

    /// Sends one request and reads its response.
    ///
    /// # Errors
    /// The connection failed or the response is not valid.
    pub async fn call(&mut self, request: &DeckRequest) -> io::Result<DeckResponse> {
        self.0.call(request).await
    }
}
