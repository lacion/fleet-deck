//! The owner routes of the wall pages (B17 §3.2.4):
//! - `GET /api/wall-pages` (owner, scope Read) lists the allow-list, oldest
//!   first;
//! - `POST /api/wall-pages` (owner, scope Drive, at most 4 KiB, a fresh
//!   elevation) adds an entry;
//! - `POST /api/wall-pages/{id}/delete` (owner, scope Drive) deletes an entry
//!   and resets, in one transaction, the displays that show it.
//!
//! No row is open to a display. The audit rows hold the page id and the host,
//! never the path or the query of the address (they can hold a token).

use std::sync::Arc;

use axum::extract::rejection::JsonRejection;
use axum::extract::{DefaultBodyLimit, Extension, Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use fleetdeck_proto::wall_pages::{MAX_WALL_PAGES, NewWallPage, PageFit, WallPage, WallPageList};
use serde::Deserialize;
use serde_json::{Value, json};

use super::Board;
use super::account_routes::body_error;
use super::guard::ApiError;
use super::settings_routes::{is_elevated, owner};
use crate::store::auth::LoginSession;
use crate::store::wall_pages::{check_page, origin_host};
use crate::store::{AuditResult, NewAudit, Principal};

const ADD: &str = "wall_page.add";
const DELETE: &str = "wall_page.delete";
const LIST: &str = "wall_page.list";
/// The largest body of an add, in bytes.
const MAX_ADD_BODY: usize = 4096;

/// The routes of this part; the caller adds the guard.
pub fn routes() -> Router<Arc<Board>> {
    Router::new()
        .route(
            "/api/wall-pages",
            get(list)
                .post(add)
                .layer(DefaultBodyLimit::max(MAX_ADD_BODY)),
        )
        .route("/api/wall-pages/{id}/delete", post(delete))
}

fn invalid_page(text: &'static str) -> ApiError {
    ApiError::new(StatusCode::BAD_REQUEST, "invalid_page", text)
}

fn too_many_pages() -> ApiError {
    ApiError::new(
        StatusCode::CONFLICT,
        "too_many_pages",
        format!("A deck holds at most {MAX_WALL_PAGES} wall pages."),
    )
}

fn unknown_page() -> ApiError {
    ApiError::new(StatusCode::NOT_FOUND, "unknown_page", "No such page.")
}

/// 400 `elevation_required` (`keys: ["wall_pages"]`), the same shape as the
/// display routes.
fn elevation_required() -> ApiError {
    let mut keys = serde_json::Map::new();
    keys.insert("keys".to_owned(), json!(["wall_pages"]));
    ApiError::new(
        StatusCode::BAD_REQUEST,
        "elevation_required",
        "a recent sign-in is needed to add a wall page",
    )
    .with_details(keys)
}

/// `GET /api/wall-pages`.
///
/// # Errors
/// 403 `owner_only`.
async fn list(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
) -> Result<Json<WallPageList>, ApiError> {
    owner(&board, &principal, LIST, "", "reads the wall pages").await?;
    let pages = board.store.wall_pages().await.map_err(ApiError::internal)?;
    Ok(Json(WallPageList { pages }))
}

/// The body of an add: the fields of [`NewWallPage`] and the elevation (the
/// same way as "Add a wall display").
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AddBody {
    label: String,
    url: String,
    fit: PageFit,
    #[serde(default)]
    elevation: Option<String>,
}

/// `POST /api/wall-pages { label, url, fit, elevation }`.
///
/// # Errors
/// 400 `invalid_body`, `invalid_page`, `elevation_required`; 403
/// `owner_only`; 409 `too_many_pages`; 413 `too_large`.
async fn add(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    body: Result<Json<AddBody>, JsonRejection>,
) -> Result<Json<WallPage>, ApiError> {
    let session = owner(&board, &principal, ADD, "", "adds a wall page").await?;
    let Json(body) = body.map_err(|e| body_error(&e))?;
    let result = add_page(&board, &session, body).await;
    let row = match &result {
        Ok(page) => NewAudit::new("owner", ADD, AuditResult::Ok)
            .target(page.page_id.as_str())
            .detail(format!(
                "page={} host={}",
                page.page_id,
                origin_host(&page.origin)
            )),
        Err(e) => NewAudit::new("owner", ADD, AuditResult::Failed).detail(e.code),
    };
    board.store.audit_or_log(row).await;
    result.map(Json)
}

/// The checks in order: the entry (400 with its text), then the elevation,
/// then the limit of 50 (409).
async fn add_page(
    board: &Board,
    session: &LoginSession,
    body: AddBody,
) -> Result<WallPage, ApiError> {
    let new = NewWallPage {
        label: body.label,
        url: body.url,
        fit: body.fit,
    };
    let checked = check_page(new, |origin| board.origins.allows(origin)).map_err(invalid_page)?;
    if !is_elevated(board, session, body.elevation.as_deref()) {
        return Err(elevation_required());
    }
    board
        .store
        .wall_page_add(checked)
        .await
        .map_err(ApiError::internal)?
        .ok_or_else(too_many_pages)
}

/// `POST /api/wall-pages/{id}/delete`: answers `{ "displays_reset": n }`.
///
/// # Errors
/// 403 `owner_only`; 404 `unknown_page`.
async fn delete(
    State(board): State<Arc<Board>>,
    Extension(principal): Extension<Principal>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    owner(&board, &principal, DELETE, &id, "deletes a wall page").await?;
    let result = board
        .store
        .wall_page_delete(id.clone())
        .await
        .map_err(ApiError::internal)
        .and_then(|reset| reset.ok_or_else(unknown_page));
    let row = match &result {
        Ok(n) => NewAudit::new("owner", DELETE, AuditResult::Ok)
            .target(id.as_str())
            .detail(format!("page={id} displays_reset={n}")),
        Err(e) => NewAudit::new("owner", DELETE, AuditResult::Failed)
            .target(id.as_str())
            .detail(e.code),
    };
    board.store.audit_or_log(row).await;
    result.map(|n| Json(json!({ "displays_reset": n })))
}
