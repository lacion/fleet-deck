//! The redacted error line of Fleet Git (design §2.5, O4, DL-163; FG-1 to FG-4).

use crate::workspace::provision;

/// The most characters of a redacted line.
const MAX_CHARS: usize = 300;
/// Words that start with one of these are tokens.
const TOKEN_PREFIXES: &[&str] = &["ghp_", "gho_", "github_pat_", "glpat-", "xox"];
/// What replaces a masked word.
const MASK: &str = "[redacted]";

/// Leading characters that can wrap a token in an error text.
fn is_wrapper(c: char) -> bool {
    matches!(c, '\'' | '"' | '(' | '<')
}

/// Masks the token words and the value after `Bearer`, `Basic` and
/// `Authorization:`. The white space of `text` stays as it is.
fn mask_tokens(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut mask_next = false;
    let mut rest = text;
    while !rest.is_empty() {
        let space = rest.len() - rest.trim_start().len();
        out.push_str(&rest[..space]);
        rest = &rest[space..];
        let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        let (word, tail) = rest.split_at(end);
        rest = tail;
        if word.is_empty() {
            break;
        }
        let bare = word.trim_start_matches(is_wrapper);
        if mask_next || TOKEN_PREFIXES.iter().any(|p| bare.starts_with(p)) {
            out.push_str(MASK);
        } else {
            out.push_str(word);
        }
        mask_next = matches!(
            word.to_ascii_lowercase().as_str(),
            "bearer" | "basic" | "authorization:"
        );
    }
    out
}

/// The text of a failed Git call for the board (design §2.5): the last
/// non-empty line of `stderr`, without URL user information, without token
/// words (`ghp_`, `gho_`, `github_pat_`, `glpat-`, `xox`) and without the
/// value after `Bearer`, `Basic` and `Authorization:`, without control
/// characters, cut to 300 characters.
#[must_use]
pub fn redact(stderr: &str) -> String {
    let line = stderr
        .lines()
        .rev()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    let line = mask_tokens(&provision::scrub(line));
    line.chars()
        .filter(|c| !c.is_control())
        .take(MAX_CHARS)
        .collect()
}

#[cfg(test)]
#[path = "redact_tests.rs"]
mod tests;
