//! The coordinator hashes of R13 P6 §5.4 and §5.7.

use fleetdeck_proto::coordinator_desktop_wire::{CoordinatorContext, ProtectedRead};
use fleetdeck_proto::desktop::{DesktopAction, Urgency};

use super::*;

fn ctx() -> CoordinatorContext {
    CoordinatorContext {
        grant_id: "g1".into(),
        binding_id: "b1".into(),
        generation: 2,
        caller_station: "st-a".into(),
        caller_session: "01J0000000000000000000000S".into(),
        caller_label: "swift-otter".into(),
    }
}

fn notify() -> DesktopAction {
    DesktopAction::Notify {
        title: "t".into(),
        body: "b".into(),
        urgency: Urgency::Low,
    }
}

const CTX_JSON: &str = r#"{"binding_id":"b1","caller_label":"swift-otter","caller_session":"01J0000000000000000000000S","caller_station":"st-a","generation":2,"grant_id":"g1"}"#;

#[test]
fn without_a_context_the_new_functions_equal_the_owner_ones() {
    for a in [
        DesktopAction::ScreenLock,
        notify(),
        DesktopAction::ScreenCapture {
            monitor: "DP-1".into(),
        },
        DesktopAction::WindowClose {
            address: "0x1".into(),
        },
    ] {
        assert_eq!(canonical_bytes_for(&a, "n", None), canonical_bytes(&a, "n"));
        assert_eq!(action_hash_for(&a, "n", None), action_hash(&a, "n"));
    }
}

#[test]
fn the_coordinator_bytes_add_the_context_between_action_and_monitor() {
    let text = String::from_utf8(canonical_bytes_for(&notify(), "ff00", Some(&ctx()))).unwrap();
    assert_eq!(
        text,
        format!(
            r#"{{"action":{{"action":"notify","body":"b","title":"t","urgency":"low"}},"coordinator":{CTX_JSON},"monitor":null,"station":"ff00"}}"#
        )
    );
    let shot = DesktopAction::ScreenCapture {
        monitor: "DP-1".into(),
    };
    let text = String::from_utf8(canonical_bytes_for(&shot, "ff00", Some(&ctx()))).unwrap();
    assert!(
        text.contains(r#""monitor":"DP-1","station":"ff00"}"#),
        "{text}"
    );
}

/// The input is the JSON of `the_coordinator_bytes_add_the_context_between_
/// action_and_monitor` (a `notify` action, the context above, node `ff00`).
/// The hash was computed once with `sha256sum` and is pinned here.
#[test]
fn a_coordinator_notify_hash_has_a_pinned_value() {
    assert_eq!(
        action_hash_for(&notify(), "ff00", Some(&ctx())),
        "6eb744cd3bb23528003c97ac4caa951b7c38fadab8b3b90f1f81a7afb9103cdb"
    );
}

#[test]
fn the_owner_hash_fixtures_stay_unchanged() {
    // SHA-256 of {"action":{"action":"screen_lock"},"monitor":null,"station":"node"}
    assert_eq!(
        action_hash(&DesktopAction::ScreenLock, "node"),
        "c0ec8ef2d0e2f483eddae5fb6f817123d0804c22279e2c5c44e81f34ab4031b2"
    );
}

#[test]
fn a_changed_or_removed_context_changes_the_hash() {
    let base = action_hash_for(&notify(), "ff00", Some(&ctx()));
    assert_ne!(base, action_hash_for(&notify(), "ff00", None));
    let edits: [fn(&mut CoordinatorContext); 6] = [
        |c| c.grant_id = "g2".into(),
        |c| c.binding_id = "b2".into(),
        |c| c.generation = 3,
        |c| c.caller_station = "st-b".into(),
        |c| c.caller_session = "01J0000000000000000000000T".into(),
        |c| c.caller_label = "other".into(),
    ];
    for edit in edits {
        let mut c = ctx();
        edit(&mut c);
        assert_ne!(base, action_hash_for(&notify(), "ff00", Some(&c)));
    }
    assert_ne!(base, action_hash_for(&notify(), "other", Some(&ctx())));
    let other = DesktopAction::Notify {
        title: "t".into(),
        body: "b2".into(),
        urgency: Urgency::Low,
    };
    assert_ne!(base, action_hash_for(&other, "ff00", Some(&ctx())));
}

#[test]
fn a_coordinator_hash_binds_the_preview_close_handle() {
    let close = |id: &str| DesktopAction::PreviewClose {
        window_id: id.into(),
    };
    let a = action_hash_for(&close("aa"), "n", Some(&ctx()));
    assert_ne!(a, action_hash_for(&close("ab"), "n", Some(&ctx())));
    assert_ne!(a, action_hash_for(&close("aa"), "n", None));
}

#[test]
fn issue_coordinator_mirrors_issue_and_sets_the_context() {
    let a = notify();
    let auth = issue_coordinator(&a, "n", "id".into(), "ep".into(), 1_000, ctx());
    assert_eq!(auth.action_id, "id");
    assert_eq!(auth.desktop_epoch, "ep");
    assert_eq!(auth.deadline_ms, 11_000);
    assert_eq!(auth.deck_now_ms, 1_000);
    assert_eq!(auth.quiet_hours, None);
    assert_eq!(auth.coordinator, Some(ctx()));
    assert_eq!(auth.action_hash, action_hash_for(&a, "n", Some(&ctx())));
    // The owner authority of the same action has no context and another hash.
    let owner = issue(&a, "n", "id".into(), "ep".into(), 1_000, None);
    assert_eq!(owner.coordinator, None);
    assert_ne!(owner.action_hash, auth.action_hash);
}

fn file(offset: u64, length: u64) -> ProtectedRead {
    ProtectedRead::CaptureFile {
        artifact_id: "a1".into(),
        version: 1,
        offset,
        length,
    }
}

const READ_ID: &str = "00112233445566778899aabbccddeeff";

/// The input is `{"coordinator":<CTX_JSON>,"kind":"read","link":"epoch-1",
/// "read":{"kind":"info"},"read_id":"00112233445566778899aabbccddeeff",
/// "station":"ff00"}`. The hash was computed once with `sha256sum`.
#[test]
fn a_read_hash_has_a_pinned_value() {
    assert_eq!(
        read_hash(&ctx(), "epoch-1", &ProtectedRead::Info, READ_ID, "ff00"),
        "ff758309183b45a1cc7e7c114985cd5046a29fc4084b80d9565c50b45da0e74b"
    );
}

#[test]
fn a_read_hash_binds_every_input() {
    let base = read_hash(&ctx(), "epoch-1", &file(0, 100), READ_ID, "ff00");
    assert_eq!(base.len(), 64);
    assert_ne!(
        base,
        read_hash(&ctx(), "epoch-2", &file(0, 100), READ_ID, "ff00")
    );
    assert_ne!(
        base,
        read_hash(&ctx(), "epoch-1", &file(0, 100), "other", "ff00")
    );
    assert_ne!(
        base,
        read_hash(&ctx(), "epoch-1", &file(0, 100), READ_ID, "ee00")
    );
    // The offset and the length.
    assert_ne!(
        base,
        read_hash(&ctx(), "epoch-1", &file(1, 100), READ_ID, "ff00")
    );
    assert_ne!(
        base,
        read_hash(&ctx(), "epoch-1", &file(0, 101), READ_ID, "ff00")
    );
    // The kind and the resource.
    let other_resource = ProtectedRead::CaptureFile {
        artifact_id: "a2".into(),
        version: 1,
        offset: 0,
        length: 100,
    };
    let other_version = ProtectedRead::CaptureFile {
        artifact_id: "a1".into(),
        version: 2,
        offset: 0,
        length: 100,
    };
    for read in [
        ProtectedRead::Info,
        ProtectedRead::ActionStatus {
            operation_id: "a1".into(),
        },
        ProtectedRead::CaptureStatus {
            capture_id: "a1".into(),
        },
        other_resource,
        other_version,
    ] {
        assert_ne!(base, read_hash(&ctx(), "epoch-1", &read, READ_ID, "ff00"));
    }
    // The context.
    let mut other = ctx();
    other.generation = 3;
    assert_ne!(
        base,
        read_hash(&other, "epoch-1", &file(0, 100), READ_ID, "ff00")
    );
    // Two statuses of different kinds with the same ID differ.
    let a = read_hash(
        &ctx(),
        "l",
        &ProtectedRead::ActionStatus {
            operation_id: "x".into(),
        },
        READ_ID,
        "n",
    );
    let b = read_hash(
        &ctx(),
        "l",
        &ProtectedRead::CaptureStatus {
            capture_id: "x".into(),
        },
        READ_ID,
        "n",
    );
    assert_ne!(a, b);
}

#[test]
fn a_read_hash_never_equals_an_action_hash() {
    // The `kind` field keeps the two apart even for the same context and node.
    let read = read_hash(&ctx(), "l", &ProtectedRead::Info, READ_ID, "ff00");
    for a in [
        notify(),
        DesktopAction::ScreenLock,
        DesktopAction::ScreenCapture {
            monitor: "DP-1".into(),
        },
    ] {
        assert_ne!(read, action_hash_for(&a, "ff00", Some(&ctx())));
    }
}
