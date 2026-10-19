//! The wire contract of the desktop tools and of the maintenance channel
//! (`board-gaps.md` section 2.3, 2.4, 2.7 and 2.9, BG-1): the answers of
//! `station.tools` and `station.desktop_tools_plan`, and the params and
//! answers of `maintenance.open`, `maintenance.list`, `maintenance.attach`
//! and `maintenance.resize`.
//!
//! `Option` fields serialize as `null`; they are never skipped.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// The stream purpose prefix of a maintenance run: `maintenance:<id>`.
pub const STREAM_PURPOSE: &str = "maintenance:";

/// The elevation label of `maintenance.open`.
pub const ELEVATION_LABEL: &str = "Install desktop tools";

/// The fixed tool IDs of `station.tools`.
pub mod tool {
    pub const WTYPE: &str = "wtype";
    pub const WF_RECORDER: &str = "wf_recorder";
    pub const WLRCTL: &str = "wlrctl";
    pub const GRIM: &str = "grim";
    pub const BUSCTL: &str = "busctl";
    pub const JQ: &str = "jq";
    pub const HYPRLAND: &str = "hyprland";
    pub const CHROMIUM: &str = "chromium";
    pub const NOTIFY_SEND: &str = "notify_send";

    /// Every tool ID, in the order of the answer.
    pub const ALL: [&str; 9] = [
        WTYPE,
        WF_RECORDER,
        WLRCTL,
        GRIM,
        BUSCTL,
        JQ,
        HYPRLAND,
        CHROMIUM,
        NOTIFY_SEND,
    ];
}

/// The codes and the fixed texts of a refused or ended maintenance request.
pub mod code {
    /// The package transaction needs packages that are not allowed.
    pub const TRANSACTION_REFUSED: &str = "transaction_refused";
    /// The installed packages differ from the reviewed plan.
    pub const TRANSACTION_CHANGED: &str = "transaction_changed";
    /// Input after the exit of the runner.
    pub const MAINTENANCE_ENDED: &str = "maintenance_ended";
    /// `allow_shell` is off.
    pub const SHELL_DISABLED: &str = "shell_disabled";
    /// A run is `admitted` or `running`.
    pub const MAINTENANCE_BUSY: &str = "maintenance_busy";
    /// A request ID that another plan already used.
    pub const REQUEST_ID_CONFLICT: &str = "request_id_conflict";
    /// The plan changed or expired.
    pub const PLAN_CHANGED: &str = "plan_changed";
    /// No such maintenance ID on this station.
    pub const UNKNOWN_MAINTENANCE: &str = "unknown_maintenance";

    /// The board text of a runner that ended without a result.
    pub const RUNNER_LOST_TEXT: &str =
        "The installation runner ended without a result. Check the desktop tools again.";

    /// Every code with its fixed text.
    pub const ALL: [(&str, &str); 8] = [
        (
            TRANSACTION_REFUSED,
            "The package transaction needs packages that Fleet Deck does not allow. Nothing was installed.",
        ),
        (
            TRANSACTION_CHANGED,
            "The installed packages differ from the reviewed plan.",
        ),
        (
            MAINTENANCE_ENDED,
            "The installation has ended. Input is not sent.",
        ),
        (
            SHELL_DISABLED,
            "Board terminals are disabled on this station. Enable them in station settings to install desktop tools.",
        ),
        (
            MAINTENANCE_BUSY,
            "Another installation is running on this station.",
        ),
        (
            REQUEST_ID_CONFLICT,
            "This request was already used for another plan.",
        ),
        (
            PLAN_CHANGED,
            "The plan changed or expired. Review the desktop tools again.",
        ),
        (UNKNOWN_MAINTENANCE, "No such installation on this station."),
    ];

    /// The fixed text of a code, or `None` for another string.
    #[must_use]
    pub fn text(code: &str) -> Option<&'static str> {
        ALL.iter().find(|(c, _)| *c == code).map(|(_, t)| *t)
    }
}

/// Exactly 32 lowercase hex characters (a maintenance ID or a request ID).
#[must_use]
pub fn is_hex_id(s: &str) -> bool {
    s.len() == 32 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// A terminal size of 1 to 1000 cells in each direction.
#[must_use]
pub fn size_ok(cols: u16, rows: u16) -> bool {
    (1..=1000).contains(&cols) && (1..=1000).contains(&rows)
}

/// The params of `station.tools`: `{}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "desktop_tools/")]
pub struct ToolsQuery {}

/// The params of `station.desktop_tools_plan`: `{}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "desktop_tools/")]
pub struct ToolsPlanQuery {}

/// The desktop that `station.tools` found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "desktop_tools/")]
pub enum ToolsDesktop {
    Omarchy,
    Hyprland,
    None,
}

/// Where the version text of a tool comes from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "desktop_tools/")]
pub enum VersionSource {
    Executable,
    Package,
    None,
}

/// The result of the probe of one tool.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "desktop_tools/")]
pub enum Probe {
    Ok,
    Missing,
    Unknown,
    Timeout,
    Failed,
}

/// The facts of one tool.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "desktop_tools/")]
pub struct ToolFact {
    /// One of [`tool::ALL`].
    pub id: String,
    pub present: bool,
    /// Bounded version text, or `null`.
    pub version: Option<String>,
    pub version_source: VersionSource,
    /// `null` when unknown.
    pub package_matches_executable: Option<bool>,
    pub probe: Probe,
}

/// The status of a group of facts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "desktop_tools/")]
pub enum FactStatus {
    Ok,
    Unavailable,
    Failed,
}

/// The keyboard layouts of the desktop. No device identifier.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "desktop_tools/")]
pub struct KeyboardFacts {
    pub status: FactStatus,
    pub layouts: Vec<String>,
}

/// The validity of the Fleet Hyprland plugin.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "desktop_tools/")]
pub enum PluginStatus {
    Valid,
    Invalid,
    Unavailable,
    Unknown,
}

/// The plugin facts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "desktop_tools/")]
pub struct PluginFacts {
    pub status: PluginStatus,
}

/// A switch that can be unknown.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "desktop_tools/")]
pub enum Switch {
    On,
    Off,
    Unknown,
}

/// The answer of `station.tools`: parsed facts only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "desktop_tools/")]
pub struct StationTools {
    /// Unix milliseconds.
    pub sampled_at: i64,
    pub desktop: ToolsDesktop,
    pub tools: Vec<ToolFact>,
    pub keyboard: KeyboardFacts,
    pub plugin: PluginFacts,
    pub passwordless_sudo: Switch,
}

/// One package of the expected transaction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "desktop_tools/")]
pub struct ResolvedPackage {
    pub repo: String,
    pub name: String,
    pub version: String,
}

/// The answer of `station.desktop_tools_plan`. `plan_id`, `expires_at` and
/// `command_display` are `null` when `blocked` is set or nothing is
/// installable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "desktop_tools/")]
pub struct DesktopToolsPlan {
    pub plan_id: Option<String>,
    /// Unix milliseconds.
    pub expires_at: Option<i64>,
    pub installable: Vec<String>,
    pub unavailable: Vec<String>,
    pub already_present: Vec<String>,
    pub command_display: Option<String>,
    pub system_upgrade: bool,
    pub blocked: Option<String>,
    pub refused: Vec<String>,
    pub transaction: Vec<ResolvedPackage>,
}

/// The params of `maintenance.open`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "desktop_tools/")]
pub struct MaintenanceOpen {
    pub plan_id: String,
    /// 32 lowercase hex characters from the board.
    pub request_id: String,
    pub cols: u16,
    pub rows: u16,
}

/// The state of a maintenance run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "desktop_tools/")]
pub enum MaintenanceState {
    Admitted,
    Running,
    Exited,
    Lost,
}

/// The post-check of the installed packages against the plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "desktop_tools/")]
pub enum PostCheck {
    Ok,
    TransactionChanged,
}

/// One maintenance run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "desktop_tools/")]
pub struct MaintenanceInfo {
    /// 32 lowercase hex characters.
    pub maintenance_id: String,
    pub request_id: String,
    pub state: MaintenanceState,
    pub exit_code: Option<i32>,
    pub post_check: Option<PostCheck>,
    /// Unix milliseconds.
    pub created_at: i64,
    /// The tool IDs of the plan.
    pub tools: Vec<String>,
}

/// The params of `maintenance.list`: `{}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "desktop_tools/")]
pub struct MaintenanceListQuery {}

/// The answer of `maintenance.list`: newest first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "desktop_tools/")]
pub struct MaintenanceList {
    pub runs: Vec<MaintenanceInfo>,
}

/// The params of `maintenance.attach`. The answer is
/// [`crate::shell::ShellAttached`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "desktop_tools/")]
pub struct MaintenanceAttach {
    pub maintenance_id: String,
}

/// The params of `maintenance.resize`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "desktop_tools/")]
pub struct MaintenanceResize {
    pub maintenance_id: String,
    pub cols: u16,
    pub rows: u16,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::de::DeserializeOwned;
    use serde_json::{Value, json};

    fn round_trip<T>(value: &T) -> Value
    where
        T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug,
    {
        let json = serde_json::to_value(value).unwrap();
        let back: T = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(&back, value);
        json
    }

    fn refuses_an_extra_field<T: DeserializeOwned>(mut good: Value) {
        serde_json::from_value::<T>(good.clone()).expect("the good value parses");
        good["extra"] = json!(1);
        assert!(serde_json::from_value::<T>(good).is_err());
    }

    #[test]
    fn the_station_tools_answer_round_trips_and_keeps_null_fields() {
        let tools = StationTools {
            sampled_at: 1_792_400_000_000,
            desktop: ToolsDesktop::Omarchy,
            tools: vec![
                ToolFact {
                    id: tool::WTYPE.to_owned(),
                    present: true,
                    version: Some("0.4".to_owned()),
                    version_source: VersionSource::Package,
                    package_matches_executable: Some(true),
                    probe: Probe::Ok,
                },
                ToolFact {
                    id: tool::WLRCTL.to_owned(),
                    present: false,
                    version: None,
                    version_source: VersionSource::None,
                    package_matches_executable: None,
                    probe: Probe::Missing,
                },
            ],
            keyboard: KeyboardFacts {
                status: FactStatus::Ok,
                layouts: vec!["us".to_owned()],
            },
            plugin: PluginFacts {
                status: PluginStatus::Unavailable,
            },
            passwordless_sudo: Switch::Unknown,
        };
        let v = round_trip(&tools);
        assert_eq!(v["desktop"], "omarchy");
        assert_eq!(v["passwordless_sudo"], "unknown");
        assert_eq!(v["plugin"]["status"], "unavailable");
        assert_eq!(v["tools"][0]["version_source"], "package");
        assert_eq!(v["tools"][1]["version"], Value::Null);
        assert_eq!(v["tools"][1]["package_matches_executable"], Value::Null);
        assert_eq!(v["tools"][1]["probe"], "missing");
        assert!(v["tools"][1].as_object().unwrap().contains_key("version"));
    }

    #[test]
    fn the_enums_use_their_snake_case_names() {
        fn name<T: Serialize>(v: T) -> Value {
            serde_json::to_value(v).unwrap()
        }
        assert_eq!(name(ToolsDesktop::None), "none");
        assert_eq!(name(ToolsDesktop::Hyprland), "hyprland");
        assert_eq!(name(VersionSource::Executable), "executable");
        assert_eq!(name(Probe::Timeout), "timeout");
        assert_eq!(name(Probe::Failed), "failed");
        assert_eq!(name(FactStatus::Failed), "failed");
        assert_eq!(name(PluginStatus::Invalid), "invalid");
        assert_eq!(name(Switch::On), "on");
        assert_eq!(name(Switch::Off), "off");
        assert_eq!(name(MaintenanceState::Admitted), "admitted");
        assert_eq!(name(MaintenanceState::Lost), "lost");
        assert_eq!(name(PostCheck::TransactionChanged), "transaction_changed");
    }

    #[test]
    fn the_plan_round_trips_with_null_fields_kept() {
        let blocked = DesktopToolsPlan {
            plan_id: None,
            expires_at: None,
            installable: vec![],
            unavailable: vec![tool::WLRCTL.to_owned()],
            already_present: vec![tool::GRIM.to_owned()],
            command_display: None,
            system_upgrade: false,
            blocked: Some(code::TRANSACTION_REFUSED.to_owned()),
            refused: vec!["extra/other".to_owned()],
            transaction: vec![],
        };
        let v = round_trip(&blocked);
        for key in ["plan_id", "expires_at", "command_display"] {
            assert_eq!(v[key], Value::Null, "{key}");
            assert!(v.as_object().unwrap().contains_key(key), "{key}");
        }
        assert_eq!(v["blocked"], "transaction_refused");
        let open = DesktopToolsPlan {
            plan_id: Some("0".repeat(32)),
            expires_at: Some(1_792_400_120_000),
            installable: vec![tool::WTYPE.to_owned()],
            unavailable: vec![],
            already_present: vec![],
            command_display: Some("sudo pacman -S --needed --noconfirm extra/wtype".to_owned()),
            system_upgrade: false,
            blocked: None,
            refused: vec![],
            transaction: vec![ResolvedPackage {
                repo: "extra".to_owned(),
                name: "wtype".to_owned(),
                version: "0.4-1".to_owned(),
            }],
        };
        let v = round_trip(&open);
        assert_eq!(v["blocked"], Value::Null);
        assert_eq!(v["transaction"][0]["repo"], "extra");
    }

    #[test]
    fn the_maintenance_structs_round_trip() {
        let info = MaintenanceInfo {
            maintenance_id: "a".repeat(32),
            request_id: "b".repeat(32),
            state: MaintenanceState::Exited,
            exit_code: Some(0),
            post_check: Some(PostCheck::Ok),
            created_at: 1_792_400_000_000,
            tools: vec![tool::WTYPE.to_owned()],
        };
        let v = round_trip(&info);
        assert_eq!(v["state"], "exited");
        assert_eq!(v["post_check"], "ok");
        let running = MaintenanceInfo {
            state: MaintenanceState::Running,
            exit_code: None,
            post_check: None,
            ..info.clone()
        };
        let v = round_trip(&running);
        assert_eq!(v["exit_code"], Value::Null);
        assert_eq!(v["post_check"], Value::Null);
        round_trip(&MaintenanceList {
            runs: vec![info, running],
        });
        round_trip(&MaintenanceOpen {
            plan_id: "c".repeat(32),
            request_id: "d".repeat(32),
            cols: 100,
            rows: 30,
        });
        round_trip(&MaintenanceAttach {
            maintenance_id: "e".repeat(32),
        });
        round_trip(&MaintenanceResize {
            maintenance_id: "f".repeat(32),
            cols: 80,
            rows: 24,
        });
        round_trip(&ToolsQuery {});
        round_trip(&ToolsPlanQuery {});
        round_trip(&MaintenanceListQuery {});
    }

    #[test]
    fn a_params_struct_refuses_an_extra_field() {
        refuses_an_extra_field::<ToolsQuery>(json!({}));
        refuses_an_extra_field::<ToolsPlanQuery>(json!({}));
        refuses_an_extra_field::<MaintenanceListQuery>(json!({}));
        refuses_an_extra_field::<MaintenanceOpen>(
            json!({"plan_id": "p", "request_id": "r", "cols": 1, "rows": 1}),
        );
        refuses_an_extra_field::<MaintenanceAttach>(json!({"maintenance_id": "m"}));
        refuses_an_extra_field::<MaintenanceResize>(
            json!({"maintenance_id": "m", "cols": 1, "rows": 1}),
        );
    }

    #[test]
    fn the_tool_ids_are_the_nine_fixed_ones_in_order() {
        assert_eq!(
            tool::ALL,
            [
                "wtype",
                "wf_recorder",
                "wlrctl",
                "grim",
                "busctl",
                "jq",
                "hyprland",
                "chromium",
                "notify_send"
            ]
        );
    }

    #[test]
    fn a_hex_id_is_32_lowercase_hex_characters() {
        assert!(is_hex_id(&"0123456789abcdef".repeat(2)));
        assert!(is_hex_id(&"a".repeat(32)));
        assert!(!is_hex_id(""));
        assert!(!is_hex_id(&"a".repeat(31)));
        assert!(!is_hex_id(&"a".repeat(33)));
        assert!(!is_hex_id(&"A".repeat(32)));
        assert!(!is_hex_id(&format!("{}g", "a".repeat(31))));
        assert!(!is_hex_id(&format!("{}/", "a".repeat(31))));
        assert!(!is_hex_id(&format!("{}\u{e9}", "a".repeat(29))));
    }

    #[test]
    fn a_size_is_one_to_one_thousand_cells() {
        assert!(size_ok(1, 1));
        assert!(size_ok(1000, 1000));
        assert!(size_ok(100, 30));
        assert!(!size_ok(0, 30));
        assert!(!size_ok(100, 0));
        assert!(!size_ok(1001, 30));
        assert!(!size_ok(100, 1001));
        assert!(!size_ok(u16::MAX, u16::MAX));
    }

    #[test]
    fn every_code_has_its_fixed_text_and_another_string_has_none() {
        assert_eq!(code::ALL.len(), 8);
        let expected = [
            (
                "transaction_refused",
                "The package transaction needs packages that Fleet Deck does not allow. Nothing was installed.",
            ),
            (
                "transaction_changed",
                "The installed packages differ from the reviewed plan.",
            ),
            (
                "maintenance_ended",
                "The installation has ended. Input is not sent.",
            ),
            (
                "shell_disabled",
                "Board terminals are disabled on this station. Enable them in station settings to install desktop tools.",
            ),
            (
                "maintenance_busy",
                "Another installation is running on this station.",
            ),
            (
                "request_id_conflict",
                "This request was already used for another plan.",
            ),
            (
                "plan_changed",
                "The plan changed or expired. Review the desktop tools again.",
            ),
            (
                "unknown_maintenance",
                "No such installation on this station.",
            ),
        ];
        assert_eq!(code::ALL, expected);
        for (c, t) in expected {
            assert_eq!(code::text(c), Some(t), "{c}");
        }
        assert_eq!(code::text("nope"), None);
        assert_eq!(code::text(""), None);
        assert_eq!(code::text("Transaction_Refused"), None);
    }

    #[test]
    fn the_constants_have_their_fixed_values() {
        assert_eq!(STREAM_PURPOSE, "maintenance:");
        assert_eq!(ELEVATION_LABEL, "Install desktop tools");
        assert_eq!(
            code::RUNNER_LOST_TEXT,
            "The installation runner ended without a result. Check the desktop tools again."
        );
    }
}
