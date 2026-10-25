//! The rows of the setup report routes (BG-3, design
//! `docs/v2/design/board-gaps.md` section 4.4): `admin`, and the handlers
//! also require the owner's board login (DL-138). The window and the retry
//! permission also need the owner's elevation (checked in the handler).

use axum::http::Method;
use fleetdeck_proto::link::Scope;

use super::{ApiMethod, m};

/// The setup report rows; [`super::api`] joins them with the others.
pub(super) const API_REPORTS: &[ApiMethod] = &[
    m(
        "provision_reports.list",
        Method::GET,
        "/api/provision-reports",
        Some(Scope::Admin),
        false,
    ),
    m(
        "provision_reports.window",
        Method::POST,
        "/api/provision-reports/window",
        Some(Scope::Admin),
        true,
    ),
    m(
        "provision_reports.retry",
        Method::POST,
        "/api/provision-reports/{fingerprint}/retry",
        Some(Scope::Admin),
        true,
    ),
    m(
        "provision_reports.retry_cancel",
        Method::DELETE,
        "/api/provision-reports/{fingerprint}/retry",
        Some(Scope::Admin),
        true,
    ),
    m(
        "provision_reports.dismiss",
        Method::DELETE,
        "/api/provision-reports/{fingerprint}",
        Some(Scope::Admin),
        true,
    ),
];
