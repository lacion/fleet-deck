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

/// A preview window that an agent opened on the station (R13 P6). It holds
/// no address, no PID, no URL and no profile: `window_id` is the opaque
/// handle of the station.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "desktop/")]
pub struct PreviewWindow {
    pub window_id: String,
    pub monitor: Option<String>,
    /// The label of the agent caller (at most 64 characters).
    pub caller_label: String,
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
    /// `wf-recorder` (R13 P5b).
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
    /// The previews that agents opened for the deck that asks (R13 P6). The
    /// station fills it only on a link with `desktop_mcp.v1`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub previews: Vec<PreviewWindow>,
    /// R13 P7 §7.6: the canonical addresses of the up interfaces of this
    /// machine (at most 16, loopback and link-local excluded). The station
    /// sends it only on a link with `desktop_input_mcp.v1`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub host_addresses: Vec<String>,
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
    /// R13 P5b: one H.264 MP4 recording of one monitor, 1 to 120 seconds
    /// (needs `desktop_recording.v1`).
    ScreenRecord {
        monitor: String,
        seconds: u32,
    },
    /// R13 P6 §4.5: close a coordinator preview by its opaque `window_id`
    /// (needs `desktop_mcp.v1`; a coordinator authority only). The hash of
    /// the action covers the handle; the deck never learns the address.
    PreviewClose {
        window_id: String,
    },
    /// R13 P7 §6.2: one key chord into a preview that the caller owns (needs
    /// `desktop_input_mcp.v1` and a coordinator authority). The target
    /// resolves `window_id`; the hash covers the handle, never an address.
    InputKeyPress {
        window_id: String,
        chord: String,
    },
    /// R13 P7 §6.2: typed text into a preview that the caller owns.
    InputTypeText {
        window_id: String,
        text: String,
    },
    /// R13 P7 §6.2: one pointer movement inside a preview that the caller
    /// owns. `x` and `y` are logical coordinates relative to the preview.
    InputPointerMove {
        window_id: String,
        x: u32,
        y: u32,
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
    /// R13 P5b: one H.264 MP4 recording of one monitor (needs
    /// `desktop_recording.v1`). `seconds` is a JSON number so that the route
    /// can answer a fraction or an out-of-range value with `recording_duration`.
    ScreenRecord {
        monitor: String,
        #[ts(type = "number")]
        seconds: serde_json::Number,
    },
    /// R13 P6: the owner closes a preview that an agent opened (needs
    /// `desktop_mcp.v1`).
    PreviewClose {
        window_id: String,
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
    fn preview_close_has_its_wire_form_in_both_enums() {
        let json = r#"{"action":"preview_close","window_id":"ab12"}"#;
        let a: DesktopAction = serde_json::from_str(json).unwrap();
        assert_eq!(
            a,
            DesktopAction::PreviewClose {
                window_id: "ab12".into()
            }
        );
        assert_eq!(serde_json::to_string(&a).unwrap(), json);
        // The owner's HTTP body offers it too (R13 P6 follow-up).
        let b: DesktopActBody = serde_json::from_str(json).unwrap();
        assert_eq!(
            b,
            DesktopActBody::PreviewClose {
                window_id: "ab12".into()
            }
        );
        assert_eq!(serde_json::to_string(&b).unwrap(), json);
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

    fn info_json(previews: &str) -> String {
        format!(
            r#"{{"kind":"omarchy","omarchy_version":null,"monitors":[],"locked":null,
            "dnd":null,"idle_inhibited":null,"walls":[]{previews},
            "allowed":{{"desktop":true,"capture":false,"input":false}},
            "tools":{{"wtype":false,"pointer":false,"recorder":false,"grim":true,
                     "chromium":false,"notify_send":false}}}}"#
        )
    }

    #[test]
    fn previews_are_absent_when_empty_and_parse_when_present() {
        let info: DesktopInfo = serde_json::from_str(&info_json("")).unwrap();
        assert!(info.previews.is_empty());
        let v = serde_json::to_value(&info).unwrap();
        assert!(!v.as_object().unwrap().contains_key("previews"));

        let with = info_json(
            r#","previews":[{"window_id":"w1","monitor":"DP-1","caller_label":"agent"}]"#,
        );
        let info: DesktopInfo = serde_json::from_str(&with).unwrap();
        assert_eq!(
            info.previews,
            vec![PreviewWindow {
                window_id: "w1".into(),
                monitor: Some("DP-1".into()),
                caller_label: "agent".into(),
            }]
        );
        let v = serde_json::to_value(&info).unwrap();
        assert_eq!(v["previews"][0]["window_id"], "w1");
        assert_eq!(v["previews"][0]["monitor"], "DP-1");
    }

    #[test]
    fn host_addresses_are_absent_when_empty_and_parse_when_present() {
        let info: DesktopInfo = serde_json::from_str(&info_json("")).unwrap();
        assert!(info.host_addresses.is_empty());
        let v = serde_json::to_value(&info).unwrap();
        assert!(!v.as_object().unwrap().contains_key("host_addresses"));

        let with = info_json(r#","host_addresses":["192.0.2.7","2001:db8::1"]"#);
        let info: DesktopInfo = serde_json::from_str(&with).unwrap();
        assert_eq!(info.host_addresses, ["192.0.2.7", "2001:db8::1"]);
        let v = serde_json::to_value(&info).unwrap();
        assert_eq!(v["host_addresses"][1], "2001:db8::1");
    }

    #[test]
    fn the_input_actions_have_their_wire_forms_and_are_not_owner_actions() {
        let window = "0123456789abcdef0123456789abcdef";
        for (json, action) in [
            (
                format!(
                    r#"{{"action":"input_key_press","window_id":"{window}","chord":"Ctrl+a"}}"#
                ),
                DesktopAction::InputKeyPress {
                    window_id: window.into(),
                    chord: "Ctrl+a".into(),
                },
            ),
            (
                format!(r#"{{"action":"input_type_text","window_id":"{window}","text":"hi"}}"#),
                DesktopAction::InputTypeText {
                    window_id: window.into(),
                    text: "hi".into(),
                },
            ),
            (
                format!(r#"{{"action":"input_pointer_move","window_id":"{window}","x":3,"y":4}}"#),
                DesktopAction::InputPointerMove {
                    window_id: window.into(),
                    x: 3,
                    y: 4,
                },
            ),
        ] {
            let parsed: DesktopAction = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed, action);
            assert_eq!(serde_json::to_string(&action).unwrap(), json);
            // The owner's HTTP body never offers an input action (§6.2).
            assert!(serde_json::from_str::<DesktopActBody>(&json).is_err());
        }
        // Unknown fields are refused, and so is an address instead of a handle.
        for bad in [
            format!(r#"{{"action":"input_key_press","window_id":"{window}","chord":"a","x":1}}"#),
            format!(r#"{{"action":"input_type_text","window_id":"{window}"}}"#),
            format!(r#"{{"action":"input_pointer_move","window_id":"{window}","x":-1,"y":0}}"#),
            format!(r#"{{"action":"input_pointer_move","window_id":"{window}","x":1.5,"y":0}}"#),
            format!(r#"{{"action":"input_pointer_click","window_id":"{window}"}}"#),
            r#"{"action":"input_key_press","address":"0x1","chord":"a"}"#.to_owned(),
        ] {
            assert!(
                serde_json::from_str::<DesktopAction>(&bad).is_err(),
                "{bad}"
            );
        }
    }
}
