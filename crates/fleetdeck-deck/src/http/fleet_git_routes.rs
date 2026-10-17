//! The Fleet Git routes of the board (FG-5, `display_routes` pattern): one
//! `routes()` that `mod.rs` merges before the guard layer. The handlers are in
//! [`super::project_home_routes`] and [`super::project_remote_routes`].

use std::sync::Arc;

use axum::Router;
use axum::routing::{delete, post, put};

use super::{Board, project_home_routes, project_remote_routes};

/// The routes of this part; the caller adds the guard.
pub fn routes() -> Router<Arc<Board>> {
    Router::new()
        .route(
            "/api/projects/{project}/homes",
            post(project_home_routes::add),
        )
        .route(
            "/api/projects/{project}/homes/{station}/retry",
            post(project_home_routes::retry),
        )
        .route(
            "/api/projects/{project}/homes/{station}",
            delete(project_home_routes::remove),
        )
        .route(
            "/api/projects/{project}/remote",
            put(project_remote_routes::set),
        )
}
