//! The dispatch authority of `desktop.act` (R13 §5E item 1, §5F items 1 and
//! 2, §5G item 1; feature `desktop_authority.v1`).
//!
//! The deck adds the `authority` key to the flat params of `desktop.act`.
//! The station splits it off before it parses the action. The hash and the
//! checks that need a digest are in `fleetdeck_link::desktop_authority`.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use ts_rs::TS;

use crate::coordinator_desktop_wire::CoordinatorContext;
use crate::desktop::DesktopAction;

/// The key of the authority in the params of `desktop.act`.
pub const AUTHORITY_KEY: &str = "authority";
/// An authority is valid for 10 seconds after the deck made it.
pub const AUTHORITY_TTL_MS: i64 = 10_000;
/// The station and the deck wall clocks may differ by this much (§5F item 1).
pub const CLOCK_SKEW_MS: i64 = 5_000;
/// The most consumed action ids that a station keeps for one connection epoch
/// (§5G item 1).
pub const MAX_USED_IDS: usize = 4096;
/// The station text for a missing or invalid authority.
pub const AUTHORITY_REFUSED: &str = "The deck did not authorize this desktop action.";

/// What the deck gives with one desktop action.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "desktop/")]
pub struct DesktopAuthority {
    /// A fresh id of the action (26 characters, ULID form). The station
    /// accepts each id once for the connection epoch.
    pub action_id: String,
    /// The hex SHA-256 of the canonical JSON of the action, the station node
    /// fingerprint and the target monitor.
    pub action_hash: String,
    /// The deck clock plus [`AUTHORITY_TTL_MS`], in Unix ms.
    pub deadline_ms: i64,
    /// The deck wall clock when the deck made the authority, in Unix ms.
    pub deck_now_ms: i64,
    /// The `Hello.desktop_epoch` of the station connection that carries it.
    pub desktop_epoch: String,
    /// The `quiet_hours` of the display of a wake: the station refuses when
    /// its own local time is inside the interval (§5E item 5).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub quiet_hours: Option<String>,
    /// The coordinator that the action runs for (R13 P6 §5.4, feature
    /// `desktop_mcp.v1`). Absent for an owner or wake action, whose bytes
    /// stay unchanged. The action hash covers it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub coordinator: Option<CoordinatorContext>,
}

/// Splits the `authority` key off the params of `desktop.act`. The rest is
/// the flat action.
///
/// # Errors
/// The params are not an object, or the authority does not parse.
pub fn split_params(params: Value) -> Result<(Value, Option<DesktopAuthority>), String> {
    let Value::Object(mut map) = params else {
        return Err("params: expected an object".to_owned());
    };
    let authority = match map.remove(AUTHORITY_KEY) {
        None => None,
        Some(v) => Some(serde_json::from_value(v).map_err(|e| format!("authority: {e}"))?),
    };
    Ok((Value::Object(map), authority))
}

/// The params of `desktop.act`: the flat action with the `authority` key.
#[must_use]
pub fn with_authority(action: &DesktopAction, authority: &DesktopAuthority) -> Value {
    // An action and an authority always serialize to JSON.
    let mut value = serde_json::to_value(action).unwrap_or(Value::Null);
    if let (Value::Object(map), Ok(a)) = (&mut value, serde_json::to_value(authority)) {
        map.insert(AUTHORITY_KEY.to_owned(), a);
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;

    fn authority() -> DesktopAuthority {
        DesktopAuthority {
            action_id: "01J00000000000000000000000".to_owned(),
            action_hash: "ab".repeat(32),
            deadline_ms: 20_000,
            deck_now_ms: 10_000,
            desktop_epoch: "cd".repeat(16),
            quiet_hours: None,
            coordinator: None,
        }
    }

    /// The bytes of an owner authority, written down from the struct before
    /// `coordinator` existed: an owner or wake action must not change.
    #[test]
    fn an_owner_authority_serializes_to_its_pinned_bytes() {
        let owner = concat!(
            r#"{"action_id":"01J00000000000000000000000","#,
            r#""action_hash":"abababababababababababababababababababababababababababababababab","#,
            r#""deadline_ms":20000,"deck_now_ms":10000,"#,
            r#""desktop_epoch":"cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd"}"#
        );
        assert_eq!(serde_json::to_string(&authority()).unwrap(), owner);
        let wake = DesktopAuthority {
            quiet_hours: Some("22:00-07:00".to_owned()),
            ..authority()
        };
        assert_eq!(
            serde_json::to_string(&wake).unwrap(),
            owner.replace('}', r#","quiet_hours":"22:00-07:00"}"#)
        );
        // The old bytes parse, with no coordinator.
        let parsed: DesktopAuthority = serde_json::from_str(owner).unwrap();
        assert_eq!(parsed, authority());
    }

    #[test]
    fn a_coordinator_context_is_written_after_the_owner_fields_and_round_trips() {
        let with = DesktopAuthority {
            coordinator: Some(CoordinatorContext {
                grant_id: "g1".into(),
                binding_id: "b1".into(),
                generation: 3,
                caller_station: "st-a".into(),
                caller_session: "01J0000000000000000000000S".into(),
                caller_label: "swift-otter".into(),
            }),
            ..authority()
        };
        let v = serde_json::to_value(&with).unwrap();
        assert_eq!(v["coordinator"]["generation"], 3);
        assert_eq!(v["coordinator"]["caller_label"], "swift-otter");
        assert_eq!(serde_json::from_value::<DesktopAuthority>(v).unwrap(), with);
        // A context with an unknown field is refused.
        let bad = serde_json::json!({
            "action_id": "a", "action_hash": "b", "deadline_ms": 1, "deck_now_ms": 1,
            "desktop_epoch": "c",
            "coordinator": {"grant_id": "g", "binding_id": "b", "generation": 1,
                "caller_station": "s", "caller_session": "x", "caller_label": "l", "z": 1}
        });
        assert!(serde_json::from_value::<DesktopAuthority>(bad).is_err());
    }

    #[test]
    fn the_authority_is_a_key_of_the_flat_params_and_splits_off_again() {
        let action = DesktopAction::ScreenDnd { on: true };
        let params = with_authority(&action, &authority());
        assert_eq!(params["action"], "screen_dnd");
        assert_eq!(params["authority"]["deadline_ms"], 20_000);
        let (rest, found) = split_params(params).unwrap();
        assert_eq!(found, Some(authority()));
        assert_eq!(
            serde_json::from_value::<DesktopAction>(rest).unwrap(),
            action
        );
    }

    #[test]
    fn params_without_an_authority_split_to_none() {
        let (rest, found) = split_params(serde_json::json!({"action": "screen_lock"})).unwrap();
        assert!(found.is_none());
        assert_eq!(rest["action"], "screen_lock");
    }

    #[test]
    fn bad_params_and_a_bad_authority_are_errors() {
        assert!(split_params(serde_json::json!([1])).is_err());
        let bad = serde_json::json!({"action": "screen_lock", "authority": {"action_id": 1}});
        assert!(split_params(bad).is_err());
        let extra = serde_json::json!({
            "action": "screen_lock",
            "authority": {
                "action_id": "a", "action_hash": "b", "deadline_ms": 1,
                "deck_now_ms": 1, "desktop_epoch": "c", "x": 1
            }
        });
        assert!(split_params(extra).is_err());
    }
}
