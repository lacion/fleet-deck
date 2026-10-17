//! Fetch before a start (FG-1, design §4, O7 and O8).

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicU32;
use std::time::Duration;

use fleetdeck_proto::fleet_git::{GitSettings, code};

use super::identity::{READ_TIMEOUT, Refusal, net_git, read, read_git, require_git};
use super::redact::redact;
use crate::workspace::GitError;
use crate::workspace::GitOptions;

/// The time limit of the fetch (O7).
const FETCH_TIMEOUT: Duration = Duration::from_secs(30);
/// The most bytes of stderr that the fetch keeps (design §2.4).
const FETCH_STDERR: usize = 2 * 1024;
/// The most bytes of the answer of a small read.
const SMALL_CAP: usize = 4 * 1024;

/// Why the start did not use the fetched remote (O8): the session starts from
/// the local `HEAD` and gets the event `workspace.base_stale`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BaseStale {
    /// `fetch_failed`, `fetch_timeout` or `no_default_branch`.
    pub code: &'static str,
    /// The text of the event (design §4.3), with the redacted Git line.
    pub reason: String,
}

/// Where the new branch starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartPoint {
    /// The revision for `git worktree add`: the owner's base, or
    /// `<remote>/<branch>`. `None`: the local `HEAD`.
    pub rev: Option<String>,
    /// Set when the fetch failed or the remote has no default branch.
    pub stale: Option<BaseStale>,
}

fn stale(code: &'static str, reason: impl Into<String>) -> BaseStale {
    BaseStale {
        code,
        reason: reason.into(),
    }
}

/// The fetch, the start point and the namespace check of a worktree start
/// (design §4.2 step 2). `repo_dir` is the pinned source repository, `remote`
/// is `push_remote`, `slug` and `callsign` name the new branch
/// `fd/<slug>/<callsign>`. A base that the owner gave (`owner_base`) wins over
/// the default branch. `group` holds the process group of the Git call that
/// runs, so a cancel can stop it (design §4.2 step 4). `options` carries the
/// confinement of the fetch (DL-200).
///
/// A fetch that fails, times out, or finds no default branch does not fail
/// the start: the answer has a [`BaseStale`] and the start goes on from the
/// local `HEAD` (or the owner's base).
///
/// # Errors
/// A [`Refusal`] that fails the start (`start_failed`): `git_too_old`, or
/// `branch_namespace_blocked` when the local branch `fd/<slug>` or the
/// remote-tracking branch `<remote>/fd/<slug>` exists.
#[allow(
    clippy::too_many_arguments,
    reason = "the start effect has these inputs and no struct for them"
)]
pub async fn fetch_for_start(
    options: &GitOptions,
    group: &Arc<AtomicU32>,
    repo_dir: &Path,
    remote: &str,
    settings: &GitSettings,
    slug: &str,
    callsign: &str,
    owner_base: Option<&str>,
) -> Result<StartPoint, Refusal> {
    start_point(
        options,
        group,
        repo_dir,
        remote,
        settings,
        (slug, callsign),
        owner_base,
        FETCH_TIMEOUT,
    )
    .await
}

#[allow(
    clippy::too_many_arguments,
    reason = "one seam for the fetch time limit in the tests"
)]
async fn start_point(
    options: &GitOptions,
    group: &Arc<AtomicU32>,
    repo_dir: &Path,
    remote: &str,
    settings: &GitSettings,
    (slug, callsign): (&str, &str),
    owner_base: Option<&str>,
    timeout: Duration,
) -> Result<StartPoint, Refusal> {
    require_git(options).await?;
    let net = net_git(options, repo_dir, timeout).with_group(Arc::clone(group));
    let fetch = net
        .status_stderr(
            &[
                "-c",
                "protocol.ext.allow=never",
                "fetch",
                "--prune",
                "--no-tags",
                "--no-recurse-submodules",
                "--no-write-fetch-head",
                "--",
                remote,
            ],
            &[],
            FETCH_STDERR,
        )
        .await;
    let failure = match fetch {
        Ok((0, _)) => None,
        Ok((_, stderr)) => Some(stale(
            code::FETCH_FAILED,
            format!(
                "Fetch failed; this session started from the local HEAD: {}",
                redact(&stderr)
            ),
        )),
        Err(GitError::Timeout { .. }) => Some(stale(
            code::FETCH_TIMEOUT,
            "Fetch took more than 30 s; this session started from the local HEAD.",
        )),
        Err(e) => Some(stale(
            code::FETCH_FAILED,
            format!(
                "Fetch failed; this session started from the local HEAD: {}",
                redact(&e.to_string())
            ),
        )),
    };
    let local = read_git(options, repo_dir, READ_TIMEOUT).with_group(Arc::clone(group));
    let point = match failure {
        Some(stale) => StartPoint {
            rev: owner_base.map(str::to_owned),
            stale: Some(stale),
        },
        None => choose(&local, remote, settings, owner_base).await,
    };
    namespace_check(&local, remote, slug, callsign).await?;
    Ok(point)
}

/// The start point after a good fetch (design §4.2 step 2): the owner's base,
/// else `default_branch` of the settings, else the target of
/// `refs/remotes/<remote>/HEAD`, else `main`, else `master`. The first name
/// whose `<remote>/<name>` is a commit wins.
async fn choose(
    git: &crate::workspace::Git,
    remote: &str,
    settings: &GitSettings,
    owner_base: Option<&str>,
) -> StartPoint {
    if let Some(base) = owner_base {
        return StartPoint {
            rev: Some(base.to_owned()),
            stale: None,
        };
    }
    let mut names: Vec<String> = Vec::new();
    names.extend(settings.default_branch.clone());
    names.extend(remote_head(git, remote).await);
    names.push("main".to_owned());
    names.push("master".to_owned());
    for name in names {
        let rev = format!("{remote}/{name}");
        let probe = format!("{rev}^{{commit}}");
        let args = [
            "rev-parse",
            "--verify",
            "--quiet",
            "--end-of-options",
            &probe,
        ];
        if matches!(read(git, &args, SMALL_CAP).await, Ok((0, _, _))) {
            return StartPoint {
                rev: Some(rev),
                stale: None,
            };
        }
    }
    StartPoint {
        rev: None,
        stale: Some(stale(
            code::NO_DEFAULT_BRANCH,
            "The remote has no default branch; this session started from the local HEAD.",
        )),
    }
}

/// The branch that `refs/remotes/<remote>/HEAD` points to.
async fn remote_head(git: &crate::workspace::Git, remote: &str) -> Option<String> {
    let head = format!("refs/remotes/{remote}/HEAD");
    let (exit, out, _) = read(git, &["symbolic-ref", "--quiet", &head], SMALL_CAP)
        .await
        .ok()?;
    let target = out.trim();
    let prefix = format!("refs/remotes/{remote}/");
    (exit == 0)
        .then(|| target.strip_prefix(&prefix))
        .flatten()
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
}

/// The D/F check of design §3.2 item 1: a ref named exactly `fd/<slug>` (local
/// or remote-tracking) blocks the branch `fd/<slug>/<callsign>`. A ref below
/// `fd/<slug>/` does not.
async fn namespace_check(
    git: &crate::workspace::Git,
    remote: &str,
    slug: &str,
    callsign: &str,
) -> Result<(), Refusal> {
    let local = format!("refs/heads/fd/{slug}");
    let tracking = format!("refs/remotes/{remote}/fd/{slug}");
    let args = [
        "for-each-ref",
        "--format=%(refname)",
        "--",
        &local,
        &tracking,
    ];
    let (exit, out, err) = read(git, &args, SMALL_CAP).await.map_err(|detail| {
        Refusal::new(
            code::INTERNAL,
            format!("Fleet Git could not read the branches: {detail}"),
        )
    })?;
    if exit != 0 {
        return Err(Refusal::new(
            code::INTERNAL,
            format!("Fleet Git could not read the branches: {}", redact(&err)),
        ));
    }
    // `for-each-ref` also lists the refs below a pattern; only an exact name
    // blocks.
    if out.lines().any(|line| line == local || line == tracking) {
        return Err(Refusal::new(
            code::BRANCH_NAMESPACE_BLOCKED,
            format!(
                "The branch fd/{slug} blocks the session branch fd/{slug}/{callsign}. Rename it, for example: git branch -m fd/{slug} fd-{slug}."
            ),
        ));
    }
    Ok(())
}

#[cfg(test)]
#[path = "fetch_tests.rs"]
mod tests;
