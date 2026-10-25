//! The total deadlines of the desktop methods (R13 §6D item 9): 90 s for a
//! wall open, a wall reload and a browser launch, and 30 s for every other
//! action and for `desktop.info`. The deadline covers the whole call,
//! including the wait for the per-display lock. A call that passes it is
//! dropped; its outcome on the desktop is then unknown, and the deck never
//! retries it by itself.

use std::time::Duration;

use fleetdeck_proto::desktop::DesktopAction;

/// The deadline of a wall open, a wall reload and a browser launch.
pub const LONG: Duration = Duration::from_secs(90);
/// The deadline of every other action and of `desktop.info`.
pub const SHORT: Duration = Duration::from_secs(30);
/// The failure text of an action that passed its deadline.
pub const ACT_TIMEOUT: &str = "The desktop action did not finish in time.";
/// The failure text of an info call that passed its deadline.
pub const INFO_TIMEOUT: &str = "The desktop info did not finish in time.";

/// The two deadlines of one adapter. A test makes them short.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    pub long: Duration,
    pub short: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            long: LONG,
            short: SHORT,
        }
    }
}

impl Limits {
    /// The deadline of `action`.
    #[must_use]
    pub fn of(self, action: &DesktopAction) -> Duration {
        match action {
            DesktopAction::WallOpen { .. }
            | DesktopAction::WallReload { .. }
            | DesktopAction::BrowserOpen { .. } => self.long,
            _ => self.short,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_launch_actions_get_90_seconds_and_the_rest_30() {
        let limits = Limits::default();
        let url = "https://deck/display#/display-pair".to_owned();
        let long = [
            DesktopAction::WallOpen {
                display_id: "d".into(),
                monitor: "DP-1".into(),
                workspace: None,
                url: url.clone(),
            },
            DesktopAction::WallReload {
                display_id: "d".into(),
                url: url.clone(),
            },
            DesktopAction::BrowserOpen {
                url,
                monitor: None,
                session: None,
            },
        ];
        for a in &long {
            assert_eq!(limits.of(a), Duration::from_secs(90), "{a:?}");
        }
        let short = [
            DesktopAction::ScreenLock,
            DesktopAction::ScreenDnd { on: true },
            DesktopAction::WallClose {
                display_id: "d".into(),
            },
            DesktopAction::WindowClose {
                address: "0x1".into(),
            },
        ];
        for a in &short {
            assert_eq!(limits.of(a), Duration::from_secs(30), "{a:?}");
        }
    }
}
