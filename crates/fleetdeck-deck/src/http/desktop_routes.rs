//! The owner's desktop relay routes (R13 §6D items 6 to 10).
//!
//! STEP 1 STUB. The routes and their method rows (`desktop.show` and
//! `desktop.act`) are final; the handlers answer `501 not_built` until the
//! P3b relay task replaces them with calls to
//! [`crate::desktop_relay`]:
//! - `GET /api/stations/{station}/desktop` (owner, scope Read): the info;
//! - `POST /api/stations/{station}/desktop/act` (owner, scope Drive,
//!   mutating, CSRF): the body is a
//!   [`fleetdeck_proto::desktop::DesktopActBody`] and the answer a
//!   [`fleetdeck_proto::desktop::DesktopActResult`].

use std::sync::Arc;

use axum::Router;
use axum::routing::{get, post};

use super::Board;
use super::guard::ApiError;
use super::not_built::not_built;

/// The routes of this part; the caller adds the guard.
pub fn routes() -> Router<Arc<Board>> {
    Router::new()
        .route("/api/stations/{station}/desktop", get(show))
        .route("/api/stations/{station}/desktop/act", post(act))
}

async fn show() -> ApiError {
    not_built()
}

async fn act() -> ApiError {
    not_built()
}
