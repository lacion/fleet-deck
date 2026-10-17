//! The Git work of the target of a move (FG-9, design §12.3 step 4, §12.5):
//! the publication check and the fetch of the exact ref (M8), the local
//! branch and the worktree, and the publication ref under the own slug (M9).
//! The `MoveIn` effect (T4.2d) calls these functions in this order:
//! [`check_publication`], [`fetch_publication`], [`add_move_worktree`],
//! [`pick_publish_ref`], then the publication transaction of the store.
//!
//! The target never uses a URL from the source: the URL is its own verified
//! URL (design §3.8). Every value that came from the source is checked again
//! here.

use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicU32;
use std::time::Duration;

use fleetdeck_proto::fleet_git::{MovePublication, code};

use super::fetch::namespace_check;
use super::identity::{
    PushDestination, READ_TIMEOUT, Refusal, VerifiedUrl, effective_push_url, net_git, read,
    read_git, require_git,
};
use super::radar::branch_name_ok;
use super::redact::redact;
use crate::workspace::{GitError, GitOptions};

/// The prefix of the private ref that holds the fetched publication.
pub const PRIVATE_REF_PREFIX: &str = "refs/fleetdeck/move/";
/// The time limit of the fetch (design §12.3 step 4, M8).
const FETCH_TIMEOUT: Duration = Duration::from_secs(30);
/// The time limit of `git worktree add`.
const WORKTREE_TIMEOUT: Duration = Duration::from_secs(120);
/// The time limit of the `ls-remote` of the publication ref (M9).
const LIST_TIMEOUT: Duration = Duration::from_secs(20);
/// The most bytes of the `ls-remote` answer (M9).
const LIST_CAP: usize = 64 * 1024;
/// The most bytes of stderr that a call keeps.
const STDERR_CAP: usize = 2 * 1024;
/// The most bytes of the answer of a small read.
const SMALL_CAP: usize = 4 * 1024;
/// The last suffix of a publication ref: `…-99`.
const MAX_SUFFIX: u32 = 99;

/// The private ref of a move: `refs/fleetdeck/move/<move_id>`.
#[must_use]
pub fn private_ref(move_id: &str) -> String {
    format!("{PRIVATE_REF_PREFIX}{move_id}")
}

fn invalid(what: &str) -> Refusal {
    Refusal::new(
        code::INVALID_PARAMS,
        format!("The publication of the move is not valid: {what}."),
    )
}

fn move_id_ok(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
}

/// Is `text` a full object id (40 or 64 lower-case hex characters)?
#[must_use]
pub fn oid_valid(text: &str) -> bool {
    matches!(text.len(), 40 | 64) && text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Checks the values that came from the source (design §12.3 step 4): `B` and
/// `ref_name` are plain `fd/<slug>/<name>` branches, the object id is a full
/// hex id, and the destination has a sane size.
///
/// # Errors
/// An `invalid_params` [`Refusal`].
pub fn check_publication_values(publication: &MovePublication) -> Result<(), Refusal> {
    if !branch_name_ok(&publication.local_branch) {
        return Err(invalid("the branch name"));
    }
    if !branch_name_ok(&publication.ref_name) {
        return Err(invalid("the ref name"));
    }
    if !oid_valid(&publication.oid) {
        return Err(invalid("the commit id"));
    }
    if publication.dest.is_empty() || publication.dest.len() > 1024 {
        return Err(invalid("the destination"));
    }
    Ok(())
}

/// The publication check (M8): the target's own verified URL `U` for the
/// project (`wip_remote` or `push_remote`, design §3.8) must have the same
/// destination as the source's publication. `own_dest` is the destination of
/// the project (or of the `wip_remote` URL).
///
/// # Errors
/// `move_publication_mismatch` when the destinations differ, an
/// `invalid_params` for a bad value, or the [`Refusal`] of
/// `effective_push_url`.
pub async fn check_publication(
    options: &GitOptions,
    home: &Path,
    destination: &PushDestination,
    own_dest: &str,
    publication: &MovePublication,
    allow_local_remotes: bool,
    station: &str,
) -> Result<VerifiedUrl, Refusal> {
    check_publication_values(publication)?;
    let url = effective_push_url(options, home, destination, own_dest, allow_local_remotes).await?;
    if url.dest != publication.dest {
        return Err(Refusal::new(
            code::MOVE_PUBLICATION_MISMATCH,
            format!(
                "The branch is on {}, but {station} pushes this project to {}.",
                publication.dest, url.dest
            ),
        ));
    }
    Ok(url)
}

/// Fetches the exact ref of the publication from `url` into the private ref
/// (M8): `git fetch --no-tags --no-recurse-submodules --no-write-fetch-head
/// -- <U> +refs/heads/<ref_name>:refs/fleetdeck/move/<move_id>` (30 s,
/// confined). The private ref must then be the commit `publication.oid`.
///
/// # Errors
/// `git_too_old`, `fetch_failed`, `fetch_timeout` (the redacted Git line),
/// `move_branch_moved`, or `invalid_params`.
pub async fn fetch_publication(
    options: &GitOptions,
    group: &Arc<AtomicU32>,
    home: &Path,
    url: &str,
    publication: &MovePublication,
    move_id: &str,
) -> Result<(), Refusal> {
    check_publication_values(publication)?;
    if !move_id_ok(move_id) {
        return Err(invalid("the move id"));
    }
    require_git(options).await?;
    let private = private_ref(move_id);
    let refspec = format!("+refs/heads/{}:{private}", publication.ref_name);
    let net = net_git(options, home, FETCH_TIMEOUT).with_group(Arc::clone(group));
    let args = [
        "-c",
        "protocol.ext.allow=never",
        "fetch",
        "--no-tags",
        "--no-recurse-submodules",
        "--no-write-fetch-head",
        "--",
        url,
        refspec.as_str(),
    ];
    match net.status_stderr(&args, &[], STDERR_CAP).await {
        Ok((0, _)) => {}
        Ok((_, stderr)) => {
            return Err(Refusal::new(
                code::FETCH_FAILED,
                format!("The fetch of the branch failed: {}", redact(&stderr)),
            ));
        }
        Err(GitError::Timeout { .. }) => {
            return Err(Refusal::new(
                code::FETCH_TIMEOUT,
                "The fetch of the branch took more than 30 s.",
            ));
        }
        Err(e) => {
            return Err(Refusal::new(
                code::FETCH_FAILED,
                format!("The fetch of the branch failed: {}", redact(&e.to_string())),
            ));
        }
    }
    let local = read_git(options, home, READ_TIMEOUT);
    let probe = format!("{private}^{{commit}}");
    let args = [
        "rev-parse",
        "--verify",
        "--quiet",
        "--end-of-options",
        &probe,
    ];
    match read(&local, &args, SMALL_CAP).await {
        Ok((0, out, _)) if out.trim() == publication.oid => Ok(()),
        Ok((0, _, _)) => Err(Refusal::new(
            code::MOVE_BRANCH_MOVED,
            "The branch changed on the remote after the hold. Try again.",
        )),
        Ok((_, _, err)) => Err(Refusal::new(
            code::MOVE_BRANCH_MOVED,
            format!(
                "The branch changed on the remote after the hold. Try again. {}",
                redact(&err)
            ),
        )),
        Err(detail) => Err(Refusal::new(
            code::INTERNAL,
            format!("Fleet Git could not read the fetched branch: {detail}"),
        )),
    }
}

/// The local branch and the worktree (M9): the namespace check of design §3.2
/// item 1 for `B`, a refusal when `B` exists in the home
/// (`move_branch_exists`), then `git worktree add -b <B> -- <path> <oid>` and
/// `git update-ref -d <private ref> <oid>`. `remote` is the remote name of the
/// home (`push_remote`), `station` the name of this station. `group` receives
/// the process group of the Git call, so that a cancel can stop it.
///
/// # Errors
/// `branch_namespace_blocked`, `move_branch_exists`, `invalid_params`, or
/// `internal` (the redacted Git line).
#[allow(clippy::too_many_arguments)]
pub async fn add_move_worktree(
    options: &GitOptions,
    group: &Arc<AtomicU32>,
    home: &Path,
    path: &Path,
    publication: &MovePublication,
    move_id: &str,
    station: &str,
    remote: &str,
) -> Result<(), Refusal> {
    check_publication_values(publication)?;
    if !move_id_ok(move_id) {
        return Err(invalid("the move id"));
    }
    require_git(options).await?;
    let branch = publication.local_branch.as_str();
    // `fd/<slug>/<callsign...>`: the slug is the second part.
    let mut parts = branch.splitn(3, '/');
    let (_, slug, callsign) = (
        parts.next(),
        parts.next().unwrap_or_default(),
        parts.next().unwrap_or_default(),
    );
    let local = read_git(options, home, READ_TIMEOUT);
    namespace_check(&local, remote, slug, callsign).await?;
    let exists = read(
        &local,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ],
        SMALL_CAP,
    )
    .await;
    match exists {
        Ok((0, _, _)) => {
            return Err(Refusal::new(
                code::MOVE_BRANCH_EXISTS,
                format!(
                    "The branch {branch} already exists in the home on {station}. Trash its old worktree and delete the branch there, then move again."
                ),
            ));
        }
        Ok((1, _, _)) => {}
        Ok((_, _, err)) => return Err(internal(&err)),
        Err(detail) => return Err(internal(&detail)),
    }
    let path_text = path.to_string_lossy().into_owned();
    let net = net_git(options, home, WORKTREE_TIMEOUT).with_group(Arc::clone(group));
    let args = [
        "worktree",
        "add",
        "-b",
        branch,
        "--",
        path_text.as_str(),
        publication.oid.as_str(),
    ];
    match net.status_stderr(&args, &[], STDERR_CAP).await {
        Ok((0, _)) => {}
        Ok((_, stderr)) => return Err(internal(&stderr)),
        Err(e) => return Err(internal(&e.to_string())),
    }
    let private = private_ref(move_id);
    let args = [
        "update-ref",
        "-d",
        private.as_str(),
        publication.oid.as_str(),
    ];
    match read(&local, &args, SMALL_CAP).await {
        Ok((0, _, _)) => Ok(()),
        Ok((_, _, err)) => Err(internal(&err)),
        Err(detail) => Err(internal(&detail)),
    }
}

fn internal(detail: &str) -> Refusal {
    Refusal::new(
        code::INTERNAL,
        format!("Fleet Git could not make the worktree: {}", redact(detail)),
    )
}

/// Is `candidate` taken? A remote ref is `refs/heads/<candidate>` or below
/// `refs/heads/<candidate>/`; a local push row holds the name when `used` has
/// it.
fn taken(candidate: &str, listed: &[String], used: &HashSet<String>) -> bool {
    if used.contains(candidate) {
        return true;
    }
    let exact = format!("refs/heads/{candidate}");
    let below = format!("{exact}/");
    listed
        .iter()
        .any(|name| *name == exact || name.starts_with(&below))
}

/// The publication ref (M9): the first free name of `fd/<slug>/<callsign>`,
/// `…-2`, `…-3`, … `…-99`. `url` is the verified URL, `used` holds the
/// `git_pushes.ref` values of the same destination. The remote is listed with
/// `git ls-remote --refs -- <U> 'refs/heads/fd/<slug>/*'` (20 s, 64 KiB).
///
/// # Errors
/// `fetch_failed` (the list failed, timed out or is too long: a cut list could
/// hide a used name), `move_ref_exhausted` when all 99 names are taken, or
/// `invalid_params`.
pub async fn pick_publish_ref(
    options: &GitOptions,
    group: &Arc<AtomicU32>,
    home: &Path,
    url: &str,
    slug: &str,
    callsign: &str,
    used: &HashSet<String>,
) -> Result<String, Refusal> {
    if slug.is_empty()
        || callsign.is_empty()
        || !slug
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        || !callsign
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    {
        return Err(invalid("the slug or the callsign"));
    }
    require_git(options).await?;
    let pattern = format!("refs/heads/fd/{slug}/*");
    let git = net_git(options, home, LIST_TIMEOUT).with_group(Arc::clone(group));
    let args = [
        "-c",
        "protocol.ext.allow=never",
        "ls-remote",
        "--refs",
        "--",
        url,
        pattern.as_str(),
    ];
    let (exit, out, err) = match read(&git, &args, LIST_CAP).await {
        Ok(answer) => answer,
        Err(detail) => {
            let text = if detail.contains("too much text") {
                "The remote branch list is too long.".to_owned()
            } else {
                format!("The remote branch list failed: {detail}")
            };
            return Err(Refusal::new(code::FETCH_FAILED, text));
        }
    };
    if exit != 0 {
        return Err(Refusal::new(
            code::FETCH_FAILED,
            format!("The remote branch list failed: {}", redact(&err)),
        ));
    }
    let listed: Vec<String> = out
        .lines()
        .filter_map(|line| line.split_once('\t').map(|(_, name)| name.to_owned()))
        .collect();
    let base = format!("fd/{slug}/{callsign}");
    if !taken(&base, &listed, used) {
        return Ok(base);
    }
    for n in 2..=MAX_SUFFIX {
        let candidate = format!("{base}-{n}");
        if !taken(&candidate, &listed, used) {
            return Ok(candidate);
        }
    }
    Err(Refusal::new(
        code::MOVE_REF_EXHAUSTED,
        format!("The names {base} to {base}-{MAX_SUFFIX} are all taken on the remote."),
    ))
}

#[cfg(test)]
#[path = "move_fetch_tests.rs"]
mod tests;
