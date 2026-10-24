//! The P7b deck side of the pointer click and the `any_window` scope (R13
//! P7b task K1): the click through admission, authorization, the policy
//! purposes and the actions (`click.rs`), the codes, the texts and the
//! unknown outcome (`outcomes.rs`), the live station gates (`gates.rs`), the
//! execute and delivery checks and the board sign-out of a click
//! (`authority.rs`), and the `window_list` read (`window_list.rs`).

#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "coordinator_desktop_p7b/authority.rs"]
mod authority;
#[path = "coordinator_desktop_p7b/click.rs"]
mod click;
mod common;
mod coordinator_desktop_support;
#[path = "coordinator_desktop_p7b/gates.rs"]
mod gates;
#[path = "coordinator_desktop_p7b/outcomes.rs"]
mod outcomes;
#[path = "coordinator_desktop_p7b/window_list.rs"]
mod window_list;

use coordinator_desktop_support::*;

/// The P7 unknown-outcome text and the P7b one (R13 P7b §13).
pub const P7_UNKNOWN_TEXT: &str =
    "The input outcome is unknown. Inspect the preview before issuing another input request.";
pub const P7B_UNKNOWN_TEXT: &str = "The input outcome is unknown. Inspect the target application before issuing another input request. Do not retry automatically.";

/// True when `needle` stands in `text` as a whole token: no letter or digit
/// touches it on either side.
pub fn has_token(text: &str, needle: &str) -> bool {
    text.match_indices(needle).any(|(at, _)| {
        let before = text[..at].chars().next_back();
        let after = text[at + needle.len()..].chars().next();
        !before.is_some_and(char::is_alphanumeric) && !after.is_some_and(char::is_alphanumeric)
    })
}

/// No request of the deck reached the fake station.
pub fn assert_no_request(w: &World) {
    assert!(w.fake.log().is_empty(), "{:?}", w.fake.log());
}

#[test]
fn a_coordinate_inside_an_identifier_is_not_a_token() {
    assert!(!has_token("ab7351cd", "7351"));
    assert!(has_token("{\"x\":7351,\"y\":1}", "7351"));
}
