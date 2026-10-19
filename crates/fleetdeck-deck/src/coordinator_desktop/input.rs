//! The input tools `key_press`, `type_text`, `pointer_move` and
//! `pointer_click` on the deck (R13 P7 §4, §6.2, §8.3, §12, §13, R13 P7b §9):
//! the typed call ([`build`]; the scope of the grant decides the action
//! variant), the station
//! codes that pass through to the agent ([`passed_code`]), the final state of
//! a send ([`outcome_of`]), the status of an input operation ([`status_of`])
//! and the audit keys. The run is in [`super::input_run`].
//!
//! Nothing here keeps the text, the chord or the coordinates: the operation
//! row holds the validated `window_id` and the length only (§11), and an audit
//! row holds the length only (§12).

use fleetdeck_proto::capture;
use fleetdeck_proto::coordinator_desktop::{ActionState, DesktopActionStatus, DesktopCall};
use fleetdeck_proto::coordinator_desktop_text::{
    INPUT_ANY_OUTCOME_UNKNOWN_TEXT, INPUT_OUTCOME_UNKNOWN_TEXT, code, text, text_any,
};
use fleetdeck_proto::coordinator_spec::InputScope;
use fleetdeck_proto::desktop::{DesktopActResult, DesktopAction};
use serde_json::{Value, json};

use super::authorize::Tool;
use super::calls::Built;
use super::queries;
use crate::desktop_relay::RelayError;
use crate::link::desktop_authority::{CallError, map_wire};
use crate::store::coordinator_desktop::{DesktopOp, OpOutcome, OpTool};

/// The checked call of an input tool: the length is 1 for a chord, the Unicode
/// scalar count for a text and 0 for a movement (§3.5); the resource holds the
/// `window_id` only.
fn built(
    (op_tool, action): (OpTool, DesktopAction),
    (request_id, window_id): (&str, &str),
    length: u32,
    args: Value,
) -> Built {
    Built {
        op_tool,
        action,
        request_id: request_id.to_owned(),
        args,
        resource: json!({ "window_id": window_id }),
        window_id: Some(window_id.to_owned()),
        monitor: None,
        seconds: None,
        input_length: Some(length),
    }
}

/// Builds the typed action of §6.2 from the validated arguments. Under the
/// `any_window` scope (`any`) every tool has its broader variant (R13 P7b
/// §9.1); under `own_windows` the three P7 tools keep their P7 actions and a
/// click is `input_pointer_click`. `None` for a call that is not an input
/// call.
pub(super) fn build(call: &DesktopCall, any: bool) -> Option<Result<Built, &'static str>> {
    let invalid = |_| code::INVALID_PARAMS;
    Some(match call {
        DesktopCall::KeyPress(a) => serde_json::to_value(a).map_err(invalid).map(|args| {
            let (window_id, chord) = (a.window_id.clone(), a.chord.clone());
            let action = if any {
                DesktopAction::InputAnyKeyPress { window_id, chord }
            } else {
                DesktopAction::InputKeyPress { window_id, chord }
            };
            built(
                (OpTool::KeyPress, action),
                (&a.request_id, &a.window_id),
                1,
                args,
            )
        }),
        DesktopCall::TypeText(a) => serde_json::to_value(a).map_err(invalid).and_then(|args| {
            let length = u32::try_from(a.text.chars().count()).map_err(|_| code::INVALID_PARAMS)?;
            let (window_id, text) = (a.window_id.clone(), a.text.clone());
            let action = if any {
                DesktopAction::InputAnyTypeText { window_id, text }
            } else {
                DesktopAction::InputTypeText { window_id, text }
            };
            Ok(built(
                (OpTool::TypeText, action),
                (&a.request_id, &a.window_id),
                length,
                args,
            ))
        }),
        DesktopCall::PointerMove(a) => serde_json::to_value(a).map_err(invalid).map(|args| {
            let (window_id, x, y) = (a.window_id.clone(), a.x, a.y);
            let action = if any {
                DesktopAction::InputAnyPointerMove { window_id, x, y }
            } else {
                DesktopAction::InputPointerMove { window_id, x, y }
            };
            built(
                (OpTool::PointerMove, action),
                (&a.request_id, &a.window_id),
                0,
                args,
            )
        }),
        DesktopCall::PointerClick(a) => serde_json::to_value(a).map_err(invalid).map(|args| {
            let (window_id, x, y) = (a.window_id.clone(), a.x, a.y);
            let action = if any {
                DesktopAction::InputAnyPointerClick { window_id, x, y }
            } else {
                DesktopAction::InputPointerClick { window_id, x, y }
            };
            built(
                (OpTool::PointerClick, action),
                (&a.request_id, &a.window_id),
                0,
                args,
            )
        }),
        _ => return None,
    })
}

/// True when the action of an input operation is one of the five P7b
/// variants (R13 P7b §13): every operation under the `any_window` scope, and
/// every click. These have the broader texts.
pub(super) fn is_any(tool: OpTool, scope: Option<InputScope>) -> bool {
    tool == OpTool::PointerClick || scope == Some(InputScope::AnyWindow)
}

/// The audit action of `key_press` (R13 P7 §12).
pub(super) const KEY_PRESS: &str = "desktop.key_press";
/// The audit action of `type_text`.
pub(super) const TYPE_TEXT: &str = "desktop.type_text";
/// The audit action of `pointer_move`.
pub(super) const POINTER_MOVE: &str = "desktop.pointer_move";
/// The audit action of `pointer_click` (R13 P7b §11.5), in both scopes.
pub(super) const POINTER_CLICK: &str = "desktop.pointer_click";

/// The audit action of an input operation row.
pub(super) fn audit_action(tool: OpTool) -> &'static str {
    match tool {
        OpTool::KeyPress => KEY_PRESS,
        OpTool::TypeText => TYPE_TEXT,
        OpTool::PointerClick => POINTER_CLICK,
        _ => POINTER_MOVE,
    }
}

/// True for the four input tools of a call.
pub(super) fn is_input_tool(tool: Tool) -> bool {
    matches!(
        tool,
        Tool::KeyPress | Tool::TypeText | Tool::PointerMove | Tool::PointerClick
    )
}

/// True for the four input tools of an operation row.
pub(super) fn is_input(tool: OpTool) -> bool {
    matches!(
        tool,
        OpTool::KeyPress | OpTool::TypeText | OpTool::PointerMove | OpTool::PointerClick
    )
}

/// The wire codes of the station that pass through to the agent unchanged:
/// the input codes of §13 that a station answers (R13 P7b §13 adds six),
/// `screen_locked`, `desktop_window_not_found`, `authority_lost` and
/// `authority_unknown`. Any other code is mapped as every relay code.
pub(super) fn passed_code(wire: &str) -> Option<&'static str> {
    const PASSED: [&str; 23] = [
        code::INPUT_NOT_ALLOWED,
        code::INPUT_SCOPE_UNSUPPORTED,
        code::INPUT_TOOL_MISSING,
        code::INPUT_TOOL_UNSUPPORTED,
        code::INPUT_BOARD_FORBIDDEN,
        code::INPUT_CAP_REACHED,
        code::INPUT_CHARACTERS_REACHED,
        code::INPUT_DESTINATION_UNAVAILABLE,
        code::INPUT_FOCUS_UNKNOWN,
        code::FOCUS_CHANGED,
        code::INPUT_INTERRUPTED,
        code::INPUT_FAILED,
        code::INPUT_TIMEOUT,
        capture::code::SCREEN_LOCKED,
        code::DESKTOP_WINDOW_NOT_FOUND,
        code::AUTHORITY_LOST,
        code::AUTHORITY_UNKNOWN,
        code::INPUT_CLICK_NOT_ALLOWED,
        code::INPUT_SCOPE_NOT_ALLOWED,
        code::INPUT_CLICK_DISABLED,
        code::INPUT_ANY_DISABLED,
        code::INPUT_CLICK_TOOL_MISSING,
        code::INPUT_CLICK_TOOL_UNSUPPORTED,
    ];
    PASSED.iter().copied().find(|c| *c == wire)
}

/// The fixed text of a code of an input reply: the text of
/// `coordinator_desktop_text` (`text_any` for the P7b variants, R13 P7b §13),
/// and the capture text of `screen_locked`.
pub(super) fn message(error: &str, any: bool) -> &'static str {
    if error == capture::code::SCREEN_LOCKED {
        return capture::text::SCREEN_LOCKED;
    }
    if any {
        text_any(error).unwrap_or_default()
    } else {
        text(error).unwrap_or_default()
    }
}

/// The fixed text of an unknown input outcome (R13 P7 §13, R13 P7b §13).
pub(super) fn unknown_text(any: bool) -> &'static str {
    if any {
        INPUT_ANY_OUTCOME_UNKNOWN_TEXT
    } else {
        INPUT_OUTCOME_UNKNOWN_TEXT
    }
}

/// The final state of the send of an input action (R13 P7 §8.3): a deck wait
/// that ran out or a link that ended after the send is `unknown` (never
/// "nothing happened"); a station answer is `done` (an empty result) or
/// `failed` with a fixed code.
pub(super) fn outcome_of(sent: Result<DesktopActResult, CallError>) -> OpOutcome {
    match sent {
        Ok(_) => OpOutcome::Done(json!({})),
        Err(CallError::Relay(RelayError::Timeout | RelayError::Unreachable)) => OpOutcome::Unknown,
        Err(CallError::Relay(e)) => OpOutcome::Failed(e.http().1),
        Err(CallError::Wire {
            code: wire,
            message,
        }) => OpOutcome::Failed(
            passed_code(&wire).unwrap_or_else(|| map_wire(&wire, &message).http().1),
        ),
    }
}

/// The status of an input operation (§4.8, §8.3): the P6 status, with the
/// fixed text of `screen_locked`, and for an unknown outcome the code
/// `desktop_timeout` with the explanatory text of §13. `scope` is the scope
/// of the grant of the operation (see [`is_any`]).
pub(super) fn status_of(
    request_id: &str,
    op: &DesktopOp,
    scope: Option<InputScope>,
) -> DesktopActionStatus {
    let any = is_any(op.tool, scope);
    let mut status = queries::status_of(request_id, op);
    match status.state {
        ActionState::Failed => {
            status.message = status
                .error
                .as_deref()
                .map(|e| message(e, any))
                .map(str::to_owned);
        }
        ActionState::Unknown => {
            status.error = Some(code::DESKTOP_TIMEOUT.to_owned());
            status.message = Some(unknown_text(any).to_owned());
        }
        _ => {}
    }
    status
}

/// The extra keys of the attempt row (§12): the length, and the counters that
/// the acceptance charged (the typed characters only for a `type_text`).
pub(super) fn attempt_extras(op: &DesktopOp) -> Vec<(&'static str, Value)> {
    let length = op.input_length.unwrap_or(0);
    let characters = if op.tool == OpTool::TypeText {
        length
    } else {
        0
    };
    vec![
        ("input_length", json!(length)),
        (
            "charged",
            json!({
                "desktop": op.charged.desktop,
                "capture": op.charged.capture,
                "input": 1,
                "input_characters": characters,
            }),
        ),
    ]
}

/// The extra keys of the outcome row (§12): the length.
pub(super) fn outcome_extras(op: &DesktopOp) -> Vec<(&'static str, Value)> {
    vec![("input_length", json!(op.input_length.unwrap_or(0)))]
}
