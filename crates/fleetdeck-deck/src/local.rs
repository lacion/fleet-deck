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
use fleetdeck_proto::command::CommandOrigin;
use fleetdeck_proto::settings::{SettingsChange, SettingsSaved, SettingsView};
use fleetdeck_proto::version::feature;
use serde::{Deserialize, Serialize};
use tokio::io::BufReader;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::watch;

pub use fleetdeck_core::localsock::InvitationText;

use crate::link::{CommandKind, DeckNode};
use crate::local_auth::{self, AuthRequest, AuthResponse, LOCAL_ACTOR};
use crate::store::{
    AuditResult, DeckCommand, DeckError, DeckSession, DeckStore, Direction, LinkFeature, NewAudit,
    StationRecord,
};

/// What the socket serves.
#[derive(Debug)]
pub struct LocalDeck {
    pub node: Arc<DeckNode>,
    /// The first board listener (login links), if any.
    pub board: Option<crate::local_auth::BoardEndpoint>,
    /// The live endpoints of the board: the trusted address comes first in
    /// a login link (DL-190).
    pub endpoints: Arc<crate::http::endpoints::Endpoints>,
    /// The elevation codes and elevations that the board checks (DL-172).
    pub elevations: Arc<crate::elevation::Elevations>,
    /// The settings file of the deck (`None`: the settings requests answer
    /// `settings_unavailable`).
    pub settings: Option<crate::http::settings_routes::DeckSettings>,
    /// The live settings that a save publishes (shared with the board).
    pub live: crate::live::LiveCell,
}

/// The socket's file name in the deck directory.
pub const SOCKET_NAME: &str = "deck.sock";

/// The socket path of a deck directory.
#[must_use]
pub fn socket_path(deck_dir: &Path) -> PathBuf {
    deck_dir.join(SOCKET_NAME)
}

/// One request of the local CLI.
// One request per CLI call: the size of `Command` costs nothing.
#[allow(clippy::large_enum_variant)]
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
    /// `fleetdeck open --elevate`: a one-time elevation code for the board
    /// (DL-172, design 1.5).
    ElevateCode,
    /// `fleetdeck settings get`: the settings of the deck (DL-172).
    SettingsGet,
    /// `fleetdeck settings set|secret`: a change of the deck settings. The
    /// local socket is the owner, so it counts as elevated; the
    /// `elevation` field of the change is not used.
    SettingsSet { change: SettingsChange },
    /// `fleetdeck deck provision-file`: the deck identity for a
    /// provisioning file, if the deck listens on `host:port` (P8 2.2). A
    /// read: no audit row.
    #[serde(rename = "deck.provision_info")]
    ProvisionInfo { host: String, port: u16 },
}

/// How long `deck command` waits for a receipt before it answers with the
/// `pending` command; the router keeps resending until `retry_until`.
pub const COMMAND_WAIT: Duration = Duration::from_secs(15);

pub use fleetdeck_proto::board::StationView;

/// The view of a station record with its reachability and last
/// `station.info` from `store`.
#[must_use]
pub fn station_view(st: &StationRecord, store: &DeckStore) -> StationView {
    let info = store.station_info(&st.station_id);
    StationView {
        station_id: st.station_id.clone(),
        name: st.name.clone(),
        key: st.key.fingerprint().short(),
        direction: match st.direction {
            Direction::DeckDials => "deck_dials",
            Direction::StationDials => "station_dials",
        }
        .to_owned(),
        dial_url: st.dial_url.clone(),
        reachable: store.is_reachable(&st.station_id),
        last_seen: st.last_seen,
        cursor: st.cursor,
        info_at: info.as_ref().map(|(_, at)| *at),
        info: info.map(|(i, _)| i),
        display_resources: None,
        coordinator_live: store.link_feature(&st.station_id, feature::COORDINATOR_LIVE)
            == LinkFeature::Negotiated,
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
    /// The code of [`DeckRequest::ElevateCode`], shown once.
    ElevateCode {
        code: InvitationText,
        expires_at: i64,
    },
    /// The answer to [`DeckRequest::SettingsGet`].
    Settings {
        view: SettingsView,
    },
    /// The answer to [`DeckRequest::SettingsSet`].
    SettingsSaved {
        saved: SettingsSaved,
    },
    /// A settings request that failed, with its fixed error code.
    SettingsError {
        code: String,
        message: String,
    },
    /// The answer to [`DeckRequest::ProvisionInfo`] (P8 2.2).
    ProvisionInfo {
        features: Vec<String>,
        deck: String,
        deck_fingerprint: String,
    },
    /// A [`DeckRequest::ProvisionInfo`] that failed, with its fixed code:
    /// `invalid_request` or `deck_not_listening`.
    ProvisionError {
        code: String,
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
        DeckRequest::Auth { auth } => local_auth::respond(
            local_auth::handle(store, deck.board.as_ref(), &deck.endpoints, auth).await,
        ),
        DeckRequest::SettingsGet => settings_get(deck).await,
        DeckRequest::SettingsSet { change } => settings_set(deck, change).await,
        DeckRequest::ProvisionInfo { host, port } => {
            crate::local_provision::provision_info(deck, host, port).await
        }
        DeckRequest::ElevateCode => {
            let made = deck.elevations.new_code(store.now_ms());
            let row = NewAudit::new(
                LOCAL_ACTOR,
                "auth.elevate_code",
                if made.is_ok() {
                    AuditResult::Ok
                } else {
                    AuditResult::Failed
                },
            );
            store.audit_or_log(row).await;
            match made {
                Ok((code, expires_at)) => DeckResponse::ElevateCode {
                    code: InvitationText(code.expose().clone()),
                    expires_at,
                },
                Err(e) => error(e.to_string()),
            }
        }
        DeckRequest::Invite => match audited(
            node,
            "station.invite",
            "",
            node.invite(store.knobs().invite),
        )
        .await
        {
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
                    station: station_view(&st, store),
                },
                Err(e) => error(e.to_string()),
            }
        }
        DeckRequest::Stations => match store.stations().await {
            Ok(list) => DeckResponse::Stations {
                stations: list.iter().map(|st| station_view(st, store)).collect(),
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
                station: station_view(&st, store),
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

fn settings_error(code: &str, message: impl Into<String>) -> DeckResponse {
    DeckResponse::SettingsError {
        code: code.to_owned(),
        message: message.into(),
    }
}

fn settings_unavailable() -> DeckResponse {
    settings_error(
        "settings_unavailable",
        "this deck has no settings file set up",
    )
}

async fn settings_get(deck: &LocalDeck) -> DeckResponse {
    let Some(settings) = deck.settings.as_ref() else {
        return settings_unavailable();
    };
    match crate::http::settings_routes::deck_view(settings).await {
        Ok(view) => DeckResponse::Settings { view },
        Err(e) => {
            tracing::warn!(error = %e, "deck settings");
            settings_error(
                "settings_unreadable",
                "the deck config file cannot be read or does not parse; see the deck log",
            )
        }
    }
}

/// Saves a change as the local owner and audits the keys, never a value.
async fn settings_set(deck: &LocalDeck, change: SettingsChange) -> DeckResponse {
    const ACTION: &str = "settings.deck_set";
    let Some(settings) = deck.settings.as_ref() else {
        return settings_unavailable();
    };
    let store = &deck.node.link().store;
    let mut keys: Vec<String> = change.changes.keys().cloned().collect();
    keys.sort();
    let keys = keys.join(",");
    let saved =
        crate::http::settings_routes::save_and_publish(settings, store, &deck.live, change, true)
            .await;
    let row = match &saved {
        Ok(_) => NewAudit::new(LOCAL_ACTOR, ACTION, AuditResult::Ok).detail(keys),
        Err(e) => NewAudit::new(LOCAL_ACTOR, ACTION, AuditResult::Failed)
            .detail(format!("{}: {keys}", e.code())),
    };
    store.audit_or_log(row.target("deck")).await;
    match saved {
        Ok(saved) => DeckResponse::SettingsSaved { saved },
        Err(e) => settings_error(e.code(), e.to_string()),
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
