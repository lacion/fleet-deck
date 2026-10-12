//! Workspace provisioning (PLAN §6.6, M3 6-1, DL-200, DL-201): callsigns,
//! the worktree and clone plans that the decision checks, the Git work that
//! the start effect does, and the repo catalog facts of `workspace.list`.
//!
//! The decision step only reads the file system (no `git` process); the
//! effect worker runs `git` before the provider starts.

use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use crate::store::WorkspacePlan;
use crate::workspace::{Git, GitOptions};

/// The v0 callsign rotation (`src/daemon/derive.ts`).
pub const CALLSIGNS: [&str; 12] = [
    "falcon", "otter", "raven", "lynx", "orca", "wren", "viper", "heron", "badger", "comet",
    "ember", "drift",
];

/// The longest clone URL that the station takes.
pub const MAX_URL_CHARS: usize = 2048;
/// The time limit of `git worktree add`.
pub const WORKTREE_TIMEOUT: Duration = Duration::from_secs(120);
/// The time limit of `git clone`.
pub const CLONE_TIMEOUT: Duration = Duration::from_secs(15 * 60);

/// Why a plan is refused; `forbidden` maps to `RejectCode::Forbidden`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refused {
    pub forbidden: bool,
    pub message: String,
}

fn invalid(message: impl Into<String>) -> Refused {
    Refused {
        forbidden: false,
        message: message.into(),
    }
}

fn forbidden(message: impl Into<String>) -> Refused {
    Refused {
        forbidden: true,
        message: message.into(),
    }
}

/// The callsign candidates of a new session, best first: the rotation from
/// `start` with the last four id characters, then the rotation with the
/// whole id (unique, because the id is).
#[must_use]
pub fn callsign_candidates(session_id: &str, start: u64) -> Vec<String> {
    let id: String = session_id
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|c| c.to_ascii_lowercase())
        .collect();
    let short: String = id.chars().skip(id.len().saturating_sub(4)).collect();
    let n = CALLSIGNS.len();
    let offset = usize::try_from(start % n as u64).unwrap_or(0);
    let animal = |i: usize| CALLSIGNS[(offset + i) % n];
    (0..n)
        .map(|i| format!("{}-{short}", animal(i)))
        .chain((0..n).map(|i| format!("{}-{id}", animal(i))))
        .collect()
}

/// The worktree plan for `repo` (canonical, inside an allowed root):
/// `<parent>/<name>--fd-<callsign>` on branch `fd/<callsign>`. `inside`
/// says whether a canonical directory is inside the allowed roots.
///
/// # Errors
/// [`Refused`]: `repo` is not the top of a main checkout, the new path is
/// outside the roots, or it exists.
pub fn worktree_plan(
    repo: &Path,
    callsign: &str,
    inside: impl Fn(&Path) -> bool,
) -> Result<(WorkspacePlan, PathBuf), Refused> {
    if !std::fs::symlink_metadata(repo.join(".git")).is_ok_and(|m| m.is_dir()) {
        return Err(invalid(format!(
            "`{}` is not the top directory of a Git repository",
            repo.display()
        )));
    }
    let name = repo
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| invalid("the repository name is not UTF-8"))?;
    let parent = repo
        .parent()
        .filter(|p| inside(p))
        .ok_or_else(|| forbidden("the worktree would be outside the allowed roots"))?;
    let path = parent.join(format!("{name}--fd-{callsign}"));
    if std::fs::symlink_metadata(&path).is_ok() {
        return Err(invalid(format!("`{}` exists already", path.display())));
    }
    let text = |p: &Path| p.to_string_lossy().into_owned();
    Ok((
        WorkspacePlan::Worktree {
            repo: text(repo),
            path: text(&path),
            branch: format!("fd/{callsign}"),
        },
        path,
    ))
}

/// The clone plan of `url` into the new directory `target`.
///
/// # Errors
/// [`Refused`]: a URL with credentials or an unsupported transport
/// (forbidden), a target that exists, has an unusual name, or whose parent
/// is outside the allowed roots.
pub fn clone_plan(
    url: &str,
    target: &str,
    canonical_inside: impl Fn(&Path) -> Option<PathBuf>,
) -> Result<(WorkspacePlan, PathBuf), Refused> {
    check_url(url, &canonical_inside)?;
    let target = Path::new(target);
    let name = match target.components().next_back() {
        Some(Component::Normal(n)) => n.to_str().unwrap_or(""),
        _ => "",
    };
    if name.is_empty() || name.starts_with('.') || name.chars().any(char::is_control) {
        return Err(invalid("give a new directory with a plain name"));
    }
    let parent = target
        .parent()
        .and_then(&canonical_inside)
        .ok_or_else(|| forbidden("the clone directory must be inside the allowed roots"))?;
    let path = parent.join(name);
    if std::fs::symlink_metadata(&path).is_ok() {
        return Err(invalid(format!("`{}` exists already", path.display())));
    }
    Ok((
        WorkspacePlan::Clone {
            url: url.to_owned(),
            path: path.to_string_lossy().into_owned(),
        },
        path,
    ))
}

/// Accepts `https://`, `ssh://` and `user@host:path` URLs without a
/// password or token, and `file://` URLs of a repository inside the roots.
fn check_url(
    url: &str,
    canonical_inside: impl Fn(&Path) -> Option<PathBuf>,
) -> Result<(), Refused> {
    if url.is_empty()
        || url.chars().count() > MAX_URL_CHARS
        || url.starts_with('-')
        || url.chars().any(|c| c.is_control() || c.is_whitespace())
    {
        return Err(invalid(
            "the clone URL is empty, too long or has unusual characters",
        ));
    }
    let authority = |rest: &str| rest.split('/').next().unwrap_or("").to_owned();
    if let Some(rest) = url.strip_prefix("https://") {
        if authority(rest).contains('@') {
            return Err(forbidden(
                "the clone URL carries credentials; the station uses its own Git credentials",
            ));
        }
        return if authority(rest).is_empty() {
            Err(invalid("the clone URL has no host"))
        } else {
            Ok(())
        };
    }
    if let Some(rest) = url.strip_prefix("ssh://") {
        let auth = authority(rest);
        let user = auth.rsplit_once('@').map(|(u, _)| u);
        if user.is_some_and(|u| u.contains(':')) {
            return Err(forbidden("the clone URL carries a password"));
        }
        return Ok(());
    }
    if let Some(path) = url.strip_prefix("file://") {
        return canonical_inside(Path::new(path))
            .map(|_| ())
            .ok_or_else(|| forbidden("a file:// clone source must be inside the allowed roots"));
    }
    // scp-like `user@host:path`: no scheme, a host before the first `:`.
    // `<helper>::<address>` starts a remote helper (`ext::` runs commands).
    if !url.contains("://")
        && !url.contains("::")
        && let Some((user_host, path)) = url.split_once(':')
        && !path.is_empty()
        && !user_host.contains('/')
    {
        let (user, host) = user_host.rsplit_once('@').unwrap_or(("", user_host));
        if host.is_empty() || user.contains(':') {
            return Err(invalid("the clone URL has no host"));
        }
        return Ok(());
    }
    Err(forbidden(
        "use an https://, ssh://, user@host:path or file:// clone URL",
    ))
}

/// Does the Git work of `plan` (the start effect, DL-200). A worktree that
/// exists on the planned branch counts as done (a repeated effect).
///
/// # Errors
/// The `git` failure as text.
pub async fn provision(plan: &WorkspacePlan, program: &Path) -> Result<(), String> {
    let options = |timeout| GitOptions {
        program: program.to_path_buf(),
        timeout,
        // Clone needs the owner's credential helpers and SSH settings.
        isolated: false,
    };
    match plan {
        WorkspacePlan::Worktree { repo, path, branch } => {
            if Path::new(path).is_dir() {
                let git = Git::new(path, options(WORKTREE_TIMEOUT));
                let head = git
                    .text(&["rev-parse", "--abbrev-ref", "HEAD"])
                    .await
                    .map_err(|e| e.to_string())?;
                return if head.trim() == branch {
                    Ok(())
                } else {
                    Err(format!("`{path}` exists on another branch"))
                };
            }
            Git::new(repo, options(WORKTREE_TIMEOUT))
                .run(&["worktree", "add", "-b", branch, "--", path])
                .await
                .map(|_| ())
                .map_err(|e| e.to_string())
        }
        WorkspacePlan::Clone { url, path } => {
            let parent = Path::new(path).parent().unwrap_or(Path::new("/"));
            Git::new(parent, options(CLONE_TIMEOUT))
                .run(&[
                    "-c",
                    "protocol.ext.allow=never",
                    "clone",
                    "--no-recurse-submodules",
                    "--",
                    url,
                    path,
                ])
                .await
                .map(|_| ())
                .map_err(|e| scrub(&e.to_string()))
        }
    }
}

/// `url` without user information, query and fragment (v0
/// `repo-identity.ts`): what the catalog shows.
#[must_use]
pub fn scrub_url(url: &str) -> String {
    let url = url.split(['?', '#']).next().unwrap_or("");
    if let Some((scheme, rest)) = url.split_once("://") {
        let (auth, path) = rest.split_once('/').map_or((rest, ""), |(a, p)| (a, p));
        let host = auth.rsplit_once('@').map_or(auth, |(_, h)| h);
        return if path.is_empty() {
            format!("{scheme}://{host}")
        } else {
            format!("{scheme}://{host}/{path}")
        };
    }
    url.to_owned()
}

/// The group key of an origin: host and path, lower case, without the
/// scheme, the user, `.git` and a final `/` (a heuristic, PLAN §6.6).
#[must_use]
pub fn normalize_origin(url: &str) -> String {
    let url = scrub_url(url);
    let rest = match url.split_once("://") {
        Some((_, rest)) => rest.to_owned(),
        None => {
            let after_user = url.rsplit_once('@').map_or(url.as_str(), |(_, h)| h);
            after_user.replacen(':', "/", 1)
        }
    };
    let rest = rest.trim_end_matches('/');
    rest.strip_suffix(".git").unwrap_or(rest).to_lowercase()
}

/// Text of a `git` error without anything that looks like a credential.
fn scrub(text: &str) -> String {
    text.split(' ')
        .map(|w| {
            if w.contains("://") {
                scrub_url(w)
            } else {
                w.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Repo catalog facts of a main checkout (DL-201), from its files only:
/// the scrubbed origin, the default branch, and the number of remotes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RepoFacts {
    pub origin: Option<String>,
    pub default_branch: Option<String>,
    pub remotes: usize,
}

/// The most bytes read from `.git/config`.
const MAX_CONFIG: u64 = 64 * 1024;

/// Reads [`RepoFacts`] of the repository at `dir` (a `.git` directory).
#[must_use]
pub fn repo_facts(dir: &Path) -> RepoFacts {
    use std::io::Read;
    let git = dir.join(".git");
    let mut facts = RepoFacts::default();
    let mut text = String::new();
    if let Ok(file) = std::fs::File::open(git.join("config")) {
        let _ = file.take(MAX_CONFIG).read_to_string(&mut text);
    }
    let mut section = String::new();
    for line in text.lines().map(str::trim) {
        if line.starts_with('[') {
            section = line.to_owned();
            if line.starts_with("[remote ") {
                facts.remotes += 1;
            }
            continue;
        }
        if section == "[remote \"origin\"]"
            && let Some((key, value)) = line.split_once('=')
            && key.trim() == "url"
        {
            facts.origin = Some(scrub_url(value.trim()));
        }
    }
    let mut head = String::new();
    if let Ok(file) = std::fs::File::open(git.join("refs/remotes/origin/HEAD")) {
        let _ = file.take(4096).read_to_string(&mut head);
    }
    facts.default_branch = head
        .trim()
        .strip_prefix("ref: refs/remotes/origin/")
        .map(str::to_owned)
        .or_else(|| fleetdeck_core::git::current_branch(dir));
    facts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn callsigns_rotate_and_fall_back_to_the_whole_id() {
        let c = callsign_candidates("01HZX-ABCD9", 1);
        assert_eq!(c[0], "otter-bcd9");
        assert_eq!(c[11], "falcon-bcd9");
        assert_eq!(c[12], "otter-01hzxabcd9");
        assert_eq!(c.len(), 24);
    }

    #[test]
    fn origins_are_scrubbed_and_normalized() {
        assert_eq!(
            scrub_url("https://bot:tok@github.com/a/b.git?x=1#f"),
            "https://github.com/a/b.git"
        );
        assert_eq!(
            normalize_origin("git@GitHub.com:Acme/App.git"),
            "github.com/acme/app"
        );
        assert_eq!(
            normalize_origin("https://u@github.com/acme/app/"),
            "github.com/acme/app"
        );
        assert_eq!(
            normalize_origin("ssh://git@github.com/acme/app.git"),
            "github.com/acme/app"
        );
    }

    #[test]
    fn clone_urls_with_credentials_or_unknown_transports_are_forbidden() {
        let none = |_: &Path| None;
        for url in [
            "https://u:p@h/r",
            "https://tok@h/r",
            "ssh://u:p@h/r",
            "ext::sh%20-c%20x",
            "fd::x",
            "file:///etc",
        ] {
            assert!(check_url(url, none).unwrap_err().forbidden, "{url}");
        }
        for url in ["https://h/o/r.git", "ssh://git@h/o/r", "git@h:o/r.git"] {
            assert_eq!(check_url(url, none), Ok(()), "{url}");
        }
        for url in ["", "-u", "https://h/a b", "https:///r"] {
            assert!(check_url(url, none).is_err(), "{url}");
        }
    }
}
