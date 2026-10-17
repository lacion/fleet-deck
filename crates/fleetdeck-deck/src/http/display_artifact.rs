//! The display artifact route (R13 §5A item 6, §5D item 1, §5E item 3b).
//!
//! STEP 1 STUB. The route and its method row (`displays.self_artifact`) are
//! final; the handler answers `501 not_built` until the P2b artifacts task
//! replaces it. The route is `GET
//! /api/displays/self/artifact/{station}/{id}/{version}/{*path}`. It
//! authenticates with `fd_display` only, serves a version that holds exactly
//! one self-contained file, checks the ceiling and the directive on every
//! request, and sets the inert response header
//! `Content-Security-Policy: sandbox; default-src 'none'; img-src data:;
//! style-src 'unsafe-inline'; media-src data:; font-src data:`.

use std::sync::Arc;

use axum::Router;
use axum::routing::get;

use super::Board;
use super::guard::ApiError;
use super::not_built::not_built;

/// The routes of this part; the caller adds the guard.
pub fn routes() -> Router<Arc<Board>> {
    Router::new().route(
        "/api/displays/self/artifact/{station}/{id}/{version}/{*path}",
        get(artifact),
    )
}

async fn artifact() -> ApiError {
    not_built()
}
