//! The Fleet Git settings of a project and the start in a sync home (design
//! §3.1, §3.8, §8.5, §17.1; phase 1).
//!
//! - `PUT /api/projects/{id}/git` (owner login, CSRF, audited
//!   `project.git_set`): saves the settings of the project with the optimistic
//!   `revision`, then sends the snapshot to the station of its home.
//! - [`attach_start`]: the `FleetGitStart` of a start in the verified home of
//!   a sync project (the board start and the batch start).

use std::sync::Arc;

use axum::Json;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Extension, Path, State};
use axum::http::StatusCode;
use fleetdeck_proto::StationId;
use fleetdeck_proto::fleet_git::{
    FleetGitStart, GitSettingsAnswer, GitSettingsPut, SettingsError, StartKind, check_settings,
    code, station_slug,
};
use fleetdeck_proto::version::feature;

use super::Board;
use super::artifact_share_check::check_share;
use super::guard::{ApiError, owner_only};
use super::projects_routes::bad_body;
use crate::link::fleet_git_sync;
use crate::store::fleet_git_start::{StartOutcome, StartQuery};
use crate::store::projects_git::GitWrite;
use crate::store::{AuditResult, NewAudit, Principal, StationRecord};

const ACTION: &str = "project.git_set";

/// The 400 of a refused setting, with the struct field name at the top level.
fn invalid(e: &SettingsError) -> ApiError {
    let mut details = serde_json::Map::new();
    details.insert("field".to_owned(), e.field.clone().into());
    ApiError::new(
        StatusCode::BAD_REQUEST,
        code::INVALID_GIT_SETTINGS,
        e.message.clone(),
    )
    .with_details(details)
}

async fn save(
    board: &Board,
    id: &str,
    body: Result<Json<GitSettingsPut>, JsonRejection>,
) -> Result<GitSettingsAnswer, ApiError> {
    let Json(put) = body.map_err(|e| bad_body(&e))?;
    // A new `artifact_share` that the exporting station reports as exposed or
    // unverified is refused (FG-12, decision D8): after the 404 and the
    // settings rules, before the revision check.
    if let Some(share) = &put.settings.artifact_share {
        let Some(stored) = board
            .store
            .project(id.to_owned())
            .await
            .map_err(ApiError::internal)?
        else {
            return Err(ApiError::new(
                StatusCode::NOT_FOUND,
                "unknown_project",
                format!("no project `{id}`"),
            ));
        };
        if stored.git.artifact_share.as_ref() != Some(share) {
            check_settings(&put.settings).map_err(|e| invalid(&e))?;
            check_share(board, share).await?;
        }
    }
    match board
        .store
        .set_project_git(id.to_owned(), put)
        .await
        .map_err(ApiError::internal)?
    {
        GitWrite::Saved(answer) => Ok(*answer),
        GitWrite::UnknownProject => Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "unknown_project",
            format!("no project `{id}`"),
        )),
        GitWrite::Stale => Err(ApiError::new(
            StatusCode::CONFLICT,
            "stale_revision",
            "The Git settings changed. Reload and try again.",
        )),
        GitWrite::RebindRequired => Err(ApiError::new(
            StatusCode::CONFLICT,
            "rebind_required",
            "Turn off sync to change the remote URL.",
        )),
        GitWrite::Invalid(e) => Err(invalid(&e)),
    }
}

/// `PUT /api/projects/{id}/git`.
///
/// # Errors
/// 400 `invalid_body` / `invalid_git_settings` (with `field`); 403
/// `owner_only`; 404 `unknown_project`; 409 `stale_revision` /
/// `rebind_required`.
pub async fn put_git(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    Path(id): Path<String>,
    body: Result<Json<GitSettingsPut>, JsonRejection>,
) -> Result<Json<GitSettingsAnswer>, ApiError> {
    owner_only(&principal, "changes the Git settings")?;
    let result = save(&board, &id, body).await;
    let row = match &result {
        Ok(answer) => NewAudit::new(principal.actor(), ACTION, AuditResult::Ok)
            .target(id.clone())
            .detail(format!("revision {}", answer.git_revision)),
        Err(e) => NewAudit::new(principal.actor(), ACTION, AuditResult::Refused)
            .target(id.clone())
            .detail(e.code),
    };
    board.store.audit_or_log(row).await;
    let answer = result?;
    // Every save sends the snapshot (the revision grew).
    fleet_git_sync::spawn_project(&board.link, id);
    Ok(Json(answer))
}

/// The `FleetGitStart` of a start on `rec` in `cwd` (design §2.1, §8.5), or
/// `None` for a plain start (O1). `project` names the project of a batch
/// start. `Err` is the 409 of a home that cannot start now.
///
/// # Errors
/// 409 `fleet_git_unsupported` / `station_info_missing` / `home_unverified` /
/// `home_not_ready` / `slug_taken`.
pub(super) async fn attach_start(
    board: &Board,
    rec: &StationRecord,
    cwd: &str,
    kind: StartKind,
    project: Option<String>,
) -> Result<Option<Box<FleetGitStart>>, ApiError> {
    let station: &StationId = &rec.station_id;
    let query = StartQuery {
        station: station.0.clone(),
        cwd: cwd.to_owned(),
        kind,
        slug: board
            .store
            .station_info(station)
            .map(|(info, _)| station_slug(&info.name)),
        feature: board.store.link_feature(station, feature::FLEET_GIT),
        project,
    };
    let conflict = |code, message: String| ApiError::new(StatusCode::CONFLICT, code, message);
    match board
        .store
        .git_start(query)
        .await
        .map_err(ApiError::internal)?
    {
        StartOutcome::Plain => Ok(None),
        StartOutcome::Attach(start) => Ok(Some(start)),
        StartOutcome::Unsupported => Err(conflict(
            code::FLEET_GIT_UNSUPPORTED,
            "This station does not support Fleet Git. Update it first.".to_owned(),
        )),
        StartOutcome::StationInfoMissing => Err(conflict(
            "station_info_missing",
            "The deck does not know this station's name yet. Try again in a moment.".to_owned(),
        )),
        StartOutcome::Unverified { path, repo } => Err(conflict(
            "home_unverified",
            format!(
                "The home {path} on {} does not point to {repo}. Fix its remote, then try again.",
                rec.name
            ),
        )),
        StartOutcome::NotReady(text) => Err(conflict(code::HOME_NOT_READY, text)),
        StartOutcome::SlugTaken { repo, slug, other } => Err(conflict(
            "slug_taken",
            format!(
                "The Git name {slug} is reserved for station {other} in {repo}. Rename this station."
            ),
        )),
    }
}
