//! `GET /api/features` (R13 P5a task E, design `omarchy-p5a.md` §2.5): the
//! features of this deck build, `{"features":[...]}`. Owner only. A deck
//! without the route is an old deck.

use std::sync::Arc;

use axum::Json;
use axum::Router;
use axum::extract::Extension;
use axum::routing::get;
use fleetdeck_proto::version::feature;
use serde_json::{Value, json};

use super::Board;
use super::guard::{ApiError, owner_only};
use crate::store::Principal;

/// The routes of this part; the caller adds the guard.
pub fn routes() -> Router<Arc<Board>> {
    Router::new().route("/api/features", get(features))
}

/// `GET /api/features`.
///
/// # Errors
/// 403 `owner_only`.
async fn features(Extension(principal): Extension<Principal>) -> Result<Json<Value>, ApiError> {
    owner_only(&principal, "reads the features of the deck")?;
    Ok(Json(json!({ "features": feature::ALL })))
}
