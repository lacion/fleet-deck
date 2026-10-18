//! Desktop control on the station (R13 §6, feature `desktop.v1`): the
//! answers of `desktop.info` and `desktop.act` and the closed action enum.
//! The station runs fixed programs with typed argv; no field is a command.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::capture::CaptureResult;
use crate::ids::SessionId;

/// The desktop that the station found.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "desktop/")]
pub enum DesktopKind {
    /// Hyprland with Omarchy.
    Omarchy,
    /// Hyprland alone: the Omarchy actions are `unsupported`.
    Hyprland,
}

/// One monitor, from `hyprctl -j monitors`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "desktop/")]
pub struct MonitorInfo {
    pub name: String,
    pub description: String,
    pub width: u32,
    pub height: u32,
    /// The scale times 100, rounded.
    pub scale_pct: u32,
    /// The refresh rate, rounded.
    pub refresh_hz: u32,
    pub focused: bool,
    /// `None` when Hyprland does not say.
    pub dpms_on: Option<bool>,
}

/// A wall window that the station opened and still owns.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "desktop/")]
pub struct WallWindow {
    pub display_id: String,
    pub monitor: Option<String>,
    pub address: String,
}

/// The live station settings of the desktop gates.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "desktop/")]
pub struct DesktopAllowed {
    pub desktop: bool,
    /// The live `allow_desktop` and `allow_desktop_capture` (R13 P5a).
    pub capture: bool,
    /// The live `capture_monitors` of the station; empty means every monitor
    /// (R13 P5a).
    #[serde(default)]
    pub capture_monitors: Vec<String>,
    /// Always `false` until the input part.
    pub input: bool,
}

/// The tools that are on the desktop `PATH`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "desktop/")]
pub struct DesktopTools {
    pub wtype: bool,
    /// `ydotool` or `wlrctl`.
    pub pointer: bool,
    /// `gpu-screen-recorder`.
    pub recorder: bool,
    pub grim: bool,
    pub chromium: bool,
    pub notify_send: bool,
}

/// The response to `desktop.info` and the `info` of `StationInfo.desktop`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "desktop/")]
pub struct DesktopInfo {
    pub kind: DesktopKind,
    pub omarchy_version: Option<String>,
    pub monitors: Vec<MonitorInfo>,
    pub locked: Option<bool>,
    pub dnd: Option<bool>,
    pub idle_inhibited: Option<bool>,
    pub walls: Vec<WallWindow>,
    pub allowed: DesktopAllowed,
    pub tools: DesktopTools,
    /// The UTC offset of the station in minutes east of UTC, from one fixed
    /// `date +%z` call (R13 §6D item 3). `None` when the call failed.
    #[serde(default)]
    #[ts(optional = nullable)]
    pub utc_offset_minutes: Option<i32>,
    /// Unix ms at which the station sampled [`Self::utc_offset_minutes`].
    #[serde(default)]
    #[ts(optional = nullable)]
    pub offset_sampled_at: Option<i64>,
    /// The P5a features of this station that the deck and the link support.
    /// The station sends `[]`; the deck fills it (task E).
    #[serde(default)]
    pub features: Vec<String>,
}

/// The params of `desktop.info` (R13 §6D item 4). An empty object is
/// `fresh: false`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "desktop/")]
pub struct DesktopInfoQuery {
    /// Bypass the station cache of 10 seconds.
    #[serde(default)]
    pub fresh: bool,
}

/// The urgency of a notification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "desktop/")]
pub enum Urgency {
    Low,
    Normal,
    Critical,
}

/// The closed list of `desktop.act` actions. An unknown action or an
/// unknown field is a parse error.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
#[ts(export_to = "desktop/")]
pub enum DesktopAction {
    ScreenPower {
        monitor: Option<String>,
        on: bool,
    },
    ScreenLock,
    ScreenDnd {
        on: bool,
    },
    ScreenIdleInhibit {
        on: bool,
    },
    WorkspaceShow {
        monitor: String,
        workspace: u8,
    },
    /// `url` is the pair page of the deck: it holds no secret.
    WallOpen {
        display_id: String,
        monitor: String,
        workspace: Option<u8>,
        url: String,
    },
    WallClose {
        display_id: String,
    },
    WallReload {
        display_id: String,
        url: String,
    },
    Notify {
        title: String,
        body: String,
        urgency: Urgency,
    },
    BrowserOpen {
        url: String,
        monitor: Option<String>,
        session: Option<SessionId>,
    },
    WindowClose {
        address: String,
    },
    /// R13 P5a: one PNG of one monitor (needs `desktop_capture.v1`).
    ScreenCapture {
        monitor: String,
    },
    /// R13 P6 §4.5: close a coordinator preview by its opaque `window_id`
    /// (needs `desktop_mcp.v1`; a coordinator authority only). The hash of
    /// the action covers the handle; the deck never learns the address.
    PreviewClose {
        window_id: String,
    },
}

/// The action of the owner's HTTP relay (R13 §6D item 6): the closed list of
/// `desktop.act` actions without a wall URL. The deck builds the pair page
/// URL of `wall_open` and `wall_reload` itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
#[ts(export_to = "desktop/")]
pub enum DesktopActBody {
    ScreenPower {
        monitor: Option<String>,
        on: bool,
    },
    ScreenLock,
    ScreenDnd {
        on: bool,
    },
    ScreenIdleInhibit {
        on: bool,
    },
    WorkspaceShow {
        monitor: String,
        workspace: u8,
    },
    WallOpen {
        display_id: String,
        monitor: String,
        workspace: Option<u8>,
    },
    WallClose {
        display_id: String,
    },
    WallReload {
        display_id: String,
    },
    Notify {
        title: String,
        body: String,
        urgency: Urgency,
    },
    BrowserOpen {
        url: String,
        monitor: Option<String>,
        session: Option<SessionId>,
    },
    WindowClose {
        address: String,
    },
    /// R13 P5a: one PNG of one monitor (needs `desktop_capture.v1`).
    ScreenCapture {
        monitor: String,
    },
}

/// The response to `desktop.act`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "desktop/")]
pub struct DesktopActResult {
    /// The window address of `wall_open` and `browser_open`.
    pub address: Option<String>,
    /// The opaque handle of a coordinator preview (`preview_open`, R13 P6
    /// §4.5); the station sends `address: None` then.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub window_id: Option<String>,
    /// The fresh info after the action.
    pub info: DesktopInfo,
    /// The screenshot result; required for a `screen_capture` success.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub capture: Option<CaptureResult>,
    /// Absent means true.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub info_fresh: Option<bool>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_action_has_a_snake_case_tag_and_refuses_unknown_fields() {
        let a: DesktopAction =
            serde_json::from_str(r#"{"action":"screen_dnd","on":true}"#).unwrap();
        assert_eq!(a, DesktopAction::ScreenDnd { on: true });
        let lock: DesktopAction = serde_json::from_str(r#"{"action":"screen_lock"}"#).unwrap();
        assert_eq!(lock, DesktopAction::ScreenLock);
        assert!(
            serde_json::from_str::<DesktopAction>(r#"{"action":"screen_dnd","on":true,"x":1}"#)
                .is_err()
        );
        assert!(serde_json::from_str::<DesktopAction>(r#"{"action":"exec","cmd":"x"}"#).is_err());
    }

    #[test]
    fn screen_capture_parses_into_both_enums_and_serializes_back() {
        let json = r#"{"action":"screen_capture","monitor":"DP-1"}"#;
        let a: DesktopAction = serde_json::from_str(json).unwrap();
        assert_eq!(
            a,
            DesktopAction::ScreenCapture {
                monitor: "DP-1".into()
            }
        );
        assert_eq!(serde_json::to_string(&a).unwrap(), json);
        let b: DesktopActBody = serde_json::from_str(json).unwrap();
        assert_eq!(
            b,
            DesktopActBody::ScreenCapture {
                monitor: "DP-1".into()
            }
        );
        assert_eq!(serde_json::to_string(&b).unwrap(), json);
    }

    #[test]
    fn preview_close_has_its_wire_form_and_is_not_an_owner_body() {
        let json = r#"{"action":"preview_close","window_id":"ab12"}"#;
        let a: DesktopAction = serde_json::from_str(json).unwrap();
        assert_eq!(
            a,
            DesktopAction::PreviewClose {
                window_id: "ab12".into()
            }
        );
        assert_eq!(serde_json::to_string(&a).unwrap(), json);
        // The owner's HTTP body does not offer it.
        assert!(serde_json::from_str::<DesktopActBody>(json).is_err());
        assert!(
            serde_json::from_str::<DesktopAction>(
                r#"{"action":"preview_close","window_id":"a","address":"0x1"}"#
            )
            .is_err()
        );
    }

    #[test]
    fn an_act_result_skips_an_absent_window_id() {
        let info: DesktopInfo = serde_json::from_str(
            r#"{"kind":"omarchy","omarchy_version":null,"monitors":[],"locked":null,
            "dnd":null,"idle_inhibited":null,"walls":[],
            "allowed":{"desktop":true,"capture":false,"input":false},
            "tools":{"wtype":false,"pointer":false,"recorder":false,"grim":true,
                     "chromium":false,"notify_send":false}}"#,
        )
        .unwrap();
        let mut result = DesktopActResult {
            address: None,
            window_id: None,
            info,
            capture: None,
            info_fresh: None,
        };
        let v = serde_json::to_value(&result).unwrap();
        assert!(!v.as_object().unwrap().contains_key("window_id"));
        result.window_id = Some("w1".into());
        let v = serde_json::to_value(&result).unwrap();
        assert_eq!(v["window_id"], "w1");
        assert_eq!(v["address"], serde_json::Value::Null);
        assert_eq!(
            serde_json::from_value::<DesktopActResult>(v).unwrap(),
            result
        );
        // An old result without the key parses.
        let mut old = serde_json::to_value(&result).unwrap();
        old.as_object_mut().unwrap().remove("window_id");
        assert_eq!(
            serde_json::from_value::<DesktopActResult>(old)
                .unwrap()
                .window_id,
            None
        );
    }

    #[test]
    fn an_old_info_without_p5a_fields_parses() {
        let json = r#"{
            "kind":"omarchy","omarchy_version":null,"monitors":[],"locked":null,
            "dnd":null,"idle_inhibited":null,"walls":[],
            "allowed":{"desktop":true,"capture":false,"input":false},
            "tools":{"wtype":false,"pointer":false,"recorder":false,"grim":true,
                     "chromium":false,"notify_send":false}
        }"#;
        let info: DesktopInfo = serde_json::from_str(json).unwrap();
        assert!(info.features.is_empty());
        assert!(info.allowed.capture_monitors.is_empty());
    }
}
