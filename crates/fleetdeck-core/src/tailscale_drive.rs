//! `tailscale drive list` and its strict table parser (Fleet Git design
//! §15.1 to §15.3, FG-12, O35, O36).
//!
//! Fleet Deck only reads Taildrive: [`TailscaleCli::drive_list`] is the one
//! `drive` call of the code (a test greps the source tree for the others).
//! The CLI has no JSON output, so the table is read with a parser that
//! refuses anything it does not know. The parser never uses the file system
//! to choose between two readings (revision 3, M10): a padded path field is
//! `ambiguous` with every candidate.

use std::time::Instant;

use fleetdeck_proto::fleet_git::{ExposureState, SharePathState, TaildriveShare};

use super::{TIMEOUT, TailscaleCli, TailscaleError, ambient_env, parse_error};

/// The most shares of one report.
pub const MAX_SHARES: usize = 64;
/// The longest share name and `as` user, in characters.
const MAX_FIELD: usize = 64;

impl TailscaleCli {
    /// `tailscale drive list`, parsed. The share rows have no exposure yet:
    /// `exposure_state` is `unknown` until the station checks them.
    ///
    /// # Errors
    /// [`TailscaleError`]: no program, no answer in time, a bad exit, output
    /// that is too large, or a table that the strict parser refuses
    /// ([`TailscaleError::Parse`]).
    pub async fn drive_list(&self) -> Result<Vec<TaildriveShare>, TailscaleError> {
        let out = self
            .run(&["drive", "list"], ambient_env(), Instant::now() + TIMEOUT)
            .await?;
        parse_drive_list(&out)
    }
}

/// The run of `-` at the start of `chars`, and the rest.
fn dashes(chars: &[char]) -> (usize, &[char]) {
    let n = chars.iter().take_while(|c| **c == '-').count();
    (n, &chars[n..])
}

/// The widths `(a, b, c)` of the dash line, or `None`.
fn dash_widths(line: &[char]) -> Option<(usize, usize, usize)> {
    let (a, rest) = dashes(line);
    let rest = rest.strip_prefix(&[' '; 4])?;
    let (b, rest) = dashes(rest);
    let rest = rest.strip_prefix(&[' '; 4])?;
    let (c, rest) = dashes(rest);
    (a >= 4 && b >= 4 && c >= 2 && rest.is_empty()).then_some((a, b, c))
}

fn padded(text: &str, width: usize) -> String {
    format!("{text:<width$}")
}

fn is_absolute(path: &str) -> bool {
    let mut chars = path.chars();
    path.starts_with('/')
        || matches!(
            (chars.next(), chars.next(), chars.next()),
            (Some(d), Some(':'), Some('\\')) if d.is_ascii_alphabetic()
        )
}

fn name_ok(name: &str) -> bool {
    let n = name.chars().count();
    (1..=MAX_FIELD).contains(&n)
        && !name.starts_with(' ')
        && name
            .chars()
            .all(|c| matches!(c, 'a'..='z' | '0'..='9' | '_' | '(' | ')' | ' '))
}

fn as_user(rest: &str) -> Result<Option<String>, TailscaleError> {
    if rest.is_empty() {
        return Ok(None);
    }
    if rest.chars().count() > MAX_FIELD || rest.contains(' ') {
        return Err(parse_error("the `as` user is not valid"));
    }
    Ok(Some(rest.to_owned()))
}

/// One share row. The path state of one candidate uses `lstat` only
/// (item 9 of §15.3); an ambiguous path is never looked up.
fn share_row(line: &[char], a: usize, b: usize) -> Result<TaildriveShare, TailscaleError> {
    let spaces = |from: usize| line[from..from + 4].iter().all(|c| *c == ' ');
    if line.len() < a + 8 + b || !spaces(a) || !spaces(a + 4 + b) {
        return Err(parse_error("a share row does not fit the table"));
    }
    let text = |s: &[char]| s.iter().collect::<String>();
    let name = text(&line[..a]).trim_end_matches(' ').to_owned();
    if !name_ok(&name) {
        return Err(parse_error("a share name is not valid"));
    }
    let field = text(&line[a + 4..a + 4 + b]);
    let path = field.trim_end_matches(' ').to_owned();
    if !is_absolute(&path) {
        return Err(parse_error("a share path is not absolute"));
    }
    let k = field.len() - path.len();
    let (candidates, path_state) = if k == 0 {
        let state = match std::fs::symlink_metadata(&path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => SharePathState::Missing,
            _ => SharePathState::Ok,
        };
        (vec![path.clone()], state)
    } else {
        let all = (0..=k)
            .map(|n| format!("{path}{}", " ".repeat(n)))
            .collect();
        (all, SharePathState::Ambiguous)
    };
    Ok(TaildriveShare {
        name,
        path,
        candidates,
        as_user: as_user(&text(&line[a + 8 + b..]))?,
        exposed: Vec::new(),
        exposure_state: ExposureState::Unknown,
        unknown_reason: None,
        path_state,
    })
}

/// Parses the stdout of `tailscale drive list` (design §15.3).
///
/// # Errors
/// [`TailscaleError::Parse`] for any line that does not fit the table. No
/// partial list is returned.
pub fn parse_drive_list(bytes: &[u8]) -> Result<Vec<TaildriveShare>, TailscaleError> {
    let text = std::str::from_utf8(bytes).map_err(|_| parse_error("the table is not UTF-8"))?;
    let mut lines: Vec<&str> = text.split('\n').collect();
    if lines.pop() != Some("") || lines.len() < 2 {
        return Err(parse_error("the table does not end with a new line"));
    }
    if lines.iter().any(|l| l.chars().any(char::is_control)) {
        return Err(parse_error("the table has a control character"));
    }
    let dash: Vec<char> = lines[1].chars().collect();
    let (a, b, _) = dash_widths(&dash).ok_or_else(|| parse_error("the dash line is not valid"))?;
    if lines[0] != format!("{}    {}    as", padded("name", a), padded("path", b)) {
        return Err(parse_error("the header line is not valid"));
    }
    let mut shares: Vec<TaildriveShare> = Vec::new();
    for line in &lines[2..] {
        let chars: Vec<char> = line.chars().collect();
        let share = share_row(&chars, a, b)?;
        if shares.iter().any(|s| s.name == share.name) {
            return Err(parse_error("two shares have the same name"));
        }
        shares.push(share);
        if shares.len() > MAX_SHARES {
            return Err(parse_error("more than 64 shares"));
        }
    }
    Ok(shares)
}

#[cfg(test)]
#[path = "tailscale_drive_tests.rs"]
mod tests;
