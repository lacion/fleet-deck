//! The remote URL of a project (design §8.4 "Change the remote", decision
//! B9; FG-5 phase 2): `PUT /api/projects/{id}/remote { remote_url, rebind,
//! command_id }` (`project.remote`). Owner's board login only, CSRF, audited.
//! The `command_id` is checked and audited, not stored: the PUT is idempotent
//! by content.

use std::sync::Arc;

use axum::Json;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Extension, Path, State};
use fleetdeck_proto::CommandId;
use serde::Deserialize;

use super::Board;
use super::guard::{ApiError, owner_only};
use super::project_home_routes::{audit, check_command_id, known_project, refusal};
use super::projects_routes::bad_body;
use crate::link::fleet_git_sync;
use crate::store::Principal;
use crate::store::project_home_ops::RemoteChanged;

const ACTION: &str = "project.remote";

/// The body of the route.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteBody {
    pub remote_url: String,
    #[serde(default)]
    pub rebind: bool,
    pub command_id: String,
}

async fn set_inner(
    board: &Board,
    id: &str,
    body: Result<Json<RemoteBody>, JsonRejection>,
) -> (Option<CommandId>, Result<RemoteChanged, ApiError>) {
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
        board
            .store
            .remote_set(id.to_owned(), body.remote_url.clone(), body.rebind)
            .await
            .map_err(ApiError::internal)?
            .map_err(refusal)
    };
    (Some(command), run.await)
}

/// `PUT /api/projects/{id}/remote`.
///
/// # Errors
/// 400 `invalid_body` / `invalid_command_id` / `invalid_git_settings` (field
/// `remote_url`); 403 `owner_only`; 404 `unknown_project`; 409
/// `rebind_needed` / `home_busy`.
pub async fn set(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    Path(id): Path<String>,
    body: Result<Json<RemoteBody>, JsonRejection>,
) -> Result<Json<RemoteChanged>, ApiError> {
    owner_only(&principal, "changes the remote")?;
    let (command, result) = set_inner(&board, &id, body).await;
    audit(&board, &principal, ACTION, &id, command, &result, |done| {
        format!("revision {}", done.git_revision)
    })
    .await;
    let changed = result?;
    // Every station with a home gets the new snapshot.
    fleet_git_sync::spawn_project(&board.link, id);
    Ok(Json(changed))
}
