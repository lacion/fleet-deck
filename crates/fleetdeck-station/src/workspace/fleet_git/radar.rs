//! The predictive conflict radar (FG-7, design §10.2, O21, O22): the radar
//! home fetches the remote and runs `git merge-tree --write-tree` for each
//! pair of pushed session branches, and for each branch against the default
//! branch. It changes no ref and no work tree file. The deck drives it
//! (`project.radar`); this file is the run on the station.

use std::collections::HashSet;
use std::path::Path;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use fleetdeck_proto::fleet_git::{
    GitProjectView, LocalRemotes, RADAR_DEFAULT_KEY, RADAR_MAX_BRANCHES, RadarBranch, RadarPair,
    RadarResult, code, remote_identity,
};

use super::fetch::{default_branch, fetch_remote};
use super::identity::{
    PushDestination, READ_TIMEOUT, Refusal, effective_push_url, net_git, read, read_git,
    require_git,
};
use super::redact::redact;
use crate::workspace::{GitError, GitOptions};

/// The time limit of one `git merge-tree` (design §2.4).
const PAIR_TIMEOUT: Duration = Duration::from_secs(20);
/// The time limit of one run (design §10.2).
const RUN_TIMEOUT: Duration = Duration::from_secs(120);
/// The time limit of the WIP fetch (design §2.4).
const WIP_FETCH_TIMEOUT: Duration = Duration::from_secs(30);
/// The most pairs that one run compares (design §10.2).
const MAX_PAIRS: usize = 50;
/// The most files that one pair reports (design §10.2).
const MAX_FILES: usize = 100;
/// The most bytes of the answer of one `git merge-tree` (design §2.4).
const OUT_CAP: usize = 64 * 1024;
/// The most bytes of stderr of the WIP fetch (design §2.4).
const FETCH_STDERR: usize = 2 * 1024;
/// The most bytes of the answer of a small read.
const SMALL_CAP: usize = 4 * 1024;

/// What one radar run needs.
pub struct RadarEnv<'a> {
    /// The confined Git options of the network commands (DL-200).
    pub options: &'a GitOptions,
    /// The canonical home folder (the repository).
    pub repo_dir: &'a Path,
    pub view: &'a GitProjectView,
    pub allow_local_remotes: bool,
}

/// The time limits of a run (a seam for the tests).
#[derive(Debug, Clone, Copy)]
struct Limits {
    pair: Duration,
    run: Duration,
}

const LIMITS: Limits = Limits {
    pair: PAIR_TIMEOUT,
    run: RUN_TIMEOUT,
};

/// Is `name` a branch of a session of Fleet Git: `fd/<slug>/<rest>` made of
/// plain branch-name parts? The station never gives another name to Git.
#[must_use]
pub fn branch_name_ok(name: &str) -> bool {
    let mut parts = name.split('/');
    parts.next() == Some("fd")
        && name.split('/').count() >= 3
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

/// The refusal of a `project.radar` params value that is not valid (design
/// §10.2): at most [`RADAR_MAX_BRANCHES`] branches, each key not empty and
/// used once, each ref a plain `fd/<slug>/<name>`. `None`: valid.
#[must_use]
pub fn check_branches(branches: &[RadarBranch]) -> Option<&'static str> {
    if branches.len() > RADAR_MAX_BRANCHES {
        return Some("At most 50 branches.");
    }
    let mut keys = HashSet::new();
    for branch in branches {
        if branch.key.is_empty() || branch.key == RADAR_DEFAULT_KEY || !keys.insert(&branch.key) {
            return Some("The branch keys must be different and not empty.");
        }
        if !branch_name_ok(&branch.ref_name) {
            return Some("The branch name is not valid.");
        }
    }
    None
}

/// The runs that are active now, by home folder: at most one run of a project
/// at a time (design §10.2).
static ACTIVE: Mutex<Option<HashSet<String>>> = Mutex::new(None);

/// The right to run the radar of one home. It ends when dropped.
#[derive(Debug)]
pub struct RadarGuard(String);

impl RadarGuard {
    /// Takes the right for the home `repo_dir`; `None` when a run is active.
    #[must_use]
    pub fn try_acquire(repo_dir: &Path) -> Option<Self> {
        let key = repo_dir.to_string_lossy().into_owned();
        let mut active = ACTIVE.lock().unwrap_or_else(PoisonError::into_inner);
        active
            .get_or_insert_with(HashSet::new)
            .insert(key.clone())
            .then(|| Self(key))
    }
}

impl Drop for RadarGuard {
    fn drop(&mut self) {
        let mut active = ACTIVE.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(set) = active.as_mut() {
            set.remove(&self.0);
        }
    }
}

fn failed(now_ms: i64, code: &str, text: impl Into<String>) -> RadarResult {
    RadarResult {
        checked_at: now_ms,
        base_oid: None,
        pairs: Vec::new(),
        skipped: 0,
        code: Some(code.to_owned()),
        text: Some(text.into()),
    }
}

/// The result of a refusal before any pair: Git older than 2.45 has its own
/// text; the others keep theirs.
fn refused(now_ms: i64, refusal: Refusal) -> RadarResult {
    if refusal.code == code::GIT_TOO_OLD {
        return failed(now_ms, code::GIT_TOO_OLD, "Git is older than 2.45.");
    }
    failed(now_ms, refusal.code, refusal.text)
}

/// Fetches the WIP URL into `refs/fleetdeck/wip/fd/*` (design §2.4, §10.2).
/// The URL is the verified one (§3.8 rule 2).
pub(super) async fn fetch_wip(env: &RadarEnv<'_>, url: &str) -> Result<(), Refusal> {
    let local = if env.allow_local_remotes {
        LocalRemotes::Allow
    } else {
        LocalRemotes::Deny
    };
    let identity = remote_identity(url, local)
        .map_err(|e| Refusal::new(e.code(), format!("The WIP remote is refused: {e}")))?;
    let destination = PushDestination::Wip {
        url: url.to_owned(),
    };
    let verified = effective_push_url(
        env.options,
        env.repo_dir,
        &destination,
        &identity.dest,
        env.allow_local_remotes,
    )
    .await?;
    let net = net_git(env.options, env.repo_dir, WIP_FETCH_TIMEOUT);
    let args = [
        "-c",
        "protocol.ext.allow=never",
        "fetch",
        "--prune",
        "--no-tags",
        "--no-recurse-submodules",
        "--no-write-fetch-head",
        "--",
        &verified.url,
        "+refs/heads/fd/*:refs/fleetdeck/wip/fd/*",
    ];
    let text = match net.status_stderr(&args, &[], FETCH_STDERR).await {
        Ok((0, _)) => return Ok(()),
        Ok((_, stderr)) => redact(&stderr),
        Err(GitError::Timeout { .. }) => "it took more than 30 s.".to_owned(),
        Err(e) => redact(&e.to_string()),
    };
    Err(Refusal::new(
        code::FETCH_FAILED,
        format!("The fetch failed: {text}"),
    ))
}

/// The commit of `reference`, or `None` when it is not a commit.
pub(super) async fn commit_of(git: &crate::workspace::Git, reference: &str) -> Option<String> {
    let probe = format!("{reference}^{{commit}}");
    let args = [
        "rev-parse",
        "--verify",
        "--quiet",
        "--end-of-options",
        &probe,
    ];
    let (exit, out, _) = read(git, &args, SMALL_CAP).await.ok()?;
    let oid = out.trim();
    (exit == 0 && matches!(oid.len(), 40 | 64)).then(|| oid.to_owned())
}

/// The files of a conflicting `git merge-tree --name-only` answer: the lines
/// after the tree id, up to the first empty line (design §10.2). At most
/// [`MAX_FILES`]; the flag says that more existed.
fn conflict_files(out: &str) -> (Vec<String>, bool) {
    let mut files: Vec<String> = Vec::new();
    let mut truncated = false;
    for line in out.lines().skip(1) {
        if line.is_empty() {
            break;
        }
        if files.iter().any(|f| f == line) {
            continue;
        }
        if files.len() == MAX_FILES {
            truncated = true;
            break;
        }
        files.push(line.to_owned());
    }
    (files, truncated)
}

/// One side of a pair: the key and the commit.
type Side<'a> = (&'a str, &'a str);

/// One pair: `Some(pair)` for exit 0 and 1, `None` for another exit, a
/// timeout or too much output (the pair is skipped).
async fn compare(
    env: &RadarEnv<'_>,
    timeout: Duration,
    (a_key, a_oid): Side<'_>,
    (b_key, b_oid): Side<'_>,
) -> Option<RadarPair> {
    let git = read_git(env.options, env.repo_dir, timeout);
    let args = [
        "merge-tree",
        "--write-tree",
        "--name-only",
        "--no-messages",
        a_oid,
        b_oid,
    ];
    let (exit, out, _) = git.status(&args, &[], None).await.ok()?;
    let mut pair = RadarPair {
        a: a_key.to_owned(),
        b: b_key.to_owned(),
        conflict: false,
        files: Vec::new(),
        files_truncated: false,
    };
    match exit {
        0 => {}
        1 => {
            let kept = &out[..out.len().min(OUT_CAP)];
            let (files, truncated) = conflict_files(&String::from_utf8_lossy(kept));
            pair.conflict = true;
            pair.files = files;
            pair.files_truncated = truncated || out.len() > OUT_CAP;
        }
        _ => return None,
    }
    Some(pair)
}

/// One radar run (design §10.2): fetch `push_remote` (and the WIP URL),
/// then compare. `branches` are the pushed branches, newest session first;
/// the caller checked them with [`check_branches`]. A failure before the
/// pairs is the `code` and `text` of the result, with no pair.
///
/// The pairs run in this order: each branch against the default branch
/// (newest branch first), then each pair of branches (the newer one first,
/// then the older one). It runs at most 50 pairs and stops at 120 s. `skipped`
/// counts the pairs that did not give a result: over the limit, after the
/// stop, or with a failing `git merge-tree`. A branch that is not in the
/// fetched refs is left out and does not count.
pub async fn run_radar(env: &RadarEnv<'_>, branches: &[RadarBranch], now_ms: i64) -> RadarResult {
    run_with(env, branches, now_ms, LIMITS).await
}

async fn run_with(
    env: &RadarEnv<'_>,
    branches: &[RadarBranch],
    now_ms: i64,
    limits: Limits,
) -> RadarResult {
    let started = Instant::now();
    let settings = &env.view.settings;
    let remote = settings.push_remote.as_str();
    if let Err(refusal) = require_git(env.options).await {
        return refused(now_ms, refusal);
    }
    if let Err(refusal) = fetch_remote(env.options, env.repo_dir, remote).await {
        return refused(now_ms, refusal);
    }
    if let Some(url) = &settings.wip_remote
        && let Err(refusal) = fetch_wip(env, url).await
    {
        return refused(now_ms, refusal);
    }
    let git = read_git(env.options, env.repo_dir, READ_TIMEOUT);
    let Some(default) = default_branch(&git, remote, settings).await else {
        return failed(
            now_ms,
            code::NO_DEFAULT_BRANCH,
            "The remote has no default branch.",
        );
    };
    let Some(base) = commit_of(&git, &format!("{remote}/{default}")).await else {
        return failed(
            now_ms,
            code::NO_DEFAULT_BRANCH,
            "The remote has no default branch.",
        );
    };
    // The branches that exist in the fetched refs, in the given order.
    let mut found: Vec<(&str, String)> = Vec::new();
    for branch in branches {
        let reference = if settings.wip_remote.is_some() {
            format!("refs/fleetdeck/wip/{}", branch.ref_name)
        } else {
            format!("refs/remotes/{remote}/{}", branch.ref_name)
        };
        if let Some(oid) = commit_of(&git, &reference).await {
            found.push((branch.key.as_str(), oid));
        }
    }
    let mut candidates: Vec<(Side<'_>, Side<'_>)> = Vec::new();
    for (key, oid) in &found {
        candidates.push(((key, oid), (RADAR_DEFAULT_KEY, base.as_str())));
    }
    for (i, (a_key, a_oid)) in found.iter().enumerate() {
        for (b_key, b_oid) in &found[i + 1..] {
            candidates.push(((a_key, a_oid), (b_key, b_oid)));
        }
    }
    let total = candidates.len();
    let mut pairs = Vec::new();
    for (a, b) in candidates.into_iter().take(MAX_PAIRS) {
        let Some(left) = limits
            .run
            .checked_sub(started.elapsed())
            .filter(|d| !d.is_zero())
        else {
            break;
        };
        if let Some(pair) = compare(env, limits.pair.min(left), a, b).await {
            pairs.push(pair);
        }
    }
    let skipped = u32::try_from(total - pairs.len()).unwrap_or(u32::MAX);
    RadarResult {
        checked_at: now_ms,
        base_oid: Some(base),
        pairs,
        skipped,
        code: None,
        text: None,
    }
}

#[cfg(test)]
#[path = "radar_tests.rs"]
mod tests;
