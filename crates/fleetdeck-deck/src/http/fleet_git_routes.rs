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

// The land routes (FG-8): declared here, `http/mod.rs` has no room for a line.
#[path = "land_routes.rs"]
pub mod land_routes;
// The branches of other stations (FG-10).
#[path = "project_branch_routes.rs"]
pub mod project_branch_routes;
// The radar route (FG-7), declared here for the same reason.
#[path = "radar_routes.rs"]
pub mod radar_routes;

/// The routes of this part; the caller adds the guard.
pub fn routes() -> Router<Arc<Board>> {
    Router::new()
        .route(
            "/api/sessions/{station}/{session}/land",
            post(land_routes::land),
        )
        .route(
            "/api/projects/{project}/lands",
            get(land_routes::project_lands),
        )
        .route(
            "/api/projects/{project}/branches",
            get(project_branch_routes::branches),
        )
        .route(
            "/api/projects/{project}/fetch",
            post(project_branch_routes::fetch),
        )
        .route("/api/radar", get(radar_routes::radar))
        .route("/api/lands/{land}/cancel", post(land_routes::cancel))
        .route("/api/lands/{land}/abandon", post(land_routes::abandon))
        .route("/api/lands/{land}/forget", post(land_routes::forget))
        .route("/api/lands/{land}/send", post(land_routes::send))
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
