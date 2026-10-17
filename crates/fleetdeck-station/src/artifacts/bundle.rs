//! The local files that an HTML page names (R12, DL-165).
//!
//! [`references`] scans the page for `src="…"`, `href="…"` and CSS
//! `url(…)` values and keeps those that name a file below the page's own
//! directory: a relative path, no scheme, no `..`, no absolute path, no
//! backslash, no hidden directory (`.git`, a credential directory). The
//! page keeps its links; the station only copies the files next to it.
//! The scan is one level: a bundled file's own references are not
//! followed.

use std::collections::BTreeSet;

use fleetdeck_proto::files::path_segments;

/// The bundle paths that `html` names, sorted, without duplicates. Each is
/// `/`-separated and relative to the page's directory.
#[must_use]
pub fn references(html: &str) -> Vec<String> {
    let mut found = BTreeSet::new();
    for raw in attribute_values(html, "src")
        .into_iter()
        .chain(attribute_values(html, "href"))
        .chain(css_urls(html))
    {
        if let Some(path) = local_path(&raw) {
            found.insert(path);
        }
    }
    found.into_iter().collect()
}

/// The values of `name="…"`, `name='…'` and `name=value` attributes. The
/// name must start after whitespace, a quote or `<`, so `data-src` does not
/// match `src`.
fn attribute_values(html: &str, name: &str) -> Vec<String> {
    let lower = html.to_ascii_lowercase();
    let bytes = html.as_bytes();
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(at) = lower[from..].find(name) {
        let start = from + at;
        from = start + name.len();
        let before = start.checked_sub(1).map(|i| bytes[i]);
        if !matches!(before, Some(b) if b.is_ascii_whitespace() || b == b'"' || b == b'\'' || b == b'<')
        {
            continue;
        }
        let mut i = from;
        while bytes.get(i).is_some_and(u8::is_ascii_whitespace) {
            i += 1;
        }
        if bytes.get(i) != Some(&b'=') {
            continue;
        }
        i += 1;
        while bytes.get(i).is_some_and(u8::is_ascii_whitespace) {
            i += 1;
        }
        let (value, end) = match bytes.get(i) {
            Some(&q) if q == b'"' || q == b'\'' => {
                let rest = &html[i + 1..];
                match rest.find(q as char) {
                    Some(n) => (&rest[..n], i + 1 + n + 1),
                    None => continue,
                }
            }
            Some(_) => {
                let rest = &html[i..];
                let n = rest
                    .find(|c: char| c.is_ascii_whitespace() || c == '>')
                    .unwrap_or(rest.len());
                (&rest[..n], i + n)
            }
            None => continue,
        };
        out.push(value.to_owned());
        from = end;
    }
    out
}

/// The values of `url(…)`, with or without quotes.
fn css_urls(html: &str) -> Vec<String> {
    let lower = html.to_ascii_lowercase();
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(at) = lower[from..].find("url(") {
        let start = from + at + 4;
        from = start;
        let rest = html[start..].trim_start();
        let (value, consumed) = match rest.chars().next() {
            Some(q @ ('"' | '\'')) => match rest[1..].find(q) {
                Some(n) => (&rest[1..=n], n + 2),
                None => continue,
            },
            Some(_) => match rest.find(')') {
                Some(n) => (rest[..n].trim_end(), n),
                None => continue,
            },
            None => continue,
        };
        out.push(value.to_owned());
        from = start + (html[start..].len() - rest.len()) + consumed;
    }
    out
}

/// The bundle path of one reference, or `None` when it does not name a
/// local file below the page's directory.
fn local_path(raw: &str) -> Option<String> {
    let value = raw.trim();
    if value.is_empty() || value.starts_with('#') || value.starts_with('/') {
        return None;
    }
    // A scheme (`https:`, `data:`, `javascript:`, `mailto:`) or a network
    // path: not a local file.
    let head = value.split(['/', '?', '#']).next().unwrap_or("");
    if head.contains(':') || value.starts_with("\\\\") {
        return None;
    }
    let cut = value.split(['?', '#']).next().unwrap_or("");
    let decoded = percent_decode(cut)?;
    if decoded.is_empty() || decoded.contains('\0') || decoded.contains('\\') {
        return None;
    }
    let mut segments = Vec::new();
    for segment in decoded.split('/') {
        match segment {
            "." => {}
            ".." | "" => return None,
            s => segments.push(s),
        }
    }
    if segments.is_empty() {
        return None;
    }
    let joined = segments.join("/");
    // The same lexical rules as the file service: length, hidden names.
    path_segments(&joined).ok()?;
    Some(joined)
}

fn percent_decode(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = text.get(i + 1..i + 3)?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_page_names_local_files_through_src_href_and_css_urls() {
        let html = r#"
            <link rel="stylesheet" href="style.css">
            <script src='js/app.js'></script>
            <img src=img/a.png alt=x>
            <div style="background: url('img/bg.png')"></div>
            <style>@font-face { src: url( fonts/f.woff2 ) }</style>
            <a href="./notes/page one.html?x=1#top">n</a>
            <img data-src="lazy.png">
            <img SRC="UPPER.png">
        "#;
        assert_eq!(
            references(html),
            [
                "UPPER.png",
                "fonts/f.woff2",
                "img/a.png",
                "img/bg.png",
                "js/app.js",
                "notes/page one.html",
                "style.css",
            ]
        );
    }

    #[test]
    fn a_reference_that_leaves_the_directory_or_is_not_a_file_is_not_bundled() {
        let html = r##"
            <img src="../secret.png"><img src="a/../b.png"><img src="/etc/passwd">
            <img src="https://cdn.example/x.js"><script src="//cdn.example/x.js"></script>
            <a href="#top"></a><a href="mailto:a@b.c"></a><a href="javascript:alert(1)"></a>
            <img src="data:image/png;base64,AAAA"><img src="C:\x.png"><img src="a\b.png">
            <img src=".git/config"><img src="x/.ssh/id_rsa"><img src="dir/"><img src="">
            <img src="%2e%2e/x.png"><img src="a%00b.png"><img src="%zz.png">
            <img src="ok.png">
        "##;
        assert_eq!(references(html), ["ok.png"]);
    }

    #[test]
    fn percent_escapes_decode_and_a_missing_closing_quote_is_skipped() {
        assert_eq!(references(r#"<img src="my%20file.png">"#), ["my file.png"]);
        assert!(references(r#"<img src="broken.png"#).is_empty());
        assert!(references("<p>no references</p>").is_empty());
    }
}
