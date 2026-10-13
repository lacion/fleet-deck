use super::*;

#[test]
fn names_and_routes_are_unique_and_only_login_is_public() {
    for (i, a) in API.iter().enumerate() {
        for b in &API[i + 1..] {
            assert_ne!(a.name, b.name);
            assert!(a.method != b.method || a.path != b.path, "{}", a.name);
        }
        assert_eq!(
            a.scope.is_none(),
            matches!(a.name, "auth.login" | "auth.link" | "auth.join"),
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
    let mut open: Vec<&str> = API.iter().filter(|a| a.member).map(|a| a.name).collect();
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
            "fleet.extras",
            "mail.send",
            "session.follow_up",
            "session.interrupt",
            "session.prompt",
            "session.steer",
            "sessions.list",
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
        assert!(
            !API.iter().find(|a| a.name == name).unwrap().member,
            "{name}"
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
    assert_eq!(n, 16);
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
        let api = API.iter().find(|a| a.name == name).unwrap();
        assert_eq!(api.scope, Some(Scope::Admin), "{name}");
        assert!(!api.member, "{name}");
        assert_eq!(api.mutating, api.method == Method::POST, "{name}");
    }
    assert_eq!(
        API.iter()
            .filter(|a| a.name.starts_with("settings."))
            .count(),
        6
    );
    assert_eq!(
        find(&Method::POST, "/api/secrets/stations/{station}/settings").map(|m| m.name),
        Some("settings.station_secret_set")
    );
}
