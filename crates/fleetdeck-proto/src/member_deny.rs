//! The member file deny list (R11.9, DL-154, DL-156, DL-166).
//!
//! A member of a share never sees a sensitive file: the station hides every
//! path that matches the deny list from `files.list`, `files.read`,
//! `files.search` and `files.diff` when the deck marks the call as made for
//! a member ([`MEMBER_PARAM`]). The owner sees everything.
//!
//! A pattern is one of:
//!
//! - a name pattern without `/`: it matches any segment of a path, and
//!   `*` stands for any text and `?` for one character (case-insensitive,
//!   so a case-folding file system gives no way around it);
//! - a name pattern with a trailing `/` (`.aws/`): it matches a directory
//!   segment, so the files below it are hidden;
//! - a path pattern with `/` inside (`config/prod.yml`): it matches the
//!   path from the root of the workspace, and everything below it.
//!
//! A path that matches is hidden by a segment test, so `.env.local`,
//! `a/b/.env` and `.ssh/id_rsa` are all hidden.

use serde_json::Value;

/// The deny list of every station: R11.9 of `FLEETDECK-V2.md`.
pub const DEFAULT_MEMBER_DENY: &[&str] = &[
    ".env*",
    "*.pem",
    "*.key",
    "id_rsa*",
    "id_ed25519*",
    ".npmrc",
    ".pypirc",
    ".netrc",
    ".git-credentials",
    "credentials*.json",
    "*.keystore",
    ".aws/",
    ".ssh/",
    ".gnupg/",
    "secrets*",
];

/// The param that the deck adds to a `files.*` call for a member. The
/// station removes it before it decodes the query ([`take_member_flag`]).
pub const MEMBER_PARAM: &str = "member";

/// Removes the member flag from the params of a `files.*` call and tells
/// whether the call is for a member. A flag that is not exactly `true` makes
/// the call a member call too (fail closed).
pub fn take_member_flag(params: &mut Value) -> bool {
    params
        .as_object_mut()
        .and_then(|o| o.remove(MEMBER_PARAM))
        .is_some()
}

/// The longest pattern in characters.
pub const MAX_PATTERN_CHARS: usize = 200;

/// Checks a pattern of the station config `member_deny`.
///
/// # Errors
/// The reason for a refused pattern.
pub fn check_pattern(pattern: &str) -> Result<(), &'static str> {
    let body = pattern.trim_end_matches('/');
    if body.is_empty() || pattern.chars().count() > MAX_PATTERN_CHARS {
        return Err("a pattern has 1 to 200 characters");
    }
    if pattern.chars().any(|c| c.is_control() || c == '\\') {
        return Err("a pattern has no control character and no backslash");
    }
    if body.starts_with('/')
        || body
            .split('/')
            .any(|s| s.is_empty() || s == "." || s == "..")
    {
        return Err("a pattern is relative, without `.`, `..` or an empty segment");
    }
    Ok(())
}

/// The compiled deny list: the default patterns and the extra patterns of a
/// station.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberDeny {
    patterns: Vec<Pattern>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Pattern {
    /// The pattern without the trailing `/`, lower case.
    glob: String,
    /// The pattern has a `/` inside: it matches from the workspace root.
    anchored: bool,
}

impl Default for MemberDeny {
    fn default() -> Self {
        Self::new(&[])
    }
}

impl MemberDeny {
    /// The default patterns plus `extra`.
    #[must_use]
    pub fn new(extra: &[String]) -> Self {
        let patterns = DEFAULT_MEMBER_DENY
            .iter()
            .copied()
            .chain(extra.iter().map(String::as_str))
            .map(|p| {
                let glob = p.trim_end_matches('/').to_lowercase();
                Pattern {
                    anchored: glob.contains('/'),
                    glob,
                }
            })
            .collect();
        Self { patterns }
    }

    /// True when a member must not see the relative `path` (or a directory
    /// path). The empty path (the root) is never denied.
    #[must_use]
    pub fn denies(&self, path: &str) -> bool {
        let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
        let lower: Vec<String> = segments.iter().map(|s| s.to_lowercase()).collect();
        self.patterns.iter().any(|p| {
            if p.anchored {
                let want: Vec<&str> = p.glob.split('/').collect();
                lower.len() >= want.len() && want.iter().zip(&lower).all(|(w, s)| glob_match(w, s))
            } else {
                // A directory pattern (trailing `/`) matches the same way: the
                // directory itself and everything below it are hidden.
                lower.iter().any(|s| glob_match(&p.glob, s))
            }
        })
    }

    /// Removes the files of `patch` (a `git diff-tree -p --binary` text) that
    /// the deny list hides. A file section whose header cannot be read is
    /// hidden too (fail closed).
    #[must_use]
    pub fn filter_patch(&self, patch: &str) -> FilteredPatch {
        let mut out = FilteredPatch {
            patch: String::with_capacity(patch.len()),
            ..FilteredPatch::default()
        };
        let mut keep = true;
        let mut in_hunk = false;
        for line in patch.split_inclusive('\n') {
            if let Some(rest) = line.strip_prefix("diff --git ") {
                let paths = header_paths(rest.trim_end_matches(['\n', '\r']));
                keep = paths
                    .as_ref()
                    .is_some_and(|p| !p.iter().any(|p| self.denies(p)));
                in_hunk = false;
                if !keep {
                    out.hidden_files += 1;
                }
            }
            if keep {
                out.patch.push_str(line);
            } else if line.starts_with("@@") {
                in_hunk = true;
            } else if in_hunk {
                match line.as_bytes().first() {
                    Some(b'+') => out.hidden_insertions += 1,
                    Some(b'-') => out.hidden_deletions += 1,
                    _ => {}
                }
            }
        }
        out
    }
}

/// The result of [`MemberDeny::filter_patch`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FilteredPatch {
    /// The patch without the hidden files.
    pub patch: String,
    /// The number of hidden file sections.
    pub hidden_files: u32,
    /// The added lines of the hidden sections.
    pub hidden_insertions: u64,
    /// The removed lines of the hidden sections.
    pub hidden_deletions: u64,
}

/// The candidate paths of a `diff --git` header (the text after the
/// prefix). A header can name `a/<old> b/<new>`; a path with a space can be
/// split in several ways, so every split is a candidate. `None` for a header
/// that cannot be read.
#[must_use]
pub fn header_paths(rest: &str) -> Option<Vec<String>> {
    if rest.starts_with('"') {
        // Quoted names (special characters): `"a/x" "b/x"`, or one quoted.
        let mut names = Vec::new();
        let mut text = rest;
        while !text.is_empty() {
            text = text.trim_start();
            if let Some(body) = text.strip_prefix('"') {
                let (name, after) = unquote(body)?;
                names.push(name);
                text = after;
            } else {
                // A plain name next to a quoted one: `a/x "b/y z"`.
                let end = text.find(" \"").unwrap_or(text.len());
                names.push(text[..end].to_owned());
                text = &text[end..];
            }
        }
        let paths: Vec<String> = names
            .iter()
            .filter_map(|n| n.split_once('/').map(|(_, p)| p.to_owned()))
            .collect();
        return (!paths.is_empty()).then_some(paths);
    }
    let body = rest.strip_prefix("a/")?;
    let mut out = Vec::new();
    for (i, _) in body.match_indices(" b/") {
        out.push(body[..i].to_owned());
        out.push(body[i + 3..].to_owned());
    }
    (!out.is_empty()).then_some(out)
}

/// Reads a C-style quoted name (git's `core.quotePath` form) after the
/// opening quote. Returns the name and the text after the closing quote.
fn unquote(body: &str) -> Option<(String, &str)> {
    let bytes = body.as_bytes();
    let mut raw = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'"' => {
                return Some((String::from_utf8_lossy(&raw).into_owned(), &body[i + 1..]));
            }
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

/// `*` (any text) and `?` (one character) match; both texts are lower case.
fn glob_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    let (mut pi, mut ti) = (0usize, 0usize);
    let (mut star, mut mark) = (None::<usize>, 0usize);
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            mark = ti;
            pi += 1;
        } else if let Some(s) = star {
            pi = s + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn deny() -> MemberDeny {
        MemberDeny::default()
    }

    #[test]
    fn the_default_list_hides_secrets_at_any_depth() {
        let d = deny();
        for hidden in [
            ".env",
            ".env.local",
            "a/b/.ENV",
            "server.pem",
            "x/tls.key",
            "id_rsa",
            "id_rsa.pub",
            "home/id_ed25519",
            ".npmrc",
            "pkg/.pypirc",
            ".netrc",
            ".git-credentials",
            "credentials.json",
            "credentials-prod.json",
            "app.keystore",
            ".aws/config",
            "deep/.ssh/known_hosts",
            ".gnupg",
            "secrets.yml",
            "secrets/db.txt",
            "SECRETS.toml",
        ] {
            assert!(d.denies(hidden), "{hidden}");
        }
        for shown in [
            "",
            "src/main.rs",
            "README.md",
            "environment.rs",
            "keys.rs",
            "monkey.rs",
            "credentials.md",
            "docs/secret-santa.md",
            ".github/workflows/ci.yml",
            "env",
        ] {
            assert!(!d.denies(shown), "{shown}");
        }
    }

    #[test]
    fn a_station_adds_name_directory_and_path_patterns() {
        let d = MemberDeny::new(&[
            "*.tfstate".to_owned(),
            "private/".to_owned(),
            "config/prod.yml".to_owned(),
        ]);
        assert!(d.denies("infra/main.tfstate"));
        assert!(d.denies("private/a.txt"));
        assert!(d.denies("x/private/a.txt"));
        assert!(d.denies("config/prod.yml"));
        assert!(d.denies("config/prod.yml/extra"));
        assert!(!d.denies("other/config/prod.yml"));
        assert!(!d.denies("config/dev.yml"));
        assert!(!d.denies("src/deploy.rs"));
    }

    #[test]
    fn a_pattern_is_checked() {
        for ok in ["*.pem", ".aws/", "a/b.txt"] {
            assert_eq!(check_pattern(ok), Ok(()), "{ok}");
        }
        for bad in ["", "/", "/abs", "a//b", "a/../b", "./a", "a\\b", "a\nb"] {
            assert!(check_pattern(bad).is_err(), "{bad:?}");
        }
        assert!(check_pattern(&"x".repeat(MAX_PATTERN_CHARS + 1)).is_err());
    }

    #[test]
    fn a_patch_loses_the_files_that_the_list_hides() {
        let patch = "\
diff --git a/src/a.rs b/src/a.rs
index 1..2 100644
--- a/src/a.rs
+++ b/src/a.rs
@@ -1 +1 @@
-x
+y
diff --git a/.env b/.env
new file mode 100644
--- /dev/null
+++ b/.env
@@ -0,0 +1 @@
+TOKEN=abc
diff --git a/deploy/server.pem b/deploy/server.pem
deleted file mode 100644
--- a/deploy/server.pem
+++ /dev/null
@@ -1 +0,0 @@
-KEY
diff --git a/README.md b/README.md
--- a/README.md
+++ b/README.md
@@ -1 +1 @@
-a
+b
";
        let f = deny().filter_patch(patch);
        let (out, hidden) = (f.patch, f.hidden_files);
        assert_eq!(hidden, 2);
        assert_eq!((f.hidden_insertions, f.hidden_deletions), (1, 1));
        assert!(out.contains("src/a.rs"));
        assert!(out.contains("README.md"));
        assert!(!out.contains(".env"));
        assert!(!out.contains("TOKEN"));
        assert!(!out.contains("server.pem"));
        assert!(!out.contains("KEY"));
    }

    #[test]
    fn a_renamed_or_oddly_named_file_is_still_hidden() {
        let d = deny();
        // The old name is public, the new name is secret.
        let f = d.filter_patch(
            "diff --git a/notes.txt b/.env.prod\nrename from notes.txt\nrename to .env.prod\n",
        );
        let (out, hidden) = (f.patch, f.hidden_files);
        assert_eq!((out.as_str(), hidden), ("", 1));
        // A quoted name with an escaped byte.
        let f =
            d.filter_patch("diff --git \"a/caf\\303\\251/.env\" \"b/caf\\303\\251/.env\"\n+x\n");
        let (out, hidden) = (f.patch, f.hidden_files);
        assert_eq!((out.as_str(), hidden), ("", 1));
        // A path with a space and a fake ` b/` inside.
        let f = d.filter_patch("diff --git a/x b/.env b/x b/.env\n+x\n");
        let (out, hidden) = (f.patch, f.hidden_files);
        assert_eq!((out.as_str(), hidden), ("", 1));
        // A header that cannot be read is hidden.
        let f = d.filter_patch("diff --git nonsense\n+x\n");
        let (out, hidden) = (f.patch, f.hidden_files);
        assert_eq!((out.as_str(), hidden), ("", 1));
        // Text before the first header stays (a commit header).
        let f = d.filter_patch("preamble\ndiff --git a/ok.rs b/ok.rs\n+x\n");
        let (out, hidden) = (f.patch, f.hidden_files);
        assert_eq!(hidden, 0);
        assert!(out.starts_with("preamble\n"));
    }

    #[test]
    fn the_member_flag_is_taken_from_the_params() {
        let mut p = serde_json::json!({"root": {"session": "s"}, "member": true});
        assert!(take_member_flag(&mut p));
        assert!(p.get("member").is_none());
        let mut q = serde_json::json!({"root": {"session": "s"}});
        assert!(!take_member_flag(&mut q));
    }

    #[test]
    fn glob_handles_stars_and_marks() {
        assert!(glob_match("*", ""));
        assert!(glob_match(".env*", ".env"));
        assert!(glob_match("a?c", "abc"));
        assert!(!glob_match("a?c", "ac"));
        assert!(glob_match("*.pem", "x.y.pem"));
        assert!(!glob_match("*.pem", "x.pem.txt"));
        assert!(glob_match("credentials*.json", "credentials.json"));
    }
}
