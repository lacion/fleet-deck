//! The secret files of a project (Fleet Sync FS-3, design §3.6): the list
//! (`GET` and `PUT /api/projects/{id}/secret-files`) and the sync run
//! (`POST /api/projects/{id}/secret-files/sync`). Owner's board login only,
//! CSRF on the writes, audited. The board never receives a content.

use std::sync::Arc;

use axum::Json;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Extension, Path, State};
use axum::http::StatusCode;
use fleetdeck_proto::CommandId;
use fleetdeck_proto::fleet_git_secrets::{
    MAX_SECRET_FILES, SECRET_HIDDEN_TEXT, SECRET_PATH_TEXT, SecretFilesView, SecretPathError,
    SecretSyncReport, check_secret_path,
};
use serde::Deserialize;

use crate::fleet_git_secrets::{SyncError, lock_project, sync_project};
use crate::http::Board;
use crate::http::guard::{ApiError, owner_only};
use crate::http::project_home_routes::{audit, check_command_id, known_project};
use crate::http::projects_routes::bad_body;
use crate::store::Principal;

const SET: &str = "project.secret_files";
const RUN: &str = "project.secret_sync_run";

/// The body of the `PUT`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetBody {
    pub paths: Vec<String>,
    pub command_id: String,
}

/// The body of the `POST`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SyncBody {
    pub command_id: String,
}

/// 400 with the field `paths`.
fn paths_error(code: &'static str, text: &'static str) -> ApiError {
    let mut details = serde_json::Map::new();
    details.insert("field".to_owned(), "paths".into());
    ApiError::new(StatusCode::BAD_REQUEST, code, text).with_details(details)
}

/// The list as the owner typed it: each path trimmed, empty lines dropped,
/// duplicates removed (the first stays), at most 20, each path checked.
fn clean(paths: Vec<String>) -> Result<Vec<String>, ApiError> {
    let mut out: Vec<String> = Vec::new();
    for path in paths {
        let path = path.trim().to_owned();
        if !path.is_empty() && !out.contains(&path) {
            out.push(path);
        }
    }
    if out.len() > MAX_SECRET_FILES {
        return Err(paths_error(
            "too_many_secret_files",
            "A project can list at most 20 secret files.",
        ));
    }
    for path in &out {
        match check_secret_path(path) {
            Ok(()) => {}
            Err(SecretPathError::Invalid) => {
                return Err(paths_error("invalid_secret_path", SECRET_PATH_TEXT));
            }
            Err(SecretPathError::NotMemberHidden) => {
                return Err(paths_error("invalid_secret_path", SECRET_HIDDEN_TEXT));
            }
        }
    }
    Ok(out)
}

async fn view(board: &Board, id: &str) -> Result<SecretFilesView, ApiError> {
    let rows = board
        .store
        .secret_files(id.to_owned())
        .await
        .map_err(ApiError::internal)?;
    Ok(SecretFilesView {
        paths: rows.iter().map(|r| r.path.clone()).collect(),
        files: rows.into_iter().map(|r| r.last_report).collect(),
    })
}

/// `GET /api/projects/{id}/secret-files`.
///
/// # Errors
/// 403 `owner_only`; 404 `unknown_project`.
pub async fn list(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    Path(id): Path<String>,
) -> Result<Json<SecretFilesView>, ApiError> {
    owner_only(&principal, "reads the secret files")?;
    known_project(&board, &id).await?;
    Ok(Json(view(&board, &id).await?))
}

async fn set_inner(
    board: &Board,
    id: &str,
    body: Result<Json<SetBody>, JsonRejection>,
) -> (Option<CommandId>, Result<SecretFilesView, ApiError>) {
    let Json(body) = match body {
        Ok(b) => b,
        Err(e) => return (None, Err(bad_body(&e))),
    };
    let command = match check_command_id(&body.command_id) {
        Ok(c) => c,
        Err(e) => return (None, Err(e)),
    };
    let run = async {
        known_project(board, id).await?;
        let paths = clean(body.paths)?;
        // A run holds this lock for its whole time; the list changes between
        // two runs (R6).
        let _lock = lock_project(id).await;
        board
            .store
            .secret_files_set(id.to_owned(), paths)
            .await
            .map_err(ApiError::internal)?
            .ok_or_else(|| {
                ApiError::new(StatusCode::NOT_FOUND, "unknown_project", "Unknown project.")
            })?;
        view(board, id).await
    };
    (Some(command), run.await)
}

/// `PUT /api/projects/{id}/secret-files`.
///
/// # Errors
/// 400 `invalid_body` / `invalid_command_id` / `too_many_secret_files` /
/// `invalid_secret_path` (field `paths`); 403 `owner_only`; 404
/// `unknown_project`.
pub async fn set(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    Path(id): Path<String>,
    body: Result<Json<SetBody>, JsonRejection>,
) -> Result<Json<SecretFilesView>, ApiError> {
    owner_only(&principal, "changes the secret files")?;
    let (command, result) = set_inner(&board, &id, body).await;
    audit(&board, &principal, SET, &id, command, &result, |view| {
        format!("{} files", view.paths.len())
    })
    .await;
    result.map(Json)
}

async fn sync_inner(
    board: &Board,
    principal: &Principal,
    id: &str,
    body: Result<Json<SyncBody>, JsonRejection>,
) -> (Option<CommandId>, Result<SecretSyncReport, ApiError>) {
    let Json(body) = match body {
        Ok(b) => b,
        Err(e) => return (None, Err(bad_body(&e))),
    };
    let command = match check_command_id(&body.command_id) {
        Ok(c) => c,
        Err(e) => return (None, Err(e)),
    };
    let run = async {
        known_project(board, id).await?;
        sync_project(&board.store, &board.link, id, &principal.actor())
            .await
            .map_err(|e| match e {
                SyncError::UnknownProject => {
                    ApiError::new(StatusCode::NOT_FOUND, "unknown_project", "Unknown project.")
                }
                SyncError::NeedsRemote => ApiError::new(
                    StatusCode::CONFLICT,
                    "needs_remote",
                    "Set the forge of the project first.",
                ),
                SyncError::Store(e) => ApiError::internal(e),
            })
    };
    (Some(command), run.await)
}

/// `POST /api/projects/{id}/secret-files/sync`.
///
/// # Errors
/// 400 `invalid_body` / `invalid_command_id`; 403 `owner_only`; 404
/// `unknown_project`; 409 `needs_remote`.
pub async fn sync(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    Path(id): Path<String>,
    body: Result<Json<SyncBody>, JsonRejection>,
) -> Result<Json<SecretSyncReport>, ApiError> {
    owner_only(&principal, "syncs the secret files")?;
    let (command, result) = sync_inner(&board, &principal, &id, body).await;
    audit(&board, &principal, RUN, &id, command, &result, |report| {
        format!("{} files", report.files.len())
    })
    .await;
    result.map(Json)
}
