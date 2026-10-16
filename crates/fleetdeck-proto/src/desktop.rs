//! Desktop control on the station (R13 §6, feature `desktop.v1`): the
//! answers of `desktop.info` and `desktop.act` and the closed action enum.
//! The station runs fixed programs with typed argv; no field is a command.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

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
    /// Always `false` until the capture part.
    pub capture: bool,
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
}

/// The response to `desktop.act`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "desktop/")]
pub struct DesktopActResult {
    /// The window address of `wall_open` and `browser_open`.
    pub address: Option<String>,
    /// The fresh info after the action.
    pub info: DesktopInfo,
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
}
