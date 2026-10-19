//! The grammar of the input actions on the target (R13 P7 §4, §5.1, §5.2,
//! §8.1; R13 P7b §9.1): the validation with the proto validators, and the fixed `wtype`
//! argv of a chord. The text never becomes an argv: it travels on a private
//! stdin pipe (argv `-`).

use fleetdeck_proto::coordinator_desktop::{
    KeyPressArguments, PointerMoveArguments, TypeTextArguments,
};
use fleetdeck_proto::coordinator_desktop_text::code as p6_code;
use fleetdeck_proto::desktop::DesktopAction;

use crate::desktop::Fail;
use crate::desktop::coordinator::p6_fail;

/// The request id that the validators need. The target has no request id: the
/// deck checked it and the action hash covers the action.
const PLACEHOLDER_REQUEST: &str = "r";

/// What an input action does, with its fixed data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Plan {
    /// `wtype` with this argv; stdin is null.
    Key { argv: Vec<String> },
    /// `wtype -`, and the validated UTF-8 bytes on stdin.
    Text { bytes: Vec<u8> },
    /// `hyprctl dispatch movecursor` at the window-relative point.
    Pointer { x: u32, y: u32 },
}

impl Plan {
    /// The text of the fixed disclosure of §8.2 for `label`; `any` is true for
    /// the `InputAny*` variants (R13 P7b §14: another application window).
    pub(crate) fn disclosure(&self, label: &str, any: bool) -> String {
        match (self, any) {
            (Self::Key { .. } | Self::Text { .. }, false) => {
                format!("Fleet Deck is sending keyboard input for {label} to an agent preview.")
            }
            (Self::Key { .. } | Self::Text { .. }, true) => {
                format!(
                    "Fleet Deck is sending keyboard input for {label} to an application window."
                )
            }
            (Self::Pointer { .. }, false) => {
                format!("Fleet Deck is moving the pointer for {label} in an agent preview.")
            }
            (Self::Pointer { .. }, true) => {
                format!("Fleet Deck is moving the pointer for {label} in an application window.")
            }
        }
    }
}

/// Whether `action` is one of the input actions: the three of P7, the click
/// of a preview and the four `any_window` variants (R13 P7b §9.1).
#[must_use]
pub(crate) fn is_input(action: &DesktopAction) -> bool {
    matches!(
        action,
        DesktopAction::InputKeyPress { .. }
            | DesktopAction::InputTypeText { .. }
            | DesktopAction::InputPointerMove { .. }
            | DesktopAction::InputPointerClick { .. }
            | DesktopAction::InputAnyKeyPress { .. }
            | DesktopAction::InputAnyTypeText { .. }
            | DesktopAction::InputAnyPointerMove { .. }
            | DesktopAction::InputAnyPointerClick { .. }
    )
}

/// Whether `action` is one of the four `InputAny*` variants.
#[must_use]
pub(crate) fn is_any(action: &DesktopAction) -> bool {
    matches!(
        action,
        DesktopAction::InputAnyKeyPress { .. }
            | DesktopAction::InputAnyTypeText { .. }
            | DesktopAction::InputAnyPointerMove { .. }
            | DesktopAction::InputAnyPointerClick { .. }
    )
}

/// Whether `action` is one of the two click variants.
#[must_use]
pub(crate) fn is_click(action: &DesktopAction) -> bool {
    matches!(
        action,
        DesktopAction::InputPointerClick { .. } | DesktopAction::InputAnyPointerClick { .. }
    )
}

fn invalid() -> Fail {
    p6_fail(p6_code::INVALID_PARAMS)
}

/// Checks the arguments of an input action with the proto validators. `Ok`
/// for an action that is not an input action.
///
/// # Errors
/// `invalid_params` (the P6 text); the text never names the value.
pub(crate) fn check(action: &DesktopAction) -> Result<(), Fail> {
    let checked = match action {
        DesktopAction::InputKeyPress { window_id, chord }
        | DesktopAction::InputAnyKeyPress { window_id, chord } => KeyPressArguments {
            request_id: PLACEHOLDER_REQUEST.to_owned(),
            station: None,
            window_id: window_id.clone(),
            chord: chord.clone(),
        }
        .validate(),
        DesktopAction::InputTypeText { window_id, text }
        | DesktopAction::InputAnyTypeText { window_id, text } => TypeTextArguments {
            request_id: PLACEHOLDER_REQUEST.to_owned(),
            station: None,
            window_id: window_id.clone(),
            text: text.clone(),
        }
        .validate(),
        DesktopAction::InputPointerMove { window_id, x, y }
        | DesktopAction::InputAnyPointerMove { window_id, x, y } => PointerMoveArguments {
            request_id: PLACEHOLDER_REQUEST.to_owned(),
            station: None,
            window_id: window_id.clone(),
            x: *x,
            y: *y,
        }
        .validate(),
        _ => return Ok(()),
    };
    checked.map_err(|_| invalid())
}

/// The `window_id` and the plan of an input action, after [`check`].
///
/// # Errors
/// `invalid_params` for an action that is not valid or not an input action.
pub(crate) fn plan(action: &DesktopAction) -> Result<(String, Plan), Fail> {
    check(action)?;
    match action {
        DesktopAction::InputKeyPress { window_id, chord }
        | DesktopAction::InputAnyKeyPress { window_id, chord } => {
            let argv = chord_argv(chord).ok_or_else(invalid)?;
            Ok((window_id.clone(), Plan::Key { argv }))
        }
        DesktopAction::InputTypeText { window_id, text }
        | DesktopAction::InputAnyTypeText { window_id, text } => Ok((
            window_id.clone(),
            Plan::Text {
                bytes: text.clone().into_bytes(),
            },
        )),
        DesktopAction::InputPointerMove { window_id, x, y }
        | DesktopAction::InputAnyPointerMove { window_id, x, y } => {
            Ok((window_id.clone(), Plan::Pointer { x: *x, y: *y }))
        }
        _ => Err(invalid()),
    }
}

/// The XKB name of a named key of §5.1. A letter or a digit is its own name.
fn keysym(key: &str) -> &str {
    match key {
        "Space" => "space",
        "PageUp" => "Page_Up",
        "PageDown" => "Page_Down",
        other => other,
    }
}

/// The fixed `wtype` argv of a chord (§5.1): `-M ctrl` and `-M shift` when
/// requested, `-k <keysym>`, then the modifiers released in reverse order
/// with `-m`. `None` for a text that is not a chord of the grammar.
#[must_use]
pub(crate) fn chord_argv(chord: &str) -> Option<Vec<String>> {
    let (ctrl, rest) = chord
        .strip_prefix("Ctrl+")
        .map_or((false, chord), |r| (true, r));
    let (shift, key) = rest
        .strip_prefix("Shift+")
        .map_or((false, rest), |r| (true, r));
    let valid = KeyPressArguments {
        request_id: PLACEHOLDER_REQUEST.to_owned(),
        station: None,
        window_id: "0".repeat(32),
        chord: chord.to_owned(),
    }
    .validate()
    .is_ok();
    if !valid {
        return None;
    }
    let mut argv: Vec<String> = Vec::new();
    for (on, name) in [(ctrl, "ctrl"), (shift, "shift")] {
        if on {
            argv.extend(["-M".to_owned(), name.to_owned()]);
        }
    }
    argv.extend(["-k".to_owned(), keysym(key).to_owned()]);
    for (on, name) in [(shift, "shift"), (ctrl, "ctrl")] {
        if on {
            argv.extend(["-m".to_owned(), name.to_owned()]);
        }
    }
    Some(argv)
}

/// The argv of a text: `-` and nothing else.
#[must_use]
pub(crate) fn text_argv() -> Vec<String> {
    vec!["-".to_owned()]
}

#[cfg(test)]
#[path = "grammar_tests.rs"]
mod tests;
