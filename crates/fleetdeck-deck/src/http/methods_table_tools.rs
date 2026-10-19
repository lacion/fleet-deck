//! The rows of the desktop tools routes and the maintenance channel (BG-1,
//! design `docs/v2/design/board-gaps.md` section 2.9): `admin`, and the
//! handlers also require the owner's board login. The install (the open of
//! a maintenance run) also needs the owner's elevation, checked in the
//! handler.

use axum::http::Method;
use fleetdeck_proto::link::Scope;

use super::{ApiMethod, MAINTENANCE_WS_METHOD, m};

/// The desktop tools rows; [`super::api`] joins them with the others.
pub(super) const API_TOOLS: &[ApiMethod] = &[
    m(
        "station.desktop_tools",
        Method::GET,
        "/api/stations/{station}/desktop-tools",
        Some(Scope::Admin),
        false,
    ),
    m(
        "station.desktop_tools_plan",
        Method::GET,
        "/api/stations/{station}/desktop-tools/plan",
        Some(Scope::Admin),
        false,
    ),
    m(
        "station.maintenance_open",
        Method::POST,
        "/api/stations/{station}/maintenance",
        Some(Scope::Admin),
        true,
    ),
    m(
        "station.maintenance_list",
        Method::GET,
        "/api/stations/{station}/maintenance",
        Some(Scope::Admin),
        false,
    ),
    m(
        "station.maintenance_resize",
        Method::POST,
        "/api/stations/{station}/maintenance/{id}/resize",
        Some(Scope::Admin),
        true,
    ),
    m(
        MAINTENANCE_WS_METHOD,
        Method::GET,
        "/api/stations/{station}/maintenance/{id}/attach",
        Some(Scope::Admin),
        false,
    ),
];
