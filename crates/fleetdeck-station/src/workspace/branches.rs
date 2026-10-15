//! The branches of the repository of a session and the committed diff of
//! one of them (DL-191, `docs/v2/design/subagents-worktrees.md` 2.4 to 2.5):
//! the link methods `session.branches` and `files.branch_diff`.
//!
//! Git runs in the working directory of the session only, through a pinned
//! directory descriptor (never in the folder by name) and through the
//! hardened runner ([`hardened_args`], [`HARDENED_ENV`]): no pager, no
//! external diff, no text conversion, no submodule recursion, no lazy
//! fetch, no prompt. The diff leaves out the hidden names and the private
//! paths, like the session diff. A Git below 2.45 is refused (`git_too_old`). One
//! deadline covers a whole request, and one request of this kind runs for a
//! session at a time. The listing is one `for-each-ref` with NUL-separated
//! fields and a strict parser.

use std::collections::HashMap;
use std::future::Future;
use std::os::fd::OwnedFd;
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use fleetdeck_proto::SessionId;
use fleetdeck_proto::branches::{
    BranchDiffQuery, BranchesQuery, MAX_ANSWER_BYTES, MAX_BRANCHES, RepoBranch, RepoBranches,
    code as branch_code,
};
use fleetdeck_proto::event::DiffSummary;
use fleetdeck_proto::files::{DiffScope, FileRoot, HIDDEN_NAMES, MAX_DIFF_BYTES, SessionDiff};
use fleetdeck_proto::history::code as history_code;
use tokio::sync::OnceCell;

use super::GitOptions;
use super::git::{Git, GitError, HARDENED_ENV, hardened_args};
use super::private_paths::PrivatePaths;
use crate::manager::policy::Policy;
use crate::store::Store;

/// The deadline of one request: the wait for the session, and all commands.
pub const DEADLINE: Duration = Duration::from_secs(10);
/// The most bytes that the branch listing reads.
const LIST_MAX: usize = 256 * 1024;
/// The most bytes that a numstat reads.
const NUMSTAT_MAX: usize = 4 * 1024 * 1024;
/// The longest branch name, in bytes (with `refs/heads/`).
const REF_MAX: usize = 256;
/// The longest subject, in characters.
const SUBJECT_MAX: usize = 200;
/// The oldest Git that the methods accept.
const MIN_VERSION: (u32, u32) = (2, 45);

/// A wire error of the branch methods.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchError {
    /// `invalid_params`, `unknown_session`, `git_too_old`, `busy`, `internal`,
    /// `platform_unsupported`, or a code of the file service (`missing_workspace`, `outside_roots`).
    pub code: &'static str,
    pub message: String,
}

impl BranchError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    fn internal(e: impl std::fmt::Display) -> Self {
        Self::new("internal", e.to_string())
    }

    fn busy() -> Self {
        Self::new(history_code::BUSY, "the branches took too long")
    }
}

impl From<super::files::FileError> for BranchError {
    fn from(e: super::files::FileError) -> Self {
        Self {
            code: e.code,
            message: e.message,
        }
    }
}

fn from_git(e: GitError) -> BranchError {
    match e {
        GitError::Timeout { .. } => BranchError::busy(),
        other => BranchError::internal(other),
    }
}

// ------------------------------------------------------------ the runner

/// Runs `git <sub>` hardened; returns `(exit code, stdout, cut)`.
async fn run(git: &Git, sub: &[&str], max: usize) -> Result<(i32, Vec<u8>, bool), BranchError> {
    git.bounded_with_env(&hardened_args(sub), HARDENED_ENV, max)
        .await
        .map_err(from_git)
}

/// `major.minor` of the text that `git version` prints.
#[must_use]
pub fn parse_git_version(text: &str) -> Option<(u32, u32)> {
    let version = text.trim().strip_prefix("git version ")?;
    let mut parts = version.split(|c: char| !c.is_ascii_digit());
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    Some((major, minor))
}

/// The Git version, read once for this process.
static GIT_VERSION: OnceCell<(u32, u32)> = OnceCell::const_new();

/// Refuses a Git below 2.45. `seam` replaces the version in tests.
async fn require_git(git: &Git, seam: Option<(u32, u32)>) -> Result<(), BranchError> {
    let version = match seam {
        Some(version) => version,
        None => {
            *GIT_VERSION
                .get_or_try_init(|| async {
                    let (code, out, _) = run(git, &["version"], 1024).await?;
                    let text = String::from_utf8_lossy(&out).into_owned();
                    match (code, parse_git_version(&text)) {
                        (0, Some(version)) => Ok(version),
                        _ => Err(BranchError::internal("cannot read the Git version")),
                    }
                })
                .await?
        }
    };
    if version < MIN_VERSION {
        return Err(BranchError::new(
            branch_code::GIT_TOO_OLD,
            format!("Git {}.{} is older than 2.45", version.0, version.1),
        ));
    }
    Ok(())
}

// ------------------------------------------------------- the request lock

type Locks = Mutex<HashMap<SessionId, Arc<tokio::sync::Mutex<()>>>>;

/// One request of this kind runs for a session at a time. The map lives in
/// this module: the manager and the link context are not part of DL-191.
fn session_lock(session: &SessionId) -> Arc<tokio::sync::Mutex<()>> {
    static LOCKS: OnceLock<Locks> = OnceLock::new();
    let mut map = LOCKS
        .get_or_init(Locks::default)
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    // A lock that nobody holds or waits for is dropped.
    map.retain(|_, lock| Arc::strong_count(lock) > 1);
    Arc::clone(map.entry(session.clone()).or_default())
}

/// Runs `work` under the deadline. With `session`, the wait for the
/// session's lock counts against the deadline.
async fn within<T>(
    session: Option<&SessionId>,
    work: impl Future<Output = Result<T, BranchError>>,
) -> Result<T, BranchError> {
    within_for(DEADLINE, session, work).await
}

/// [`within`] with a chosen deadline (tests use a short one).
async fn within_for<T>(
    deadline: Duration,
    session: Option<&SessionId>,
    work: impl Future<Output = Result<T, BranchError>>,
) -> Result<T, BranchError> {
    tokio::time::timeout(deadline, async {
        let _guard = match session {
            Some(session) => Some(session_lock(session).lock_owned().await),
            None => None,
        };
        work.await
    })
    .await
    .map_err(|_| BranchError::busy())?
}

// ------------------------------------------------------------ the listing

fn is_oid(text: &str) -> bool {
    matches!(text.len(), 40 | 64) && text.bytes().all(|b| b.is_ascii_hexdigit())
}

/// The trimmed UTF-8 stdout of a small command.
fn line_of(out: &[u8]) -> Option<String> {
    Some(std::str::from_utf8(out).ok()?.trim().to_owned())
}

/// The repository top that `git rev-parse --show-toplevel` printed: the
/// bytes before its one final newline, kept as they are, because a folder
/// name can end in a space (DL-191). `None` for a cut or empty answer.
fn top_of(out: &[u8]) -> Option<std::path::PathBuf> {
    use std::os::unix::ffi::OsStrExt;
    let path = out.strip_suffix(b"\n")?;
    if path.is_empty() || path.contains(&b'\n') {
        return None;
    }
    Some(std::path::PathBuf::from(std::ffi::OsStr::from_bytes(path)))
}

/// One record of the `for-each-ref` output, or `None` when it is malformed.
fn parse_record(record: &[u8]) -> Option<RepoBranch> {
    let mut fields = record.split(|b| *b == 0);
    let (name, oid, unix, subject, counts) = (
        fields.next()?,
        fields.next()?,
        fields.next()?,
        fields.next()?,
        fields.next()?,
    );
    if fields.next().is_some() || name.len() > REF_MAX {
        return None;
    }
    let name = std::str::from_utf8(name)
        .ok()?
        .strip_prefix("refs/heads/")
        .filter(|n| !n.is_empty())?;
    let head = std::str::from_utf8(oid).ok().filter(|o| is_oid(o))?;
    let digits = |b: &[u8]| -> Option<String> {
        let t = std::str::from_utf8(b).ok()?;
        (!t.is_empty() && t.bytes().all(|c| c.is_ascii_digit())).then(|| t.to_owned())
    };
    let at: i64 = digits(unix)?.parse().ok()?;
    let counts = std::str::from_utf8(counts).ok()?;
    let (ahead, behind) = counts.split_once(' ')?;
    let ahead: u32 = digits(ahead.as_bytes())?.parse().ok()?;
    let behind: u32 = digits(behind.as_bytes())?.parse().ok()?;
    let subject: String = String::from_utf8_lossy(subject)
        .chars()
        .take(SUBJECT_MAX)
        .collect();
    Some(RepoBranch {
        name: name.to_owned(),
        head: head.to_ascii_lowercase(),
        ahead,
        behind,
        at,
        subject,
    })
}

/// Parses the `for-each-ref` output (records end with LF) into the branches
/// other than `current`; true when the list is cut (a malformed record, a
/// cut output, or 201 records).
#[must_use]
pub fn parse_branches(out: &[u8], cut: bool, current: Option<&str>) -> (Vec<RepoBranch>, bool) {
    let mut branches = Vec::new();
    let mut truncated = cut;
    let mut records: Vec<&[u8]> = out.split(|b| *b == b'\n').collect();
    // The text after the last LF is no record: empty for a complete output,
    // an unfinished record for a cut one.
    let tail = records.pop().unwrap_or_default();
    if !tail.is_empty() {
        truncated = true;
    }
    for (seen, record) in records.into_iter().enumerate() {
        let Some(branch) = parse_record(record) else {
            truncated = true;
            break;
        };
        if Some(branch.name.as_str()) != current {
            if branches.len() == MAX_BRANCHES {
                truncated = true;
                break;
            }
            branches.push(branch);
        }
        // The 201st record shows that more may follow.
        if seen >= MAX_BRANCHES {
            truncated = true;
            break;
        }
    }
    (branches, truncated)
}

/// The commands of `session.branches` (no deadline here).
async fn list(git: &Git) -> Result<RepoBranches, BranchError> {
    let (code, out, _) = run(git, &["rev-parse", "--verify", "-q", "HEAD"], 256).await?;
    let head = match code {
        0 => Some(
            line_of(&out)
                .filter(|h| is_oid(h))
                .ok_or_else(|| BranchError::internal("HEAD is not an object id"))?
                .to_ascii_lowercase(),
        ),
        // An unborn HEAD.
        1 => None,
        _ => {
            return Err(BranchError::new(
                branch_code::NOT_A_REPOSITORY,
                "the folder is not a Git repository",
            ));
        }
    };
    let (code, out, _) = run(git, &["symbolic-ref", "-q", "--short", "HEAD"], 1024).await?;
    let branch = match code {
        0 => Some(
            line_of(&out)
                .filter(|b| !b.is_empty())
                .ok_or_else(|| BranchError::internal("the current branch name is not text"))?,
        ),
        // A detached HEAD.
        1 => None,
        _ => return Err(BranchError::internal("cannot read the current branch")),
    };
    let mut answer = RepoBranches {
        head: head.clone(),
        branch: branch.clone(),
        branches: Vec::new(),
        truncated: false,
    };
    if head.is_none() {
        return Ok(answer);
    }
    let count = format!("--count={}", MAX_BRANCHES + 1);
    let format = "--format=%(refname)%00%(objectname)%00%(committerdate:unix)%00%(contents:subject)%00%(ahead-behind:HEAD)";
    let sub = [
        "for-each-ref",
        count.as_str(),
        "--sort=-committerdate",
        format,
        "refs/heads",
    ];
    let (code, out, cut) = run(git, &sub, LIST_MAX).await?;
    if code != 0 && !cut {
        return Err(BranchError::internal("git for-each-ref failed"));
    }
    let (branches, truncated) = parse_branches(&out, cut, branch.as_deref());
    answer.branches = branches;
    answer.truncated = truncated;
    Ok(answer)
}

fn git_for(options: &GitOptions, cwd: &Path) -> Git {
    let options = GitOptions {
        timeout: DEADLINE,
        ..options.clone()
    };
    Git::new(cwd, options)
}

/// Opens `dir` without following a link and returns the descriptor with a
/// `Git` that runs in it. The caller keeps the descriptor open while Git
/// runs, so a rename of the folder or of an ancestor cannot move Git to
/// another folder (DL-191). Without a pinned path (macOS) it fails closed.
fn pinned_git(options: &GitOptions, dir: &Path) -> Result<(OwnedFd, Git), BranchError> {
    let fd = super::files::open_root(dir)
        .map_err(|_| BranchError::internal("the session folder cannot be opened"))?;
    match super::files::pinned_dir(&fd) {
        Some(path) => {
            let git = git_for(options, &path);
            Ok((fd, git))
        }
        None => Err(BranchError::new(
            branch_code::PLATFORM_UNSUPPORTED,
            "the branch view needs Linux for now",
        )),
    }
}

/// The branches of the repository in `cwd`. `version` replaces the Git
/// version in tests.
///
/// # Errors
/// [`BranchError`]: `git_too_old`, `busy`, `internal`.
pub async fn branches_with(
    options: &GitOptions,
    cwd: &Path,
    version: Option<(u32, u32)>,
) -> Result<RepoBranches, BranchError> {
    let git = git_for(options, cwd);
    within(None, async {
        require_git(&git, version).await?;
        list(&git).await
    })
    .await
}

/// The branches of the repository in `cwd`.
///
/// # Errors
/// As [`branches_with`].
pub async fn branches(options: &GitOptions, cwd: &Path) -> Result<RepoBranches, BranchError> {
    branches_with(options, cwd, None).await
}

// -------------------------------------------------------------- the diff

fn numstat(text: &str) -> DiffSummary {
    let mut summary = DiffSummary {
        files_changed: 0,
        insertions: 0,
        deletions: 0,
    };
    for line in text.lines().filter(|l| !l.is_empty()) {
        let mut fields = line.splitn(3, '\t');
        let count = |f: Option<&str>| f.and_then(|v| v.parse::<u64>().ok()).unwrap_or(0);
        summary.insertions += count(fields.next());
        summary.deletions += count(fields.next());
        summary.files_changed = summary.files_changed.saturating_add(1);
    }
    summary
}

/// The flags of every diff of this module: no external program, no text
/// conversion, no submodule recursion.
const DIFF_FLAGS: [&str; 5] = [
    "--no-ext-diff",
    "--no-textconv",
    "--submodule=short",
    "--ignore-submodules=dirty",
    "--no-renames",
];

/// `spec` with `top` as the first magic word, so that it is read from the
/// top of the repository; empty for a spec without magic words.
fn with_top(spec: &str) -> String {
    spec.strip_prefix(":(")
        .map_or_else(String::new, |rest| format!(":(top,{rest}"))
}

/// The pathspec of a branch diff: the whole repository except the hidden
/// names at any depth and the private paths, all from the top of the
/// repository, also when the session folder is a subdirectory (DL-191).
fn branch_pathspec(top: &Path, private: &PrivatePaths) -> Vec<String> {
    let mut spec = vec!["--".to_owned(), ":/".to_owned()];
    for name in HIDDEN_NAMES {
        spec.push(format!(":(top,exclude,icase,glob)**/{name}"));
        spec.push(format!(":(top,exclude,icase,glob)**/{name}/**"));
    }
    spec.extend(
        private
            .excludes_for(top)
            .iter()
            .map(|entry| with_top(entry))
            .filter(|entry| !entry.is_empty()),
    );
    spec
}

/// The commands of `files.branch_diff` (no deadline here).
async fn diff_of(
    git: &Git,
    session: &SessionId,
    head: &str,
    private: &PrivatePaths,
) -> Result<SessionDiff, BranchError> {
    let invalid = |why: &str| BranchError::new(history_code::INVALID_PARAMS, why);
    if !is_oid(head) {
        return Err(invalid("head must be a full object id"));
    }
    let head = head.to_ascii_lowercase();
    let listed = list(git).await?;
    // Only the tip of a listed branch is accepted; Git gets the listed
    // (lowercase) id.
    if !listed.branches.iter().any(|b| b.head == head) {
        return Err(invalid(
            "head is not the tip of a branch of this repository",
        ));
    }
    let (code, out, _) = run(git, &["merge-base", "HEAD", &head], 256).await?;
    let base = match (code, line_of(&out)) {
        (0, Some(base)) if is_oid(&base) => base.to_ascii_lowercase(),
        (1, _) => {
            return Err(BranchError::new(
                branch_code::NO_COMMON_HISTORY,
                "the branch has no common history with HEAD",
            ));
        }
        _ => return Err(BranchError::internal("git merge-base failed")),
    };
    // The top of the repository is used for the relative excludes only; Git
    // keeps running in the pinned folder.
    let (code, out, _) = run(git, &["rev-parse", "--show-toplevel"], 4096).await?;
    let top = match (code, top_of(&out)) {
        (0, Some(top)) => top,
        _ => return Err(BranchError::internal("git rev-parse failed")),
    };
    let spec = branch_pathspec(&top, private);
    let mut patch_args = vec!["diff", "--no-color"];
    patch_args.extend(DIFF_FLAGS);
    patch_args.extend([base.as_str(), head.as_str()]);
    patch_args.extend(spec.iter().map(String::as_str));
    let (code, out, cut) = run(git, &patch_args, MAX_DIFF_BYTES as usize).await?;
    if code != 0 && !cut {
        return Err(BranchError::internal("git diff failed"));
    }
    let patch = String::from_utf8_lossy(&out).into_owned();
    let mut stat_args = vec!["diff", "--numstat"];
    stat_args.extend(DIFF_FLAGS);
    stat_args.extend([base.as_str(), head.as_str()]);
    stat_args.extend(spec.iter().map(String::as_str));
    let (code, out, _) = run(git, &stat_args, NUMSTAT_MAX).await?;
    if code != 0 {
        return Err(BranchError::internal("git diff --numstat failed"));
    }
    let summary = numstat(&String::from_utf8_lossy(&out));
    Ok(fit(SessionDiff {
        session_id: session.clone(),
        base_oid: base,
        summary,
        patch,
        truncated: cut,
        scope: DiffScope::Committed,
    }))
}

/// The bytes that `c` takes inside a JSON string.
fn json_len(c: char) -> usize {
    match c {
        '"' | '\\' | '\n' | '\r' | '\t' | '\u{8}' | '\u{c}' => 2,
        c if (c as u32) < 0x20 => 6,
        c => c.len_utf8(),
    }
}

/// Cuts the patch at a character boundary until the serialized answer is at
/// most [`MAX_ANSWER_BYTES`]; a cut sets `truncated`.
fn fit(mut diff: SessionDiff) -> SessionDiff {
    let size = serde_json::to_vec(&diff).map_or(0, |v| v.len());
    if size <= MAX_ANSWER_BYTES {
        return diff;
    }
    // The patch string with its quotes, and the rest of the answer.
    let escaped = serde_json::to_string(&diff.patch).map_or(0, |s| s.len());
    let budget = MAX_ANSWER_BYTES.saturating_sub(size - escaped + 2);
    let (mut used, mut end) = (0_usize, 0_usize);
    for (at, c) in diff.patch.char_indices() {
        used += json_len(c);
        if used > budget {
            break;
        }
        end = at + c.len_utf8();
    }
    diff.patch.truncate(end);
    diff.truncated = true;
    diff
}

/// The committed diff of the branch with the tip `head` against its merge
/// base with `HEAD`, for the repository in `cwd`. `version` replaces the Git
/// version in tests.
///
/// # Errors
/// [`BranchError`]: `invalid_params`, `git_too_old`, `busy`, `internal`.
pub async fn branch_diff_with(
    options: &GitOptions,
    cwd: &Path,
    session: &SessionId,
    head: &str,
    version: Option<(u32, u32)>,
    private: &PrivatePaths,
) -> Result<SessionDiff, BranchError> {
    let git = git_for(options, cwd);
    within(None, async {
        require_git(&git, version).await?;
        diff_of(&git, session, head, private).await
    })
    .await
}

/// [`branch_diff_with`] with the real Git version.
///
/// # Errors
/// As [`branch_diff_with`].
pub async fn branch_diff(
    options: &GitOptions,
    cwd: &Path,
    session: &SessionId,
    head: &str,
    private: &PrivatePaths,
) -> Result<SessionDiff, BranchError> {
    branch_diff_with(options, cwd, session, head, None, private).await
}

// ------------------------------------------------------ the link methods

/// The directory of a session for the branch methods: the same policy and
/// private-path checks as `files.session_diff`.
async fn session_dir(
    store: &Store,
    policy: &Policy,
    private: &PrivatePaths,
    session: &SessionId,
) -> Result<std::path::PathBuf, BranchError> {
    Ok(
        crate::manager::files::root(store, policy, private, &FileRoot::Session(session.clone()))
            .await?,
    )
}

/// `session.branches`.
///
/// # Errors
/// [`BranchError`]: `unknown_session`, `missing_workspace`, `outside_roots`,
/// `git_too_old`, `busy`, `internal`.
pub async fn run_branches(
    store: &Store,
    policy: &Policy,
    private: &PrivatePaths,
    options: &GitOptions,
    query: BranchesQuery,
) -> Result<RepoBranches, BranchError> {
    let dir = session_dir(store, policy, private, &query.session_id).await?;
    // The descriptor stays open until the request ends (DL-191).
    let (_dir, git) = pinned_git(options, &dir)?;
    within(Some(&query.session_id), async {
        require_git(&git, None).await?;
        list(&git).await
    })
    .await
}

/// `files.branch_diff`.
///
/// # Errors
/// [`BranchError`]: as [`run_branches`], and `invalid_params`.
pub async fn run_branch_diff(
    store: &Store,
    policy: &Policy,
    private: &PrivatePaths,
    options: &GitOptions,
    query: BranchDiffQuery,
) -> Result<SessionDiff, BranchError> {
    let dir = session_dir(store, policy, private, &query.session_id).await?;
    // The descriptor stays open until the request ends (DL-191).
    let (_dir, git) = pinned_git(options, &dir)?;
    within(Some(&query.session_id), async {
        require_git(&git, None).await?;
        diff_of(&git, &query.session_id, &query.head, private).await
    })
    .await
}

#[cfg(test)]
#[path = "branches_private_tests.rs"]
mod branches_private_tests;
#[cfg(test)]
#[path = "branches_tests.rs"]
mod branches_tests;
