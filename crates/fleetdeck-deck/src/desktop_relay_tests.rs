#![allow(clippy::unwrap_used)]

use super::wall::{WallGuard, wall_locks};
use super::*;

const ALL: [RelayError; 10] = [
    RelayError::Unsupported,
    RelayError::Forbidden,
    RelayError::Invalid,
    RelayError::NotFound,
    RelayError::Conflict,
    RelayError::Failed,
    RelayError::Overloaded,
    RelayError::Timeout,
    RelayError::Unreachable,
    RelayError::ControlOff,
];

#[test]
fn the_http_mapping_follows_the_design() {
    let status = |e: RelayError| e.http().0.as_u16();
    assert_eq!(
        RelayError::Unsupported.http(),
        (StatusCode::CONFLICT, "desktop_unsupported")
    );
    assert_eq!(
        RelayError::Forbidden.http(),
        (StatusCode::FORBIDDEN, "desktop_forbidden")
    );
    assert_eq!(
        RelayError::Unreachable.http(),
        (StatusCode::CONFLICT, "station_unreachable")
    );
    assert_eq!(
        RelayError::ControlOff.http(),
        (StatusCode::CONFLICT, "desktop_control_off")
    );
    assert_eq!(status(RelayError::Invalid), 400);
    assert_eq!(status(RelayError::NotFound), 404);
    assert_eq!(status(RelayError::Conflict), 409);
    assert_eq!(status(RelayError::Failed), 502);
    assert_eq!(status(RelayError::Overloaded), 503);
    assert_eq!(status(RelayError::Timeout), 504);
}

#[test]
fn the_codes_are_unique_and_the_messages_are_fixed_and_do_not_blame_a_setting() {
    let mut codes: Vec<&str> = ALL.iter().map(|e| e.http().1).collect();
    codes.sort_unstable();
    codes.dedup();
    assert_eq!(codes.len(), ALL.len());
    for e in ALL {
        let m = e.message();
        assert!(!m.contains("allow_desktop"), "{m}");
        assert!(!m.contains("http"), "{m}");
    }
}
#[test]
fn loopback_and_unspecified_origins_are_not_reachable_for_a_wall() {
    for o in [
        "https://localhost:7420",
        "https://127.0.0.1:7420",
        "https://127.9.9.9",
        "https://[::1]:7420",
        "https://0.0.0.0:7420",
        "https://[::]:7420",
        "https://wall.localhost",
        "https://[::ffff:127.0.0.1]:1",
    ] {
        assert!(is_loopback_origin(o), "{o}");
    }
    for o in [
        "https://deck.tail1.ts.net:7420",
        "https://100.64.0.1:7420",
        "https://[fd7a::1]:7420",
        "https://192.168.1.5",
    ] {
        assert!(!is_loopback_origin(o), "{o}");
    }
}

#[test]
fn display_ids_follow_the_station_grammar_and_the_url_has_no_secret() {
    assert!(valid_display_id("01J0ABCDEF_x-9"));
    assert!(!valid_display_id(""));
    assert!(!valid_display_id("a b"));
    assert!(!valid_display_id(&"a".repeat(65)));
    assert_eq!(
        wall_url("https://deck.ts.net:7420/", "01J", 7),
        "https://deck.ts.net:7420/display#/display-pair?expect=01J&op=7"
    );
}

#[test]
fn the_audit_name_and_result_follow_the_design() {
    assert_eq!(
        tag_of(&DesktopAction::ScreenPower {
            monitor: None,
            on: true
        }),
        "screen_power"
    );
    assert_eq!(tag_of(&DesktopActBody::ScreenLock), "screen_lock");
    let err = |e| audit_result(&Err(e));
    for e in [
        RelayError::ControlOff,
        RelayError::Unsupported,
        RelayError::Forbidden,
        RelayError::Invalid,
        RelayError::NotFound,
        RelayError::Conflict,
    ] {
        assert_eq!(err(e), AuditResult::Refused, "{e:?}");
    }
    for e in [
        RelayError::Failed,
        RelayError::Overloaded,
        RelayError::Timeout,
        RelayError::Unreachable,
    ] {
        assert_eq!(err(e), AuditResult::Failed, "{e:?}");
    }
}

#[test]
fn a_second_wall_lock_of_one_display_conflicts_and_the_entry_goes_away() {
    let id = "lock-unit-test-display";
    let first = WallGuard::take(id).unwrap();
    assert!(matches!(WallGuard::take(id), Err(RelayError::Conflict)));
    assert!(WallGuard::take("lock-unit-test-other").is_ok());
    drop(first);
    let again = WallGuard::take(id).unwrap();
    drop(again);
    let map = wall_locks().lock().unwrap();
    assert!(!map.contains_key(id));
}
