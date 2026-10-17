//! The anchor of a review comment: the lines of a unified diff that the
//! comment sits on (DL-179, design 2.2). The deck reads them from the diff
//! that the station returned for the caller; the client never sends them.
//!
//! Rules (design answers Q9):
//!
//! - the new side numbers the context lines and the `+` lines of a hunk, the
//!   old side the context lines and the `-` lines; `\ No newline at end of
//!   file` is skipped;
//! - a range must lie inside one hunk: a range over two hunks, or over a line
//!   that no hunk shows, is not in the diff;
//! - the anchor text is the lines joined with `\n` (no trailing newline);
//! - the paths come from the `diff --git a/<old> b/<new>` header, replaced by
//!   the `rename from/to` or `copy from/to` lines when present; a new file has
//!   no old path and a deleted file no new path;
//! - a binary file and a rename without hunks have no lines;
//! - a diff that the station cut (`\n[diff truncated]\n`) loses its last,
//!   possibly partial, line.

use fleetdeck_proto::review::{MAX_ANCHOR_BYTES, ReviewSide};

/// The text that the station appends to a cut patch.
pub const CUT_MARKER: &str = "\n[diff truncated]\n";

/// The lines of one comment target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Anchor {
    pub text: String,
    /// The path of the commented side.
    pub path: String,
    /// The path of the other side, when it differs.
    pub other_path: Option<String>,
}

/// Why no anchor exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnchorError {
    /// The path, the side or the range is not shown by the diff.
    NotInDiff,
    /// The lines have more than [`MAX_ANCHOR_BYTES`] bytes.
    TooLarge,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Context,
    Added,
    Removed,
}

#[derive(Debug, Clone)]
struct Line {
    text: String,
    old_no: Option<u32>,
    new_no: Option<u32>,
}

#[derive(Debug, Clone, Default)]
struct Hunk {
    lines: Vec<Line>,
}

#[derive(Debug, Clone, Default)]
struct File {
    old: Option<String>,
    new: Option<String>,
    hunks: Vec<Hunk>,
}

/// A parsed unified diff.
#[derive(Debug, Clone, Default)]
pub struct ParsedDiff {
    files: Vec<File>,
}

/// The patch without a cut marker and without the partial line that a cut
/// leaves before it.
fn without_cut(patch: &str) -> &str {
    match patch.strip_suffix(CUT_MARKER) {
        None => patch,
        Some(body) if body.is_empty() || body.ends_with('\n') => body,
        Some(body) => body.rfind('\n').map_or("", |i| &body[..=i]),
    }
}

/// Reads a C-style quoted name (the `core.quotePath` form) after the opening
/// quote. Returns the name and the text after the closing quote.
fn unquote(body: &str) -> Option<(String, &str)> {
    let bytes = body.as_bytes();
    let mut raw = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'"' => return Some((String::from_utf8_lossy(&raw).into_owned(), &body[i + 1..])),
            b'\\' => {
                i += 1;
                match *bytes.get(i)? {
                    b'n' => raw.push(b'\n'),
                    b't' => raw.push(b'\t'),
                    b'r' => raw.push(b'\r'),
                    c @ b'0'..=b'3' => {
                        let digits = bytes.get(i..i + 3)?;
                        if !digits.iter().all(|d| (b'0'..=b'7').contains(d)) {
                            return None;
                        }
                        let v = (u16::from(c - b'0') << 6)
                            | (u16::from(digits[1] - b'0') << 3)
                            | u16::from(digits[2] - b'0');
                        raw.push(u8::try_from(v).ok()?);
                        i += 2;
                    }
                    other => raw.push(other),
                }
            }
            c => raw.push(c),
        }
        i += 1;
    }
    None
}

/// A name of a `rename`, `copy`, `---` or `+++` line: plain or quoted.
fn name(text: &str) -> Option<String> {
    let text = text.trim_end_matches(['\t', '\r']);
    match text.strip_prefix('"') {
        Some(body) => unquote(body).map(|(n, _)| n),
        None => Some(text.to_owned()),
    }
}

/// A name of a `---` or `+++` line without its `a/` or `b/`; `None` for
/// `/dev/null`.
fn side_path(text: &str, prefix: &str) -> Option<String> {
    let n = name(text)?;
    if n == "/dev/null" {
        return None;
    }
    n.strip_prefix(prefix).map(str::to_owned)
}

/// The `(old, new)` paths that a `diff --git` header names. A header whose
/// paths have spaces can be split in several ways: the split with the same
/// path on both sides wins, else the header does not decide (the `---`,
/// `+++` and `rename` lines do).
fn header_paths(rest: &str) -> Option<(String, String)> {
    let rest = rest.trim_end_matches('\r');
    if let Some(body) = rest.strip_prefix('"') {
        let (first, after) = unquote(body)?;
        let after = after.trim_start();
        let second = match after.strip_prefix('"') {
            Some(b) => unquote(b)?.0,
            None => after.to_owned(),
        };
        return Some((
            first.strip_prefix("a/")?.to_owned(),
            second.strip_prefix("b/")?.to_owned(),
        ));
    }
    let body = rest.strip_prefix("a/")?;
    if let Some(i) = body.find(" \"b/") {
        let second = unquote(&body[i + 2..])?.0;
        return Some((body[..i].to_owned(), second.strip_prefix("b/")?.to_owned()));
    }
    let candidates: Vec<(&str, &str)> = body
        .match_indices(" b/")
        .map(|(i, _)| (&body[..i], &body[i + 3..]))
        .collect();
    match candidates.as_slice() {
        [(o, n)] => Some(((*o).to_owned(), (*n).to_owned())),
        many => many
            .iter()
            .find(|(o, n)| o == n)
            .map(|(o, n)| ((*o).to_owned(), (*n).to_owned())),
    }
}

/// `(old_start, old_count, new_start, new_count)` of a `@@ -a[,b] +c[,d] @@`
/// line.
fn hunk_counts(line: &str) -> Option<(u32, u32, u32, u32)> {
    let rest = line.strip_prefix("@@ -")?;
    let (old, rest) = rest.split_once(" +")?;
    let (new, _) = rest.split_once(" @@")?;
    let range = |s: &str| -> Option<(u32, u32)> {
        match s.split_once(',') {
            Some((a, b)) => Some((a.parse().ok()?, b.parse().ok()?)),
            None => Some((s.parse().ok()?, 1)),
        }
    };
    let (os, oc) = range(old)?;
    let (ns, nc) = range(new)?;
    Some((os, oc, ns, nc))
}

/// What the parser holds for the file it reads.
#[derive(Default)]
struct Pending {
    header: Option<(String, String)>,
    minus: Option<Option<String>>,
    plus: Option<Option<String>>,
    rename: (Option<String>, Option<String>),
    is_new: bool,
    is_deleted: bool,
    hunks: Vec<Hunk>,
}

impl Pending {
    fn finish(self) -> File {
        let (mut old, mut new) = match self.header {
            Some((o, n)) => (Some(o), Some(n)),
            // An ambiguous header: the `---` and `+++` lines decide.
            None => (self.minus.flatten(), self.plus.flatten()),
        };
        if self.is_new {
            old = None;
        }
        if self.is_deleted {
            new = None;
        }
        if let (Some(from), Some(to)) = self.rename {
            old = Some(from);
            new = Some(to);
        }
        File {
            old,
            new,
            hunks: self.hunks,
        }
    }
}

/// The hunk that the parser reads and the lines that it still owes.
struct Open {
    hunk: Hunk,
    old_no: u32,
    new_no: u32,
    old_left: u32,
    new_left: u32,
}

impl Open {
    fn owes(&self) -> bool {
        self.old_left > 0 || self.new_left > 0
    }

    /// Takes `line` as a line of the hunk. `false`: it is not one.
    fn take(&mut self, line: &str) -> bool {
        let (kind, text) = match line.as_bytes().first() {
            Some(b' ') => (Kind::Context, &line[1..]),
            Some(b'+') => (Kind::Added, &line[1..]),
            Some(b'-') => (Kind::Removed, &line[1..]),
            // `\ No newline at end of file`: no line, still in the hunk.
            Some(b'\\') => return true,
            _ => return false,
        };
        let mut l = Line {
            text: text.to_owned(),
            old_no: None,
            new_no: None,
        };
        if kind != Kind::Added {
            l.old_no = Some(self.old_no);
            self.old_no += 1;
            self.old_left = self.old_left.saturating_sub(1);
        }
        if kind != Kind::Removed {
            l.new_no = Some(self.new_no);
            self.new_no += 1;
            self.new_left = self.new_left.saturating_sub(1);
        }
        self.hunk.lines.push(l);
        true
    }
}

impl ParsedDiff {
    /// Parses `patch`, a unified diff of `diff-tree -p` or `diff`.
    #[must_use]
    pub fn parse(patch: &str) -> Self {
        let patch = without_cut(patch);
        let mut files = Vec::new();
        let mut cur: Option<Pending> = None;
        let mut open: Option<Open> = None;
        for line in patch.split('\n') {
            if let Some(o) = open.as_mut()
                && o.owes()
                && o.take(line)
            {
                continue;
            }
            // Any other line ends the hunk: the counts are used up, or the
            // patch is cut or damaged.
            if let Some(o) = open.take()
                && let Some(p) = cur.as_mut()
            {
                p.hunks.push(o.hunk);
            }
            if let Some(rest) = line.strip_prefix("diff --git ") {
                if let Some(p) = cur.take() {
                    files.push(p.finish());
                }
                cur = Some(Pending {
                    header: header_paths(rest),
                    ..Pending::default()
                });
                continue;
            }
            let Some(p) = cur.as_mut() else { continue };
            if let Some((old_start, old_left, new_start, new_left)) = hunk_counts(line) {
                open = Some(Open {
                    hunk: Hunk::default(),
                    old_no: old_start,
                    new_no: new_start,
                    old_left,
                    new_left,
                });
            } else if let Some(n) = line.strip_prefix("--- ") {
                p.minus = Some(side_path(n, "a/"));
            } else if let Some(n) = line.strip_prefix("+++ ") {
                p.plus = Some(side_path(n, "b/"));
            } else if let Some(n) = line
                .strip_prefix("rename from ")
                .or_else(|| line.strip_prefix("copy from "))
            {
                p.rename.0 = name(n);
            } else if let Some(n) = line
                .strip_prefix("rename to ")
                .or_else(|| line.strip_prefix("copy to "))
            {
                p.rename.1 = name(n);
            } else if line.starts_with("new file mode") {
                p.is_new = true;
            } else if line.starts_with("deleted file mode") {
                p.is_deleted = true;
            }
        }
        if let Some(o) = open.take()
            && let Some(p) = cur.as_mut()
        {
            p.hunks.push(o.hunk);
        }
        if let Some(p) = cur.take() {
            files.push(p.finish());
        }
        Self { files }
    }

    /// The anchor of `start..=end` of `side` of the file `path`.
    ///
    /// # Errors
    /// [`AnchorError::NotInDiff`] when the file, the side or a line of the
    /// range is not shown, or the range crosses two hunks;
    /// [`AnchorError::TooLarge`] for more than [`MAX_ANCHOR_BYTES`] bytes.
    pub fn anchor(
        &self,
        side: ReviewSide,
        path: &str,
        start: u32,
        end: u32,
    ) -> Result<Anchor, AnchorError> {
        if start == 0 || end < start {
            return Err(AnchorError::NotInDiff);
        }
        let mut error = AnchorError::NotInDiff;
        for file in &self.files {
            let (here, other) = match side {
                ReviewSide::New => (&file.new, &file.old),
                ReviewSide::Old => (&file.old, &file.new),
            };
            if here.as_deref() != Some(path) {
                continue;
            }
            match file.range(side, start, end) {
                Ok(text) => {
                    return Ok(Anchor {
                        text,
                        path: path.to_owned(),
                        other_path: other.clone().filter(|o| o != path),
                    });
                }
                Err(AnchorError::TooLarge) => error = AnchorError::TooLarge,
                Err(AnchorError::NotInDiff) => {}
            }
        }
        Err(error)
    }
}

impl File {
    /// The lines `start..=end` of `side`, when one hunk shows all of them.
    fn range(&self, side: ReviewSide, start: u32, end: u32) -> Result<String, AnchorError> {
        let number = |l: &Line| match side {
            ReviewSide::New => l.new_no,
            ReviewSide::Old => l.old_no,
        };
        let hunk = self
            .hunks
            .iter()
            .find(|h| h.lines.iter().any(|l| number(l) == Some(start)))
            .ok_or(AnchorError::NotInDiff)?;
        let lines: Vec<&str> = hunk
            .lines
            .iter()
            .filter(|l| number(l).is_some_and(|n| n >= start && n <= end))
            .map(|l| l.text.as_str())
            .collect();
        if lines.len() != (end - start + 1) as usize {
            return Err(AnchorError::NotInDiff);
        }
        let text = lines.join("\n");
        if text.len() > MAX_ANCHOR_BYTES {
            return Err(AnchorError::TooLarge);
        }
        Ok(text)
    }
}

#[cfg(test)]
#[path = "review_anchor_tests.rs"]
mod tests;
