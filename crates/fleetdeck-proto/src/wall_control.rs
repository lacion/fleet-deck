//! Wall controls (B17 §3.1): transient steps that the owner sends to one
//! wall display, and the remote state that the deck keeps for them.
//! Navigation (open, back, the next or previous view) is not a control: it
//! is an owner directive (`RemoteNavigate`).

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::display_directive::{DirectiveBody, DirectiveView, SessionRef};

/// The most controls that the deck keeps for each display (for the
/// incremental poll of a remote; the checkpoint does not expire).
pub const MAX_CONTROLS_KEPT: usize = 32;
/// The age after which the deck drops a kept control, in milliseconds.
pub const CONTROL_KEEP_MS: u64 = 30_000;
/// The most controls and navigations for each display in one second.
pub const MAX_CONTROLS_PER_SECOND: usize = 10;
/// How long auto-focus and the ambient timer wait after a control or a
/// navigation, in milliseconds (owner question Q4).
pub const REMOTE_HOLD_MS: u64 = 120_000;
/// The scroll range of a frame with the long fit (8 screens tall, so 7
/// screens to scroll), in tenths of a screen.
pub const FRAME_TENTHS_MAX: u8 = 70;

/// A scroll area of the wall. A control for an area that the wall does not
/// show now does nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "display/")]
pub enum WallArea {
    Needs,
    Stream,
    Chat,
    Changes,
    Artifact,
    Page,
}

/// One scroll step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "display/")]
pub enum ScrollStep {
    LineUp,
    LineDown,
    PageUp,
    PageDown,
    Top,
    Bottom,
}

/// One control. A closed list; an unknown op or field is a parse error.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
#[ts(export_to = "display/")]
pub enum WallControl {
    Scroll {
        area: WallArea,
        step: ScrollStep,
    },
    /// The selected item, absolute: the sender computes it. `None` clears
    /// the selection. The deck checks that the session is inside the
    /// display's effective scope.
    Select {
        item: Option<SessionRef>,
    },
}

/// The body of `POST /api/displays/{id}/control`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "display/")]
pub struct WallControlBody {
    pub control: WallControl,
}

/// The body of `POST /api/displays/{id}/remote/navigate`: the directive that
/// the remote computed for "Open", "Back" or the next or previous view.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export_to = "display/")]
pub struct RemoteNavigate {
    pub directive: DirectiveBody,
}

/// The checkpoint of one directive generation: the result of every control
/// of that generation, in `seq` order. A new generation starts from
/// `WallRemoteState::default()` (no selection, the frame at the top).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "display/")]
pub struct WallRemoteState {
    pub selected: Option<SessionRef>,
    /// The position of a frame with the long fit (a page, or an HTML
    /// artifact), in tenths of a screen, `0..=FRAME_TENTHS_MAX`.
    pub frame_tenths: u8,
}

impl WallRemoteState {
    /// The state after `control`: `select` sets `selected`; `scroll` of the
    /// area `page` or `artifact` moves `frame_tenths` (`line_up` -1,
    /// `line_down` +1, `page_up` -9, `page_down` +9, `top` 0, `bottom` 70,
    /// clamped to 0..=70); every other scroll leaves the state as it is.
    #[must_use]
    pub fn after(&self, control: &WallControl) -> Self {
        let mut next = self.clone();
        match control {
            WallControl::Select { item } => next.selected.clone_from(item),
            WallControl::Scroll { area, step } => {
                if matches!(area, WallArea::Page | WallArea::Artifact) {
                    let max = i16::from(FRAME_TENTHS_MAX);
                    let now = i16::from(self.frame_tenths);
                    let moved = match step {
                        ScrollStep::LineUp => now - 1,
                        ScrollStep::LineDown => now + 1,
                        ScrollStep::PageUp => now - 9,
                        ScrollStep::PageDown => now + 9,
                        ScrollStep::Top => 0,
                        ScrollStep::Bottom => max,
                    };
                    // The clamp keeps the value in 0..=70, so it fits a u8.
                    next.frame_tenths = u8::try_from(moved.clamp(0, max)).unwrap_or(0);
                }
            }
        }
        next
    }
}

/// What a frame reports.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[ts(export_to = "display/")]
pub enum WallEvent {
    /// The state now: sent when a display socket attaches and after it
    /// lagged. It uses the current `seq` (no new number), never sets the
    /// hold, and is never kept.
    Sync,
    /// The owner remote stored a new directive with `generation`: read the
    /// directive now, and hold.
    Navigate,
    /// A control: apply it, and hold.
    Control { control: WallControl },
}

/// One relayed frame. `state` is the checkpoint of `generation` after this
/// frame, so a receiver that missed frames still has the exact selection
/// and frame position.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "display/")]
pub struct WallControlFrame {
    /// The id of the relay (a new one at each deck start): 32 random bytes
    /// as hex (`store::auth::random_token`).
    pub incarnation: String,
    pub seq: u64,
    pub generation: u64,
    pub event: WallEvent,
    pub state: WallRemoteState,
}

/// The answer of the control and navigate POSTs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "display/")]
pub struct WallControlAck {
    pub incarnation: String,
    pub seq: u64,
    pub generation: u64,
}

/// The link of the wall page of a display, as the deck sees it now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export_to = "display/")]
pub enum WallLink {
    /// No socket of this display is open.
    Offline,
    /// Sockets are open, and none declared `wall_control.v1`.
    Older,
    /// At least one socket declared `wall_control.v1`.
    Ready,
}

/// `GET /api/displays/{id}/remote?after=<seq>`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export_to = "display/")]
pub struct WallRemoteView {
    pub directive: DirectiveView,
    pub link: WallLink,
    pub incarnation: String,
    /// The sequence number of the newest control or navigation (0: none
    /// since the deck started).
    pub seq: u64,
    /// The checkpoint of `directive.generation` (the default when no control
    /// of this generation exists).
    pub state: WallRemoteState,
    /// What is left of the hold, in milliseconds (0: no hold).
    pub hold_left_ms: u64,
    /// True when `controls` holds every frame with a `seq` above `after`
    /// (no gap). False when `after` is above `seq` or older frames expired.
    pub complete: bool,
    /// The kept frames with a `seq` above `after`, oldest first (`navigate`
    /// and `control` events only).
    pub controls: Vec<WallControlFrame>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(id: &str) -> SessionRef {
        SessionRef {
            station_id: "st".into(),
            session_id: id.into(),
        }
    }

    fn scroll(area: WallArea, step: ScrollStep) -> WallControl {
        WallControl::Scroll { area, step }
    }

    fn at(tenths: u8) -> WallRemoteState {
        WallRemoteState {
            selected: None,
            frame_tenths: tenths,
        }
    }

    #[test]
    fn wall_control_ops_are_closed() {
        for bad in [
            r#"{"op":"open"}"#,
            r#"{"op":"back"}"#,
            r#"{"op":"mode"}"#,
            r#"{"op":"other"}"#,
            r#"{"op":"scroll","area":"chat","step":"top","extra":1}"#,
            r#"{"op":"scroll","area":"/etc/passwd","step":"top"}"#,
            r#"{"op":"scroll","area":"chat","step":"sideways"}"#,
            r#"{"op":"select","item":null,"extra":1}"#,
        ] {
            assert!(serde_json::from_str::<WallControl>(bad).is_err(), "{bad}");
        }
        let controls = [
            scroll(WallArea::Needs, ScrollStep::LineUp),
            scroll(WallArea::Page, ScrollStep::Bottom),
            WallControl::Select { item: None },
            WallControl::Select {
                item: Some(session("s1")),
            },
        ];
        for control in controls {
            let text = serde_json::to_string(&control).unwrap();
            let back: WallControl = serde_json::from_str(&text).unwrap();
            assert_eq!(back, control, "{text}");
        }
        let body = r#"{"control":{"op":"scroll","area":"stream","step":"page_down"}}"#;
        assert!(serde_json::from_str::<WallControlBody>(body).is_ok());
        assert!(
            serde_json::from_str::<WallControlBody>(r#"{"control":{"op":"scroll"},"x":1}"#)
                .is_err()
        );
    }

    #[test]
    fn remote_state_after_moves_and_clamps_frame_tenths() {
        for area in [WallArea::Page, WallArea::Artifact] {
            let from = |tenths: u8, step: ScrollStep| at(tenths).after(&scroll(area, step));
            assert_eq!(from(10, ScrollStep::LineUp), at(9));
            assert_eq!(from(10, ScrollStep::LineDown), at(11));
            assert_eq!(from(30, ScrollStep::PageUp), at(21));
            assert_eq!(from(30, ScrollStep::PageDown), at(39));
            assert_eq!(from(30, ScrollStep::Top), at(0));
            assert_eq!(from(30, ScrollStep::Bottom), at(70));
            // The limits.
            assert_eq!(from(0, ScrollStep::LineUp), at(0));
            assert_eq!(from(5, ScrollStep::PageUp), at(0));
            assert_eq!(from(70, ScrollStep::LineDown), at(70));
            assert_eq!(from(65, ScrollStep::PageDown), at(70));
        }
        // Another area leaves the state as it is.
        for area in [
            WallArea::Needs,
            WallArea::Stream,
            WallArea::Chat,
            WallArea::Changes,
        ] {
            let state = WallRemoteState {
                selected: Some(session("s1")),
                frame_tenths: 12,
            };
            assert_eq!(state.after(&scroll(area, ScrollStep::Bottom)), state);
        }
        // A scroll keeps the selection.
        let state = WallRemoteState {
            selected: Some(session("s1")),
            frame_tenths: 0,
        };
        let moved = state.after(&scroll(WallArea::Page, ScrollStep::PageDown));
        assert_eq!(moved.selected, Some(session("s1")));
        assert_eq!(moved.frame_tenths, 9);
    }

    #[test]
    fn remote_state_after_select_sets_and_clears() {
        let state = at(12);
        let set = state.after(&WallControl::Select {
            item: Some(session("s2")),
        });
        assert_eq!(set.selected, Some(session("s2")));
        assert_eq!(set.frame_tenths, 12);
        let cleared = set.after(&WallControl::Select { item: None });
        assert_eq!(cleared.selected, None);
        assert_eq!(cleared.frame_tenths, 12);
    }
}
