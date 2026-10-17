//! The directive routes of a wall display (R13 §5D items 3 to 5).
//!
//! STEP 1 STUB. The routes and their method rows are final
//! (`displays.directive` and `displays.self_directive`); the handlers answer
//! `501 not_built` until the P2b directives task replaces them:
//! - `POST /api/displays/{id}/directive` (owner, scope Drive, body
//!   [`fleetdeck_proto::display_directive::DirectiveBody`], at most 8 KiB);
//! - `GET /api/displays/self/directive` (a display login only; the answer is
//!   a [`fleetdeck_proto::display_directive::DirectiveView`]).

use std::sync::Arc;

use axum::Router;
use axum::routing::{get, post};

use super::Board;
use super::guard::ApiError;
use super::not_built::not_built;

/// The routes of this part; the caller adds the guard.
pub fn routes() -> Router<Arc<Board>> {
    Router::new()
        .route("/api/displays/{id}/directive", post(set))
        .route("/api/displays/self/directive", get(read))
}

async fn set() -> ApiError {
    not_built()
}

async fn read() -> ApiError {
    not_built()
}
