//! The remote work of a land publication (FG-8, design §11.5 steps 5 to 7):
//! the push of the rebased branch and of the default branch, and the reads
//! that confirm an uncertain write. Every command runs in the hardened
//! runner (§2.4).
//!
//! A push is built first ([`LandPush::branch`], [`LandPush::default`]) and
//! started with [`LandPush::spawn`], which has no `.await`: the caller puts
//! the authority check right before it (design §11.5, revision 4, B3).
//! [`RunningPush::finish`] reads the output and classifies the outcome.

use std::path::{Path, PathBuf};
use std::time::Duration;

use fleetdeck_proto::fleet_git::code;

use super::fetch::fetch_remote;
use super::identity::{Refusal, net_git, read, read_git};
use super::push::{PushFailure, RefspecError, refspec_for};
use super::push_outcome::{PushOutcome, push_outcome};
use super::redact::redact;
use crate::workspace::git::BoundedChild;
use crate::workspace::{Git, GitError, GitOptions};

/// The most bytes of the porcelain output that a push keeps (design §2.4).
const PUSH_STDOUT_CAP: usize = 16 * 1024;
/// The most bytes of stderr that a push keeps (design §2.4).
const PUSH_STDERR_CAP: usize = 2 * 1024;
/// The most bytes of the answer of a small read.
const SMALL_CAP: usize = 4 * 1024;

/// Does the Git text say that the station has no credentials? (the DL-144
/// hints of `preflight::hint`)
fn is_auth_failure(stderr: &str) -> bool {
    [
        "Permission denied (publickey)",
        "could not read Username",
        "Authentication failed",
        "terminal prompts disabled",
    ]
    .iter()
    .any(|needle| stderr.contains(needle))
}

fn oid_ok(oid: &str) -> bool {
    matches!(oid.len(), 40 | 64)
        && oid
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Is `name` a plain branch name (the default branch of a remote)?
fn plain_branch(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with(['-', '/'])
        && !name.contains("..")
        && !name.contains("@{")
        && !name.ends_with(['.', '/'])
        && !name.contains(|c: char| {
            c.is_control()
                || c.is_whitespace()
                || matches!(c, '~' | '^' | ':' | '?' | '*' | '[' | '\\')
        })
        && name
            .split('/')
            .all(|part| !part.is_empty() && !part.starts_with('.') && !part.ends_with(".lock"))
}

/// A push of a land, built and not started.
#[derive(Debug)]
pub struct LandPush {
    git: Git,
    args: Vec<String>,
    /// The ref name that the status line names (without `refs/heads/`).
    dst: String,
    timeout: Duration,
}

/// A started push.
#[derive(Debug)]
pub struct RunningPush {
    child: BoundedChild,
    dst: String,
    timeout: Duration,
}

/// What a push did (design §5.5, M2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushEnd {
    pub outcome: PushOutcome,
    /// The code and the text of design §5.7, for an outcome that is not
    /// `Applied`.
    pub failure: PushFailure,
}

impl LandPush {
    fn build(
        options: &GitOptions,
        repo: &Path,
        timeout: Duration,
        url: &str,
        refspec: &str,
        dst: &str,
        lease: Option<&str>,
    ) -> Self {
        let mut args: Vec<String> = [
            "-c",
            "protocol.ext.allow=never",
            "push",
            "--porcelain",
            "--no-verify",
            "--no-follow-tags",
            "--recurse-submodules=no",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect();
        if let Some(lease) = lease {
            args.push(format!("--force-with-lease=refs/heads/{dst}:{lease}"));
        }
        args.extend(["--".to_owned(), url.to_owned(), refspec.to_owned()]);
        Self {
            git: net_git(options, repo, timeout),
            args,
            dst: dst.to_owned(),
            timeout,
        }
    }

    /// The push of the rebased branch: `--force-with-lease=refs/heads/<ref>:<lease>`
    /// (`lease` empty: the ref must not exist), `<oid>:refs/heads/<ref>`.
    ///
    /// # Errors
    /// [`RefspecError`]: the ref is outside `fd/<slug>/`, or an id is not valid.
    #[allow(clippy::too_many_arguments)]
    pub fn branch(
        options: &GitOptions,
        repo: &Path,
        timeout: Duration,
        url: &str,
        slug: &str,
        publish_ref: &str,
        oid: &str,
        lease: &str,
    ) -> Result<Self, RefspecError> {
        let refspec = refspec_for(slug, publish_ref, oid)?;
        if !lease.is_empty() && !oid_ok(lease) {
            return Err(RefspecError::InvalidOid);
        }
        Ok(Self::build(
            options,
            repo,
            timeout,
            url,
            &refspec,
            publish_ref,
            Some(lease),
        ))
    }

    /// The push to the default branch: no `+`, and an explicit
    /// `--force-with-lease=refs/heads/<default>:<base>` with the captured base
    /// (design §11.5 step 7, H16). A remote that is not at `base` refuses it.
    ///
    /// # Errors
    /// [`RefspecError`]: the branch name, the id or the base is not valid.
    pub fn default(
        options: &GitOptions,
        repo: &Path,
        timeout: Duration,
        url: &str,
        default_ref: &str,
        oid: &str,
        base: &str,
    ) -> Result<Self, RefspecError> {
        if !plain_branch(default_ref) {
            return Err(RefspecError::InvalidRef);
        }
        if !oid_ok(oid) || !oid_ok(base) {
            return Err(RefspecError::InvalidOid);
        }
        let refspec = format!("{oid}:refs/heads/{default_ref}");
        Ok(Self::build(
            options,
            repo,
            timeout,
            url,
            &refspec,
            default_ref,
            Some(base),
        ))
    }

    /// Starts `git push`. There is no `.await` here: the process exists when
    /// this returns.
    ///
    /// # Errors
    /// [`GitError::Spawn`] when Git does not start.
    pub fn spawn(&self) -> Result<RunningPush, GitError> {
        let args: Vec<&str> = self.args.iter().map(String::as_str).collect();
        Ok(RunningPush {
            child: self.git.spawn_bounded(&args, &[])?,
            dst: self.dst.clone(),
            timeout: self.timeout,
        })
    }
}

impl RunningPush {
    /// Reads the output and classifies the outcome with [`push_outcome`].
    pub async fn finish(self) -> PushEnd {
        match self.child.collect(PUSH_STDOUT_CAP, PUSH_STDERR_CAP).await {
            Ok((exit, stdout, stderr, _)) => {
                let porcelain = String::from_utf8_lossy(&stdout);
                let outcome = push_outcome(exit, &porcelain, false, &self.dst);
                PushEnd {
                    outcome,
                    failure: failure(outcome, &stderr, &porcelain),
                }
            }
            Err(GitError::Timeout { .. }) => PushEnd {
                outcome: PushOutcome::Unknown,
                failure: PushFailure {
                    code: code::PUSH_TIMEOUT,
                    text: format!("Push took more than {} s.", self.timeout.as_secs()),
                },
            },
            Err(e) => PushEnd {
                outcome: PushOutcome::Unknown,
                failure: failure(PushOutcome::Unknown, &e.to_string(), ""),
            },
        }
    }
}

/// The code and the text of design §5.7 for a push that did not apply.
fn failure(outcome: PushOutcome, stderr: &str, porcelain: &str) -> PushFailure {
    let mut line = redact(stderr);
    if line.is_empty() {
        line = redact(porcelain);
    }
    let (code, text) = match outcome {
        PushOutcome::Rejected(code) => (code, format!("Push was rejected: {line}")),
        _ if is_auth_failure(stderr) => (
            code::PUSH_AUTH,
            format!("Push needs credentials on this station: {line}"),
        ),
        _ => (code::PUSH_FAILED, format!("Push failed: {line}")),
    };
    PushFailure { code, text }
}

/// The answer of the read of a remote ref.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoteTip {
    /// The remote answered: the tip, or `None` when the ref does not exist.
    Answered(Option<String>),
    /// The read gave no answer (an error, a timeout).
    NoAnswer,
}

/// `git ls-remote --refs -- <url> refs/heads/<ref_name>` (20 s, 4 KiB): the
/// recovery and reconciliation read of a write.
pub async fn remote_tip(
    options: &GitOptions,
    repo: &Path,
    url: &str,
    ref_name: &str,
    timeout: Duration,
) -> RemoteTip {
    let name = if ref_name.starts_with("refs/heads/") {
        ref_name.to_owned()
    } else {
        format!("refs/heads/{ref_name}")
    };
    let git = read_git(options, repo, timeout);
    let args = [
        "-c",
        "protocol.ext.allow=never",
        "ls-remote",
        "--refs",
        "--",
        url,
        &name,
    ];
    match read(&git, &args, SMALL_CAP).await {
        Ok((0, out, _)) => RemoteTip::Answered(out.lines().find_map(|line| {
            let (id, found) = line.split_once('\t')?;
            (found == name).then(|| id.to_owned())
        })),
        _ => RemoteTip::NoAnswer,
    }
}

/// The commit of the local branch `refs/heads/<branch>`; `None` when it does
/// not exist or Git cannot read it.
pub async fn local_tip(
    options: &GitOptions,
    repo: &Path,
    branch: &str,
    timeout: Duration,
) -> Option<String> {
    let git = read_git(options, repo, timeout);
    let name = format!("refs/heads/{branch}");
    let args = ["rev-parse", "--verify", "--quiet", &name];
    match read(&git, &args, SMALL_CAP).await {
        Ok((0, out, _)) if oid_ok(out.trim()) => Some(out.trim().to_owned()),
        _ => None,
    }
}

/// `git merge-base --is-ancestor <a> <b>`: `Some(true)` when `a` is an
/// ancestor of `b`, `Some(false)` when it is not, `None` for an error or a
/// missing commit.
pub async fn is_ancestor(
    options: &GitOptions,
    repo: &Path,
    a: &str,
    b: &str,
    timeout: Duration,
) -> Option<bool> {
    if !oid_ok(a) {
        return None;
    }
    let git = read_git(options, repo, timeout);
    let args = ["merge-base", "--is-ancestor", a, b];
    match read(&git, &args, SMALL_CAP).await {
        Ok((0, _, _)) => Some(true),
        Ok((1, _, _)) => Some(false),
        _ => None,
    }
}

/// The plain fetch of `remote` (design §2.4, 30 s), so that the remote
/// tracking refs know a tip that moved. `false`: the fetch failed.
pub async fn fetch_default(options: &GitOptions, repo: &Path, remote: &str) -> bool {
    let done: Result<(), Refusal> = fetch_remote(options, repo, remote).await;
    done.is_ok()
}

/// The home folder as a path (a helper of the callers).
#[must_use]
pub fn repo_path(folder: &str) -> PathBuf {
    PathBuf::from(folder)
}

#[cfg(test)]
#[path = "land_remote_tests.rs"]
mod tests;
