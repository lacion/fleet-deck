use super::*;

#[test]
fn names_and_routes_are_unique_and_only_login_is_public() {
    let all: Vec<&ApiMethod> = api().collect();
    for (i, a) in all.iter().enumerate() {
        for b in &all[i + 1..] {
            assert_ne!(a.name, b.name);
            assert!(a.method != b.method || a.path != b.path, "{}", a.name);
        }
        assert_eq!(
            a.scope.is_none(),
            matches!(
                a.name,
                "auth.login"
                    | "auth.link"
                    | "auth.join"
                    | "auth.display_link"
                    | "displays.pair_start"
                    | "displays.pair_poll"
            ),
            "{}",
            a.name
        );
    }
    assert_eq!(
        find(&Method::GET, "/api/audit").map(|m| m.scope),
        Some(Some(Scope::Admin))
    );
    assert_eq!(find(&Method::POST, "/api/audit"), None);
    assert_eq!(
        find(&Method::GET, "/api/fleet/extras").map(|m| (m.name, m.scope, m.mutating)),
        Some(("fleet.extras", Some(Scope::Read), false))
    );
    assert_eq!(
        find(&Method::POST, "/api/commands/approval.answer").map(|m| m.scope),
        Some(Some(Scope::Approve))
    );
    assert_eq!(
        find(&Method::GET, "/api/commands/{command_id}").map(|m| m.name),
        Some("commands.get")
    );
    assert_eq!(find(&Method::POST, "/api/commands/session.nope"), None);
}

/// The member column (R11, DL-166): exactly these methods are open to a
/// member of a shared session, and each of their handlers checks the share.
/// A new method is owner-only until it is added here on purpose.
#[test]
fn exactly_these_methods_are_open_to_a_member() {
    let mut open: Vec<&str> = api().filter(|a| a.member).map(|a| a.name).collect();
    open.sort_unstable();
    assert_eq!(
        open,
        [
            "approval.answer",
            "auth.logout",
            "auth.session",
            "board.ws",
            "board.ws_ticket",
            "chat.list",
            "chat.post",
            "commands.get",
            "drive.grant",
            "drive.release",
            "drive.request",
            "files.diff",
            "files.list",
            "files.read",
            "files.search",
            "files.turn_diff",
            "fleet.extras",
            "mail.send",
            "review.comment",
            "review.delete",
            "review.edit",
            "review.list",
            "review.retry",
            "review.send",
            "session.follow_up",
            "session.interrupt",
            "session.prompt",
            "session.steer",
            "sessions.history",
            "sessions.list",
            "sessions.workflows",
            "stations.list",
        ]
    );
    // The login methods need no principal at all.
    for name in [
        "shares.create",
        "shares.revoke",
        "spaces.create",
        "shells.command",
    ] {
        assert!(!api().find(|a| a.name == name).unwrap().member, "{name}");
    }
}

/// The display column (R13 §5, DL-201): exactly these methods are open to a
/// wall display. A new method is closed to a display until it is added here on
/// purpose. The whole-table refusal of every other row is tested in
/// `tests/board_displays.rs`.
#[test]
fn exactly_these_methods_are_open_to_a_display() {
    let mut open: Vec<&str> = api().filter(|a| a.display).map(|a| a.name).collect();
    open.sort_unstable();
    assert_eq!(
        open,
        [
            "activity.list",
            "auth.logout",
            "auth.session",
            "board.ws",
            "board.ws_ticket",
            "displays.self",
            "displays.self_artifact",
            "displays.self_directive",
            "displays.self_page",
            "displays.self_palette",
            "displays.session_changes",
            "fleet.extras",
            "sessions.list",
            "stations.list",
        ]
    );
    // A display never writes, except the logout of its own login and the
    // WebSocket ticket (a ticket reads nothing and changes no board state).
    for a in api().filter(|a| a.display && a.mutating) {
        assert!(
            matches!(a.name, "auth.logout" | "board.ws_ticket"),
            "{}",
            a.name
        );
    }
}

#[test]
fn every_command_method_has_the_scope_of_its_link_method() {
    let mut n = 0;
    for api in command_methods() {
        let link = fleetdeck_link::methods::method(api.name)
            .unwrap_or_else(|| panic!("{} is not a link method", api.name));
        assert_eq!(api.scope, Some(link.scope), "{}", api.name);
        assert!(api.mutating);
        n += 1;
    }
    assert_eq!(n, 20);
}

/// The settings, secret relay, restart and elevation routes (DL-172) are
/// `admin` and closed to a member; their handlers also refuse a token.
#[test]
fn the_settings_routes_are_admin_and_owner_only() {
    let names = [
        "settings.deck_get",
        "settings.deck_set",
        "settings.station_get",
        "settings.station_set",
        "settings.station_secret_set",
        "settings.restart",
        "auth.elevate",
    ];
    for name in names {
        let api = api().find(|a| a.name == name).unwrap();
        assert_eq!(api.scope, Some(Scope::Admin), "{name}");
        assert!(!api.member, "{name}");
        assert_eq!(api.mutating, api.method == Method::POST, "{name}");
    }
    assert_eq!(api().filter(|a| a.name.starts_with("settings.")).count(), 6);
    assert_eq!(
        find(&Method::POST, "/api/secrets/stations/{station}/settings").map(|m| m.name),
        Some("settings.station_secret_set")
    );
}

/// The desktop tools rows (BG-1): `admin`, owner only, and the WebSocket row
/// is a socket that the guard admits by ticket or by cookie with the board
/// `Origin`.
#[test]
fn the_desktop_tools_rows_are_admin_owner_rows_and_one_is_a_socket() {
    let rows = [
        (
            "station.desktop_tools",
            Method::GET,
            "/api/stations/{station}/desktop-tools",
            false,
        ),
        (
            "station.desktop_tools_plan",
            Method::GET,
            "/api/stations/{station}/desktop-tools/plan",
            false,
        ),
        (
            "station.maintenance_open",
            Method::POST,
            "/api/stations/{station}/maintenance",
            true,
        ),
        (
            "station.maintenance_list",
            Method::GET,
            "/api/stations/{station}/maintenance",
            false,
        ),
        (
            "station.maintenance_resize",
            Method::POST,
            "/api/stations/{station}/maintenance/{id}/resize",
            true,
        ),
        (
            MAINTENANCE_WS_METHOD,
            Method::GET,
            "/api/stations/{station}/maintenance/{id}/attach",
            false,
        ),
    ];
    for (name, method, path, mutating) in rows {
        let api = find(&method, path).unwrap_or_else(|| panic!("{name} has no row"));
        assert_eq!(api.name, name);
        assert_eq!(api.scope, Some(Scope::Admin), "{name}");
        assert_eq!(api.mutating, mutating, "{name}");
        assert!(!api.member && !api.display, "{name}");
        assert_eq!(is_socket(name), name == MAINTENANCE_WS_METHOD, "{name}");
    }
}

/// The workflow rows (B12): both `read`, neither mutating; the page is open to
/// a member of the session, the chat of one agent is the owner only.
#[test]
fn the_method_table_has_both_rows_with_their_member_flags() {
    let rows = [
        (
            "sessions.workflows",
            "/api/sessions/{station}/{session}/workflows",
            true,
        ),
        (
            "sessions.workflow_agent",
            "/api/sessions/{station}/{session}/workflow-agents/{agent_id}",
            false,
        ),
    ];
    for (name, path, member) in rows {
        let api = find(&Method::GET, path).unwrap_or_else(|| panic!("{name} has no row"));
        assert_eq!(api.name, name);
        assert_eq!(api.scope, Some(Scope::Read), "{name}");
        assert!(!api.mutating, "{name}");
        assert_eq!(api.member, member, "{name}");
        assert!(!api.display, "{name}");
    }
}
