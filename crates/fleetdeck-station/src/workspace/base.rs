//! The base commit of a session (R1.1): the commit that the session
//! started from. The start effect reads it with `git` and the station
//! appends it as `session.base`.
//!
//! - A worktree start: the base name, or the existing branch, or the `HEAD`
//!   of the repository, read before the worktree is made.
//! - A clone: the `HEAD` of the clone.
//! - A start in place: the `HEAD` of the working directory.
//! - An import: the `HEAD` of the working directory at import time.
//! - A resume of a session without a base: the merge base of `HEAD` and
//!   `origin/HEAD`, else `HEAD`; marked `inferred`.
//!
//! A directory that is not a Git worktree, an unborn `HEAD` or a `git`
//! failure gives `None`: the session has no base.

use std::path::Path;
use std::time::Duration;

use fleetdeck_proto::session::SessionBase;

use super::{Git, GitOptions};
use crate::store::WorkspacePlan;

/// The time limit of one `git` call here.
const BASE_TIMEOUT: Duration = Duration::from_secs(10);

fn git_in(dir: &Path, options: &GitOptions) -> Git {
    Git::new(
        dir,
        GitOptions {
            timeout: BASE_TIMEOUT,
            ..options.clone()
        },
    )
}

/// The commit id that `rev` names, when it names a commit.
async fn commit_of(git: &Git, rev: &str) -> Option<String> {
    let spec = format!("{rev}^{{commit}}");
    let (code, out, _) = git
        .status(
            &["rev-parse", "-q", "--verify", "--end-of-options", &spec],
            &[],
            None,
        )
        .await
        .ok()?;
    let oid = String::from_utf8_lossy(&out).trim().to_owned();
    (code == 0 && !oid.is_empty()).then_some(oid)
}

/// The branch that `HEAD` names; `None` for a detached `HEAD`.
async fn branch_of(git: &Git) -> Option<String> {
    let (code, out, _) = git
        .status(&["symbolic-ref", "-q", "--short", "HEAD"], &[], None)
        .await
        .ok()?;
    let name = String::from_utf8_lossy(&out).trim().to_owned();
    (code == 0 && !name.is_empty()).then_some(name)
}

/// The base of a start that does not provision a workspace first: a
/// worktree plan whose folder does not exist yet (read in the repository),
/// or a start in place (read in `cwd`). A clone gives `None` here; see
/// [`at_head`]. `repo_dir` is the pinned source repository of a worktree plan
/// (B9, DL-200); without it the `repo` path of the plan is used.
pub async fn planned(
    options: &GitOptions,
    plan: Option<&WorkspacePlan>,
    cwd: &str,
    repo_dir: Option<&Path>,
) -> Option<SessionBase> {
    match plan {
        Some(WorkspacePlan::Worktree {
            repo,
            path,
            branch,
            base,
        }) => {
            if Path::new(path).exists() {
                // A repeated effect: the first run recorded the base.
                return None;
            }
            let git = git_in(repo_dir.unwrap_or(Path::new(repo)), options);
            if let Some(base) = base {
                let oid = commit_of(&git, base).await?;
                return Some(found(Some(base.clone()), oid, false));
            }
            if let Some(oid) = commit_of(&git, &format!("refs/heads/{branch}")).await {
                return Some(found(Some(branch.clone()), oid, false));
            }
            let oid = commit_of(&git, "HEAD").await?;
            Some(found(branch_of(&git).await, oid, false))
        }
        Some(WorkspacePlan::Clone { .. }) => None,
        None => at_head(options, Path::new(cwd)).await,
    }
}

/// The base that is the `HEAD` of `dir` (a clone, an import, or a start in
/// place).
pub async fn at_head(options: &GitOptions, dir: &Path) -> Option<SessionBase> {
    let git = git_in(dir, options);
    let oid = commit_of(&git, "HEAD").await?;
    Some(found(branch_of(&git).await, oid, false))
}

/// The guessed base of a session that has none (an import or a resume):
/// the merge base of `HEAD` and `origin/HEAD`, else `HEAD`.
pub async fn inferred(options: &GitOptions, dir: &Path) -> Option<SessionBase> {
    let git = git_in(dir, options);
    commit_of(&git, "HEAD").await?;
    if let Ok((0, out, _)) = git
        .status(&["merge-base", "HEAD", "origin/HEAD"], &[], None)
        .await
    {
        let oid = String::from_utf8_lossy(&out).trim().to_owned();
        if !oid.is_empty() {
            return Some(found(Some("origin/HEAD".to_owned()), oid, true));
        }
    }
    let oid = commit_of(&git, "HEAD").await?;
    Some(found(branch_of(&git).await, oid, true))
}

fn found(git_ref: Option<String>, oid: String, inferred: bool) -> SessionBase {
    SessionBase {
        git_ref,
        oid,
        inferred,
    }
}

#[cfg(test)]
#[path = "base_tests.rs"]
mod base_tests;
