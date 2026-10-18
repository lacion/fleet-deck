//! The canonical hash of a desktop action (R13 §5E item 1, DL-230). The deck
//! and the station both use these functions, so there is one canonical form.
//!
//! The hash covers the action, the station node fingerprint (the current
//! fingerprint of the station identity key, which both sides know) and the
//! target monitor. The canonical JSON is
//! `{"action": <action>, "monitor": <string|null>, "station": <string>}`:
//! `serde_json` writes the keys of a `Value` object in sorted order and with
//! no spaces.

use fleetdeck_proto::coordinator_desktop_wire::{CoordinatorContext, ProtectedRead};
use fleetdeck_proto::desktop::DesktopAction;
use fleetdeck_proto::desktop_authority::{AUTHORITY_TTL_MS, DesktopAuthority};
use serde_json::json;

/// The monitor that an action targets, if it names one.
#[must_use]
pub fn target_monitor(action: &DesktopAction) -> Option<&str> {
    match action {
        DesktopAction::ScreenPower { monitor, .. } | DesktopAction::BrowserOpen { monitor, .. } => {
            monitor.as_deref()
        }
        DesktopAction::WorkspaceShow { monitor, .. }
        | DesktopAction::WallOpen { monitor, .. }
        | DesktopAction::ScreenCapture { monitor } => Some(monitor),
        _ => None,
    }
}

/// The canonical JSON bytes that the hash of an owner or wake action covers.
#[must_use]
pub fn canonical_bytes(action: &DesktopAction, station_node: &str) -> Vec<u8> {
    canonical_bytes_for(action, station_node, None)
}

/// The canonical JSON bytes that the hash covers. With no coordinator
/// context they are exactly the bytes of [`canonical_bytes`] (owner and wake
/// actions, R13 P6 §5.4). With a context, the compact JSON adds the key
/// `coordinator`: `{"action", "coordinator", "monitor", "station"}`.
#[must_use]
pub fn canonical_bytes_for(
    action: &DesktopAction,
    station_node: &str,
    coordinator: Option<&CoordinatorContext>,
) -> Vec<u8> {
    // The action is a closed enum: it always serializes.
    let action = serde_json::to_value(action).unwrap_or(serde_json::Value::Null);
    let value = match coordinator {
        None => json!({
            "action": action,
            "monitor": target_monitor_of(&action),
            "station": station_node,
        }),
        Some(ctx) => json!({
            "action": action,
            "coordinator": serde_json::to_value(ctx).unwrap_or(serde_json::Value::Null),
            "monitor": target_monitor_of(&action),
            "station": station_node,
        }),
    };
    serde_json::to_vec(&value).unwrap_or_default()
}

fn target_monitor_of(action: &serde_json::Value) -> serde_json::Value {
    // The same rule as `target_monitor`, read from the serialized action so
    // that the bytes and the typed rule cannot drift apart: a test compares
    // them for every variant.
    let name = action.get("action").and_then(|a| a.as_str());
    match name {
        Some(
            "screen_power" | "browser_open" | "workspace_show" | "wall_open" | "screen_capture",
        ) => action
            .get("monitor")
            .cloned()
            .unwrap_or(serde_json::Value::Null),
        _ => serde_json::Value::Null,
    }
}

/// The lowercase hex SHA-256 of `bytes`.
fn sha256_hex(bytes: &[u8]) -> String {
    let digest = ring::digest::digest(&ring::digest::SHA256, bytes);
    digest.as_ref().iter().map(|b| format!("{b:02x}")).collect()
}

/// The hex SHA-256 of [`canonical_bytes`].
#[must_use]
pub fn action_hash(action: &DesktopAction, station_node: &str) -> String {
    action_hash_for(action, station_node, None)
}

/// The hex SHA-256 of [`canonical_bytes_for`].
#[must_use]
pub fn action_hash_for(
    action: &DesktopAction,
    station_node: &str,
    coordinator: Option<&CoordinatorContext>,
) -> String {
    sha256_hex(&canonical_bytes_for(action, station_node, coordinator))
}

/// The hash of a protected read (R13 P6 §5.7): the lowercase hex SHA-256 of
/// the canonical compact JSON `{"coordinator", "kind": "read", "link",
/// "read", "read_id", "station"}`. `link` is the `peer_desktop_epoch` of the
/// connection that carries the read; `station_node` is the target node
/// fingerprint. The `kind` field keeps it apart from every action hash.
#[must_use]
pub fn read_hash(
    coordinator: &CoordinatorContext,
    link: &str,
    read: &ProtectedRead,
    read_id: &str,
    station_node: &str,
) -> String {
    let value = json!({
        "coordinator": serde_json::to_value(coordinator).unwrap_or(serde_json::Value::Null),
        "kind": "read",
        "link": link,
        "read": serde_json::to_value(read).unwrap_or(serde_json::Value::Null),
        "read_id": read_id,
        "station": station_node,
    });
    sha256_hex(&serde_json::to_vec(&value).unwrap_or_default())
}

/// The authority for one action: valid until `now_ms` plus
/// [`AUTHORITY_TTL_MS`].
#[must_use]
pub fn issue(
    action: &DesktopAction,
    station_node: &str,
    action_id: String,
    desktop_epoch: String,
    now_ms: i64,
    quiet_hours: Option<String>,
) -> DesktopAuthority {
    DesktopAuthority {
        action_id,
        action_hash: action_hash(action, station_node),
        deadline_ms: now_ms.saturating_add(AUTHORITY_TTL_MS),
        deck_now_ms: now_ms,
        desktop_epoch,
        quiet_hours,
        coordinator: None,
    }
}

/// The authority for one coordinator action (R13 P6 §5.4): like [`issue`],
/// with the context in the authority and in the hash. The coordinator
/// authority never has quiet hours (those belong to a wake).
#[must_use]
pub fn issue_coordinator(
    action: &DesktopAction,
    station_node: &str,
    action_id: String,
    desktop_epoch: String,
    now_ms: i64,
    coordinator: CoordinatorContext,
) -> DesktopAuthority {
    DesktopAuthority {
        action_id,
        action_hash: action_hash_for(action, station_node, Some(&coordinator)),
        deadline_ms: now_ms.saturating_add(AUTHORITY_TTL_MS),
        deck_now_ms: now_ms,
        desktop_epoch,
        quiet_hours: None,
        coordinator: Some(coordinator),
    }
}

#[cfg(test)]
mod tests {
    use fleetdeck_proto::desktop::Urgency;

    use super::*;

    fn every_action() -> Vec<DesktopAction> {
        vec![
            DesktopAction::ScreenPower {
                monitor: Some("DP-1".into()),
                on: true,
            },
            DesktopAction::ScreenPower {
                monitor: None,
                on: false,
            },
            DesktopAction::ScreenLock,
            DesktopAction::ScreenDnd { on: true },
            DesktopAction::ScreenIdleInhibit { on: false },
            DesktopAction::WorkspaceShow {
                monitor: "DP-2".into(),
                workspace: 3,
            },
            DesktopAction::WallOpen {
                display_id: "d".into(),
                monitor: "DP-1".into(),
                workspace: None,
                url: "https://deck/display#/display-pair".into(),
            },
            DesktopAction::WallClose {
                display_id: "d".into(),
            },
            DesktopAction::WallReload {
                display_id: "d".into(),
                url: "https://deck/display#/display-pair".into(),
            },
            DesktopAction::Notify {
                title: "t".into(),
                body: "b".into(),
                urgency: Urgency::Low,
            },
            DesktopAction::BrowserOpen {
                url: "https://example.org".into(),
                monitor: Some("DP-1".into()),
                session: None,
            },
            DesktopAction::BrowserOpen {
                url: "https://example.org".into(),
                monitor: None,
                session: None,
            },
            DesktopAction::WindowClose {
                address: "0x1".into(),
            },
            DesktopAction::ScreenCapture {
                monitor: "DP-1".into(),
            },
            DesktopAction::PreviewClose {
                window_id: "0123456789abcdef0123456789abcdef".into(),
            },
        ]
    }

    #[test]
    fn the_canonical_bytes_are_sorted_and_compact() {
        let a = DesktopAction::ScreenPower {
            monitor: Some("DP-1".into()),
            on: true,
        };
        let text = String::from_utf8(canonical_bytes(&a, "ff00")).unwrap();
        assert_eq!(
            text,
            r#"{"action":{"action":"screen_power","monitor":"DP-1","on":true},"monitor":"DP-1","station":"ff00"}"#
        );
    }

    #[test]
    fn the_hash_has_a_fixed_value_and_binds_every_input() {
        let a = DesktopAction::ScreenLock;
        let h = action_hash(&a, "node");
        assert_eq!(h.len(), 64);
        // SHA-256 of {"action":{"action":"screen_lock"},"monitor":null,"station":"node"}
        assert_eq!(
            h,
            "c0ec8ef2d0e2f483eddae5fb6f817123d0804c22279e2c5c44e81f34ab4031b2"
        );
        assert_ne!(h, action_hash(&a, "other"));
        assert_ne!(
            h,
            action_hash(&DesktopAction::ScreenDnd { on: true }, "node")
        );
    }

    #[test]
    fn the_typed_target_rule_equals_the_serialized_rule() {
        for a in every_action() {
            let typed = target_monitor(&a).map(str::to_owned);
            let value = serde_json::to_value(&a).unwrap();
            let from_bytes = match target_monitor_of(&value) {
                serde_json::Value::String(s) => Some(s),
                _ => None,
            };
            assert_eq!(typed, from_bytes, "{a:?}");
        }
    }

    #[test]
    fn an_authority_lasts_ten_seconds() {
        let a = DesktopAction::ScreenLock;
        let auth = issue(
            &a,
            "n",
            "id".into(),
            "ep".into(),
            1_000,
            Some("22:00-07:00".into()),
        );
        assert_eq!(auth.deadline_ms, 11_000);
        assert_eq!(auth.deck_now_ms, 1_000);
        assert_eq!(auth.action_hash, action_hash(&a, "n"));
        assert_eq!(auth.quiet_hours.as_deref(), Some("22:00-07:00"));
        assert_eq!(auth.coordinator, None);
    }

    #[test]
    fn the_preview_close_handle_is_in_the_bytes_and_names_no_monitor() {
        let a = DesktopAction::PreviewClose {
            window_id: "ab".into(),
        };
        assert_eq!(target_monitor(&a), None);
        let text = String::from_utf8(canonical_bytes(&a, "ff00")).unwrap();
        assert_eq!(
            text,
            r#"{"action":{"action":"preview_close","window_id":"ab"},"monitor":null,"station":"ff00"}"#
        );
    }
}

#[cfg(test)]
#[path = "desktop_authority_coord_tests.rs"]
mod coord_tests;
