//! The rows of the wall display routes and the desktop relay (R13 P2b/P3b
//! step 1; split from `methods_table_rest.rs` for the module size rule).
//!
//! The rows with `.display()` are open to a wall display; every other row
//! of this part is closed to it (`403 display_read_only`). Every
//! `/api/displays/self/...` row authenticates with `fd_display` only, also
//! when an owner cookie is present (`guard::display_route`).

use axum::http::Method;
use fleetdeck_proto::link::Scope;

use super::{ApiMethod, m};

/// The display and desktop rows; [`super::api`] joins them with the others.
pub(super) const API_DISPLAY: &[ApiMethod] = &[
    m(
        "displays.directive",
        Method::POST,
        "/api/displays/{id}/directive",
        Some(Scope::Drive),
        true,
    ),
    m(
        "displays.control",
        Method::POST,
        "/api/displays/{id}/control",
        Some(Scope::Drive),
        true,
    ),
    m(
        "displays.remote_navigate",
        Method::POST,
        "/api/displays/{id}/remote/navigate",
        Some(Scope::Drive),
        true,
    ),
    m(
        "displays.remote",
        Method::GET,
        "/api/displays/{id}/remote",
        Some(Scope::Read),
        false,
    ),
    m(
        "wall_pages.list",
        Method::GET,
        "/api/wall-pages",
        Some(Scope::Read),
        false,
    ),
    m(
        "wall_pages.add",
        Method::POST,
        "/api/wall-pages",
        Some(Scope::Drive),
        true,
    ),
    m(
        "wall_pages.delete",
        Method::POST,
        "/api/wall-pages/{id}/delete",
        Some(Scope::Drive),
        true,
    ),
    m(
        "displays.self_directive",
        Method::GET,
        "/api/displays/self/directive",
        Some(Scope::Read),
        false,
    )
    .display(),
    m(
        "displays.self_page",
        Method::GET,
        "/api/displays/self/page/{page_id}",
        Some(Scope::Read),
        false,
    )
    .display(),
    m(
        "displays.self_app_feed",
        Method::GET,
        "/api/displays/self/app-feed/{station}/{id}/{version}",
        Some(Scope::Read),
        false,
    )
    .display(),
    m(
        "displays.session_changes",
        Method::GET,
        "/api/displays/self/sessions/{station}/{session}/changes",
        Some(Scope::Read),
        false,
    )
    .display(),
    m(
        "displays.self_artifact",
        Method::GET,
        "/api/displays/self/artifact/{station}/{id}/{version}/{*path}",
        Some(Scope::Read),
        false,
    )
    .display(),
    m(
        "displays.self_palette",
        Method::GET,
        "/api/displays/self/palette",
        Some(Scope::Read),
        false,
    )
    .display(),
    m(
        "stations.palette",
        Method::GET,
        "/api/stations/{station}/palette",
        Some(Scope::Read),
        false,
    ),
    m(
        "desktop.show",
        Method::GET,
        "/api/stations/{station}/desktop",
        Some(Scope::Read),
        false,
    ),
    m(
        "desktop.act",
        Method::POST,
        "/api/stations/{station}/desktop/act",
        Some(Scope::Drive),
        true,
    ),
];
