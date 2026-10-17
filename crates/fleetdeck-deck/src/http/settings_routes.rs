//! The settings routes (DL-172; design 1.2, 1.5, 1.6): the deck's own
//! settings, the settings of a station through the link, and the restart.
//! Every route is owner only (`Principal::Owner`, also with the `admin`
//! scope) and every write is audited with the changed keys, never values.
//!
//! A save with a danger change and a restart need the owner's elevation
//! ([`crate::elevation`]). The deck checks it and tells the station with
//! `elevated`; the station (or the deck's own writer) decides which changes
//! are danger changes.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::Json;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Extension, Path, State};
use axum::http::StatusCode;
use fleetdeck_core::settings::{DeckHolder, SaveError, SettingsWriter, view_deck};
use fleetdeck_proto::StationId;
use fleetdeck_proto::link::{Outcome, WireError};
use fleetdeck_proto::settings::{
    RestartAnswer, RestartRequest, SettingsChange, SettingsSaved, SettingsSetParams, SettingsView,
};
use serde::Deserialize;
use serde_json::{Map, Value, json};

use super::Board;
use super::guard::{ApiError, owner_only};
use crate::live::{DeckLive, LiveCell};
use crate::store::auth::LoginSession;
use crate::store::{AuditResult, DeckStore, NewAudit, Principal, StationRecord};

/// The longest the board waits for the station's answer.
pub const SETTINGS_WAIT: Duration = Duration::from_secs(15);
/// How long the deck waits after the answer of a restart before it starts
/// the shutdown, so that the answer leaves the socket.
const RESTART_DELAY: Duration = Duration::from_millis(300);
/// The target of a restart that names the deck itself.
const DECK_TARGET: &str = "deck";

/// The settings file of the deck and how its writer is set up.
#[derive(Clone)]
pub struct DeckSettings {
    pub writer: Arc<SettingsWriter<DeckHolder>>,
    pub path: PathBuf,
    /// `(key, flag)` of the values that the process took from the command
    /// line, for example `("listen", "--listen")`.
    pub overrides: Vec<(String, String)>,
    /// Holds a save from its start to its publish, so that two saves
    /// publish in the order of their commits (PRE-PR r1). A clone shares it.
    pub gate: Arc<tokio::sync::Mutex<()>>,
}

impl std::fmt::Debug for DeckSettings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeckSettings")
            .field("path", &self.path)
            .field("overrides", &self.overrides)
            .finish_non_exhaustive()
    }
}

/// The owner's login of this request, or the refusal (audited).
pub(super) async fn owner(
    board: &Board,
    principal: &Principal,
    action: &'static str,
    target: &str,
    what: &str,
) -> Result<LoginSession, ApiError> {
    match owner_only(principal, what) {
        Ok(()) => {}
        Err(e) => {
            board
                .store
                .audit_or_log(
                    NewAudit::new(principal.actor(), action, AuditResult::Refused)
                        .target(target)
                        .detail(e.code),
                )
                .await;
            return Err(e);
        }
    }
    match principal {
        Principal::Owner(s) => Ok(s.clone()),
        _ => Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "owner_only",
            format!("only the owner's board login {what}"),
        )),
    }
}

/// True when `token` is a live elevation of this login.
pub(super) fn is_elevated(board: &Board, session: &LoginSession, token: Option<&str>) -> bool {
    token.is_some_and(|t| {
        board
            .elevations
            .check(t, &session.session_hash, board.store.now_ms())
    })
}

/// The audit row of a settings route: the keys on success, the error code
/// on a failure. Never a value.
pub(super) async fn audit<T>(
    board: &Board,
    action: &'static str,
    target: &str,
    keys: &[String],
    result: &Result<T, ApiError>,
) {
    let row = match result {
        Ok(_) => NewAudit::new("owner", action, AuditResult::Ok).detail(keys.join(",")),
        Err(e) => NewAudit::new("owner", action, AuditResult::Failed).detail(format!(
            "{}{}{}",
            e.code,
            if keys.is_empty() { "" } else { ": " },
            keys.join(",")
        )),
    };
    board.store.audit_or_log(row.target(target)).await;
}

fn details(key: &str, value: Value) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert(key.to_owned(), value);
    m
}

/// The HTTP status of a save error code, and its fixed code.
fn save_status(code: &str) -> Option<(StatusCode, &'static str)> {
    Some(match code {
        "settings_changed" => (StatusCode::CONFLICT, "settings_changed"),
        "settings_changed_on_disk" => (StatusCode::CONFLICT, "settings_changed_on_disk"),
        "invalid" => (StatusCode::BAD_REQUEST, "invalid"),
        "cannot_apply" => (StatusCode::BAD_REQUEST, "cannot_apply"),
        "secret_via_relay" => (StatusCode::BAD_REQUEST, "secret_via_relay"),
        "elevation_required" => (StatusCode::BAD_REQUEST, "elevation_required"),
        "settings_write_failed" => (StatusCode::INTERNAL_SERVER_ERROR, "settings_write_failed"),
        "secret_store_unavailable" => (
            StatusCode::INTERNAL_SERVER_ERROR,
            "secret_store_unavailable",
        ),
        _ => return None,
    })
}

/// The error of the deck's own save.
fn save_error(e: &SaveError) -> ApiError {
    let (status, code) = save_status(e.code())
        .unwrap_or((StatusCode::INTERNAL_SERVER_ERROR, "settings_write_failed"));
    let err = ApiError::new(status, code, e.to_string());
    match e {
        SaveError::ElevationRequired { keys } => err.with_details(details("keys", json!(keys))),
        _ => err,
    }
}

/// The error for a station answer. `message` is the station's text (a key
/// name, never a value) or `None` for a fixed text (the secret relay).
pub(super) fn station_failure(e: &WireError, relay: bool) -> ApiError {
    let Some((status, code)) = save_status(&e.code) else {
        return ApiError::new(
            StatusCode::BAD_GATEWAY,
            "station_error",
            "the station answered with an error",
        );
    };
    let message = if relay {
        fixed_text(code).to_owned()
    } else {
        e.message.chars().take(300).collect()
    };
    let err = ApiError::new(status, code, message);
    let keys = e
        .data
        .as_ref()
        .and_then(|d| d.get("keys"))
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect::<Vec<_>>()
        });
    match keys {
        Some(keys) if code == "elevation_required" => {
            err.with_details(details("keys", json!(keys)))
        }
        _ => err,
    }
}

/// The fixed text of a save error code, for the secret relay.
fn fixed_text(code: &str) -> &'static str {
    match code {
        "settings_changed" => "the settings changed since you read them; reload and try again",
        "settings_changed_on_disk" => "the config file changed on disk; reload",
        "invalid" => "the key or the value is not valid",
        "cannot_apply" => "the secret cannot be applied",
        "secret_via_relay" => "this key is not set through the secret route",
        "elevation_required" => "a recent sign-in is needed",
        "settings_write_failed" => "the secret could not be written; the old file is unchanged",
        _ => "the secret store is not available",
    }
}

/// The station `name` and the outcome of `method` on its live link.
///
/// # Errors
/// 404 `unknown_station`; 409 `station_unreachable`; 504 `timeout`.
pub(super) async fn station_call(
    board: &Board,
    name: &str,
    method: &str,
    params: Value,
) -> Result<(StationRecord, Outcome), ApiError> {
    let rec = board
        .store
        .find_station(name.to_owned())
        .await
        .map_err(ApiError::internal)?
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::NOT_FOUND,
                "unknown_station",
                format!("no station is named `{name}`"),
            )
        })?;
    let unreachable = || {
        ApiError::new(
            StatusCode::CONFLICT,
            "station_unreachable",
            format!("station `{}` has no live link to this deck", rec.name),
        )
    };
    let control = board
        .link
        .control_for(&rec.station_id)
        .ok_or_else(unreachable)?;
    let outcome = tokio::time::timeout(SETTINGS_WAIT, control.request(method, None, params))
        .await
        .map_err(|_| {
            ApiError::new(
                StatusCode::GATEWAY_TIMEOUT,
                "timeout",
                "the station did not answer in time",
            )
        })?
        .map_err(|_| unreachable())?;
    Ok((rec, outcome))
}

fn bad_answer() -> ApiError {
    ApiError::new(
        StatusCode::BAD_GATEWAY,
        "station_error",
        "the station answered with data that the deck does not know",
    )
}

fn not_set_up() -> ApiError {
    ApiError::new(
        StatusCode::SERVICE_UNAVAILABLE,
        "settings_unavailable",
        "this deck has no settings file set up",
    )
}

fn body_error(_: &JsonRejection) -> ApiError {
    ApiError::new(
        StatusCode::BAD_REQUEST,
        "invalid_body",
        "the request body is not valid",
    )
}

/// The keys of a change, sorted, for the audit row.
fn change_keys(change: &SettingsChange) -> Vec<String> {
    let mut keys: Vec<String> = change.changes.keys().cloned().collect();
    keys.sort();
    keys
}

/// `GET /api/settings/deck`: the settings of the deck.
///
/// # Errors
/// 403 `owner_only`; 500 `settings_unreadable`; 503 `settings_unavailable`.
pub async fn deck_get(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
) -> Result<Json<SettingsView>, ApiError> {
    owner(
        &board,
        &principal,
        "settings.deck_get",
        DECK_TARGET,
        "reads the deck settings",
    )
    .await?;
    let settings = board.settings.as_ref().ok_or_else(not_set_up)?;
    let view = deck_view(settings).await;
    match view {
        Ok(v) => Ok(Json(v)),
        Err(e) => {
            tracing::warn!(error = %e, "deck settings");
            Err(ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "settings_unreadable",
                "the deck config file cannot be read or does not parse; see the deck log",
            ))
        }
    }
}

/// `POST /api/settings/deck { changes, revision, elevation? }`.
///
/// # Errors
/// 400 `invalid_body`, `invalid`, `cannot_apply`, `secret_via_relay`,
/// `elevation_required` (with `keys`); 403 `owner_only`; 409
/// `settings_changed`, `settings_changed_on_disk`; 500
/// `settings_write_failed`, `secret_store_unavailable`; 503
/// `settings_unavailable`.
pub async fn deck_set(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    body: Result<Json<SettingsChange>, JsonRejection>,
) -> Result<Json<SettingsSaved>, ApiError> {
    const ACTION: &str = "settings.deck_set";
    let session = owner(
        &board,
        &principal,
        ACTION,
        DECK_TARGET,
        "changes the deck settings",
    )
    .await?;
    let Json(change) = body.map_err(|e| body_error(&e))?;
    let keys = change_keys(&change);
    let result = deck_save(&board, &session, change).await;
    audit(&board, ACTION, DECK_TARGET, &keys, &result).await;
    result.map(Json)
}

async fn deck_save(
    board: &Board,
    session: &LoginSession,
    change: SettingsChange,
) -> Result<SettingsSaved, ApiError> {
    let settings = board.settings.as_ref().ok_or_else(not_set_up)?;
    let elevated = is_elevated(board, session, change.elevation.as_deref());
    save_and_publish(settings, &board.store, &board.live, change, elevated)
        .await
        .map_err(|e| save_error(&e))
}

/// Saves a change of the deck settings and publishes the live fields. The
/// board routes and the local socket both call it. The gate of the settings
/// is held until the publish. The same save reconciles the provenance lists
/// of the automatic tailnet mode (design 14.4,
/// [`crate::tailnet_auto::reconcile`]).
///
/// # Errors
/// [`SaveError`].
pub async fn save_and_publish(
    settings: &DeckSettings,
    store: &DeckStore,
    live: &LiveCell,
    change: SettingsChange,
    elevated: bool,
) -> Result<SettingsSaved, SaveError> {
    save_with(settings, store, live, change, elevated, true).await
}

/// [`save_and_publish`] for a caller that writes the provenance lists
/// itself ("Use tailnet"): no reconciliation.
pub(super) async fn save_unreconciled(
    settings: &DeckSettings,
    store: &DeckStore,
    live: &LiveCell,
    change: SettingsChange,
    elevated: bool,
) -> Result<SettingsSaved, SaveError> {
    save_with(settings, store, live, change, elevated, false).await
}

async fn save_with(
    settings: &DeckSettings,
    store: &DeckStore,
    live: &LiveCell,
    mut change: SettingsChange,
    elevated: bool,
    reconcile: bool,
) -> Result<SettingsSaved, SaveError> {
    let _gate = settings.gate.lock().await;
    if reconcile && crate::tailnet_auto::touches_provenance(&change.changes) {
        // An unreadable file gets no reconciliation here; the writer
        // refuses it below.
        match deck_view(settings).await {
            Ok(view) => crate::tailnet_auto::reconcile(&view.values, &mut change.changes),
            Err(e) => tracing::warn!(error = %e, "deck settings"),
        }
    }
    let writer = Arc::clone(&settings.writer);
    let saved = tokio::task::spawn_blocking(move || {
        writer.save(&change, elevated, |new| {
            Ok::<_, (String, String)>(DeckLive::of(new))
        })
    })
    .await
    .map_err(|_| SaveError::SettingsWriteFailed)?;
    match saved {
        // 1.4 step 2: publish, which cannot fail.
        Ok((saved, _config, new)) => {
            publish(store, live, new);
            Ok(saved)
        }
        Err(e) => {
            if e == SaveError::ChangedOnDisk {
                // A hand edit while the deck ran: take the file as the new
                // base and publish it, so that the next save can work.
                let writer = Arc::clone(&settings.writer);
                if let Ok(Ok(config)) = tokio::task::spawn_blocking(move || writer.resync()).await {
                    publish(store, live, DeckLive::of(&config));
                }
            }
            Err(e)
        }
    }
}

/// The view of the deck settings, read in a blocking task.
///
/// # Errors
/// The text of the read error (for the log only).
pub async fn deck_view(settings: &DeckSettings) -> Result<SettingsView, String> {
    let path = settings.path.clone();
    let overrides = settings.overrides.clone();
    tokio::task::spawn_blocking(move || {
        let pairs: Vec<(&str, &str)> = overrides
            .iter()
            .map(|(k, f)| (k.as_str(), f.as_str()))
            .collect();
        view_deck(&path, &pairs).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Publishes the live fields: the board reads them per use, and the store
/// reads its flags for the conflict mail at every fold and for fleet control
/// at every decision and send check.
fn publish(store: &DeckStore, cell: &LiveCell, live: DeckLive) {
    store.set_conflict_mail(live.conflict_mail);
    store.set_fleet_control(live.fleet_control);
    cell.publish(live);
}

/// `GET /api/stations/{station}/settings`: link method `settings.get`.
///
/// # Errors
/// 403 `owner_only`; 404 `unknown_station`; 409 `station_unreachable`; 502
/// `station_error`; 504 `timeout`.
pub async fn station_get(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    Path(name): Path<String>,
) -> Result<Json<SettingsView>, ApiError> {
    owner(
        &board,
        &principal,
        "settings.station_get",
        &name,
        "reads station settings",
    )
    .await?;
    let (_, outcome) = station_call(&board, &name, "settings.get", json!({})).await?;
    match outcome {
        Outcome::Ok(v) => serde_json::from_value(v)
            .map(Json)
            .map_err(|_| bad_answer()),
        Outcome::Err(e) => Err(station_failure(&e, false)),
    }
}

/// `POST /api/stations/{station}/settings { changes, revision, elevation? }`:
/// link method `settings.set`.
///
/// # Errors
/// As [`deck_set`], and 404 `unknown_station`; 409 `station_unreachable`;
/// 502 `station_error`; 504 `timeout`.
pub async fn station_set(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    Path(name): Path<String>,
    body: Result<Json<SettingsChange>, JsonRejection>,
) -> Result<Json<SettingsSaved>, ApiError> {
    const ACTION: &str = "settings.station_set";
    let session = owner(
        &board,
        &principal,
        ACTION,
        &name,
        "changes station settings",
    )
    .await?;
    let Json(mut change) = body.map_err(|e| body_error(&e))?;
    let keys = change_keys(&change);
    let elevated = is_elevated(&board, &session, change.elevation.as_deref());
    change.elevation = None;
    let new_name = trimmed_name(&mut change);
    let params = json!(SettingsSetParams { change, elevated });
    let result = async {
        if let Some(new_name) = &new_name {
            refuse_taken_name(&board, &name, new_name).await?;
        }
        let (rec, outcome) = station_call(&board, &name, "settings.set", params).await?;
        match outcome {
            Outcome::Ok(v) => {
                // The change may be a new member deny list: the review
                // caches of the station must not outlive it (NV-33).
                super::review_anchor_cache::clear_station(&rec.station_id);
                if let Some(new_name) = new_name {
                    follow_name(&board, rec.station_id.clone(), new_name).await;
                }
                serde_json::from_value::<SettingsSaved>(v).map_err(|_| bad_answer())
            }
            Outcome::Err(e) => Err(station_failure(&e, false)),
        }
    }
    .await;
    audit(&board, ACTION, &name, &keys, &result).await;
    result.map(Json)
}

/// The new station name of a save: the text value of the key `name`, which
/// is trimmed in the change.
fn trimmed_name(change: &mut SettingsChange) -> Option<String> {
    let Some(Value::String(text)) = change.changes.get_mut("name") else {
        return None;
    };
    let trimmed = text.trim().to_owned();
    text.clone_from(&trimmed);
    Some(trimmed)
}

/// Refuses a new station name that another station on this deck has.
async fn refuse_taken_name(board: &Board, station: &str, new_name: &str) -> Result<(), ApiError> {
    let Some(rec) = board
        .store
        .find_station(station.to_owned())
        .await
        .map_err(ApiError::internal)?
    else {
        // `station_call` answers the unknown station.
        return Ok(());
    };
    let taken = board
        .store
        .station_name_taken(new_name.to_owned(), rec.station_id)
        .await
        .map_err(ApiError::internal)?;
    if taken {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "name_taken",
            format!("Another station on this deck is named `{new_name}`. Choose another name."),
        ));
    }
    Ok(())
}

/// Gives the board record the name that the station saved. A failure here
/// is a race: the sync at the next link up fixes the name.
async fn follow_name(board: &Board, id: StationId, new_name: String) {
    if let Err(e) = board.store.rename_station(id.clone(), new_name).await {
        tracing::warn!(error = %e, station = %id, "the board name of the station was not renamed");
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RestartBody {
    /// `deck` or a station name or id.
    target: String,
    #[serde(default)]
    stop_direct_sessions: bool,
    #[serde(default)]
    elevation: Option<String>,
}

/// `POST /api/settings/restart { target, stop_direct_sessions, elevation }`.
/// An elevation is always required.
///
/// # Errors
/// 400 `invalid_body`, `elevation_required` (`keys: ["restart"]`); 403
/// `owner_only`; 404 `unknown_station`; 409 `station_unreachable`,
/// `station_not_paired`, `restart_would_stop_sessions` (with `would_stop`); 501
/// `restart_unsupported`; 502 `station_error`; 504 `timeout`.
pub async fn restart(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    body: Result<Json<RestartBody>, JsonRejection>,
) -> Result<Json<RestartAnswer>, ApiError> {
    const ACTION: &str = "settings.restart";
    let session = owner(&board, &principal, ACTION, "", "restarts a process").await?;
    let Json(body) = body.map_err(|e| body_error(&e))?;
    let result = if is_elevated(&board, &session, body.elevation.as_deref()) {
        restart_target(&board, &body).await
    } else {
        Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "elevation_required",
            "a recent sign-in is needed to restart",
        )
        .with_details(details("keys", json!(["restart"]))))
    };
    let stop = if body.stop_direct_sessions {
        vec!["stop_direct_sessions".to_owned()]
    } else {
        Vec::new()
    };
    audit(&board, ACTION, &body.target, &stop, &result).await;
    result.map(Json)
}

fn unsupported() -> ApiError {
    ApiError::new(
        StatusCode::NOT_IMPLEMENTED,
        "restart_unsupported",
        "this process cannot restart itself yet",
    )
}

async fn restart_target(board: &Board, body: &RestartBody) -> Result<RestartAnswer, ApiError> {
    if body.target != DECK_TARGET {
        return restart_station(board, &body.target, body.stop_direct_sessions).await;
    }
    // In `fleetdeck up` mode the deck restarts with the local station (the
    // same process); a standalone deck restarts itself.
    if let Some(id) = board.local_station.get() {
        return restart_station(board, &id.0, body.stop_direct_sessions).await;
    }
    // No handle and no local station yet: the process is `fleetdeck up`
    // and the pairing has not finished.
    let Some(handle) = board.restart.clone() else {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "station_not_paired",
            "the local station is not paired yet; try again in a moment",
        ));
    };
    tokio::spawn(async move {
        tokio::time::sleep(RESTART_DELAY).await;
        handle.request();
    });
    Ok(RestartAnswer {
        restarting: true,
        would_stop: Vec::new(),
    })
}

async fn restart_station(
    board: &Board,
    target: &str,
    stop_direct_sessions: bool,
) -> Result<RestartAnswer, ApiError> {
    let params = json!(RestartRequest {
        stop_direct_sessions,
        elevated: true,
    });
    let (_, outcome) = station_call(board, target, "station.restart", params).await?;
    match outcome {
        Outcome::Ok(v) => {
            let answer: RestartAnswer = serde_json::from_value(v).map_err(|_| bad_answer())?;
            if answer.restarting || answer.would_stop.is_empty() {
                Ok(answer)
            } else {
                Err(ApiError::new(
                    StatusCode::CONFLICT,
                    "restart_would_stop_sessions",
                    "a restart would end live work; confirm to stop it",
                )
                .with_details(details("would_stop", json!(answer.would_stop))))
            }
        }
        Outcome::Err(e) => Err(match e.code.as_str() {
            "unsupported" | "feature_not_negotiated" | "unknown_method" => unsupported(),
            _ => station_failure(&e, false),
        }),
    }
}
