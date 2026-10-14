//! Small helpers of the integration service: time stamps, URL encoding,
//! images in Markdown, log tails.

/// The most characters of an issue or pull request body in an answer.
pub const BODY_CHARS: usize = 100_000;
/// The most characters of one comment in an answer.
pub const COMMENT_CHARS: usize = 20_000;
/// The most characters of the log tail of one check in an answer.
pub const LOG_CHARS: usize = 60_000;

/// Days from 1970-01-01 to the civil date (proleptic Gregorian).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Unix ms of an ISO-8601 time stamp: `2024-05-01T10:00:00Z`,
/// `2024-05-01T10:00:00.123+02:00` or Jira's `...+0200`. `0` for a text
/// that is not a time stamp.
#[must_use]
pub fn parse_time(text: &str) -> i64 {
    parse_time_opt(text).unwrap_or(0)
}

fn parse_time_opt(text: &str) -> Option<i64> {
    let b = text.as_bytes();
    let num = |from: usize, len: usize| -> Option<i64> {
        let part = text.get(from..from + len)?;
        part.bytes()
            .all(|c| c.is_ascii_digit())
            .then(|| part.parse().ok())?
    };
    if b.len() < 19 || b[4] != b'-' || b[7] != b'-' || !matches!(b[10], b'T' | b' ') {
        return None;
    }
    let (y, mo, d) = (num(0, 4)?, num(5, 2)?, num(8, 2)?);
    let (h, mi, s) = (num(11, 2)?, num(14, 2)?, num(17, 2)?);
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || s > 60 {
        return None;
    }
    let mut at = 19;
    let mut millis = 0;
    if b.get(at) == Some(&b'.') {
        let digits = text[at + 1..]
            .bytes()
            .take_while(u8::is_ascii_digit)
            .count();
        let frac = &text[at + 1..at + 1 + digits.min(3)];
        millis = frac.parse::<i64>().ok()? * 10_i64.pow(u32::try_from(3 - frac.len()).ok()?);
        at += 1 + digits;
    }
    let offset = match b.get(at) {
        None | Some(b'Z') => 0,
        Some(sign @ (b'+' | b'-')) => {
            let rest: String = text[at + 1..].chars().filter(|c| *c != ':').collect();
            if rest.len() < 2 || !rest.bytes().all(|c| c.is_ascii_digit()) {
                return None;
            }
            let hh: i64 = rest[..2].parse().ok()?;
            let mm: i64 = rest.get(2..4).map_or(Some(0), |m| m.parse().ok())?;
            let secs = hh * 3600 + mm * 60;
            if *sign == b'-' { -secs } else { secs }
        }
        Some(_) => return None,
    };
    let days = days_from_civil(y, mo, d);
    Some(((days * 86_400 + h * 3600 + mi * 60 + s - offset) * 1000) + millis)
}

/// `text` percent-encoded for a URL path segment or a query value.
#[must_use]
pub fn urlencode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for b in text.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// The URLs of the images in Markdown or HTML text: `![alt](url)` and
/// `<img src="url">`, in order, without repeats, at most 50.
#[must_use]
pub fn image_urls(markdown: &str) -> Vec<String> {
    let mut urls: Vec<String> = Vec::new();
    let mut push = |url: &str| {
        let url = url.trim();
        if (url.starts_with("https://") || url.starts_with("http://"))
            && urls.len() < 50
            && !urls.iter().any(|u| u == url)
        {
            urls.push(url.to_owned());
        }
    };
    let mut rest = markdown;
    while let Some(at) = rest.find("![") {
        rest = &rest[at + 2..];
        if let Some(close) = rest.find("](") {
            let after = &rest[close + 2..];
            if let Some(end) = after.find(')') {
                // `![a](url "title")`
                push(after[..end].split_whitespace().next().unwrap_or_default());
            }
        }
    }
    let mut rest = markdown;
    while let Some(at) = rest.find("<img") {
        rest = &rest[at + 4..];
        let tag_end = rest.find('>').unwrap_or(rest.len());
        let tag = &rest[..tag_end];
        if let Some(src) = tag.find("src=\"") {
            let value = &tag[src + 5..];
            if let Some(end) = value.find('"') {
                push(&value[..end]);
            }
        }
    }
    urls
}

/// The URLs of pull requests and merge requests that `text` names, in
/// order, without repeats, at most 20.
#[must_use]
pub fn pr_links(text: &str) -> Vec<String> {
    let mut urls: Vec<String> = Vec::new();
    for word in text.split(|c: char| {
        c.is_whitespace() || matches!(c, '(' | ')' | '<' | '>' | '[' | ']' | '"' | '\'')
    }) {
        let word = word.trim_end_matches(['.', ',', ';']);
        if word.starts_with("https://")
            && (word.contains("/pull/") || word.contains("/merge_requests/"))
            && urls.len() < 20
            && !urls.iter().any(|u| u == word)
        {
            urls.push(word.to_owned());
        }
    }
    urls
}

/// The last `lines` lines of `text`, without ANSI escape sequences and
/// without the time stamps that CI logs put at the start of a line.
#[must_use]
pub fn tail_lines(text: &str, lines: usize) -> String {
    let clean = strip_ansi(text);
    let all: Vec<&str> = clean.lines().collect();
    let from = all.len().saturating_sub(lines);
    all[from..].join("\n")
}

fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for n in chars.by_ref() {
                    if ('@'..='~').contains(&n) {
                        break;
                    }
                }
            }
        } else if c != '\r' {
            out.push(c);
        }
    }
    out
}

/// True when `bytes` start like a PNG, JPEG, GIF or WebP image; returns the
/// content type. SVG is never an image here: it can hold script.
#[must_use]
pub fn sniff_image(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if bytes.starts_with(b"\xff\xd8\xff") {
        Some("image/jpeg")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if bytes.len() > 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else {
        None
    }
}

/// The host of an `https://` URL, lower case; `None` for any other form
/// (a user, a port with letters, no host).
#[must_use]
pub fn https_host(url: &str) -> Option<String> {
    let rest = url.strip_prefix("https://")?;
    let authority = rest.split(['/', '?', '#']).next()?;
    if authority.contains('@') {
        return None;
    }
    let host = authority.split(':').next()?;
    fleetdeck_proto::integrations::valid_host(authority).then(|| host.to_ascii_lowercase())
}

/// The origin of an `https://` URL as lower case `host:port`: port 443 when
/// the URL has none. The same rules as [`https_host`]; `None` for any other
/// form (NV-14).
#[must_use]
pub fn https_origin(url: &str) -> Option<String> {
    let host = https_host(url)?;
    let authority = url
        .strip_prefix("https://")?
        .split(['/', '?', '#'])
        .next()?;
    let port = authority.split_once(':').map_or("443", |(_, port)| port);
    Some(format!("{host}:{port}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn time_stamps_parse_in_every_form() {
        assert_eq!(parse_time("1970-01-01T00:00:00Z"), 0);
        assert_eq!(parse_time("2024-05-01T10:00:00Z"), 1_714_557_600_000);
        assert_eq!(parse_time("2024-05-01T10:00:00.123Z"), 1_714_557_600_123);
        assert_eq!(parse_time("2024-05-01T12:00:00+02:00"), 1_714_557_600_000);
        assert_eq!(
            parse_time("2024-05-01T12:00:00.000+0200"),
            1_714_557_600_000
        );
        assert_eq!(parse_time("2024-05-01T05:00:00-05:00"), 1_714_557_600_000);
        assert_eq!(parse_time("2024-05-01T10:00:00.5Z"), 1_714_557_600_500);
        assert_eq!(parse_time("not a time"), 0);
        assert_eq!(parse_time(""), 0);
        // 29 February of a leap year.
        assert_eq!(
            parse_time("2024-03-01T00:00:00Z") - parse_time("2024-02-28T00:00:00Z"),
            2 * 86_400_000
        );
    }

    #[test]
    fn url_encoding_keeps_only_safe_characters() {
        assert_eq!(urlencode("group/sub proj"), "group%2Fsub%20proj");
        assert_eq!(urlencode("a-b_c.d~e"), "a-b_c.d~e");
        assert_eq!(urlencode("é&="), "%C3%A9%26%3D");
    }

    #[test]
    fn images_come_from_markdown_and_html_without_repeats() {
        let text = "a ![x](https://i.test/a.png) b ![y](https://i.test/b.png \"t\") \
                    <img src=\"https://i.test/c.png\" width=3> ![z](https://i.test/a.png) \
                    ![no](javascript:x) ![rel](/relative.png)";
        assert_eq!(
            image_urls(text),
            [
                "https://i.test/a.png",
                "https://i.test/b.png",
                "https://i.test/c.png"
            ]
        );
    }

    #[test]
    fn pull_request_links_are_found_in_prose() {
        let text = "see (https://github.com/a/b/pull/3), and https://gitlab.com/g/p/-/merge_requests/9. \
                    also https://github.com/a/b/issues/1 and https://github.com/a/b/pull/3";
        assert_eq!(
            pr_links(text),
            [
                "https://github.com/a/b/pull/3",
                "https://gitlab.com/g/p/-/merge_requests/9"
            ]
        );
    }

    #[test]
    fn a_log_tail_drops_escapes_and_keeps_the_last_lines() {
        let log = "one\n\u{1b}[31mtwo\u{1b}[0m\r\nthree\nfour";
        assert_eq!(tail_lines(log, 3), "two\nthree\nfour");
        assert_eq!(tail_lines(log, 10), "one\ntwo\nthree\nfour");
    }

    #[test]
    fn images_are_sniffed_and_svg_is_refused() {
        assert_eq!(sniff_image(b"\x89PNG\r\n\x1a\nxxxx"), Some("image/png"));
        assert_eq!(sniff_image(b"\xff\xd8\xff\xe0"), Some("image/jpeg"));
        assert_eq!(sniff_image(b"GIF89a.."), Some("image/gif"));
        assert_eq!(sniff_image(b"RIFF\0\0\0\0WEBPVP8 "), Some("image/webp"));
        assert_eq!(sniff_image(b"<svg xmlns=\"x\"><script/></svg>"), None);
        assert_eq!(sniff_image(b""), None);
    }

    #[test]
    fn the_https_host_refuses_users_and_other_schemes() {
        assert_eq!(
            https_host("https://Acme.Atlassian.net/a?b"),
            Some("acme.atlassian.net".into())
        );
        assert_eq!(https_host("https://host:8443/x"), Some("host".into()));
        assert_eq!(https_host("https://u:p@host/x"), None);
        assert_eq!(https_host("http://host/x"), None);
        assert_eq!(https_host("https:///x"), None);
    }

    #[test]
    fn the_https_origin_has_a_lower_case_host_and_a_port() {
        assert_eq!(
            https_origin("https://T.example/x"),
            Some("t.example:443".into())
        );
        assert_eq!(
            https_origin("https://t.example:8443/x"),
            Some("t.example:8443".into())
        );
        assert_eq!(
            https_origin("https://t.example:443?a"),
            Some("t.example:443".into())
        );
        assert_eq!(https_origin("https://u:p@host/x"), None);
        assert_eq!(https_origin("http://host/x"), None);
        assert_eq!(https_origin("https:///x"), None);
        assert_eq!(https_origin("https://host:80a/x"), None);
    }
}
