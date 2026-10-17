//! The Fleet Git routes of the board (FG-5, `display_routes` pattern): one
//! `routes()` that `mod.rs` merges before the guard layer. The handlers are in
//! [`super::project_home_routes`], [`super::project_remote_routes`],
//! [`super::placement_routes`] (FG-6) and [`super::fleet_git_pref_routes`]
//! (FG-11).

use std::sync::Arc;

use axum::Router;
use axum::routing::{delete, get, post, put};

use super::{
    Board, fleet_git_pref_routes, placement_routes, project_home_routes, project_remote_routes,
};

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
        .route("/api/placement", post(placement_routes::placement))
        .route("/api/fleet-git/prefs", get(fleet_git_pref_routes::prefs))
        .route(
            "/api/fleet-git/drift-reference",
            put(fleet_git_pref_routes::set_reference),
        )
}
