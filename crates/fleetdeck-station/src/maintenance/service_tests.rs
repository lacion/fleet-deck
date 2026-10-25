//! Tests of the service: the processes, the build and the registry.

use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;

use super::service::{Unit, build, classify, host_env, socket_fits};
use super::service_support::{FakeHost, FakePlans, Launch, Rig};

fn text(lines: &[&str]) -> Vec<u8> {
    format!("{}\n", lines.join("\n")).into_bytes()
}

#[test]
fn maintenance_unit_answers_are_classified() {
    // LoadState first, ActiveState second.
    assert_eq!(classify(&text(&["not-found", "inactive"])), Unit::Gone);
    assert_eq!(classify(&text(&["loaded", "inactive"])), Unit::Gone);
    assert_eq!(classify(&text(&["loaded", "failed"])), Unit::Gone);
    for active in ["active", "activating", "deactivating", "reloading"] {
        assert_eq!(
            classify(&text(&["loaded", active])),
            Unit::Active,
            "{active}"
        );
    }
    // The order of the lines does not matter.
    assert_eq!(classify(&text(&["active", "loaded"])), Unit::Active);
    assert_eq!(classify(&text(&["inactive", "not-found"])), Unit::Gone);
}

#[test]
fn maintenance_an_unclear_unit_answer_is_unknown() {
    for bytes in [
        Vec::new(),
        text(&["loaded"]),
        text(&["loaded", "active", "extra"]),
        text(&["loaded", "maintenance"]),
        text(&["masked", "weird"]),
        vec![0xff, 0xfe, b'\n', b'x', b'\n'],
    ] {
        assert_eq!(classify(&bytes), Unit::Unknown, "{bytes:?}");
    }
}

#[test]
fn maintenance_the_host_environment_is_fixed_and_adds_four_names() {
    let env = host_env();
    assert_eq!(env[0].0, "PATH");
    assert_eq!(env[0].1, "/usr/bin:/bin");
    assert_eq!(env[1].0, "LANG");
    assert_eq!(env[1].1, "C");
    for (name, _) in &env[2..] {
        assert!(
            [
                "HOME",
                "USER",
                "XDG_RUNTIME_DIR",
                "DBUS_SESSION_BUS_ADDRESS"
            ]
            .iter()
            .any(|n| name == n),
            "{name:?}"
        );
    }
}

#[test]
fn maintenance_the_service_is_not_installed_when_the_socket_path_is_over_107_bytes() {
    let tmp = tempfile::tempdir().unwrap();
    let host = FakeHost::new(Launch::Fail);
    let plans = FakePlans::new();
    // `<dir>/<32 hex>.sock` is the length of `dir` plus 38 bytes.
    assert!(socket_fits(Path::new(&format!("/{}", "a".repeat(68)))));
    assert!(!socket_fits(Path::new(&format!("/{}", "a".repeat(69)))));
    let long = tmp.path().join("b".repeat(120));
    std::fs::create_dir(&long).unwrap();
    assert!(build(&long, host.clone(), plans.clone()).is_none());
    let short = tmp.path().join("station");
    std::fs::create_dir(&short).unwrap();
    let svc = build(&short, host, plans).unwrap();
    assert_eq!(svc.dir, short.join("maintenance"));
    let mode = std::fs::metadata(&svc.dir).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o700);
}

#[test]
fn maintenance_the_service_is_not_installed_for_an_open_folder() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("maintenance");
    std::fs::create_dir(&dir).unwrap();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    let built = build(tmp.path(), FakeHost::new(Launch::Fail), FakePlans::new());
    assert!(built.is_none());
}

#[tokio::test]
async fn maintenance_a_newer_relay_replaces_the_older_one_and_only_its_own_entry_is_removed() {
    let rig = Rig::new(Launch::Fail);
    let (first, mut first_rx) = rig.svc.register_relay("a");
    let (second, _second_rx) = rig.svc.register_relay("a");
    assert_ne!(first, second);
    // The older relay lost its sender: its queue ends.
    assert!(first_rx.recv().await.is_none());
    // The older relay leaves: the newer entry stays.
    rig.svc.unregister_relay("a", first);
    assert!(rig.svc.relay_sender("a").is_some());
    rig.svc.unregister_relay("a", second);
    assert!(rig.svc.relay_sender("a").is_none());
}
