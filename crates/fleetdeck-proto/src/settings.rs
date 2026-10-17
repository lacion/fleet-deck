//! Settings on the wire (DL-172): the field table of a config file, the
//! settings view, the save protocol and the secret relay shape.
//!
//! A key is the TOML key; a key of a nested table uses a dot
//! (`model_help.enabled`). A secret value never appears in a view or in an
//! answer: the view shows `{ "set": true }` instead.

use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use ts_rs::TS;

/// The kind of a settings field; it decides the editor and the check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "settings/")]
pub enum FieldKind {
    Bool,
    Int,
    Text,
    Enum,
    Path,
    /// A list of directories.
    Paths,
    /// A list of text lines.
    Texts,
    /// A list of `ip:port` addresses.
    Addresses,
    /// The provider instances of a station.
    Instances,
    /// A map from a repository path to a group name.
    RepoGroups,
    /// A map from a Taildrive share `<machine>/<share>` to a mount path.
    ShareMounts,
    /// The `[[trackers]]` entries (read only).
    Trackers,
}

/// When a changed field takes effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "settings/")]
pub enum Apply {
    /// At once, while the process runs.
    Live,
    /// At the next start of the process.
    Restart,
}

/// One entry of a field table. There is one entry for each key that the
/// config parser accepts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "settings/")]
pub struct FieldMeta {
    pub key: String,
    pub label: String,
    pub help: String,
    pub kind: FieldKind,
    #[ts(type = "number | null")]
    pub min: Option<i64>,
    #[ts(type = "number | null")]
    pub max: Option<i64>,
    /// The allowed values of an `enum` field.
    pub options: Vec<String>,
    pub apply: Apply,
    /// A change of this field may need a recent sign-in.
    pub danger: bool,
    /// The value holds secret entries; a view shows `{ "set": true }`.
    pub secret: bool,
    pub read_only: bool,
    /// The board does not show the field (a key that only the deck writes).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    #[ts(optional, as = "Option<bool>")]
    pub hidden: bool,
    /// The command-line flag that sets the field in the running process.
    pub cli_flag: Option<String>,
}

/// Whether the disk confirmed a write.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "settings/")]
pub enum Durability {
    Confirmed,
    /// The file was replaced and applied, but the directory sync failed.
    Uncertain,
}

/// The settings of one holder (the deck or a station).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export_to = "settings/")]
pub struct SettingsView {
    #[ts(type = "Record<string, unknown>")]
    pub values: Map<String, Value>,
    pub fields: Vec<FieldMeta>,
    /// Hex SHA-256 of the config file bytes.
    pub revision: String,
    /// The keys that the running process took from a command-line flag.
    pub overrides: Vec<String>,
}

/// A save: the keys to change (`null` removes a key) and the revision that
/// the board read.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "settings/")]
pub struct SettingsChange {
    #[ts(type = "Record<string, unknown>")]
    pub changes: Map<String, Value>,
    pub revision: String,
    #[serde(default)]
    pub elevation: Option<String>,
}

/// The answer to a save.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export_to = "settings/")]
pub struct SettingsSaved {
    #[ts(type = "Record<string, unknown>")]
    pub values: Map<String, Value>,
    /// The changed keys that apply at once.
    pub applied: Vec<String>,
    /// The changed keys that apply at the next start.
    pub restart_needed: Vec<String>,
    /// The changed keys that could not apply at once (always empty today).
    pub unapplied: Vec<String>,
    pub revision: String,
    pub durability: Durability,
}

/// A secret value that the owner types in the board. It crosses the deck
/// once and is never stored or logged there. `Debug` hides the value.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "settings/")]
pub struct SecretSet {
    pub key: String,
    pub value: String,
}

impl fmt::Debug for SecretSet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SecretSet")
            .field("key", &self.key)
            .field("value", &"<redacted>")
            .finish()
    }
}

/// The params of `settings.set` (DL-172). The deck checks the owner's
/// elevation and sends `elevated`; `change.elevation` is always `None` here.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "settings/")]
pub struct SettingsSetParams {
    pub change: SettingsChange,
    pub elevated: bool,
}

/// The params of `settings.secret_set` (the credential relay, DL-173). The
/// value is never stored or logged on the deck; `Debug` hides it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "settings/")]
pub struct SecretSetParams {
    pub secret: SecretSet,
    /// The revision of the config file that the board read.
    pub revision: String,
    pub elevated: bool,
}

/// The params of `station.restart`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "settings/")]
pub struct RestartRequest {
    /// Stop the live sessions that a restart ends.
    pub stop_direct_sessions: bool,
    pub elevated: bool,
}

/// What a restart ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "settings/")]
pub enum RestartKind {
    Session,
    Shell,
    Setup,
}

/// One thing that a restart ends.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "settings/")]
pub struct RestartItem {
    pub kind: RestartKind,
    pub id: String,
    pub title: String,
}

/// The answer to `station.restart`. When `restarting` is false,
/// `would_stop` is not empty.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "settings/")]
pub struct RestartAnswer {
    pub restarting: bool,
    pub would_stop: Vec<RestartItem>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_set_debug_hides_the_value() {
        let s = SecretSet {
            key: "provider_instance.work.env.TOKEN".into(),
            value: "canary-value-1".into(),
        };
        let text = format!("{s:?} {s:#?}");
        assert!(!text.contains("canary-value-1"), "{text}");
        assert!(text.contains("provider_instance.work.env.TOKEN"));
    }

    #[test]
    fn secret_set_params_debug_hides_the_value() {
        let p = SecretSetParams {
            secret: SecretSet {
                key: "provider_instance.work.env.TOKEN".into(),
                value: "canary-value-2".into(),
            },
            revision: "ab".into(),
            elevated: true,
        };
        let text = format!("{p:?} {p:#?}");
        assert!(!text.contains("canary-value-2"), "{text}");
    }

    #[test]
    fn restart_shapes_round_trip() {
        let a = RestartAnswer {
            restarting: false,
            would_stop: vec![RestartItem {
                kind: RestartKind::Session,
                id: "s1".into(),
                title: "Fix the build".into(),
            }],
        };
        let v = serde_json::to_value(&a).unwrap();
        assert_eq!(v["would_stop"][0]["kind"], "session");
        assert_eq!(serde_json::from_value::<RestartAnswer>(v).unwrap(), a);
        let r: RestartRequest =
            serde_json::from_str(r#"{"stop_direct_sessions":true,"elevated":false}"#).unwrap();
        assert!(r.stop_direct_sessions && !r.elevated);
    }

    #[test]
    fn change_without_elevation_parses() {
        let c: SettingsChange =
            serde_json::from_str(r#"{"changes":{"max_sessions":4},"revision":"ab"}"#).unwrap();
        assert_eq!(c.elevation, None);
        assert!(
            serde_json::from_str::<SettingsChange>(r#"{"changes":{},"revision":"a","x":1}"#)
                .is_err()
        );
    }
}
