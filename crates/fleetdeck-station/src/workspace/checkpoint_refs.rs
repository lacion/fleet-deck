//! The checkpoint refs of one session (R1.5): the purge of a worktree
//! deletes them, so a deleted worktree leaves no hidden refs in the
//! repository.
//!
//! The refs are `refs/fleetdeck/checkpoints/<incarnation>/<worktree id>/
//! <session>/<turn>/<start|end>` ([`REF_ROOT`]). The worktree id comes from
//! the path of the worktree at capture time, and a trash moves the folder,
//! so the refs are found by the session segment (a session id is unique),
//! not by the worktree id. The refs of other sessions are never touched.

use super::checkpoint::REF_ROOT;
use super::git::{Git, GitError};

/// The checkpoint refs of `session` in the repository of `git`.
///
/// # Errors
/// The `git` failure.
pub async fn session_refs(git: &Git, session: &str) -> Result<Vec<String>, GitError> {
    let out = git
        .text(&[
            "for-each-ref",
            "--format=%(refname)",
            &format!("{REF_ROOT}/"),
        ])
        .await?;
    Ok(out
        .lines()
        .filter(|name| of_session(name, session))
        .map(str::to_owned)
        .collect())
}

/// True when `name` is `<REF_ROOT>/<incarnation>/<worktree id>/<session>/
/// <turn>/<phase>` of `session`.
fn of_session(name: &str, session: &str) -> bool {
    let Some(rest) = name
        .strip_prefix(REF_ROOT)
        .and_then(|r| r.strip_prefix('/'))
    else {
        return false;
    };
    let parts: Vec<&str> = rest.split('/').collect();
    parts.len() == 5 && parts[2] == session
}

/// Deletes the checkpoint refs of `session` with `git update-ref -d`, one
/// ref at a time. Returns how many it deleted.
///
/// # Errors
/// The first `git` failure; the refs before it are deleted.
pub async fn delete_session_refs(git: &Git, session: &str) -> Result<usize, GitError> {
    let refs = session_refs(git, session).await?;
    for name in &refs {
        git.run(&["update-ref", "-d", name]).await?;
    }
    Ok(refs.len())
}

#[cfg(test)]
#[path = "checkpoint_refs_tests.rs"]
mod checkpoint_refs_tests;
