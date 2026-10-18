//! The tail of the method table (ADR-004 §9.6): the desktop, display,
//! coordinator and Fleet Git rows. Moved from `methods_table.rs` for the
//! module size rule (R13 P6 F0); the order of the rows is unchanged.

use fleetdeck_proto::link::Scope;
use fleetdeck_proto::version::feature;

use crate::methods::{Callers, MethodSpec};
use crate::methods_table::m;

/// A deck-to-station method of `fleet_git.move.v1` (FG-9).
const fn mv(name: &'static str, scope: Scope, mutating: bool) -> MethodSpec {
    m(
        name,
        Callers::DeckToStation,
        scope,
        Some(feature::FLEET_GIT_MOVE),
        mutating,
    )
}

/// A row of feature `desktop_shell.v1` (R13 P4): never mutating.
const fn shell_row(name: &'static str, callers: Callers, scope: Scope) -> MethodSpec {
    m(name, callers, scope, Some(feature::DESKTOP_SHELL), false)
}

/// A deck-to-station `admin` row of R13 P5a: never mutating at the link
/// (the deck keeps its own durable record).
const fn p5a_row(name: &'static str, feature: &'static str) -> MethodSpec {
    m(name, DeckToStation, Scope::Admin, Some(feature), false)
}

use Callers::{DeckToStation, Either, StationToDeck};

/// The last rows of [`crate::methods::METHODS`].
pub(crate) const TAIL: [MethodSpec; 47] = [
    // Desktop control on the station (R13 P3): the board gates it for the
    // owner; the station checks `allow_desktop` live.
    m(
        "desktop.info",
        DeckToStation,
        Scope::Read,
        Some(feature::DESKTOP),
        false,
    ),
    // The deck authorizes each action: the row needs `desktop_authority.v1`
    // (R13 §5E item 1).
    m(
        "desktop.act",
        DeckToStation,
        Scope::Drive,
        Some(feature::DESKTOP_AUTHORITY),
        false,
    ),
    // R13 P5a: the screenshot result lookup and artifact deletion.
    p5a_row("desktop.capture_status", feature::DESKTOP_CAPTURE),
    p5a_row("artifacts.delete", feature::ARTIFACTS_DELETE),
    // R13 P5b (`desktop_recording.v1`, no envelope, none mutating): the
    // active recording, the owner stop and the live policy query go to the
    // station; the continuation lease comes from it.
    p5a_row("desktop.capture_active", feature::DESKTOP_RECORDING),
    p5a_row("desktop.capture_stop", feature::DESKTOP_RECORDING),
    m(
        "desktop.capture_policy_now",
        DeckToStation,
        Scope::Read,
        Some(feature::DESKTOP_RECORDING),
        false,
    ),
    m(
        "desktop.capture_lease",
        StationToDeck,
        Scope::Read,
        Some(feature::DESKTOP_RECORDING),
        false,
    ),
    // Protected change statistics and patches for a wall display (R13 §5D).
    m(
        "files.display_changes",
        DeckToStation,
        Scope::Read,
        Some(feature::DISPLAY_CHANGES),
        false,
    ),
    // The Omarchy desktop shell (R13 P4, D1, D2). The two station-to-deck
    // rows use `admin`, which every paired station holds; the deck handler
    // authorizes by the caller's own station identity (revision 12c).
    shell_row("fleet.summary", DeckToStation, Scope::Read),
    shell_row("display.directive", StationToDeck, Scope::Admin),
    shell_row("station.palette", StationToDeck, Scope::Admin),
    m("approval.answer", DeckToStation, Scope::Approve, None, true),
    m("station.configure", DeckToStation, Scope::Admin, None, true),
    m("link.rotate_key", Either, Scope::Admin, None, true),
    m(
        "coordinator.query",
        StationToDeck,
        Scope::Coordinate,
        Some(feature::COORDINATOR),
        false,
    ),
    m(
        "coordinator.call",
        StationToDeck,
        Scope::Coordinate,
        Some(feature::COORDINATOR),
        true,
    ),
    // ADR-008 §6.2: the target station asks the deck whether the authority of
    // a routed command still holds, and the deck asks the caller station for
    // its live policy.
    m(
        "coordinator.authority_check",
        StationToDeck,
        Scope::Coordinate,
        Some(feature::COORDINATOR_V2),
        false,
    ),
    m(
        "coordinator.policy_now",
        DeckToStation,
        Scope::Read,
        Some(feature::COORDINATOR_V2),
        false,
    ),
    // R13 P6 (`desktop_mcp.v1`): the coordinator desktop tools. The call is
    // mutating like `coordinator.call`; the query, the dispatch authority
    // check, the live target policy and the protected read are not.
    m(
        "coordinator.desktop_call",
        StationToDeck,
        Scope::Coordinate,
        Some(feature::DESKTOP_MCP),
        true,
    ),
    m(
        "coordinator.desktop_query",
        StationToDeck,
        Scope::Coordinate,
        Some(feature::DESKTOP_MCP),
        false,
    ),
    m(
        "desktop.authority_check",
        StationToDeck,
        Scope::Coordinate,
        Some(feature::DESKTOP_MCP),
        false,
    ),
    m(
        "desktop.policy_now",
        DeckToStation,
        Scope::Read,
        Some(feature::DESKTOP_MCP),
        false,
    ),
    m(
        "desktop.coordinator_read",
        DeckToStation,
        Scope::Read,
        Some(feature::DESKTOP_MCP),
        false,
    ),
    // Fleet Git phase 1 (ADR-009): the versioned full snapshot of the
    // project settings and homes, and "Push now" for one session.
    m(
        "fleet_git.projects_set",
        DeckToStation,
        Scope::Admin,
        Some(feature::FLEET_GIT),
        false,
    ),
    m(
        "fleet_git.push_now",
        DeckToStation,
        Scope::Drive,
        Some(feature::FLEET_GIT),
        false,
    ),
    // Fleet Git phase 2 (ADR-009): homes on many stations and the artifact
    // share verdicts.
    m(
        "project.ensure_home",
        DeckToStation,
        Scope::Admin,
        Some(feature::FLEET_GIT_HOMES),
        false,
    ),
    m(
        "project.home_status",
        DeckToStation,
        Scope::Read,
        Some(feature::FLEET_GIT_HOMES),
        false,
    ),
    m(
        "fleet_git.artifact_authorize",
        StationToDeck,
        // A paired station holds `admin` only (STATION_SCOPES, DL-050).
        Scope::Admin,
        Some(feature::FLEET_GIT_HOMES),
        false,
    ),
    m(
        "fleet_git.share_verdict",
        DeckToStation,
        Scope::Read,
        Some(feature::FLEET_GIT_HOMES),
        false,
    ),
    // Fleet Git phase 3 (ADR-009): the conflict radar and the land flow.
    m(
        "project.radar",
        DeckToStation,
        Scope::Read,
        Some(feature::FLEET_GIT_LAND),
        false,
    ),
    m(
        "session.land",
        DeckToStation,
        Scope::Drive,
        Some(feature::FLEET_GIT_LAND),
        true,
    ),
    m(
        "fleet_git.land_status",
        DeckToStation,
        Scope::Read,
        Some(feature::FLEET_GIT_LAND),
        false,
    ),
    m(
        "fleet_git.land_abandon",
        DeckToStation,
        Scope::Drive,
        Some(feature::FLEET_GIT_LAND),
        false,
    ),
    // Fleet Git phase 4 (ADR-009): the branches of other stations.
    m(
        "project.branches",
        DeckToStation,
        Scope::Read,
        Some(feature::FLEET_GIT_BRANCHES),
        false,
    ),
    m(
        "project.fetch",
        DeckToStation,
        Scope::Read,
        Some(feature::FLEET_GIT_BRANCHES),
        false,
    ),
    // Fleet Git phase 4 (ADR-009): move a session to another station.
    mv("session.move_out", Scope::Admin, true),
    mv("session.move_export", Scope::Admin, false),
    mv("session.move_prepare", Scope::Admin, false),
    mv("session.move_in", Scope::Admin, true),
    mv("session.move_finish", Scope::Admin, true),
    mv("session.move_release", Scope::Admin, false),
    mv("session.move_abort", Scope::Admin, false),
    mv("session.move_discard", Scope::Admin, false),
    mv("session.move_status", Scope::Read, false),
    mv("session.move_fail", Scope::Admin, false),
    mv("session.move_undo", Scope::Admin, true),
];
