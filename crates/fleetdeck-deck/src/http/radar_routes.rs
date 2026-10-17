//! `GET /api/radar` (design §10.5, §8.6; FG-7): the conflict radar by project.
//! `GET /api/conflicts` stays as it is.

use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use serde_json::{Value, json};

use crate::http::Board;
use crate::http::guard::ApiError;

/// `GET /api/radar`: `{ groups: [RadarGroup], notices: [{project_id, code,
/// text}] }`. The notices are the last run results (memory) and the offline
/// lines (computed now).
///
/// # Errors
/// 500 `internal` for a store error.
pub async fn radar(State(board): State<Arc<Board>>) -> Result<Json<Value>, ApiError> {
    let groups = board
        .store
        .radar_groups()
        .await
        .map_err(ApiError::internal)?;
    let mut notices = board.link.radar_state.notices();
    notices.extend(
        board
            .store
            .radar_offline()
            .await
            .map_err(ApiError::internal)?,
    );
    Ok(Json(json!({ "groups": groups, "notices": notices })))
}
