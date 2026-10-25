//! Workspace provisioning (PLAN §6.6, M3 6-1, DL-200, DL-201): callsigns,
//! the worktree and clone plans that the decision checks, the Git work that
//! the start effect does, and the repo catalog facts of `workspace.list`.
//!
//! The decision step only reads the file system (no `git` process); the
//! effect worker runs `git` before the provider starts.

use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use rustix::process::{Pid, Signal, kill_process_group, test_kill_process_group};
use tokio::sync::watch;

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
/// How long a stopped provision waits for its Git process group to end after
/// each signal (TERM, then KILL).
const GROUP_WAIT: Duration = Duration::from_secs(5);

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

/// The first ticket key in `branch`: `[A-Z][A-Z0-9]{1,9}-[0-9]{1,6}`
/// (`feature/PROJ-123-login` gives `PROJ-123`). Lower case gives none.
#[must_use]
pub fn ticket_key(branch: &str) -> Option<String> {
    let b = branch.as_bytes();
    for start in 0..b.len() {
        if !b[start].is_ascii_uppercase() {
            continue;
        }
        // The key letters: 1 to 9 after the first one, then a dash.
        let mut i = start + 1;
        while i < b.len() && i - start <= 10 && (b[i].is_ascii_uppercase() || b[i].is_ascii_digit())
        {
            i += 1;
        }
        // `i` is one past the run; the dash must follow a run of 2 to 10.
        if i - start < 2 || i - start > 10 || b.get(i) != Some(&b'-') {
            continue;
        }
        let digits = b[i + 1..]
            .iter()
            .take(6)
            .take_while(|c| c.is_ascii_digit())
            .count();
        if digits >= 1 {
            return Some(branch[start..i + 1 + digits].to_owned());
        }
    }
    None
}

/// The callsign candidates of a new session, best first: with a ticket key
/// the rotation `<animal>-<KEY>` (DL-144), then the rotation from `start`
/// with the last four id characters, then the rotation with the whole id
/// (unique, because the id is).
#[must_use]
pub fn callsign_candidates(session_id: &str, start: u64, ticket: Option<&str>) -> Vec<String> {
    let id: String = session_id
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|c| c.to_ascii_lowercase())
        .collect();
    let short: String = id.chars().skip(id.len().saturating_sub(4)).collect();
    let n = CALLSIGNS.len();
    let offset = usize::try_from(start % n as u64).unwrap_or(0);
    let animal = |i: usize| CALLSIGNS[(offset + i) % n];
    let keyed = ticket
        .into_iter()
        .flat_map(|key| (0..n).map(move |i| format!("{}-{key}", animal(i))));
    keyed
        .chain((0..n).map(|i| format!("{}-{short}", animal(i))))
        .chain((0..n).map(|i| format!("{}-{id}", animal(i))))
        .collect()
}

/// The longest branch or base name that the station takes.
pub const MAX_REF_CHARS: usize = 200;

/// The ref that `prs.fetch_head` makes (R3.7) is `refs/fleetdeck/pr/<n>`: a
/// base under that prefix must end in 1 to 20 digits. Other bases are not
/// restricted here.
fn pr_base_ok(base: &str) -> bool {
    base.strip_prefix("refs/fleetdeck/pr/")
        .is_none_or(|n| (1..=20).contains(&n.len()) && n.bytes().all(|b| b.is_ascii_digit()))
}

/// Checks the branch choice of a worktree start by syntax only; Git
/// validates the names again in the effect.
///
/// # Errors
/// [`Refused`]: a bad branch or base name, or a base without a branch.
pub fn check_branch_choice(branch: Option<&str>, base: Option<&str>) -> Result<(), Refused> {
    let plain = |s: &str| {
        let n = s.chars().count();
        (1..=MAX_REF_CHARS).contains(&n)
            && !s.starts_with('-')
            && !s.chars().any(|c| c.is_whitespace() || c.is_ascii_control())
    };
    match (branch, base) {
        (None, Some(_)) => Err(invalid("a base needs a branch")),
        (Some(b), _)
            if !plain(b)
                || b.contains("..")
                || b.contains(['~', '^', ':', '?', '*', '[', '\\'])
                || b.ends_with('/')
                || b.ends_with(".lock") =>
        {
            Err(invalid(format!("the branch name `{b}` is not valid")))
        }
        (_, Some(base)) if !plain(base) || !pr_base_ok(base) => {
            Err(invalid(format!("the base `{base}` is not valid")))
        }
        _ => Ok(()),
    }
}

/// The worktree plan for `repo` (canonical, inside an allowed root):
/// `<parent>/<name>--fd-<callsign>` on the chosen `branch` (checked by
/// [`check_branch_choice`]), else on the new branch `fd/<callsign>`. `inside`
/// says whether a canonical directory is inside the allowed roots.
///
/// # Errors
/// [`Refused`]: `repo` is not the top of a checkout, the new path is
/// outside the roots, or it exists.
pub fn worktree_plan(
    repo: &Path,
    callsign: &str,
    branch: Option<&str>,
    base: Option<&str>,
    inside: impl Fn(&Path) -> bool,
) -> Result<(WorkspacePlan, PathBuf), Refused> {
    // A main checkout or a linked worktree whose Git directory is inside
    // the allowed roots.
    if !super::checkout::is_checkout_top(repo, &inside) {
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
            branch: branch.map_or_else(|| format!("fd/{callsign}"), str::to_owned),
            base: base.map(str::to_owned),
            sync: None,
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
pub(crate) fn check_url(
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
/// exists on the planned branch counts as done (a repeated effect). Each `git`
/// call runs in its own process group, and `group` holds the group id of the
/// call that runs.
///
/// The pinned paths (B9, DL-200) are the checked folder objects of the Git
/// working directories: `repo_dir` for the worktree Git calls in the source
/// repository, `parent_dir` for the clone, `dest_dir` for the branch check of
/// an existing destination. Each one that is `None` falls back to the path.
/// Path arguments to Git (the destination, the URL) stay: writes are confined
/// by Landlock.
///
/// # Errors
/// The `git` failure as text.
pub async fn provision(
    plan: &WorkspacePlan,
    options: &GitOptions,
    group: &Arc<AtomicU32>,
    repo_dir: Option<&Path>,
    parent_dir: Option<&Path>,
    dest_dir: Option<&Path>,
    clone_timeout: Duration,
) -> Result<(), String> {
    let grouped = |dir: &Path, timeout| {
        Git::new(dir, git_options(options, timeout)).with_group(Arc::clone(group))
    };
    match plan {
        WorkspacePlan::Worktree {
            repo,
            path,
            branch,
            base,
            ..
        } => {
            if Path::new(path).is_dir() {
                let git = grouped(dest_dir.unwrap_or(Path::new(path)), WORKTREE_TIMEOUT);
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
            let git = grouped(repo_dir.unwrap_or(Path::new(repo)), WORKTREE_TIMEOUT);
            let heads_ref = format!("refs/heads/{branch}");
            let exists = git
                .status(&["show-ref", "--verify", "--quiet", &heads_ref], &[], None)
                .await
                .map_err(|e| e.to_string())?
                .0
                == 0;
            if exists {
                if base.is_some() {
                    return Err(format!(
                        "the branch `{branch}` exists; a base applies only to a new branch"
                    ));
                }
                return git
                    .run(&["worktree", "add", "--", path, branch])
                    .await
                    .map(|_| ())
                    .map_err(|e| e.to_string());
            }
            let start = match base {
                Some(base) => {
                    let commit = format!("{base}^{{commit}}");
                    let verified = git
                        .status(
                            &[
                                "rev-parse",
                                "--verify",
                                "--quiet",
                                "--end-of-options",
                                &commit,
                            ],
                            &[],
                            None,
                        )
                        .await
                        .map_err(|e| e.to_string())?
                        .0;
                    if verified != 0 {
                        return Err(format!("the base `{base}` is not a commit"));
                    }
                    base.as_str()
                }
                None => "HEAD",
            };
            git.run(&["worktree", "add", "-b", branch, "--", path, start])
                .await
                .map(|_| ())
                .map_err(|e| e.to_string())
        }
        WorkspacePlan::Clone { url, path } => {
            let parent = Path::new(path).parent().unwrap_or(Path::new("/"));
            grouped(parent_dir.unwrap_or(parent), clone_timeout)
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

/// The options of a `git` call of the Git work of a plan: `options` (with
/// its confinement, B9) with the timeout of the call.
fn git_options(options: &GitOptions, timeout: Duration) -> GitOptions {
    GitOptions {
        timeout,
        // Clone needs the owner's credential helpers and SSH settings.
        isolated: false,
        ..options.clone()
    }
}

/// Why [`provision_cancellable`] did not create the workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProvisionError {
    /// The `git` failure as text.
    Failed(String),
    /// The cancel signal came first; `git` was stopped.
    Stopped,
}

/// [`provision`] that a cancel signal (`true` on `cancel`) stops. Dropping the
/// `provision` future kills the `git` child (`kill_on_drop`); the rest of its
/// process group (transport, checkout and hook processes) is then stopped
/// before the caller undoes the work.
///
/// # Errors
/// [`ProvisionError::Failed`] with the `git` failure, or with the text that
/// `git` processes are still running after a cancel (the caller must keep the
/// workspace); [`ProvisionError::Stopped`] when the cancel came first and the
/// `git` processes are gone.
pub async fn provision_cancellable(
    plan: &WorkspacePlan,
    options: &GitOptions,
    mut cancel: watch::Receiver<bool>,
    repo_dir: Option<&Path>,
    parent_dir: Option<&Path>,
    dest_dir: Option<&Path>,
    clone_timeout: Duration,
) -> Result<(), ProvisionError> {
    if *cancel.borrow() {
        return Err(ProvisionError::Stopped);
    }
    let group = Arc::new(AtomicU32::new(0));
    let done = tokio::select! {
        r = provision(plan, options, &group, repo_dir, parent_dir, dest_dir, clone_timeout) => Some(r),
        _ = cancel.wait_for(|c| *c) => None,
    };
    match done {
        Some(r) => r.map_err(ProvisionError::Failed),
        None => Err(after_cancel(group.load(Ordering::SeqCst), GROUP_WAIT).await),
    }
}

/// The error of a cancelled provision whose `git` call ran in the process
/// group `pgid` (0: none): [`ProvisionError::Stopped`] when the group is gone,
/// else a failure, so that the caller keeps the workspace.
pub(crate) async fn after_cancel(pgid: u32, wait: Duration) -> ProvisionError {
    if stop_group(pgid, wait).await {
        return ProvisionError::Stopped;
    }
    tracing::warn!(
        pgid,
        "git processes are still running after a stop; the workspace is kept"
    );
    ProvisionError::Failed(
        "the start was stopped, but git processes are still running; the workspace was kept"
            .to_owned(),
    )
}

/// Stops the process group `pgid` (0: none): TERM, wait up to `wait` for the
/// group to be empty, KILL, wait up to `wait` again. Returns whether the group
/// is gone.
pub(crate) async fn stop_group(pgid: u32, wait: Duration) -> bool {
    let Some(pgid) = i32::try_from(pgid).ok().and_then(Pid::from_raw) else {
        return true;
    };
    for sig in [Signal::TERM, Signal::KILL] {
        if let Err(e) = kill_process_group(pgid, sig)
            && e != rustix::io::Errno::SRCH
        {
            tracing::warn!(?sig, error = %e, "cannot signal the git process group");
        }
        let deadline = tokio::time::Instant::now() + wait;
        loop {
            if matches!(test_kill_process_group(pgid), Err(rustix::io::Errno::SRCH)) {
                return true;
            }
            if tokio::time::Instant::now() >= deadline {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
    false
}

/// What existed before the Git work of a plan, so that [`undo_partial`]
/// names only what the plan created.
#[derive(Debug, Clone, Copy)]
pub struct Before {
    path_existed: bool,
}

/// Reads [`Before`] for `plan`.
#[must_use]
pub fn before(plan: &WorkspacePlan) -> Before {
    let (WorkspacePlan::Worktree { path, .. } | WorkspacePlan::Clone { path, .. }) = plan;
    Before {
        path_existed: Path::new(path).exists(),
    }
}

/// The text for a stopped or failed [`provision_cancellable`] that left a
/// new folder (B9, DL-200). Nothing is deleted. `None` when the folder
/// existed before or does not exist now.
#[must_use]
pub fn undo_partial(plan: &WorkspacePlan, before: &Before) -> Option<String> {
    let (WorkspacePlan::Worktree { path, .. } | WorkspacePlan::Clone { path, .. }) = plan;
    (!before.path_existed && std::fs::symlink_metadata(path).is_ok())
        .then(|| format!("The new folder stays at {path}: remove it after a check."))
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
pub(crate) fn scrub(text: &str) -> String {
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
        let c = callsign_candidates("01HZX-ABCD9", 1, None);
        assert_eq!(c[0], "otter-bcd9");
        assert_eq!(c[11], "falcon-bcd9");
        assert_eq!(c[12], "otter-01hzxabcd9");
        assert_eq!(c.len(), 24);
    }

    #[test]
    fn callsigns_with_a_ticket_key_come_first() {
        let c = callsign_candidates("01HZX-ABCD9", 1, Some("PROJ-123"));
        assert_eq!(c[0], "otter-PROJ-123");
        assert_eq!(c[11], "falcon-PROJ-123");
        assert_eq!(c[12], "otter-bcd9");
        assert_eq!(c.len(), 36);
        assert_eq!(c[12..], callsign_candidates("01HZX-ABCD9", 1, None)[..]);
    }

    #[test]
    fn a_ticket_key_is_the_first_upper_case_key_of_the_branch() {
        let key = |b: &str| ticket_key(b);
        assert_eq!(key("feature/PROJ-123-login").as_deref(), Some("PROJ-123"));
        assert_eq!(key("PROJ-7").as_deref(), Some("PROJ-7"));
        assert_eq!(key("fix/AB1-5/CD-9").as_deref(), Some("AB1-5"));
        assert_eq!(key("a/PROJ-1234567").as_deref(), Some("PROJ-123456"));
        assert_eq!(key("fd/raven"), None);
        assert_eq!(key("feature/proj-123"), None);
        assert_eq!(key("feature/PROJ-"), None);
        assert_eq!(key("feature/P-1"), None);
        assert_eq!(key("main"), None);
        // A letter run of 11 has no key of its own; the tail does.
        assert_eq!(key("ABCDEFGHIJK-1").as_deref(), Some("BCDEFGHIJK-1"));
    }

    #[test]
    fn branch_choices_are_checked_by_syntax() {
        assert_eq!(check_branch_choice(None, None), Ok(()));
        assert_eq!(check_branch_choice(Some("feature/x-1"), None), Ok(()));
        assert_eq!(
            check_branch_choice(Some("feature/x"), Some("HEAD~1")),
            Ok(())
        );
        let long = "a".repeat(201);
        for bad in [
            "", "-x", "a b", "a..b", "a~1", "a^", "a:b", "a?", "a*", "a[", "a\\b", "a/", "x.lock",
            "a\tb", "a\u{7f}b", "a\0b", &long,
        ] {
            let err = check_branch_choice(Some(bad), None).unwrap_err();
            assert!(!err.forbidden, "{bad:?}");
            assert_eq!(err.message, format!("the branch name `{bad}` is not valid"));
        }
        assert_eq!(
            check_branch_choice(None, Some("main")).unwrap_err().message,
            "a base needs a branch"
        );
        assert_eq!(
            check_branch_choice(Some("review/pr-7"), Some("refs/fleetdeck/pr/123")),
            Ok(())
        );
        let digits = format!("refs/fleetdeck/pr/{}", "9".repeat(21));
        for bad in [
            "",
            "-x",
            "a b",
            "a\nb",
            "a\u{7f}",
            &long,
            "refs/fleetdeck/pr/x",
            "refs/fleetdeck/pr/",
            "refs/fleetdeck/pr/12a",
            "refs/fleetdeck/pr/1/2",
            &digits,
        ] {
            let err = check_branch_choice(Some("x"), Some(bad)).unwrap_err();
            assert_eq!(err.message, format!("the base `{bad}` is not valid"));
        }
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
