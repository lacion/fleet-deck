//! The read-only worktree inventory (`workspace.worktrees`, DL-142): the
//! worktrees that the station created for sessions, with what Git reports
//! about each.
//!
//! The inventory runs only Git commands that read, one at a time per
//! worktree, each with a 10 s limit. It never fetches and never writes. A
//! path outside the allowed roots is never passed to Git.
//!
//! The inventory runs no `git status`. It reads the index and the files
//! itself (see [`scan`]). No command that it runs can run a filter, a hook,
//! an fsmonitor command, a textconv or external diff program, or a lazy
//! fetch.

use std::path::{Path, PathBuf};
use std::time::Duration;

use fleetdeck_proto::event::SessionState;
use fleetdeck_proto::link::{WorktreeCommit, WorktreeInfo, WorktreeList, WorktreeVerdict};
use fleetdeck_proto::session::SessionSummary;
use tokio::time::Instant;

use crate::manager::policy::{Policy, WorkspaceError};
use crate::store::{Store, StoreError};
use crate::workspace::{Git, GitOptions};

#[path = "worktree_scan.rs"]
pub(super) mod scan;

/// The limit for one Git command.
pub const GIT_TIMEOUT: Duration = Duration::from_secs(10);
/// The limit for the whole inventory.
pub const DEADLINE: Duration = Duration::from_secs(45);

/// The note of a worktree that the deadline cut off.
pub const NOT_CHECKED: &str = "not checked: the time limit passed";
/// The note of a worktree outside the allowed roots.
pub const OUTSIDE_ROOTS: &str = "outside the allowed roots";
/// The note of a worktree that Git does not list.
pub const NOT_LISTED: &str = "not listed by git worktree list";

/// `git` from `PATH`, 10 s per call, no system or user configuration.
#[must_use]
pub fn git_options() -> GitOptions {
    GitOptions {
        timeout: GIT_TIMEOUT,
        ..GitOptions::isolated()
    }
}

/// The inventory of every session of `store` that has a worktree, newest
/// `updated_at` first.
///
/// # Errors
/// A store error.
pub async fn inventory(
    store: &Store,
    policy: &Policy,
    git: &GitOptions,
) -> Result<WorktreeList, StoreError> {
    let sessions = store.snapshot().await?.sessions;
    Ok(list(sessions, policy, git, DEADLINE).await)
}

/// The inventory of the worktrees in `sessions`, one after the other. A
/// worktree that is not checked when `deadline` passes gets the verdict
/// `unknown` with the note [`NOT_CHECKED`].
pub async fn list(
    sessions: Vec<SessionSummary>,
    policy: &Policy,
    git: &GitOptions,
    deadline: Duration,
) -> WorktreeList {
    let mut sessions: Vec<SessionSummary> = sessions
        .into_iter()
        .filter(|s| s.worktree.is_some())
        .collect();
    sessions.sort_by(|a, b| {
        b.updated_at
            .cmp(&a.updated_at)
            .then_with(|| b.session_id.cmp(&a.session_id))
    });
    let deadline = Instant::now() + deadline;
    let mut worktrees = Vec::with_capacity(sessions.len());
    for s in &sessions {
        let Some(path) = s.worktree.as_deref() else {
            continue;
        };
        let live = is_live(s);
        let info = match tokio::time::timeout_at(deadline, check(s, path, live, policy, git)).await
        {
            Ok(info) => info,
            Err(_) => unchecked(s, path, live),
        };
        worktrees.push(info);
    }
    WorktreeList { worktrees }
}

/// A session is live while it has a process or a terminal hold.
fn is_live(s: &SessionSummary) -> bool {
    !matches!(s.state, SessionState::Exited | SessionState::Lost) || s.terminal.is_some()
}

fn blank(s: &SessionSummary, path: &str, live: bool) -> WorktreeInfo {
    WorktreeInfo {
        session_id: s.session_id.clone(),
        title: s.title.clone(),
        path: path.to_owned(),
        branch: None,
        main: None,
        exists: false,
        registered: false,
        dirty: None,
        ignored: None,
        unpushed: None,
        compared_with: None,
        last_commit: None,
        live,
        verdict: WorktreeVerdict::Unknown,
        note: None,
    }
}

fn unchecked(s: &SessionSummary, path: &str, live: bool) -> WorktreeInfo {
    WorktreeInfo {
        exists: Path::new(path).is_dir(),
        note: Some(NOT_CHECKED.to_owned()),
        ..blank(s, path, live)
    }
}

/// Keeps the first failure only.
fn fail(note: &mut Option<String>, e: impl std::fmt::Display) {
    if note.is_none() {
        *note = Some(e.to_string());
    }
}

async fn check(
    s: &SessionSummary,
    path: &str,
    live: bool,
    policy: &Policy,
    options: &GitOptions,
) -> WorktreeInfo {
    let mut info = blank(s, path, live);
    let canonical = match policy.workspace(path) {
        Ok(c) => c,
        Err(WorkspaceError::Missing(_)) => {
            info.verdict = WorktreeVerdict::Missing;
            return info;
        }
        Err(WorkspaceError::Outside(_)) => {
            info.exists = true;
            if live {
                info.verdict = WorktreeVerdict::Live;
            } else {
                info.note = Some(OUTSIDE_ROOTS.to_owned());
            }
            return info;
        }
    };
    info.exists = true;
    let git = Git::new(&canonical, options.clone());
    let mut note = None;

    info.branch = match scan::text(&git, &["rev-parse", "--abbrev-ref", "HEAD"]).await {
        Ok(t) => Some(t.trim().to_owned()).filter(|b| !b.is_empty() && b != "HEAD"),
        Err(_) => None,
    };

    let mut main_head = None;
    match scan::text(&git, &["worktree", "list", "--porcelain"]).await {
        Ok(text) => {
            let entries = parse_worktrees(&text);
            if let Some(first) = entries.first() {
                info.main = Some(first.path.clone());
                main_head = first.head.clone();
            }
            info.registered = entries.iter().any(|e| same_path(&e.path, &canonical));
            if !info.registered {
                fail(&mut note, NOT_LISTED);
            }
        }
        Err(e) => fail(&mut note, e),
    }

    match scan::scan(&git, &canonical, 0).await {
        Ok(found) => {
            info.dirty = Some(found.changed.saturating_add(found.untracked));
            info.ignored = found.ignored;
        }
        Err(e) => fail(&mut note, e),
    }

    match unpushed(&git, main_head.as_deref()).await {
        Ok((n, with)) => {
            info.unpushed = Some(n);
            info.compared_with = with.map(str::to_owned);
        }
        Err(e) => fail(&mut note, e),
    }

    info.last_commit = last_commit(&git).await;
    info.verdict = verdict(&info);
    if info.verdict == WorktreeVerdict::Unknown {
        info.note = note;
    }
    info
}

/// The verdict of a worktree that exists, in the order of the rule.
fn verdict(info: &WorktreeInfo) -> WorktreeVerdict {
    if info.live {
        return WorktreeVerdict::Live;
    }
    match (info.registered, info.dirty, info.unpushed) {
        (true, Some(dirty), Some(unpushed)) => {
            if dirty > 0 || unpushed > 0 {
                WorktreeVerdict::HasWork
            } else {
                WorktreeVerdict::Clean
            }
        }
        _ => WorktreeVerdict::Unknown,
    }
}

/// One entry of `git worktree list --porcelain`.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Listed {
    pub(super) path: String,
    pub(super) head: Option<String>,
}

pub(super) fn parse_worktrees(text: &str) -> Vec<Listed> {
    let mut out: Vec<Listed> = Vec::new();
    for line in text.lines() {
        if let Some(path) = line.strip_prefix("worktree ") {
            out.push(Listed {
                path: path.to_owned(),
                head: None,
            });
        } else if let (Some(head), Some(last)) = (line.strip_prefix("HEAD "), out.last_mut()) {
            last.head = Some(head.to_owned());
        }
    }
    out
}

fn same_path(listed: &str, canonical: &Path) -> bool {
    let listed = PathBuf::from(listed);
    listed.canonicalize().unwrap_or(listed) == canonical
}

/// The commits that no fetched remote ref holds; without any remote ref,
/// the commits that the main checkout's HEAD does not hold. An unborn branch
/// has none, and nothing to compare with.
async fn unpushed(
    git: &Git,
    main_head: Option<&str>,
) -> Result<(u32, Option<&'static str>), String> {
    let head = scan::status(git, &["rev-parse", "--verify", "--quiet", "HEAD"])
        .await
        .map_err(|e| e.to_string())?;
    match head.0 {
        0 => {}
        // An unborn branch.
        1 => return Ok((0, None)),
        code => return Err(format!("git rev-parse HEAD failed ({code}): {}", head.2)),
    }
    let refs = scan::text(git, &["for-each-ref", "--count=1", "refs/remotes"])
        .await
        .map_err(|e| e.to_string())?;
    let (args, with): (Vec<&str>, &'static str) = if refs.trim().is_empty() {
        let Some(head) = main_head else {
            return Err("the main checkout has no HEAD".to_owned());
        };
        (vec!["rev-list", "--count", "HEAD", "--not", head], "main")
    } else {
        (
            vec!["rev-list", "--count", "HEAD", "--not", "--remotes"],
            "remotes",
        )
    };
    let text = scan::text(git, &args).await.map_err(|e| e.to_string())?;
    let n = text
        .trim()
        .parse::<u32>()
        .map_err(|e| format!("git rev-list count: {e}"))?;
    Ok((n, Some(with)))
}

async fn last_commit(git: &Git) -> Option<WorktreeCommit> {
    // `--no-show-signature`: `log.showSignature` would run the configured
    // signature verifier (PRE-PR review of DL-143).
    let text = scan::text(
        git,
        &["log", "-1", "--no-show-signature", "--format=%ct%x00%s"],
    )
    .await
    .ok()?;
    let (secs, subject) = text.trim_end_matches('\n').split_once('\0')?;
    Some(WorktreeCommit {
        subject: subject.to_owned(),
        at: secs.parse::<i64>().ok()?.saturating_mul(1000),
    })
}
