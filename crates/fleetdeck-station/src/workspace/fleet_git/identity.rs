//! The home check and the destination checks of Fleet Git (FG-2, design §3.8,
//! revision 3 B1 and revision 4 B2).
//!
//! [`check_home`] reads every effective URL of a home's remote and answers
//! one [`HomeCheck`]. [`effective_push_url`] is the check that runs right
//! before each remote write (rules 2 and 3): it gives the one verified URL
//! that the write and its recovery read use, never a remote name.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use fleetdeck_proto::fleet_git::{
    GitProjectView, HomeCheck, HomeCheckState, IdentityError, LocalRemotes, RemoteIdentity,
    RewriteRule, code, effective_url, parse_rewrite_rules, remote_identity, same_dest,
};

use super::redact::redact;
use crate::workspace::branches::parse_git_version;
use crate::workspace::{Git, GitOptions, provision};

/// The time limit of each local read (design §2.4).
pub(super) const READ_TIMEOUT: Duration = Duration::from_secs(10);
/// The oldest Git that Fleet Git runs (design §2.4).
const MIN_VERSION: (u32, u32) = (2, 45);
/// The most bytes of the answer of a small read.
const SMALL_CAP: usize = 4 * 1024;
/// The most bytes of the rewrite rules (design §2.4).
const RULES_CAP: usize = 16 * 1024;

/// A refused Fleet Git operation: a wire code and the text for the board.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    /// A `fleetdeck_proto::fleet_git::code` string.
    pub code: &'static str,
    pub text: String,
}

impl Refusal {
    pub(super) fn new(code: &'static str, text: impl Into<String>) -> Self {
        Self {
            code,
            text: text.into(),
        }
    }

    fn internal(detail: &str) -> Self {
        Self::new(
            code::INTERNAL,
            format!("Fleet Git could not read the Git configuration: {detail}"),
        )
    }
}

/// Where a remote write goes (design §3.8, rules 2 and 3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PushDestination {
    /// The remote `name` of the home (`push_remote`). `project` is the
    /// identity of the project and `clone_url` is its URL with the SSH login
    /// name; both appear in the texts only.
    Remote {
        name: String,
        project: RemoteIdentity,
        clone_url: String,
    },
    /// The `wip_remote` URL.
    Wip { url: String },
}

/// The one URL that a write and its recovery read use (design §3.8).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedUrl {
    /// The URL to pass to `git push` and `git ls-remote`.
    pub url: String,
    /// `RemoteIdentity.dest` of the URL.
    pub dest: String,
}

/// A Git that runs a local read in `dir`: the user configuration, no
/// confinement, the time limit `timeout`.
pub(super) fn read_git(options: &GitOptions, dir: &Path, timeout: Duration) -> Git {
    Git::new(
        dir,
        GitOptions {
            timeout,
            isolated: false,
            confine: None,
            ..options.clone()
        },
    )
}

/// A Git that runs a network command in `dir`: the user configuration
/// (credential helpers, the SSH agent), the confinement of `options`.
pub(super) fn net_git(options: &GitOptions, dir: &Path, timeout: Duration) -> Git {
    Git::new(
        dir,
        GitOptions {
            timeout,
            isolated: false,
            ..options.clone()
        },
    )
}

/// Runs `git <args>` and returns `(exit code, stdout as text, stderr)`. An
/// error is the redacted text of a Git that did not run, timed out, or
/// printed more than `cap` bytes.
pub(super) async fn read(
    git: &Git,
    args: &[&str],
    cap: usize,
) -> Result<(i32, String, String), String> {
    let (code, out, err) = git
        .status(args, &[], None)
        .await
        .map_err(|e| redact(&e.to_string()))?;
    if out.len() > cap {
        return Err("Git printed too much text".to_owned());
    }
    Ok((code, String::from_utf8_lossy(&out).into_owned(), err))
}

type Versions = Mutex<HashMap<std::path::PathBuf, (u32, u32)>>;

/// Refuses a Git below 2.45 (`git_too_old`, design §2.4). The version of a
/// program is read once for this process.
pub(super) async fn require_git(options: &GitOptions) -> Result<(), Refusal> {
    static VERSIONS: std::sync::OnceLock<Versions> = std::sync::OnceLock::new();
    let versions = VERSIONS.get_or_init(Versions::default);
    let known = versions
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(&options.program)
        .copied();
    let version = match known {
        Some(version) => version,
        None => {
            let git = read_git(options, &std::env::temp_dir(), READ_TIMEOUT);
            let (exit, out, _) = read(&git, &["--version"], 1024)
                .await
                .map_err(|e| Refusal::internal(&e))?;
            let version = match (exit, parse_git_version(&out)) {
                (0, Some(version)) => version,
                _ => return Err(Refusal::internal("cannot read the Git version")),
            };
            versions
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .insert(options.program.clone(), version);
            version
        }
    };
    if version < MIN_VERSION {
        return Err(Refusal::new(
            code::GIT_TOO_OLD,
            "Fleet Git needs Git 2.45 or newer on this station.",
        ));
    }
    Ok(())
}

/// The `url.<base>.insteadOf` and `pushInsteadOf` rules (design §2.4).
async fn rewrite_rules(git: &Git) -> Result<Vec<RewriteRule>, String> {
    let args = [
        "config",
        "--get-regexp",
        "--",
        r"^url\..*\.(pushinsteadof|insteadof)$",
    ];
    match read(git, &args, RULES_CAP).await? {
        (0, out, _) => Ok(parse_rewrite_rules(&out)),
        (1, _, _) => Ok(Vec::new()),
        (_, _, err) => Err(redact(&err)),
    }
}

/// Does Git rewrite `url` again? (design §3.8 item 3 step 5)
fn rewritten_again(url: &str, rules: &[RewriteRule]) -> bool {
    rules.iter().any(|rule| url.starts_with(&rule.prefix))
}

/// The checks of a local remote that the station does after the syntax
/// check of `remote_identity` (design §3.8 item 1, M13).
async fn check_local(options: &GitOptions, dest: &str) -> Result<(), Refusal> {
    let Some(path) = dest.strip_prefix("file://") else {
        return Ok(());
    };
    if !std::fs::canonicalize(path).is_ok_and(|c| c == Path::new(path)) {
        return Err(Refusal::new(
            code::REMOTE_PATH_INVALID,
            "A local remote path must be canonical, without links.",
        ));
    }
    let git = read_git(options, Path::new(path), READ_TIMEOUT);
    match read(&git, &["rev-parse", "--is-bare-repository"], 64).await {
        Ok((0, out, _)) if out.trim() == "true" => Ok(()),
        _ => Err(Refusal::new(
            code::REMOTE_LOCAL_REFUSED,
            "A local remote must be a bare repository.",
        )),
    }
}

fn refuse(check: &mut HomeCheck, refusal: &Refusal) {
    check.state = HomeCheckState::Refused;
    check.code = Some(refusal.code.to_owned());
}

fn identity_refusal(err: &IdentityError) -> Refusal {
    Refusal::new(err.code(), err.to_string())
}

/// Why a remote of a home cannot be verified.
enum Unverified {
    NoRemote,
    Ambiguous,
    Rewritten,
    /// The identity that the home's remote has instead (`HomeCheck.seen`).
    Mismatch(RemoteIdentity),
    Refused(Refusal),
}

impl From<Refusal> for Unverified {
    fn from(refusal: Refusal) -> Self {
        Self::Refused(refusal)
    }
}

/// The effective URLs of a verified remote.
struct Resolved {
    push_url: String,
    push_dest: String,
}

fn remote_name_ok(name: &str) -> bool {
    (1..=64).contains(&name.len())
        && !name.starts_with(['-', '.'])
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// The lines of `text` that are not empty.
fn lines(text: &str) -> Vec<&str> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect()
}

/// The home check of the remote `name` (design §3.8 item 3, steps 1 to 6).
async fn resolve(
    options: &GitOptions,
    repo_dir: &Path,
    name: &str,
    project: &RemoteIdentity,
    local: LocalRemotes,
) -> Result<Resolved, Unverified> {
    let internal = |detail: String| Unverified::Refused(Refusal::internal(&detail));
    if !remote_name_ok(name) {
        return Err(internal("the remote name is not valid".to_owned()));
    }
    let git = read_git(options, repo_dir, READ_TIMEOUT);
    // 1. The configured values.
    let pattern = format!(r"^remote\.{}\.(url|pushurl)$", name.replace('.', r"\."));
    let configured = match read(&git, &["config", "--get-regexp", "--", &pattern], SMALL_CAP)
        .await
        .map_err(internal)?
    {
        (0, out, _) => out,
        (1, _, _) => String::new(),
        (_, _, err) => return Err(internal(redact(&err))),
    };
    let (mut urls, mut push_urls) = (0, 0);
    for line in configured.lines() {
        let key = line.split(' ').next().unwrap_or("").to_ascii_lowercase();
        if key.ends_with(".pushurl") {
            push_urls += 1;
        } else if key.ends_with(".url") {
            urls += 1;
        }
    }
    if urls == 0 {
        return Err(Unverified::NoRemote);
    }
    if urls > 1 || push_urls > 1 {
        return Err(Unverified::Ambiguous);
    }
    // 2. The effective fetch URL F, 3. the effective push URL P.
    let one_url = |out: (i32, String, String)| match out {
        (0, text, _) => match lines(&text).as_slice() {
            [one] => Ok((*one).to_owned()),
            _ => Err(Unverified::Ambiguous),
        },
        (_, _, err) => Err(internal(redact(&err))),
    };
    let fetch_url = one_url(
        read(&git, &["remote", "get-url", "--all", "--", name], SMALL_CAP)
            .await
            .map_err(internal)?,
    )?;
    let push_url = one_url(
        read(
            &git,
            &["remote", "get-url", "--push", "--all", "--", name],
            SMALL_CAP,
        )
        .await
        .map_err(internal)?,
    )?;
    // 4. A second view of Git's own resolution.
    let (exit, out, err) = read(&git, &["ls-remote", "--get-url", "--", name], SMALL_CAP)
        .await
        .map_err(internal)?;
    if exit != 0 {
        return Err(internal(redact(&err)));
    }
    if out.trim() != fetch_url {
        return Err(Unverified::Ambiguous);
    }
    // 5. The fixed point.
    let rules = rewrite_rules(&git).await.map_err(internal)?;
    if rewritten_again(&push_url, &rules) {
        return Err(Unverified::Rewritten);
    }
    // 6. The identities.
    let fetch_id = remote_identity(&fetch_url, local).map_err(|e| identity_refusal(&e))?;
    let push_id = remote_identity(&push_url, local).map_err(|e| identity_refusal(&e))?;
    check_local(options, &push_id.dest).await?;
    if fetch_id.dest != push_id.dest {
        check_local(options, &fetch_id.dest).await?;
    }
    if !same_dest(&push_id.dest, &project.dest) {
        return Err(Unverified::Mismatch(push_id));
    }
    if !same_dest(&fetch_id.dest, &project.dest) {
        return Err(Unverified::Mismatch(fetch_id));
    }
    Ok(Resolved {
        push_url,
        push_dest: push_id.dest,
    })
}

fn local_remotes(allow: bool) -> LocalRemotes {
    if allow {
        LocalRemotes::Allow
    } else {
        LocalRemotes::Deny
    }
}

/// The station's check of the home `home_dir` of the project `view`
/// (design §3.8 item 3). The remote that it checks is
/// `view.settings.push_remote`. `allow_local_remotes` is the station's
/// `allow_local_remotes` setting.
///
/// The answer: `Missing` (no folder), `NotARepo` (not the top of a
/// repository), `NoRemote`, `RemoteAmbiguous`, `Mismatch` (with `seen`),
/// `Refused` (an identity error, `remote_rewritten`, `git_too_old` or
/// `internal`), or `Verified`.
pub async fn check_home(
    options: &GitOptions,
    home_dir: &Path,
    view: &GitProjectView,
    allow_local_remotes: bool,
) -> HomeCheck {
    let mut check = HomeCheck {
        project_id: view.project_id.clone(),
        generation: view.home_generation,
        state: HomeCheckState::Missing,
        canonical_path: None,
        seen: None,
        code: None,
    };
    let Ok(canonical) = std::fs::canonicalize(home_dir) else {
        return check;
    };
    check.canonical_path = Some(canonical.to_string_lossy().into_owned());
    if !canonical.is_dir() {
        check.state = HomeCheckState::NotARepo;
        return check;
    }
    if let Err(refusal) = require_git(options).await {
        refuse(&mut check, &refusal);
        return check;
    }
    let git = read_git(options, &canonical, READ_TIMEOUT);
    match read(&git, &["rev-parse", "--show-toplevel"], SMALL_CAP).await {
        Ok((0, out, _)) if std::fs::canonicalize(out.trim()).is_ok_and(|top| top == canonical) => {}
        Ok(_) => {
            check.state = HomeCheckState::NotARepo;
            return check;
        }
        Err(detail) => {
            refuse(&mut check, &Refusal::internal(&detail));
            return check;
        }
    }
    let resolved = resolve(
        options,
        &canonical,
        &view.settings.push_remote,
        &view.remote,
        local_remotes(allow_local_remotes),
    )
    .await;
    match resolved {
        Ok(_) => check.state = HomeCheckState::Verified,
        Err(Unverified::NoRemote) => {
            check.state = HomeCheckState::NoRemote;
            check.code = Some(code::NO_REMOTE.to_owned());
        }
        Err(Unverified::Ambiguous) => {
            check.state = HomeCheckState::RemoteAmbiguous;
            check.code = Some(code::REMOTE_AMBIGUOUS.to_owned());
        }
        Err(Unverified::Rewritten) => {
            check.state = HomeCheckState::Refused;
            check.code = Some(code::REMOTE_REWRITTEN.to_owned());
        }
        Err(Unverified::Mismatch(seen)) => {
            check.state = HomeCheckState::Mismatch;
            check.seen = Some(seen);
            check.code = Some(code::REMOTE_MISMATCH.to_owned());
        }
        Err(Unverified::Refused(refusal)) => refuse(&mut check, &refusal),
    }
    check
}

/// The check right before a remote write (design §3.8 rules 2 and 3): the
/// one URL that the write and its recovery read use.
///
/// - [`PushDestination::Remote`]: the whole home check of rule 3 runs again
///   in `repo_dir`; the verified push URL `P` is the answer. A `dest` that
///   differs from the project's or from `target_dest` (a lease never moves
///   to another destination) is `remote_mismatch`.
/// - [`PushDestination::Wip`]: the effective URL `W` of the rewrite rules
///   (rule 2). `remote_identity(W).dest` must equal `target_dest`, and `W`
///   must be a fixed point, else `remote_rewritten`. The answer is `W`.
///
/// `target_dest` is `git_pushes.target_dest`; for a new row it is the `dest`
/// of the target.
///
/// # Errors
/// A [`Refusal`] with the code of §5.7: `git_too_old`, `no_remote`,
/// `remote_ambiguous`, `remote_rewritten`, `remote_mismatch`, an identity
/// code (`remote_unsupported`, `remote_local_refused`, ...) or `internal`.
pub async fn effective_push_url(
    options: &GitOptions,
    repo_dir: &Path,
    destination: &PushDestination,
    target_dest: &str,
    allow_local_remotes: bool,
) -> Result<VerifiedUrl, Refusal> {
    require_git(options).await?;
    let local = local_remotes(allow_local_remotes);
    match destination {
        PushDestination::Remote {
            name,
            project,
            clone_url,
        } => {
            let home = repo_dir.display();
            let resolved = resolve(options, repo_dir, name, project, local).await;
            let resolved = resolved.map_err(|e| match e {
                Unverified::NoRemote => Refusal::new(
                    code::NO_REMOTE,
                    format!(
                        "The home {home} has no remote {name}. Add it: git remote add {name} {}.",
                        provision::scrub_url(clone_url)
                    ),
                ),
                Unverified::Ambiguous => Refusal::new(
                    code::REMOTE_AMBIGUOUS,
                    format!(
                        "The remote {name} of {home} has more than one URL. Fleet Deck pushes only to one verified repository; remove the extra url or pushurl."
                    ),
                ),
                Unverified::Rewritten => Refusal::new(
                    code::REMOTE_REWRITTEN,
                    format!(
                        "The Git configuration on this station rewrites {} to another destination.",
                        provision::scrub_url(clone_url)
                    ),
                ),
                Unverified::Mismatch(_) => mismatch(name, &home.to_string(), &project.repo),
                Unverified::Refused(refusal) => refusal,
            })?;
            if !same_dest(&resolved.push_dest, target_dest) {
                return Err(mismatch(name, &home.to_string(), &project.repo));
            }
            Ok(VerifiedUrl {
                url: resolved.push_url,
                dest: resolved.push_dest,
            })
        }
        PushDestination::Wip { url } => {
            remote_identity(url, local).map_err(|e| identity_refusal(&e))?;
            let git = read_git(options, repo_dir, READ_TIMEOUT);
            let rules = rewrite_rules(&git)
                .await
                .map_err(|detail| Refusal::internal(&detail))?;
            let effective = effective_url(url, &rules, true);
            let identity = remote_identity(&effective, local).map_err(|e| identity_refusal(&e))?;
            if !same_dest(&identity.dest, target_dest) || rewritten_again(&effective, &rules) {
                return Err(Refusal::new(
                    code::REMOTE_REWRITTEN,
                    format!(
                        "The Git configuration on this station rewrites {} to another destination.",
                        provision::scrub_url(url)
                    ),
                ));
            }
            check_local(options, &identity.dest).await?;
            Ok(VerifiedUrl {
                url: effective,
                dest: identity.dest,
            })
        }
    }
}

fn mismatch(name: &str, home: &str, repo: &str) -> Refusal {
    Refusal::new(
        code::REMOTE_MISMATCH,
        format!("The remote {name} of {home} no longer points to {repo}."),
    )
}

#[cfg(test)]
#[path = "identity_tests.rs"]
mod tests;
