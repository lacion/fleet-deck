//! The pure text and URL grammar of desktop actions (R13 §5B item 5, §5C
//! item 4; R13 P6 §4.3, §4.4). It lives here so that the deck and the station
//! apply one grammar. The station wraps these functions in its own failure
//! type (`fleetdeck_station::desktop::args`).

/// The longest URL, in bytes.
pub const URL_MAX: usize = 2048;
/// The longest notification title, in characters.
pub const TITLE_MAX: usize = 80;
/// The longest notification body, in characters.
pub const BODY_MAX: usize = 300;

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

/// A notification title: plain text, 80 characters, not empty. `None` when
/// nothing is left.
#[must_use]
pub fn title(text: &str) -> Option<String> {
    let t = plain_text(text, TITLE_MAX);
    if t.is_empty() { None } else { Some(t) }
}

/// A notification body: plain text, 300 characters; it may be empty.
#[must_use]
pub fn body(text: &str) -> String {
    plain_text(text, BODY_MAX)
}

/// Splits `host[:port]` or `[ipv6][:port]`. The bool tells whether the host
/// is valid: a name of `[A-Za-z0-9.-]`, or an IPv6 address in brackets that
/// `Ipv6Addr` parses (no zone id). `None` for a bracket that is not closed or
/// for text after the closing bracket that is not `:port`.
fn host_and_port(authority: &str) -> Option<(bool, Option<&str>)> {
    if let Some(rest) = authority.strip_prefix('[') {
        let (address, tail) = rest.split_once(']')?;
        let port = match tail {
            "" => None,
            t => Some(t.strip_prefix(':')?),
        };
        return Some((address.parse::<std::net::Ipv6Addr>().is_ok(), port));
    }
    let (host, port) = match authority.split_once(':') {
        Some((h, p)) => (h, Some(p)),
        None => (authority, None),
    };
    let host_ok = !host.is_empty()
        && host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-');
    Some((host_ok, port))
}

/// A URL for the browser: `http` or `https`, at most 2048 bytes, printable
/// non-space ASCII only, a host of `[A-Za-z0-9.-]` or a bracketed IPv6
/// address (R13 §6D item 6), with an optional port. `None` for any other
/// text.
#[must_use]
pub fn url(text: &str) -> Option<&str> {
    if text.len() > URL_MAX || !text.bytes().all(|b| (0x21..=0x7e).contains(&b)) {
        return None;
    }
    let after = ["http://", "https://"]
        .iter()
        .find(|s| {
            text.get(..s.len())
                .is_some_and(|p| p.eq_ignore_ascii_case(s))
        })
        .map(|s| &text[s.len()..])?;
    let end = after.find(['/', '?', '#']).unwrap_or(after.len());
    let (host_ok, port) = host_and_port(&after[..end])?;
    let port_ok = port.is_none_or(|p| {
        (1..=5).contains(&p.len())
            && p.bytes().all(|b| b.is_ascii_digit())
            && p.parse::<u32>().is_ok_and(|n| (1..=65535).contains(&n))
    });
    if host_ok && port_ok { Some(text) } else { None }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markup_is_removed_and_entities_are_decoded_once() {
        assert_eq!(plain_text("<b>Hi</b> there", 80), "Hi there");
        assert_eq!(plain_text("&amp;lt;", 80), "&lt;");
        assert_eq!(plain_text("&#65;&#x42;", 80), "AB");
        assert_eq!(plain_text("a\u{0}b\nc", 80), "abc");
    }

    #[test]
    fn a_title_is_not_empty_and_a_body_may_be() {
        assert_eq!(title("<b></b>"), None);
        assert_eq!(title(" a ").as_deref(), Some(" a "));
        assert_eq!(body("<i></i>"), "");
        assert_eq!(title(&"x".repeat(100)).map(|t| t.chars().count()), Some(80));
        assert_eq!(body(&"x".repeat(400)).chars().count(), 300);
    }

    #[test]
    fn a_url_keeps_the_station_grammar() {
        for ok in [
            "http://localhost:8080/a?b#c",
            "https://127.0.0.1",
            "HTTP://a.b-c.d",
            "http://[::1]:3000/x",
        ] {
            assert_eq!(url(ok), Some(ok), "{ok}");
        }
        for bad in [
            "ftp://a",
            "http://",
            "http://a b",
            "http://[::1",
            "http://[::1]x",
            "http://a:0",
            "http://a:65536",
            "http://a:123456",
            "http://u@h",
            "javascript:alert(1)",
            "",
        ] {
            assert_eq!(url(bad), None, "{bad}");
        }
        let long = format!("http://a/{}", "x".repeat(URL_MAX));
        assert_eq!(url(&long), None);
    }
}
