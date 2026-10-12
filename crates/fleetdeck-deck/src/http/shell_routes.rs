//! The human shell routes of the board API (PLAN §6.8, D10; M3 item 6-11,
//! DL-404). Shells are for the owner: every route needs the `admin` scope.
//!
//! - `GET /api/stations/{station}/shells`: the station's `shell.list`.
//! - `POST /api/stations/{station}/shells`: one change (`open`, `resize`,
//!   `close`) with the board's `command_id`. The deck sends the station's
//!   `shell.<op>` method with a [`ShellEnvelope`] and answers with the
//!   station's receipt; a rejected receipt is a 4xx with the reject code.
//!   Every call is audited with the caller and the method.
//! - `GET /api/stations/{station}/shells/{shell}/attach`: a WebSocket.
//!   The deck asks `shell.attach` for a stream token, opens the data
//!   stream on the same link and relays bytes: binary messages from the
//!   board are keystrokes, binary messages to the board are the scrollback
//!   then the live output. The keystrokes are not audited. A closed socket
//!   closes only the stream; the shell keeps running on the station.
//!   Like the board WebSocket, the relay checks every `ws_recheck` that the
//!   caller's login session or API token is still valid; after a logout,
//!   an expiry or a revocation it closes the socket with code 4401 and the
//!   stream (DL-405).

use std::sync::Arc;
use std::time::Duration;

use axum::Json;
use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Extension, Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use fleetdeck_link::Control;
use fleetdeck_link::data::DataEvent;
use fleetdeck_proto::board::valid_command_id;
use fleetdeck_proto::command::{RETRY_WINDOW_MS, ReceiptState, RejectCode};
use fleetdeck_proto::link::Outcome;
use fleetdeck_proto::shell::{ShellAttach, ShellAttached, ShellChange, ShellEnvelope, ShellInfo};
use fleetdeck_proto::{CommandId, Receipt};
use serde::Deserialize;
use serde_json::{Value, json};

use super::Board;
use super::guard::ApiError;
use super::ws::CLOSE_LOGIN_ENDED;
use crate::link::streams::DeckStream;
use crate::store::{AuditResult, NewAudit, Principal, StationRecord};

/// The longest the board waits for the station's answer.
pub const SHELL_WAIT: Duration = Duration::from_secs(8);
/// The largest keystroke message from the board.
pub const MAX_INPUT_MESSAGE: usize = 64 * 1024;

/// The body of `POST /api/stations/{station}/shells`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShellCommand {
    pub command_id: CommandId,
    pub change: ShellChange,
}

async fn station(board: &Board, station: &str) -> Result<StationRecord, ApiError> {
    board
        .store
        .find_station(station.to_owned())
        .await
        .map_err(ApiError::internal)?
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::NOT_FOUND,
                "unknown_station",
                format!("no station is named `{station}`"),
            )
        })
}

fn unreachable(rec: &StationRecord) -> ApiError {
    ApiError::new(
        StatusCode::CONFLICT,
        "station_unreachable",
        format!("station `{}` has no live link to this deck", rec.name),
    )
}

async fn ask(
    control: &Control,
    rec: &StationRecord,
    method: &str,
    command_id: Option<&CommandId>,
    params: Value,
) -> Result<Value, ApiError> {
    let outcome = tokio::time::timeout(SHELL_WAIT, control.request(method, command_id, params))
        .await
        .map_err(|_| {
            ApiError::new(
                StatusCode::GATEWAY_TIMEOUT,
                "timeout",
                "the station did not answer in time",
            )
        })?
        .map_err(|_| unreachable(rec))?;
    match outcome {
        Outcome::Ok(v) => Ok(v),
        Outcome::Err(e) => Err(match e.code.as_str() {
            "invalid_state" => ApiError::new(StatusCode::CONFLICT, "invalid_state", e.message),
            "unsupported" => ApiError::new(StatusCode::CONFLICT, "unsupported", e.message),
            _ => ApiError::new(
                StatusCode::BAD_GATEWAY,
                "station_error",
                format!("{}: {}", e.code, e.message),
            ),
        }),
    }
}

fn station_error(e: impl std::fmt::Display) -> ApiError {
    ApiError::new(StatusCode::BAD_GATEWAY, "station_error", e.to_string())
}

/// `GET /api/stations/{station}/shells`.
///
/// # Errors
/// 404 `unknown_station`; 409 `station_unreachable`; 502; 504.
pub async fn list(
    State(board): State<Arc<Board>>,
    Path(name): Path<String>,
) -> Result<Json<Vec<ShellInfo>>, ApiError> {
    let rec = station(&board, &name).await?;
    let control = board
        .link
        .control_for(&rec.station_id)
        .ok_or_else(|| unreachable(&rec))?;
    let v = ask(&control, &rec, "shell.list", None, json!({})).await?;
    serde_json::from_value(v).map(Json).map_err(station_error)
}

/// `POST /api/stations/{station}/shells`.
///
/// # Errors
/// 400 `invalid_command_id` / `invalid_command`; 404 `unknown_station`;
/// 409 `not_synced`, `station_unreachable`, `invalid_state`,
/// `unsupported`; 502; 504.
pub async fn command(
    State(board): State<Arc<Board>>,
    Path(name): Path<String>,
    Extension(principal): Extension<Principal>,
    Json(body): Json<ShellCommand>,
) -> Result<Json<Receipt>, ApiError> {
    let method = body.change.method();
    let command_id = body.command_id.clone();
    let result = decide(&board, &name, body).await;
    let (outcome, detail) = match &result {
        Ok(_) => (AuditResult::Ok, None),
        Err(e) => (AuditResult::Refused, Some(e.code)),
    };
    let mut row = NewAudit::new(principal.actor(), method, outcome)
        .target(name)
        .command(command_id);
    if let Some(d) = detail {
        row = row.detail(d);
    }
    board.store.audit_or_log(row).await;
    result.map(Json)
}

async fn decide(board: &Board, name: &str, body: ShellCommand) -> Result<Receipt, ApiError> {
    if !valid_command_id(&body.command_id) {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_command_id",
            "a command id has 1 to 64 characters from [0-9A-Za-z_-]",
        ));
    }
    let rec = station(board, name).await?;
    let Some(incarnation) = rec.incarnation.clone() else {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "not_synced",
            format!("station `{}` has not sent its first snapshot", rec.name),
        ));
    };
    let control = board
        .link
        .control_for(&rec.station_id)
        .ok_or_else(|| unreachable(&rec))?;
    let method = body.change.method();
    let envelope = ShellEnvelope {
        command_id: body.command_id,
        target_incarnation: incarnation,
        retry_until: board.store.now_ms().saturating_add(RETRY_WINDOW_MS),
        change: body.change,
    };
    let params = serde_json::to_value(&envelope).map_err(ApiError::internal)?;
    let v = ask(&control, &rec, method, Some(&envelope.command_id), params).await?;
    let receipt: Receipt = serde_json::from_value(v).map_err(station_error)?;
    if receipt.state == ReceiptState::Accepted {
        return Ok(receipt);
    }
    let (code, message) = receipt.reject.as_ref().map_or_else(
        || (RejectCode::Unknown, "rejected".to_owned()),
        |r| (r.code, r.message.clone()),
    );
    let (status, code) = match code {
        RejectCode::InvalidCommand => (StatusCode::BAD_REQUEST, "invalid_command"),
        RejectCode::Forbidden => (StatusCode::FORBIDDEN, "forbidden"),
        RejectCode::InvalidState => (StatusCode::CONFLICT, "invalid_state"),
        RejectCode::Unsupported => (StatusCode::CONFLICT, "unsupported"),
        RejectCode::CommandIdReused => (StatusCode::CONFLICT, "command_id_reused"),
        RejectCode::IncarnationMismatch => (StatusCode::CONFLICT, "incarnation_mismatch"),
        _ => (StatusCode::CONFLICT, "rejected"),
    };
    Err(ApiError::new(status, code, message))
}

/// `GET /api/stations/{station}/shells/{shell}/attach` (WebSocket).
///
/// # Errors
/// As [`list`], before the upgrade; 409 `invalid_state` for an unknown
/// shell.
pub async fn attach(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    Path((name, shell_id)): Path<(String, String)>,
    ws: WebSocketUpgrade,
) -> Result<Response, ApiError> {
    let rec = station(&board, &name).await?;
    let (control, hub) = board
        .link
        .data_for(&rec.station_id)
        .ok_or_else(|| unreachable(&rec))?;
    let params = json!(ShellAttach {
        shell_id: shell_id.clone()
    });
    let v = ask(&control, &rec, "shell.attach", None, params).await?;
    let token: ShellAttached = serde_json::from_value(v).map_err(station_error)?;
    let stream = hub
        .open(&token.stream_token)
        .await
        .map_err(|_| unreachable(&rec))?;
    Ok(ws
        .max_message_size(MAX_INPUT_MESSAGE)
        .max_frame_size(MAX_INPUT_MESSAGE)
        .on_upgrade(move |socket| relay(board, principal, socket, stream))
        .into_response())
}

/// Copies bytes both ways until either side ends or the caller's login
/// ends.
async fn relay(
    board: Arc<Board>,
    principal: Principal,
    mut socket: WebSocket,
    mut stream: DeckStream,
) {
    let mut recheck = tokio::time::interval(board.ws_recheck);
    recheck.tick().await;
    loop {
        tokio::select! {
            _ = recheck.tick() => {
                // A database error counts as an ended login (fail closed).
                if !board.store.principal_alive(&principal).await.unwrap_or(false) {
                    let _ = socket.send(Message::Close(Some(CloseFrame {
                        code: CLOSE_LOGIN_ENDED,
                        reason: "login ended".into(),
                    }))).await;
                    break;
                }
            }
            event = stream.events.recv() => match event {
                Some(DataEvent::Data { bytes, .. }) => {
                    let n = bytes.len();
                    if socket.send(Message::Binary(bytes.into())).await.is_err() {
                        break;
                    }
                    let _ = stream.handle.consumed(stream.id, n).await;
                }
                Some(DataEvent::Opened { .. }) => {}
                Some(DataEvent::Closed { .. } | DataEvent::Reset { .. }) | None => {
                    let _ = socket.send(Message::Close(None)).await;
                    return;
                }
            },
            message = socket.recv() => match message {
                Some(Ok(Message::Binary(bytes))) => {
                    if stream.handle.write(stream.id, bytes.to_vec()).await.is_err() {
                        break;
                    }
                }
                Some(Ok(Message::Text(text))) => {
                    let bytes = text.as_str().as_bytes().to_vec();
                    if stream.handle.write(stream.id, bytes).await.is_err() {
                        break;
                    }
                }
                Some(Ok(_)) => {}
                Some(Err(_)) | None => break,
            },
        }
    }
    // The board went away: end the stream, not the shell.
    let _ = stream.handle.close(stream.id).await;
}
