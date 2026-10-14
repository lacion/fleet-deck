//! The station-local control socket (PLAN §6.5, §13 Phase 2; M2 P2-7).
//!
//! `fleetdeck station sessions|run|approve|interrupt` talk to the running
//! station over a Unix socket in the station's private data directory
//! (0700). The socket file is 0600, and the server accepts only peers with
//! the station's own user id. This is the emergency path that works
//! without the deck (PLAN §6.5): its commands are `CommandOrigin::Local`
//! with the caller `local`, and they go through the same decision and
//! receipt path as deck commands.
//!
//! Wire: one JSON object per line in each direction; one response per
//! request; a line is at most [`MAX_LINE_BYTES`].

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use fleetdeck_core::localsock::{self, BindError, LineClient, read_line, send};
use fleetdeck_proto::command::{
    ApprovalDecision, CommandEnvelope, CommandOrigin, RETRY_WINDOW_MS, Receipt, SessionCommand,
};
use fleetdeck_proto::link::Outcome;
use fleetdeck_proto::settings::{
    SecretSet, SecretSetParams, SettingsChange, SettingsSaved, SettingsSetParams, SettingsView,
};
use fleetdeck_proto::{CommandId, RequestId, SessionSummary};
use serde::{Deserialize, Serialize};
use tokio::io::BufReader;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::watch;

use crate::link::StationNode;
use crate::manager::SessionManager;
use crate::store::Caller;

/// The longest request or response line.
pub const MAX_LINE_BYTES: usize = localsock::MAX_LINE_BYTES;
/// The socket's file name in the station directory.
pub const SOCKET_NAME: &str = "station.sock";

/// The socket path of a station directory.
#[must_use]
pub fn socket_path(station_dir: &Path) -> PathBuf {
    station_dir.join(SOCKET_NAME)
}

/// One request of the local CLI.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum LocalRequest {
    /// The projection of every session.
    Sessions,
    /// One session command. Without `command_id` the station mints one.
    Command {
        command_id: Option<CommandId>,
        command: SessionCommand,
    },
    /// Answers an open request; the station finds its session.
    Approve {
        command_id: Option<CommandId>,
        request_id: RequestId,
        decision: ApprovalDecision,
    },
    /// `station invite --listen`: a single-use invitation for a deck that
    /// dials this station.
    Invite { listen: std::net::SocketAddr },
    /// `station join`: pair with the deck of the invitation and dial it.
    Join { invitation: InvitationText },
    /// `station join --tailnet` (DL-178): asks the deck at `host:port`, a
    /// device of the tailnet, to pair. The station checks the host against
    /// its own `tailscale status`, writes the retry file, sends the request,
    /// waits for the owner (at most 11 minutes) and stores the deck pin, the
    /// grants (`scopes`: what this station gives the deck) and the dial
    /// target in one transaction. Without `deck_fingerprint` it only
    /// observes the key of the deck and answers
    /// [`LocalResponse::TailnetObserved`].
    JoinTailnet {
        host: String,
        port: u16,
        deck_fingerprint: Option<[u8; 32]>,
        scopes: Vec<fleetdeck_proto::link::Scope>,
        name: String,
    },
    /// `station join --tailnet --cancel`: removes the retry file only.
    JoinTailnetCancel,
    /// `station forget-deck`: revoke every deck key.
    ForgetDeck,
    /// `station drain`: refuse new work and wait until no session has open
    /// work, at most `timeout_ms` (M3 7-3, DL-128).
    Drain { timeout_ms: u64 },
    /// `fleetdeck settings get --station` (DL-172).
    SettingsGet,
    /// `fleetdeck settings set --station`. The local socket is the owner,
    /// so the save counts as elevated.
    SettingsSet { change: SettingsChange },
    /// `fleetdeck settings secret --station`: stores a secret and points
    /// the config to it. `Debug` hides the value; nothing logs the request.
    SettingsSecret { secret: SecretSet, revision: String },
}

pub use fleetdeck_core::localsock::InvitationText;

/// The answer to one [`LocalRequest`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum LocalResponse {
    Sessions {
        sessions: Vec<SessionSummary>,
    },
    /// The command's receipt (accepted or rejected).
    Receipt {
        receipt: Receipt,
    },
    /// The invitation of [`LocalRequest::Invite`].
    Invitation {
        invitation: InvitationText,
    },
    /// [`LocalRequest::Join`] paired with the deck of this fingerprint.
    Paired {
        deck: String,
    },
    /// [`LocalRequest::JoinTailnet`] without a fingerprint: the full link key
    /// fingerprint (64 hex) that the deck showed. Nothing was written.
    TailnetObserved {
        fingerprint: String,
    },
    /// [`LocalRequest::JoinTailnet`] paired with this deck device.
    TailnetPaired {
        device: String,
        deck: String,
    },
    /// The result of [`LocalRequest::JoinTailnetCancel`].
    TailnetCancelled {
        removed: bool,
    },
    /// [`LocalRequest::JoinTailnet`] ended with a stable code.
    TailnetRefused {
        code: String,
        detail: String,
    },
    /// [`LocalRequest::ForgetDeck`] revoked this many deck keys.
    Forgotten {
        count: usize,
    },
    /// The result of [`LocalRequest::Drain`].
    Drained {
        report: crate::manager::drain::DrainReport,
    },
    /// The answer to [`LocalRequest::SettingsGet`].
    Settings {
        view: SettingsView,
    },
    /// The answer to [`LocalRequest::SettingsSet`] and
    /// [`LocalRequest::SettingsSecret`].
    SettingsSaved {
        saved: SettingsSaved,
    },
    /// A settings request that failed, with its fixed error code.
    SettingsError {
        code: String,
        message: String,
    },
    /// The station could not decide the request (bad line, database error).
    Error {
        message: String,
    },
}

/// Why the local socket cannot start.
#[derive(Debug, thiserror::Error)]
pub enum LocalError {
    #[error("another station already serves `{0}`")]
    InUse(PathBuf),
    #[error("local socket `{path}`: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

/// Binds the socket (0600). A stale socket file (no server answers) is
/// replaced; a live one is an error.
///
/// # Errors
/// [`LocalError`].
pub async fn bind(path: &Path) -> Result<UnixListener, LocalError> {
    localsock::bind(path).await.map_err(|e| match e {
        BindError::InUse(path) => LocalError::InUse(path),
        BindError::Io { path, source } => LocalError::Io { path, source },
    })
}

/// Serves the local socket until `shutdown` turns true. Without `node`
/// the pairing requests are refused.
pub async fn serve(
    listener: UnixListener,
    manager: Arc<SessionManager>,
    node: Option<Arc<StationNode>>,
    mut shutdown: watch::Receiver<bool>,
) {
    // The node keeps the retry file of a tailnet join in the directory of
    // this socket (the station data directory).
    if let Some(node) = &node
        && let Ok(addr) = listener.local_addr()
        && let Some(dir) = addr.as_pathname().and_then(Path::parent)
    {
        node.set_data_dir(dir);
    }
    loop {
        tokio::select! {
            accepted = localsock::accept_own(&listener) => match accepted {
                Ok(Some(stream)) => {
                    tokio::spawn(connection(stream, Arc::clone(&manager), node.clone()));
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

async fn connection(
    stream: UnixStream,
    manager: Arc<SessionManager>,
    node: Option<Arc<StationNode>>,
) {
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
        let response = match serde_json::from_slice::<LocalRequest>(&line) {
            // A tailnet join waits for the owner for minutes. When the CLI
            // goes away, the join ends and its pair connection closes, so
            // the deck cancels the waiting request.
            Ok(request @ LocalRequest::JoinTailnet { .. }) => {
                tokio::select! {
                    r = handle(&manager, node.as_deref(), request) => r,
                    () = client_gone(&mut reader) => return,
                }
            }
            Ok(request) => handle(&manager, node.as_deref(), request).await,
            // serde's message can quote the input; never echo a line that
            // may carry an invitation (G12).
            Err(e) => error(localsock::bad_request(&e)),
        };
        if send(&mut write, &response).await.is_err() {
            return;
        }
    }
}

/// Returns when the client closed the socket (or sent more than it may
/// while it waits for an answer).
async fn client_gone(reader: &mut BufReader<tokio::net::unix::OwnedReadHalf>) {
    use tokio::io::AsyncBufReadExt as _;
    let _ = reader.fill_buf().await;
}

fn error(message: String) -> LocalResponse {
    LocalResponse::Error { message }
}

async fn pairing(node: Option<&StationNode>, request: LocalRequest) -> LocalResponse {
    let Some(node) = node else {
        return error("this station runs without its link node".to_owned());
    };
    let result = match request {
        LocalRequest::Invite { listen } => {
            node.invite(listen)
                .await
                .map(|text| LocalResponse::Invitation {
                    invitation: InvitationText(text.expose().clone()),
                })
        }
        LocalRequest::Join { invitation } => node
            .join(&fleetdeck_core::Secret::new(invitation.0))
            .await
            .map(|deck| LocalResponse::Paired {
                deck: deck.to_string(),
            }),
        LocalRequest::ForgetDeck => node
            .forget_deck()
            .await
            .map(|count| LocalResponse::Forgotten { count }),
        _ => return error("not a pairing request".to_owned()),
    };
    result.unwrap_or_else(|e| error(e.to_string()))
}

fn refused(code: &str, detail: impl Into<String>) -> LocalResponse {
    LocalResponse::TailnetRefused {
        code: code.to_owned(),
        detail: detail.into(),
    }
}

/// The `tailscale` program of this station: the setting `tailscale_bin`,
/// else the lookup of `fleetdeck_core::tailscale`.
async fn tailscale_cli(manager: &Arc<SessionManager>) -> fleetdeck_core::tailscale::TailscaleCli {
    let mut configured = None;
    if let Some(holder) = manager.settings()
        && let Outcome::Ok(v) = holder.get().await
        && let Ok(view) = serde_json::from_value::<SettingsView>(v)
    {
        configured = view
            .values
            .get("tailscale_bin")
            .and_then(|b| b.as_str())
            .filter(|b| !b.is_empty())
            .map(PathBuf::from);
    }
    fleetdeck_core::tailscale::TailscaleCli::locate(configured.as_deref())
}

/// `station join --tailnet` and its cancel (DL-178).
async fn tailnet(
    manager: &Arc<SessionManager>,
    node: Option<&StationNode>,
    request: LocalRequest,
) -> LocalResponse {
    use crate::link::{NodeError, TailnetJoin, TailnetResult};
    use fleetdeck_core::identity::Fingerprint;
    use fleetdeck_link::pair_request::{validate_name, validate_scopes};

    let Some(node) = node else {
        return error("this station runs without its link node".to_owned());
    };
    let join = match request {
        LocalRequest::JoinTailnetCancel => {
            return match node.cancel_tailnet() {
                Ok(removed) => LocalResponse::TailnetCancelled { removed },
                Err(NodeError::Tailnet { code, detail }) => refused(&code, detail),
                Err(e) => error(e.to_string()),
            };
        }
        LocalRequest::JoinTailnet {
            host,
            port,
            deck_fingerprint,
            scopes,
            name,
        } => {
            let scopes: std::collections::BTreeSet<_> = scopes.into_iter().collect();
            if let Err(e) = validate_name(&name).and_then(|()| validate_scopes(&scopes)) {
                return refused("invalid_request", e.to_string());
            }
            if host.trim().is_empty() || port == 0 {
                return refused("invalid_request", "the deck needs a host and a port");
            }
            let deck_fingerprint = match deck_fingerprint {
                None => None,
                Some(bytes) => {
                    let text: String = bytes.iter().map(|x| format!("{x:02x}")).collect();
                    match text.parse::<Fingerprint>() {
                        Ok(fp) => Some(fp),
                        Err(_) => {
                            return refused("invalid_request", "the fingerprint is not valid");
                        }
                    }
                }
            };
            TailnetJoin {
                host,
                port,
                deck_fingerprint,
                scopes,
                name,
                cli: tailscale_cli(manager).await,
                dial: None,
            }
        }
        _ => return error("not a tailnet request".to_owned()),
    };
    match node.join_tailnet(&join).await {
        Ok(TailnetResult::Observed(fp)) => LocalResponse::TailnetObserved {
            fingerprint: fp.to_string(),
        },
        Ok(TailnetResult::Paired { device, deck }) => LocalResponse::TailnetPaired {
            device,
            deck: deck.to_string(),
        },
        Err(NodeError::Tailnet { code, detail }) => refused(&code, detail),
        Err(NodeError::AlreadyPaired(fp)) => refused(
            "already_paired",
            format!(
                "this station is already paired with deck {fp}; run `fleetdeck station forget-deck` first"
            ),
        ),
        Err(e) => error(e.to_string()),
    }
}

fn settings_error(code: &str, message: &str) -> LocalResponse {
    LocalResponse::SettingsError {
        code: code.to_owned(),
        message: message.to_owned(),
    }
}

/// The response for the outcome of a settings call. The text of a decode
/// error is never used: it can quote a value.
fn settings_response<T: serde::de::DeserializeOwned>(
    outcome: Outcome,
    ok: impl FnOnce(T) -> LocalResponse,
) -> LocalResponse {
    match outcome {
        Outcome::Ok(v) => match serde_json::from_value(v) {
            Ok(t) => ok(t),
            Err(_) => settings_error("internal", "the answer could not be read"),
        },
        Outcome::Err(e) => LocalResponse::SettingsError {
            code: e.code,
            message: e.message,
        },
    }
}

/// The three settings requests, answered by the station's settings holder.
async fn settings(manager: &Arc<SessionManager>, request: LocalRequest) -> LocalResponse {
    let Some(holder) = manager.settings() else {
        return settings_error("unsupported", "this station has no settings holder");
    };
    match request {
        LocalRequest::SettingsGet => {
            settings_response(holder.get().await, |view| LocalResponse::Settings { view })
        }
        LocalRequest::SettingsSet { change } => {
            let params = serde_json::json!(SettingsSetParams {
                change,
                elevated: true,
            });
            settings_response(holder.set(manager, params).await, |saved| {
                LocalResponse::SettingsSaved { saved }
            })
        }
        LocalRequest::SettingsSecret { secret, revision } => {
            let params = serde_json::json!(SecretSetParams {
                secret,
                revision,
                elevated: true,
            });
            settings_response(holder.secret_set(manager, params).await, |saved| {
                LocalResponse::SettingsSaved { saved }
            })
        }
        _ => error("not a settings request".to_owned()),
    }
}

async fn handle(
    manager: &Arc<SessionManager>,
    node: Option<&StationNode>,
    request: LocalRequest,
) -> LocalResponse {
    let (command_id, command) = match request {
        LocalRequest::Invite { .. } | LocalRequest::Join { .. } | LocalRequest::ForgetDeck => {
            return pairing(node, request).await;
        }
        LocalRequest::JoinTailnet { .. } | LocalRequest::JoinTailnetCancel => {
            return tailnet(manager, node, request).await;
        }
        LocalRequest::SettingsGet
        | LocalRequest::SettingsSet { .. }
        | LocalRequest::SettingsSecret { .. } => return settings(manager, request).await,
        LocalRequest::Drain { timeout_ms } => {
            return match manager
                .drain(std::time::Duration::from_millis(timeout_ms))
                .await
            {
                Ok(report) => LocalResponse::Drained { report },
                Err(e) => error(e.to_string()),
            };
        }
        LocalRequest::Sessions => {
            return match manager.store().snapshot().await {
                Ok(snapshot) => LocalResponse::Sessions {
                    sessions: snapshot.sessions,
                },
                Err(e) => error(e.to_string()),
            };
        }
        LocalRequest::Command {
            command_id,
            command,
        } => (command_id, command),
        LocalRequest::Approve {
            command_id,
            request_id,
            decision,
        } => match manager.store().runtime_request(request_id.clone()).await {
            Ok(Some(request)) => (
                command_id,
                SessionCommand::Answer {
                    session_id: request.session_id,
                    request_id,
                    decision,
                },
            ),
            Ok(None) => return error(format!("no request `{request_id}` on this station")),
            Err(e) => return error(e.to_string()),
        },
    };
    let command_id = command_id.unwrap_or_else(|| manager.new_id());
    let envelope = CommandEnvelope {
        authority: None,
        command_id,
        target_incarnation: None,
        retry_until: manager.store().now_ms().saturating_add(RETRY_WINDOW_MS),
        origin: CommandOrigin::Local,
        command,
    };
    match manager.submit(envelope, Caller::local()).await {
        Ok(decided) => LocalResponse::Receipt {
            receipt: decided.receipt,
        },
        Err(e) => error(e.to_string()),
    }
}

/// The client side, for the CLI.
#[derive(Debug)]
pub struct LocalClient(LineClient);

impl LocalClient {
    /// Connects to the station's socket.
    ///
    /// # Errors
    /// No station serves `path`.
    pub async fn connect(path: &Path) -> io::Result<Self> {
        LineClient::connect(path, "station").await.map(Self)
    }

    /// Sends one request and reads its response.
    ///
    /// # Errors
    /// The connection failed or the response is not valid.
    pub async fn call(&mut self, request: &LocalRequest) -> io::Result<LocalResponse> {
        self.0.call(request).await
    }
}

#[cfg(test)]
#[path = "local_tests.rs"]
mod tests;
