//! Push of a session branch (FG-2, design §5, O9 to O11).
//!
//! This file has the pure rules (the refspec, the backoff, the classification
//! of a push) and the one serialized writer of a station ([`PushWriter`],
//! design §5.5, decision 15). The store rows, the triggers and the attempt
//! bookkeeping are the caller's (task T1.3).

use std::path::{Path, PathBuf};
use std::time::Duration;

use fleetdeck_proto::fleet_git::code;

use super::identity::{
    PushDestination, Refusal, VerifiedUrl, effective_push_url, net_git, read, read_git,
};
use super::redact::redact;
use crate::workspace::GitError;
use crate::workspace::GitOptions;

/// The time limit of `git push` (design §2.4).
const PUSH_TIMEOUT: Duration = Duration::from_secs(60);
/// The time limit of the recovery `git ls-remote` (design §2.4).
const LS_REMOTE_TIMEOUT: Duration = Duration::from_secs(20);
/// The most bytes of the porcelain output that a push keeps (design §2.4).
const PUSH_STDOUT_CAP: usize = 16 * 1024;
/// The most bytes of the answer of `git ls-remote`.
const LS_REMOTE_CAP: usize = 4 * 1024;
/// The longest delay of the backoff, in minutes (O11).
const BACKOFF_MAX_MINUTES: u64 = 10;

/// Why a refspec was refused: the push does not run (design §5.3, §18).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefspecError {
    /// The ref is not below `fd/<slug>/` (O10).
    NotOwnNamespace,
    /// The ref name or the slug is not a plain branch name.
    InvalidRef,
    /// The commit or the lease is not a full object id.
    InvalidOid,
}

impl std::fmt::Display for RefspecError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::NotOwnNamespace => "the ref is not below fd/<slug>/ of this station",
            Self::InvalidRef => "the ref is not a plain branch name",
            Self::InvalidOid => "the commit id is not valid",
        })
    }
}

impl std::error::Error for RefspecError {}

fn slug_ok(slug: &str) -> bool {
    !slug.is_empty()
        && slug
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

fn oid_ok(oid: &str) -> bool {
    matches!(oid.len(), 40 | 64)
        && oid
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Is `rest` (the part of a ref after `fd/<slug>/`) a plain branch name?
fn rest_ok(rest: &str) -> bool {
    !rest.is_empty()
        && !rest.contains("..")
        && !rest.contains("@{")
        && !rest.ends_with(['.', '/'])
        && !rest.contains(|c: char| {
            c.is_control()
                || c.is_whitespace()
                || matches!(c, '~' | '^' | ':' | '?' | '*' | '[' | '\\')
        })
        && rest
            .split('/')
            .all(|part| !part.is_empty() && !part.starts_with('.') && !part.ends_with(".lock"))
}

/// The refspec `<oid>:refs/heads/<publish_ref>` of a push (design §5.3).
/// `publish_ref` is the ref without `refs/heads/` (`fd/<slug>/<callsign>`);
/// `slug` is the slug stored with the session. The refspec has no wildcard,
/// no `+`, and names the commit, so the station never pushes any ref but
/// `refs/heads/fd/<slug>/*` (O10).
///
/// # Errors
/// [`RefspecError`] for a ref outside `fd/<slug>/`, a ref or a slug that is
/// not a plain name, or an `oid` that is not a full lower-case hex id.
pub fn refspec_for(slug: &str, publish_ref: &str, oid: &str) -> Result<String, RefspecError> {
    if !slug_ok(slug) {
        return Err(RefspecError::InvalidRef);
    }
    let prefix = format!("fd/{slug}/");
    let Some(rest) = publish_ref.strip_prefix(&prefix) else {
        return Err(RefspecError::NotOwnNamespace);
    };
    if !rest_ok(rest) {
        return Err(RefspecError::InvalidRef);
    }
    if !oid_ok(oid) {
        return Err(RefspecError::InvalidOid);
    }
    Ok(format!("{oid}:refs/heads/{publish_ref}"))
}

/// The delay after the failed attempt `attempt` (1, 2, ...): `min(2^(n-1), 10)`
/// minutes: 1, 2, 4, 8, 10, 10, ... (design §5.6, O11).
#[must_use]
pub fn backoff_delay(attempt: u32) -> Duration {
    let exp = attempt.saturating_sub(1).min(10);
    Duration::from_secs(60 * (1u64 << exp).min(BACKOFF_MAX_MINUTES))
}

/// What a push did to the one ref (design §5.5, M2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PushOutcome {
    /// Exit 0 and the ref's status line has the flag ` `, `+`, `*` or `=`.
    Applied,
    /// The server answered for the ref (flag `!`): the code is
    /// `push_rejected`. A recovery read still confirms it.
    Rejected(&'static str),
    /// Every other case: the server may have taken the pack.
    Unknown,
}

/// Classifies a `git push --porcelain` run for `publish_ref` (design §5.5,
/// M2). `porcelain` is the stdout: one status line `<flag>\t<src>:<dst>\t<summary>`
/// for each ref. The line `L` is the one whose `<dst>` is
/// `refs/heads/<publish_ref>`.
#[must_use]
pub fn push_outcome(exit: i32, porcelain: &str, timed_out: bool, publish_ref: &str) -> PushOutcome {
    if timed_out {
        return PushOutcome::Unknown;
    }
    let dst = format!("refs/heads/{publish_ref}");
    let flag = porcelain.lines().find_map(|line| {
        let mut chars = line.chars();
        let flag = chars.next()?;
        let rest = chars.as_str().strip_prefix('\t')?;
        let refs = rest.split('\t').next()?;
        (refs.rsplit_once(':')?.1 == dst).then_some(flag)
    });
    match flag {
        Some('!') => PushOutcome::Rejected(code::PUSH_REJECTED),
        Some(' ' | '+' | '*' | '=') if exit == 0 => PushOutcome::Applied,
        _ => PushOutcome::Unknown,
    }
}

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

/// One push of an attempt: the values of the open attempt of `git_pushes`
/// (design §5.5 step 2). They never change while the attempt is open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushJob {
    /// The repository of the home (`session_git.repo_root`).
    pub repo_dir: PathBuf,
    /// The verified URL of the attempt (`attempt_url`).
    pub url: String,
    /// The slug stored with the session.
    pub slug: String,
    /// `session_git.publish_ref`, without `refs/heads/`.
    pub publish_ref: String,
    /// The commit to push (`attempt_oid`).
    pub oid: String,
    /// The lease (`attempt_lease`): the commit that this station pushed
    /// last, or empty when the ref must not exist.
    pub lease: String,
}

/// The code and the text of a push that did not apply (design §5.7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushFailure {
    /// `push_rejected`, `push_auth`, `push_timeout` or `push_failed`.
    pub code: &'static str,
    pub text: String,
}

/// The settled result of a push and its recovery read (design §5.5 steps 3
/// to 5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PushResult {
    /// The remote has the attempt's commit: the push answered `Applied`, or
    /// the recovery read found the commit after a lost answer.
    Applied,
    /// The recovery read answered and the remote tip is another commit or
    /// absent: the attempt is confirmed not applied.
    NotApplied(PushFailure),
    /// The recovery read gave no answer: the attempt stays open.
    Unknown(PushFailure),
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

/// The answer of the read of the remote tip.
enum Tip {
    Same,
    Other,
    NoAnswer,
}

/// The one writer of a station (design §5.5, decision 15): [`submit`] and
/// [`recover`] run one after the other, in the order of the calls, so an FG-2
/// push and a land push never run at the same time and the lease bookkeeping
/// has one writer.
///
/// [`submit`]: Self::submit
/// [`recover`]: Self::recover
#[derive(Debug)]
pub struct PushWriter {
    options: GitOptions,
    lock: tokio::sync::Mutex<()>,
}

impl PushWriter {
    /// A writer whose Git runs with `options` (the program and, for the
    /// network commands, the confinement of DL-200).
    #[must_use]
    pub fn new(options: GitOptions) -> Self {
        Self {
            options,
            lock: tokio::sync::Mutex::new(()),
        }
    }

    /// The destination check of design §3.8 rules 2 and 3 (see
    /// [`effective_push_url`]): the verified URL to store in `attempt_url`
    /// before the push. It reads the Git configuration only and does not wait
    /// for the writer.
    ///
    /// # Errors
    /// A [`Refusal`]: a confirmed failure, nothing was sent.
    pub async fn verify(
        &self,
        repo_dir: &Path,
        destination: &PushDestination,
        target_dest: &str,
        allow_local_remotes: bool,
    ) -> Result<VerifiedUrl, Refusal> {
        effective_push_url(
            &self.options,
            repo_dir,
            destination,
            target_dest,
            allow_local_remotes,
        )
        .await
    }

    /// Pushes `job` when every earlier job has ended (design §5.5 steps 3 to
    /// 5). The command: `push --porcelain --no-verify --no-follow-tags
    /// --recurse-submodules=no --force-with-lease=refs/heads/<ref>:<lease> --
    /// <url> <oid>:refs/heads/<ref>`. It names the verified URL, never a
    /// remote. A `Rejected` or `Unknown` push runs the recovery read.
    ///
    /// # Errors
    /// [`RefspecError`]: the ref is outside `refs/heads/fd/<slug>/` or an id
    /// is not valid. No Git process runs.
    pub async fn submit(&self, job: &PushJob) -> Result<PushResult, RefspecError> {
        self.submit_within(job, PUSH_TIMEOUT).await
    }

    async fn submit_within(
        &self,
        job: &PushJob,
        timeout: Duration,
    ) -> Result<PushResult, RefspecError> {
        let refspec = refspec_for(&job.slug, &job.publish_ref, &job.oid)?;
        if !job.lease.is_empty() && !oid_ok(&job.lease) {
            return Err(RefspecError::InvalidOid);
        }
        let _turn = self.lock.lock().await;
        let lease = format!(
            "--force-with-lease=refs/heads/{}:{}",
            job.publish_ref, job.lease
        );
        let args = [
            "-c",
            "protocol.ext.allow=never",
            "push",
            "--porcelain",
            "--no-verify",
            "--no-follow-tags",
            "--recurse-submodules=no",
            &lease,
            "--",
            &job.url,
            &refspec,
        ];
        let git = net_git(&self.options, &job.repo_dir, timeout);
        let (outcome, failed) = match git.status(&args, &[], None).await {
            Ok((exit, stdout, stderr)) => {
                let kept = &stdout[..stdout.len().min(PUSH_STDOUT_CAP)];
                let porcelain = String::from_utf8_lossy(kept);
                let outcome = push_outcome(exit, &porcelain, false, &job.publish_ref);
                (outcome, failure(outcome, &stderr, &porcelain))
            }
            Err(GitError::Timeout { .. }) => (
                PushOutcome::Unknown,
                PushFailure {
                    code: code::PUSH_TIMEOUT,
                    text: "Push took more than 60 s.".to_owned(),
                },
            ),
            Err(e) => (
                PushOutcome::Unknown,
                failure(PushOutcome::Unknown, &e.to_string(), ""),
            ),
        };
        if outcome == PushOutcome::Applied {
            return Ok(PushResult::Applied);
        }
        Ok(match self.read_tip(job).await {
            Tip::Same => PushResult::Applied,
            Tip::Other => PushResult::NotApplied(failed),
            Tip::NoAnswer => PushResult::Unknown(failed),
        })
    }

    /// The recovery of an attempt that a restart interrupted (design §5.5
    /// step 6): the recovery read with the stored values, and no push.
    pub async fn recover(&self, job: &PushJob) -> PushResult {
        let _turn = self.lock.lock().await;
        let failed = |text: &str| PushFailure {
            code: code::PUSH_FAILED,
            text: text.to_owned(),
        };
        match self.read_tip(job).await {
            Tip::Same => PushResult::Applied,
            Tip::Other => PushResult::NotApplied(failed(
                "Push did not complete: the station restarted before the answer.",
            )),
            Tip::NoAnswer => PushResult::Unknown(failed(
                "Push may not have completed: the remote did not answer the check.",
            )),
        }
    }

    /// The read `git ls-remote --refs -- <url> refs/heads/<ref>` (20 s).
    async fn read_tip(&self, job: &PushJob) -> Tip {
        let name = format!("refs/heads/{}", job.publish_ref);
        let git = read_git(&self.options, &job.repo_dir, LS_REMOTE_TIMEOUT);
        let args = [
            "-c",
            "protocol.ext.allow=never",
            "ls-remote",
            "--refs",
            "--",
            &job.url,
            &name,
        ];
        match read(&git, &args, LS_REMOTE_CAP).await {
            Ok((0, out, _)) => {
                let tip = out.lines().find_map(|line| {
                    let (id, found) = line.split_once('\t')?;
                    (found == name).then_some(id)
                });
                if tip == Some(job.oid.as_str()) {
                    Tip::Same
                } else {
                    Tip::Other
                }
            }
            _ => Tip::NoAnswer,
        }
    }
}

#[cfg(test)]
#[path = "push_tests.rs"]
mod tests;
