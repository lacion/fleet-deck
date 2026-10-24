//! The input part of a coordinator grant (R13 P7 §3.1; R13 P7b §3.1): the
//! rules that `create_grant` and the board request share, and the stored text
//! of the input scope. Moved out of `grants.rs` for the module size rule.

use std::collections::BTreeSet;

use fleetdeck_proto::coordinator_desktop_text::limits;
use fleetdeck_proto::coordinator_spec::InputScope;

use super::grants::GrantClass;

/// The stored text of an input scope (the values of the `tokens.input_scope`
/// check).
pub(super) const fn scope_text(scope: InputScope) -> &'static str {
    match scope {
        InputScope::OwnWindows => "own_windows",
        InputScope::AnyWindow => "any_window",
    }
}

/// The scope of a stored row. The allowance and the scope are both durable
/// facts: an unknown scope text or a missing scope is no allowance (fail
/// closed).
pub(super) fn input_scope_of(allow_input: i64, text: Option<&str>) -> Option<InputScope> {
    if allow_input != 1 {
        return None;
    }
    match text {
        Some("own_windows") => Some(InputScope::OwnWindows),
        Some("any_window") => Some(InputScope::AnyWindow),
        _ => None,
    }
}

/// The rules of the input allowance of a new grant (R13 P7 §3.1; R13 P7b
/// §3.1), the same for the board request and for `create_grant`: the class
/// rule, scope, click and caps need `allow_input`, then the ranges. The text
/// is fixed and shows no value.
///
/// # Errors
/// The fixed text of the first rule that fails.
pub fn check_input_grant(
    classes: &BTreeSet<GrantClass>,
    (allow_input, allow_click): (bool, bool),
    scope: Option<InputScope>,
    (input_cap, input_character_cap): (u32, u32),
) -> std::result::Result<(), &'static str> {
    let all = [GrantClass::Desktop, GrantClass::Capture, GrantClass::Input];
    if allow_input && !all.iter().all(|c| classes.contains(c)) {
        return Err("allow_input needs the desktop, capture and input classes");
    }
    if !allow_input && scope.is_some() {
        return Err("input_scope needs allow_input");
    }
    if allow_click && !allow_input {
        return Err("allow_click needs allow_input");
    }
    if !allow_input && (input_cap != 0 || input_character_cap != 0) {
        return Err("input caps need allow_input");
    }
    if input_cap > limits::INPUT_CAP_MAX {
        return Err("input_cap is 0 to 5000");
    }
    if input_character_cap > limits::INPUT_CHARACTER_CAP_MAX {
        return Err("input_character_cap is 0 to 1000000");
    }
    Ok(())
}
