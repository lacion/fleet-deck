//! The board-API methods of the homes of a project (Fleet Git, FG-5): a
//! third part of the method table (split for the module size rule);
//! [`super::api`] joins it with the others. Each method is `admin`, mutating
//! (CSRF, audit), and its handler requires the owner's board login, so a
//! member and a token are refused (design §8.4).

use axum::http::Method;
use fleetdeck_proto::link::Scope;

use super::{ApiMethod, m};

/// The home methods of the board-API method table.
pub const API_GIT: &[ApiMethod] = &[
    m(
        "projects.home_add",
        Method::POST,
        "/api/projects/{project}/homes",
        Some(Scope::Admin),
        true,
    ),
    m(
        "projects.home_retry",
        Method::POST,
        "/api/projects/{project}/homes/{station}/retry",
        Some(Scope::Admin),
        true,
    ),
    m(
        "projects.home_remove",
        Method::DELETE,
        "/api/projects/{project}/homes/{station}",
        Some(Scope::Admin),
        true,
    ),
    m(
        "projects.remote_set",
        Method::PUT,
        "/api/projects/{project}/remote",
        Some(Scope::Admin),
        true,
    ),
    // FG-6: placement; `drive`, not mutating, not audited, closed to members.
    m(
        "placement.place",
        Method::POST,
        "/api/placement",
        Some(Scope::Drive),
        false,
    ),
    // FG-8: the land flow. The POST routes are `admin` and owner only.
    m(
        "sessions.land",
        Method::POST,
        "/api/sessions/{station}/{session}/land",
        Some(Scope::Admin),
        true,
    ),
    m(
        "projects.lands",
        Method::GET,
        "/api/projects/{project}/lands",
        Some(Scope::Read),
        false,
    ),
    m(
        "lands.cancel",
        Method::POST,
        "/api/lands/{land}/cancel",
        Some(Scope::Admin),
        true,
    ),
    m(
        "lands.abandon",
        Method::POST,
        "/api/lands/{land}/abandon",
        Some(Scope::Admin),
        true,
    ),
    m(
        "lands.forget",
        Method::POST,
        "/api/lands/{land}/forget",
        Some(Scope::Admin),
        true,
    ),
    m(
        "lands.send",
        Method::POST,
        "/api/lands/{land}/send",
        Some(Scope::Admin),
        true,
    ),
    // FG-7: the radar groups and notices; `read`, closed to members.
    m(
        "radar.list",
        Method::GET,
        "/api/radar",
        Some(Scope::Read),
        false,
    ),
    // FG-11: the drift reference station.
    m(
        "fleet_git.prefs",
        Method::GET,
        "/api/fleet-git/prefs",
        Some(Scope::Read),
        false,
    ),
    m(
        "fleet_git.drift_reference",
        Method::PUT,
        "/api/fleet-git/drift-reference",
        Some(Scope::Admin),
        true,
    ),
];
