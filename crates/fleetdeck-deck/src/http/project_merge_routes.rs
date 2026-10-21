//! Merge into project (Fleet Sync §2): `POST /api/projects/{id}/merge
//! { from, command_id }` (`project.merge`). Owner's board login only, CSRF,
//! audited. The `command_id` is checked and audited, not stored: a repeat
//! after success answers 404 `unknown_project` for `from`.

use std::sync::Arc;

use axum::Json;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Extension, Path, State};
use fleetdeck_proto::CommandId;
use fleetdeck_proto::integrations::Project;
use serde::Deserialize;

use crate::http::Board;
use crate::http::guard::{ApiError, owner_only};
use crate::http::project_home_routes::{audit, check_command_id, known_project, refusal};
use crate::http::projects_routes::bad_body;
use crate::link::fleet_git_sync;
use crate::store::Principal;

const ACTION: &str = "project.merge";

/// The body of the route.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MergeBody {
    pub from: String,
    pub command_id: String,
}

async fn merge_inner(
    board: &Board,
    id: &str,
    body: Result<Json<MergeBody>, JsonRejection>,
) -> (Option<CommandId>, Result<(Project, usize), ApiError>) {
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
            .project_merge(id.to_owned(), body.from.clone())
            .await
            .map_err(ApiError::internal)?
            .map_err(refusal)
    };
    (Some(command), run.await)
}

/// `POST /api/projects/{id}/merge`.
///
/// # Errors
/// 400 `invalid_body` / `invalid_command_id` / `needs_remote`; 403
/// `owner_only`; 404 `unknown_project`; 409 `not_same_repo` /
/// `merge_sync_on` / `home_exists` / `home_busy` / `slug_taken`.
pub async fn merge(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    Path(id): Path<String>,
    body: Result<Json<MergeBody>, JsonRejection>,
) -> Result<Json<Project>, ApiError> {
    owner_only(&principal, "merges projects")?;
    let from = body.as_ref().ok().map(|b| b.from.clone());
    let (command, result) = merge_inner(&board, &id, body).await;
    audit(
        &board,
        &principal,
        ACTION,
        &id,
        command,
        &result,
        |(_, n)| format!("from {} homes {n}", from.as_deref().unwrap_or_default()),
    )
    .await;
    let (project, _) = result?;
    // Every station of the target gets the new snapshot.
    fleet_git_sync::spawn_project(&board.link, id);
    Ok(Json(project))
}
