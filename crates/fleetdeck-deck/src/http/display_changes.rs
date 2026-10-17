//! The change summary route of a wall display (R13 §5A item 5, §5C item 3,
//! §5D items 13 and 14).
//!
//! STEP 1 STUB. The route and its method row (`displays.session_changes`)
//! are final; the handler answers `501 not_built` until the P2b changes task
//! replaces it. The route is `GET
//! /api/displays/self/sessions/{station}/{session}/changes` with `?patch=1`;
//! the answer is a [`fleetdeck_proto::display_changes::DisplayChanges`]
//! from the station method `files.display_changes` (feature
//! `display_changes.v1`).

use std::sync::Arc;

use axum::Router;
use axum::routing::get;

use super::Board;
use super::guard::ApiError;
use super::not_built::not_built;

/// The routes of this part; the caller adds the guard.
pub fn routes() -> Router<Arc<Board>> {
    Router::new().route(
        "/api/displays/self/sessions/{station}/{session}/changes",
        get(changes),
    )
}

async fn changes() -> ApiError {
    not_built()
}
