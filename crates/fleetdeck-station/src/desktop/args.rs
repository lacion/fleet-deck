//! The argument grammars of desktop actions (R13 §5A item 8, §5B item 5,
//! §5C item 4). Each is a pure function: it returns the value that may reach
//! an argv, or an `invalid_params` failure that names the argument and never
//! shows its value.

use super::{Fail, code};

/// The longest URL, in bytes.
pub const URL_MAX: usize = 2048;
/// The longest notification title, in characters.
pub const TITLE_MAX: usize = 80;
/// The longest notification body, in characters.
pub const BODY_MAX: usize = 300;

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
/// tag), control characters removed, then cut to `max` characters.
#[must_use]
pub fn plain_text(text: &str, max: usize) -> String {
    let decoded = decode_entities(&strip_tags(text));
    decoded
        .chars()
        .filter(|c| *c != '<' && *c != '>' && !c.is_control())
        .take(max)
        .collect()
}

/// Removes every `<…>` tag. An unclosed `<` stays for the later pass.
fn strip_tags(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(open) = rest.find('<') {
        out.push_str(&rest[..open]);
        match rest[open..].find('>') {
            Some(close) => rest = &rest[open + close + 1..],
            None => {
                out.push_str(&rest[open..]);
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}

/// Decodes `&amp; &lt; &gt; &quot; &apos; &#N; &#xN;` in one pass: the output
/// is never decoded again.
fn decode_entities(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        rest = &rest[amp..];
        // An entity is at most 10 characters long: `&#x10FFFF;`.
        let decoded = rest
            .char_indices()
            .take(12)
            .find(|(_, c)| *c == ';')
            .and_then(|(end, _)| entity(&rest[1..end]).map(|c| (c, end)));
        match decoded {
            Some((c, end)) => {
                out.push(c);
                rest = &rest[end + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

fn entity(name: &str) -> Option<char> {
    match name {
        "amp" => Some('&'),
        "lt" => Some('<'),
        "gt" => Some('>'),
        "quot" => Some('"'),
        "apos" => Some('\''),
        _ => {
            let digits = name.strip_prefix('#')?;
            let code = match digits.strip_prefix(['x', 'X']) {
                Some(hex) => u32::from_str_radix(hex, 16).ok()?,
                None => digits.parse::<u32>().ok()?,
            };
            if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_alphanumeric()) {
                return None;
            }
            char::from_u32(code)
        }
    }
}

/// A notification title: plain text, 80 characters, not empty.
///
/// # Errors
/// `invalid_params` when nothing is left.
pub fn title(text: &str) -> Result<String, Fail> {
    let t = plain_text(text, TITLE_MAX);
    if t.is_empty() {
        Err(invalid("title"))
    } else {
        Ok(t)
    }
}

/// A notification body: plain text, 300 characters; it may be empty.
#[must_use]
pub fn body(text: &str) -> String {
    plain_text(text, BODY_MAX)
}

/// A URL for the browser: `http` or `https`, at most 2048 bytes, printable
/// non-space ASCII only, a host of `[A-Za-z0-9.-]` with an optional port.
///
/// # Errors
/// `invalid_params` for any other text.
pub fn url(text: &str) -> Result<&str, Fail> {
    let bad = || invalid("url");
    if text.len() > URL_MAX || !text.bytes().all(|b| (0x21..=0x7e).contains(&b)) {
        return Err(bad());
    }
    let after = ["http://", "https://"]
        .iter()
        .find(|s| {
            text.get(..s.len())
                .is_some_and(|p| p.eq_ignore_ascii_case(s))
        })
        .map(|s| &text[s.len()..])
        .ok_or_else(bad)?;
    let end = after.find(['/', '?', '#']).unwrap_or(after.len());
    let authority = &after[..end];
    let (host, port) = match authority.split_once(':') {
        Some((h, p)) => (h, Some(p)),
        None => (authority, None),
    };
    let host_ok = !host.is_empty()
        && host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-');
    let port_ok = port.is_none_or(|p| {
        (1..=5).contains(&p.len())
            && p.bytes().all(|b| b.is_ascii_digit())
            && p.parse::<u32>().is_ok_and(|n| (1..=65535).contains(&n))
    });
    if host_ok && port_ok {
        Ok(text)
    } else {
        Err(bad())
    }
}

#[cfg(test)]
#[path = "args_tests.rs"]
mod tests;
