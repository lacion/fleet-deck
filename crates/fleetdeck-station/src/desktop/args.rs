//! The argument grammars of desktop actions (R13 §5A item 8, §5B item 5,
//! §5C item 4). Each is a pure function: it returns the value that may reach
//! an argv, or an `invalid_params` failure that names the argument and never
//! shows its value.

use fleetdeck_proto::desktop_text;
pub use fleetdeck_proto::desktop_text::{BODY_MAX, TITLE_MAX, URL_MAX};

use super::{Fail, code};

fn invalid(arg: &str) -> Fail {
    Fail::new(code::INVALID_PARAMS, format!("`{arg}` is not valid"))
}

/// A monitor: it must equal one name of the live monitor list exactly.
///
/// # Errors
/// `invalid_params` when no live monitor has this name.
pub fn monitor(name: &str, live: &[String]) -> Result<(), Fail> {
    if live.iter().any(|m| m == name) {
        Ok(())
    } else {
        Err(Fail::new(
            code::INVALID_PARAMS,
            "`monitor` is not a monitor of this desktop",
        ))
    }
}

/// A workspace number: 1 to 99.
///
/// # Errors
/// `invalid_params` outside the range.
pub fn workspace(n: u8) -> Result<u8, Fail> {
    if (1..=99).contains(&n) {
        Ok(n)
    } else {
        Err(invalid("workspace"))
    }
}

/// A window address: `^0x[0-9a-f]{1,16}$`.
///
/// # Errors
/// `invalid_params` for any other text.
pub fn address(text: &str) -> Result<&str, Fail> {
    let digits = text.strip_prefix("0x").unwrap_or("");
    let ok = (1..=16).contains(&digits.len())
        && digits
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
    if ok {
        Ok(text)
    } else {
        Err(invalid("address"))
    }
}

/// A display id: `^[A-Za-z0-9_-]{1,64}$`.
///
/// # Errors
/// `invalid_params` for any other text.
pub fn display_id(text: &str) -> Result<&str, Fail> {
    let ok = (1..=64).contains(&text.len())
        && text
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
    if ok {
        Ok(text)
    } else {
        Err(invalid("display_id"))
    }
}

/// Plain notification text: tags removed, entities decoded in one pass, every
/// remaining `<` and `>` removed (a daemon with Pango markup never sees a
/// tag), control characters removed, then cut to `max` characters. The
/// grammar is in `fleetdeck_proto::desktop_text` (the deck uses it too).
#[must_use]
pub fn plain_text(text: &str, max: usize) -> String {
    desktop_text::plain_text(text, max)
}

/// A notification title: plain text, 80 characters, not empty.
///
/// # Errors
/// `invalid_params` when nothing is left.
pub fn title(text: &str) -> Result<String, Fail> {
    desktop_text::title(text).ok_or_else(|| invalid("title"))
}

/// A notification body: plain text, 300 characters; it may be empty.
#[must_use]
pub fn body(text: &str) -> String {
    desktop_text::body(text)
}

/// A URL for the browser: `http` or `https`, at most 2048 bytes, printable
/// non-space ASCII only, a host of `[A-Za-z0-9.-]` or a bracketed IPv6
/// address (R13 §6D item 6), with an optional port.
///
/// # Errors
/// `invalid_params` for any other text.
pub fn url(text: &str) -> Result<&str, Fail> {
    desktop_text::url(text).ok_or_else(|| invalid("url"))
}

#[cfg(test)]
#[path = "args_tests.rs"]
mod tests;
