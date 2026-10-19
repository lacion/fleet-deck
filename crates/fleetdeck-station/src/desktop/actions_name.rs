//! The log name of a desktop action. Moved out of `actions.rs` for the module
//! size rule (R13 P7b task F); no behavior change.

use fleetdeck_proto::desktop::DesktopAction;

/// The kind of an action for the log: never its arguments.
pub(in crate::desktop) fn name(action: &DesktopAction) -> &'static str {
    match action {
        DesktopAction::ScreenPower { .. } => "screen_power",
        DesktopAction::ScreenLock => "screen_lock",
        DesktopAction::ScreenDnd { .. } => "screen_dnd",
        DesktopAction::ScreenIdleInhibit { .. } => "screen_idle_inhibit",
        DesktopAction::WorkspaceShow { .. } => "workspace_show",
        DesktopAction::WallOpen { .. } => "wall_open",
        DesktopAction::WallClose { .. } => "wall_close",
        DesktopAction::WallReload { .. } => "wall_reload",
        DesktopAction::Notify { .. } => "notify",
        DesktopAction::BrowserOpen { .. } => "browser_open",
        DesktopAction::WindowClose { .. } => "window_close",
        DesktopAction::ScreenCapture { .. } => "screen_capture",
        DesktopAction::ScreenRecord { .. } => "screen_record",
        DesktopAction::PreviewClose { .. } => "preview_close",
        DesktopAction::InputKeyPress { .. } => "input_key_press",
        DesktopAction::InputTypeText { .. } => "input_type_text",
        DesktopAction::InputPointerMove { .. } => "input_pointer_move",
        DesktopAction::InputPointerClick { .. } => "input_pointer_click",
        DesktopAction::InputAnyKeyPress { .. } => "input_any_key_press",
        DesktopAction::InputAnyTypeText { .. } => "input_any_type_text",
        DesktopAction::InputAnyPointerMove { .. } => "input_any_pointer_move",
        DesktopAction::InputAnyPointerClick { .. } => "input_any_pointer_click",
    }
}
