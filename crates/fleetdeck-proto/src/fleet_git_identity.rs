//! Repository identity of Fleet Git (design §3.8 item 1, FG-5 and FG-2):
//! [`remote_identity`] turns a remote URL into a grouping key (`repo`) and a
//! write authority (`dest`), and [`effective_url`] applies the `insteadOf`
//! rewrite rules of Git (the WIP rule, §3.8 rule 2).
//!
//! github.com and gitlab.com remotes get one canonical https dest (Fleet Sync §1.1).
//!
//! The functions are pure. They read no file and run no Git. The checks that
//! need the station (a canonical local path, a bare repository) are in the
//! station's home check.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// The repository of a remote (§3.8). `repo` groups; `dest` is the write
/// authority and the key of a push lease (FG-2).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, TS)]
#[ts(export_to = "fleet_git/")]
pub struct RemoteIdentity {
    /// `host/path`: the O16 grouping key. Two aliases can share it.
    pub repo: String,
    /// The write authority, for example `https://github.com:443/acme/app.git`.
    /// github.com and gitlab.com remotes get one canonical https dest (Fleet Sync §1.1).
    pub dest: String,
}

/// Whether a local remote (`file://` or an absolute path) is allowed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalRemotes {
    Deny,
    Allow,
}

/// Why [`remote_identity`] refused a URL. Each variant has one wire code
/// (`fleet_git::code`) and one text for the board.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdentityError {
    /// A password or a token in the URL (`remote_has_credentials`).
    RemoteHasCredentials,
    /// A `file://` URL or a local path while local remotes are denied
    /// (`remote_local_refused`).
    RemoteLocalRefused,
    /// An empty path, a `..` segment, or a bad local path (`remote_path_invalid`).
    RemotePathInvalid,
    /// An empty, non-ASCII or malformed host, or a bad port (`remote_host_invalid`).
    RemoteHostInvalid,
    /// A path with `%` (`escaped_path`, revision 4, B2), or a URL form that
    /// Fleet Git does not use (`remote_unsupported`).
    RemoteUnsupported { escaped_path: bool },
}

impl IdentityError {
    /// The wire code.
    #[must_use]
    pub fn code(&self) -> &'static str {
        use crate::fleet_git::code;
        match self {
            Self::RemoteHasCredentials => code::REMOTE_HAS_CREDENTIALS,
            Self::RemoteLocalRefused => code::REMOTE_LOCAL_REFUSED,
            Self::RemotePathInvalid => code::REMOTE_PATH_INVALID,
            Self::RemoteHostInvalid => code::REMOTE_HOST_INVALID,
            Self::RemoteUnsupported { .. } => code::REMOTE_UNSUPPORTED,
        }
    }
}

impl std::fmt::Display for IdentityError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::RemoteHasCredentials => {
                "The remote URL carries credentials; the station uses its own Git credentials."
            }
            Self::RemoteLocalRefused => {
                "Local remotes are allowed only on stations with allow_local_remotes = true."
            }
            Self::RemotePathInvalid => "The remote URL has an invalid path.",
            Self::RemoteHostInvalid => "The remote URL has an invalid host or port.",
            Self::RemoteUnsupported { escaped_path: true } => {
                "This remote URL has an escaped path. Use a plain path."
            }
            Self::RemoteUnsupported {
                escaped_path: false,
            } => "Use an https, ssh or user@host:path remote URL.",
        })
    }
}

impl std::error::Error for IdentityError {}

/// What kind of URL it is, after the scheme or the scp-like split.
enum Form<'a> {
    /// `https://` and `ssh://`: the text after the scheme.
    Url { ssh: bool, rest: &'a str },
    /// `[user@]host:path`.
    Scp { host_part: &'a str, path: &'a str },
    /// `file://<path>` or an absolute local path.
    File { path: &'a str },
}

fn has_scheme(url: &str, scheme: &str) -> bool {
    url.get(..scheme.len())
        .is_some_and(|head| head.eq_ignore_ascii_case(scheme))
}

fn unsupported() -> IdentityError {
    IdentityError::RemoteUnsupported {
        escaped_path: false,
    }
}

fn classify(url: &str) -> Result<Form<'_>, IdentityError> {
    if url.is_empty() || url.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err(unsupported());
    }
    if has_scheme(url, "https://") {
        return Ok(Form::Url {
            ssh: false,
            rest: &url[8..],
        });
    }
    if has_scheme(url, "ssh://") {
        return Ok(Form::Url {
            ssh: true,
            rest: &url[6..],
        });
    }
    if has_scheme(url, "file://") {
        return Ok(Form::File { path: &url[7..] });
    }
    if url.starts_with('/') {
        return Ok(Form::File { path: url });
    }
    // `<helper>::<address>` starts a remote helper (`ext::` runs commands); a
    // `::` inside an IPv6 literal is not one.
    let helper = url.find("::").is_some_and(|at| !url[..at].contains('['));
    if url.contains("://") || helper {
        return Err(unsupported());
    }
    // scp-like `[user@]host:path`; an IPv6 literal keeps its brackets.
    let host_at = url.find('@').map_or(0, |at| at + 1);
    let colon = if url[host_at..].starts_with('[') {
        url[host_at..].find("]:").map(|end| host_at + end + 1)
    } else {
        url.find(':')
    };
    match colon {
        Some(at) if !url[..at].contains('/') && at + 1 < url.len() => Ok(Form::Scp {
            host_part: &url[..at],
            path: &url[at + 1..],
        }),
        _ => Err(unsupported()),
    }
}

/// Splits `[user@]host[:port]`. Returns `(login name, host, port)`; the host
/// is in lower case and an IPv6 literal keeps its brackets.
fn split_authority(
    authority: &str,
    default_port: u16,
    allow_port: bool,
) -> Result<(Option<String>, String, u16), IdentityError> {
    let (user, host_port) = match authority.rsplit_once('@') {
        Some((user, rest)) => (Some(user), rest),
        None => (None, authority),
    };
    if user.is_some_and(|u| u.contains(':')) {
        return Err(IdentityError::RemoteHasCredentials);
    }
    let (host, port) = if host_port.starts_with('[') {
        let end = host_port
            .find(']')
            .ok_or(IdentityError::RemoteHostInvalid)?;
        let host = &host_port[..=end];
        match &host_port[end + 1..] {
            "" => (host, None),
            tail => (
                host,
                Some(
                    tail.strip_prefix(':')
                        .ok_or(IdentityError::RemoteHostInvalid)?,
                ),
            ),
        }
    } else {
        match host_port.split_once(':') {
            Some((host, port)) => (host, Some(port)),
            None => (host_port, None),
        }
    };
    if host.is_empty() || host.starts_with('-') || !host.is_ascii() {
        return Err(IdentityError::RemoteHostInvalid);
    }
    let port = match port {
        None => default_port,
        Some(_) if !allow_port => return Err(IdentityError::RemoteHostInvalid),
        Some(text) => match text.parse::<u16>() {
            Ok(port) if port != 0 && text.bytes().all(|b| b.is_ascii_digit()) => port,
            _ => return Err(IdentityError::RemoteHostInvalid),
        },
    };
    let user = user.filter(|u| !u.is_empty()).map(str::to_owned);
    Ok((user, host.to_ascii_lowercase(), port))
}

/// Checks the path rules that every remote form shares: no `..` segment.
fn check_segments(path: &str) -> Result<(), IdentityError> {
    if path.split('/').any(|segment| segment == "..") {
        return Err(IdentityError::RemotePathInvalid);
    }
    Ok(())
}

/// The O16 grouping path: no leading `/`, `/~/` or `/~`, no trailing `/`, one
/// final `.git` removed.
fn repo_path(path: &str) -> String {
    let path = path
        .strip_prefix("/~/")
        .or_else(|| path.strip_prefix("/~"))
        .or_else(|| path.strip_prefix('/'))
        .unwrap_or(path);
    let path = path.strip_suffix('/').unwrap_or(path);
    path.strip_suffix(".git").unwrap_or(path).to_owned()
}

/// The identity of a remote URL (design §3.8 item 1).
///
/// # Errors
/// An [`IdentityError`] for a URL that Fleet Git refuses.
pub fn remote_identity(url: &str, local: LocalRemotes) -> Result<RemoteIdentity, IdentityError> {
    match classify(url)? {
        Form::File { path } => {
            if local == LocalRemotes::Deny {
                return Err(IdentityError::RemoteLocalRefused);
            }
            file_identity(path)
        }
        Form::Url { ssh, rest } => {
            let (authority, path) = rest.find('/').map_or((rest, ""), |at| rest.split_at(at));
            let (user, host, port) = split_authority(authority, if ssh { 22 } else { 443 }, true)?;
            if !ssh && user.is_some() {
                return Err(IdentityError::RemoteHasCredentials);
            }
            if path.contains('%') {
                return Err(IdentityError::RemoteUnsupported { escaped_path: true });
            }
            let path = if ssh {
                path
            } else {
                path.strip_suffix('/').unwrap_or(path)
            };
            if path.is_empty() || path == "/" {
                return Err(IdentityError::RemotePathInvalid);
            }
            check_segments(path)?;
            Ok(identity(ssh, user.as_deref(), &host, port, path))
        }
        Form::Scp { host_part, path } => {
            let (user, host, port) = split_authority(host_part, 22, false)?;
            if path.contains('%') {
                return Err(IdentityError::RemoteUnsupported { escaped_path: true });
            }
            // Git reads `host:p` from the login folder and expands `~`.
            let path = if path.starts_with('/') {
                path.to_owned()
            } else if path.starts_with('~') {
                format!("/{path}")
            } else {
                format!("/~/{path}")
            };
            check_segments(&path)?;
            Ok(identity(true, user.as_deref(), &host, port, &path))
        }
    }
}

/// The hosts whose remotes get one canonical `dest` (Fleet Sync §1.1).
pub const FORGE_HOSTS: &[&str] = &["github.com", "gitlab.com"];

/// The lower-case path `owner/repo` of a forge URL, or `None` for any other
/// URL (Fleet Sync §1.1, revision 2 R1).
fn forge_path(ssh: bool, user: Option<&str>, host: &str, port: u16, path: &str) -> Option<String> {
    let form_ok = if ssh {
        port == 22 && user == Some("git")
    } else {
        port == 443 && user.is_none()
    };
    if !FORGE_HOSTS.contains(&host) || !form_ok {
        return None;
    }
    let p = path
        .strip_prefix("/~/")
        .or_else(|| path.strip_prefix('/'))
        .unwrap_or(path);
    let p = p.strip_suffix('/').unwrap_or(p);
    let ends_git = |text: &str| {
        text.len() >= 4
            && text.is_char_boundary(text.len() - 4)
            && text[text.len() - 4..].eq_ignore_ascii_case(".git")
    };
    let p = if ends_git(p) { &p[..p.len() - 4] } else { p };
    if p.is_empty() || ends_git(p) {
        return None;
    }
    let segments: Vec<&str> = p.split('/').collect();
    let segment_ok = |s: &&str| {
        !s.is_empty()
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
    };
    if segments.len() < 2 || !segments.iter().all(segment_ok) {
        return None;
    }
    Some(p.to_ascii_lowercase())
}

fn identity(ssh: bool, user: Option<&str>, host: &str, port: u16, path: &str) -> RemoteIdentity {
    if let Some(p) = forge_path(ssh, user, host, port, path) {
        // A forge URL: one canonical `dest`, and `repo` is the lower-case
        // `host/owner/repo` (Fleet Sync §1.1, R2).
        return RemoteIdentity {
            repo: format!("{host}/{p}"),
            dest: format!("https://{host}:443/{p}"),
        };
    }
    let dest = if ssh {
        let login = user.map(|u| format!("{u}@")).unwrap_or_default();
        format!("ssh://{login}{host}:{port}{path}")
    } else {
        format!("https://{host}:{port}{path}")
    };
    RemoteIdentity {
        repo: format!("{host}/{}", repo_path(path)),
        dest,
    }
}

/// Whether two destinations name the same repository. A stored `dest` can be
/// in the form of an older release: `a == b`, else both are read again with
/// [`remote_identity`] and their `dest` values are equal (Fleet Sync §1.1,
/// revision 2 R3).
#[must_use]
pub fn same_dest(a: &str, b: &str) -> bool {
    if a == b {
        return true;
    }
    match (
        remote_identity(a, LocalRemotes::Allow),
        remote_identity(b, LocalRemotes::Allow),
    ) {
        (Ok(a), Ok(b)) => a.dest == b.dest,
        _ => false,
    }
}

/// A local remote: the deck checks the syntax only (absolute, no `.` or `..`
/// segment, no `//`); the station checks the rest in its home check.
fn file_identity(path: &str) -> Result<RemoteIdentity, IdentityError> {
    if path.contains('%') {
        return Err(IdentityError::RemoteUnsupported { escaped_path: true });
    }
    if !path.starts_with('/')
        || path.contains("//")
        || path.split('/').any(|s| s == "." || s == "..")
    {
        return Err(IdentityError::RemotePathInvalid);
    }
    Ok(RemoteIdentity {
        repo: format!("localhost/{}", &path[1..]),
        dest: format!("file://{path}"),
    })
}

/// One `url.<base>.insteadOf` or `url.<base>.pushInsteadOf` rule of the Git
/// configuration (design §3.8 rule 2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RewriteRule {
    /// The text that replaces the matched prefix.
    pub base: String,
    /// The prefix that the rule matches.
    pub prefix: String,
    /// True for `pushInsteadOf`, false for `insteadOf`.
    pub push: bool,
}

/// Parses the output of `git config --get-regexp
/// '^url\..*\.(pushinsteadof|insteadof)$'`: one `key value` line per rule.
/// A line that is not a rule, and a rule with an empty prefix, is skipped.
#[must_use]
pub fn parse_rewrite_rules(config_output: &str) -> Vec<RewriteRule> {
    config_output
        .lines()
        .filter_map(|line| {
            let (key, prefix) = line.split_once(' ')?;
            let key = key.strip_prefix("url.")?;
            let lower = key.to_ascii_lowercase();
            let (base_len, push) = if lower.ends_with(".pushinsteadof") {
                (key.len() - ".pushinsteadof".len(), true)
            } else if lower.ends_with(".insteadof") {
                (key.len() - ".insteadof".len(), false)
            } else {
                return None;
            };
            (!prefix.is_empty()).then(|| RewriteRule {
                base: key[..base_len].to_owned(),
                prefix: prefix.to_owned(),
                push,
            })
        })
        .collect()
}

/// The URL that Git uses after the rewrite rules. The longest matching prefix
/// wins. For a push, a `pushInsteadOf` match wins over every `insteadOf`
/// match; a fetch uses `insteadOf` rules only.
#[must_use]
pub fn effective_url(url: &str, rules: &[RewriteRule], push: bool) -> String {
    let longest = |want_push: bool| {
        let mut best: Option<&RewriteRule> = None;
        for rule in rules
            .iter()
            .filter(|r| r.push == want_push && url.starts_with(&r.prefix))
        {
            if best.is_none_or(|b| rule.prefix.len() > b.prefix.len()) {
                best = Some(rule);
            }
        }
        best
    };
    let chosen = if push { longest(true) } else { None }.or_else(|| longest(false));
    match chosen {
        Some(rule) => format!("{}{}", rule.base, &url[rule.prefix.len()..]),
        None => url.to_owned(),
    }
}
