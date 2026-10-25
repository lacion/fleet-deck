//! The Git work of a land (FG-8, design §11.5 steps 1 to 4, O24, O26): the
//! capture (fetch, default branch, the commits), the temporary worktree, the
//! rebase with its conflict list, and the proof that a check did not change
//! the tested commit. The run, its journal and its texts are
//! `manager/land.rs`. Every command runs in the hardened runner (§2.4).

use std::path::Path;
use std::time::Duration;

use fleetdeck_proto::fleet_git::{GitSettings, code};

use super::fetch::{default_branch, fetch_remote};
use super::identity::{READ_TIMEOUT, Refusal, net_git, read, read_git};
use super::redact::redact;
use crate::workspace::git::nul_fields;
use crate::workspace::{Git, GitError, GitOptions};

/// The time limit of `git worktree add` and `git rebase` (design §2.4).
const WRITE_TIMEOUT: Duration = Duration::from_secs(120);
/// The time limit of the merge proof (design §2.4).
const MERGED_TIMEOUT: Duration = Duration::from_secs(20);
/// The most bytes of the answer of a small read.
const SMALL_CAP: usize = 4 * 1024;
/// The most bytes of stderr that a write command keeps (design §2.4).
const STDERR_CAP: usize = 2 * 1024;
/// The most bytes of the conflict list (design §2.4).
const CONFLICT_CAP: usize = 1024 * 1024;
/// The most bytes of the status answer (design §2.4).
const STATUS_CAP: usize = 16 * 1024;
/// The most conflict paths that one land reports (design §11.5 step 3).
const MAX_CONFLICT_PATHS: usize = 200;
/// The longest conflict path, in bytes (design §11.5 step 3).
const MAX_PATH_BYTES: usize = 4096;

/// The refusal text of a Git step that failed for another reason (Q12).
fn internal(detail: &str) -> Refusal {
    Refusal::new(
        code::INTERNAL,
        format!("The land failed: {}", redact(detail)),
    )
}

/// What the capture fixed (design §11.5 step 1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Captured {
    pub default_ref: String,
    pub base_oid: String,
    pub source_oid: String,
}

fn is_oid(text: &str) -> bool {
    matches!(text.len(), 40 | 64) && text.bytes().all(|b| b.is_ascii_hexdigit())
}

/// The commit that `rev` names in `git`'s repository; `None` when it is not
/// a commit.
async fn commit_of(git: &Git, rev: &str) -> Result<Option<String>, Refusal> {
    let probe = format!("{rev}^{{commit}}");
    let args = [
        "rev-parse",
        "--verify",
        "--quiet",
        "--end-of-options",
        &probe,
    ];
    match read(git, &args, SMALL_CAP)
        .await
        .map_err(|e| internal(&e))?
    {
        (0, out, _) if is_oid(out.trim()) => Ok(Some(out.trim().to_owned())),
        (0 | 1, _, _) => Ok(None),
        (exit, _, err) => Err(internal(&format!("git rev-parse failed ({exit}): {err}"))),
    }
}

/// Step 1 of a land (design §11.5): the fetch of `remote`, `default_ref`
/// (the setting, else the FG-1 order), `base_oid`, and the `source_oid` of
/// the local session branch `branch_ref` (`known_source` is the one that an
/// earlier attempt fixed). The home check of §3.8 is the caller's, before.
///
/// # Errors
/// A [`Refusal`] with the end code: `git_too_old`, `fetch_failed`,
/// `no_default_branch`, `nothing_to_land`, or `internal`.
pub async fn capture(
    options: &GitOptions,
    repo: &Path,
    remote: &str,
    default_branch_setting: Option<&str>,
    branch_ref: &str,
    known_source: Option<&str>,
) -> Result<Captured, Refusal> {
    fetch_remote(options, repo, remote).await?;
    let local = read_git(options, repo, READ_TIMEOUT);
    let unknown = || {
        Refusal::new(
            code::NO_DEFAULT_BRANCH,
            format!("The default branch of {remote} is unknown."),
        )
    };
    let default_ref = match default_branch_setting {
        Some(name) => name.to_owned(),
        None => default_branch(&local, remote, &GitSettings::default())
            .await
            .ok_or_else(unknown)?,
    };
    let Some(base_oid) = commit_of(&local, &format!("{remote}/{default_ref}")).await? else {
        return Err(Refusal::new(
            code::NO_DEFAULT_BRANCH,
            format!("{remote}/{default_ref} does not exist."),
        ));
    };
    let source_oid = match known_source {
        Some(oid) => oid.to_owned(),
        None => commit_of(&local, &format!("refs/heads/{branch_ref}"))
            .await?
            .ok_or_else(|| internal("the session branch is missing"))?,
    };
    let proof = read_git(options, repo, MERGED_TIMEOUT);
    let args = ["merge-base", "--is-ancestor", &source_oid, &base_oid];
    match read(&proof, &args, SMALL_CAP)
        .await
        .map_err(|e| internal(&e))?
    {
        (0, _, _) => {
            return Err(Refusal::new(
                code::NOTHING_TO_LAND,
                format!("The branch has no commits that are not on {remote}/{default_ref}."),
            ));
        }
        (1, _, _) => {}
        (exit, _, err) => {
            return Err(internal(&format!("git merge-base failed ({exit}): {err}")));
        }
    }
    Ok(Captured {
        default_ref,
        base_oid,
        source_oid,
    })
}

/// What `git worktree add` made (design §11.6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TempFacts {
    pub identity: (u64, u64),
    /// `<common git dir>/worktrees/<name>`.
    pub admin_dir: String,
}

/// Step 2: `git worktree add --detach -- <temp> <source_oid>` in `repo`
/// (120 s, confined), then the identity of the folder and the Git admin
/// entry, which must lie inside `<common>/worktrees/`.
///
/// # Errors
/// An `internal` [`Refusal`].
pub async fn add_worktree(
    options: &GitOptions,
    repo: &Path,
    temp: &Path,
    source_oid: &str,
) -> Result<TempFacts, Refusal> {
    let temp_text = temp.to_string_lossy().into_owned();
    let net = net_git(options, repo, WRITE_TIMEOUT);
    let args = [
        "worktree",
        "add",
        "--detach",
        "--",
        temp_text.as_str(),
        source_oid,
    ];
    match net.status_stderr(&args, &[], STDERR_CAP).await {
        Ok((0, _)) => {}
        Ok((_, stderr)) => return Err(internal(&stderr)),
        Err(e) => return Err(internal(&e.to_string())),
    }
    let identity = super::land_temp::identity(temp)
        .ok_or_else(|| internal("the temporary folder is missing"))?;
    let here = read_git(options, temp, READ_TIMEOUT);
    let (exit, out, err) = read(&here, &["rev-parse", "--absolute-git-dir"], SMALL_CAP)
        .await
        .map_err(|e| internal(&e))?;
    if exit != 0 {
        return Err(internal(&err));
    }
    let admin = std::fs::canonicalize(out.trim()).map_err(|e| internal(&e.to_string()))?;
    let home = read_git(options, repo, READ_TIMEOUT);
    let (exit, out, err) = read(&home, &["rev-parse", "--git-common-dir"], SMALL_CAP)
        .await
        .map_err(|e| internal(&e))?;
    if exit != 0 {
        return Err(internal(&err));
    }
    let common = Path::new(out.trim());
    let common = if common.is_absolute() {
        common.to_path_buf()
    } else {
        repo.join(common)
    };
    let common = std::fs::canonicalize(common).map_err(|e| internal(&e.to_string()))?;
    if !admin.starts_with(common.join("worktrees")) {
        return Err(internal("the Git admin entry is not below worktrees/"));
    }
    Ok(TempFacts {
        identity,
        admin_dir: admin.to_string_lossy().into_owned(),
    })
}

/// True when Git has a committer identity from its configuration or from the
/// environment (`user.useConfigOnly`: no guess from the host name).
pub async fn has_identity(options: &GitOptions, dir: &Path) -> bool {
    let git = read_git(options, dir, READ_TIMEOUT);
    let args = [
        "-c",
        "user.useConfigOnly=true",
        "var",
        "GIT_COMMITTER_IDENT",
    ];
    matches!(read(&git, &args, SMALL_CAP).await, Ok((0, _, _)))
}

/// The result of the rebase (design §11.5 step 3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rebased {
    /// `R`: the commit that `HEAD` has after the rebase.
    Done { rebased_oid: String },
    /// Exit 1 with unmerged paths; the rebase was aborted.
    Conflict { files: Vec<String> },
}

/// A path of `-z` output as text: at most 4096 bytes, and each byte that is
/// not UTF-8 written as `\xNN`.
fn path_text(raw: &[u8]) -> String {
    let mut rest = &raw[..raw.len().min(MAX_PATH_BYTES)];
    let mut out = String::new();
    loop {
        match std::str::from_utf8(rest) {
            Ok(text) => {
                out.push_str(text);
                return out;
            }
            Err(e) => {
                let (good, bad) = rest.split_at(e.valid_up_to());
                out.push_str(&String::from_utf8_lossy(good));
                // A sequence cut by the 4096-byte limit is invalid, too.
                let skip = e.error_len().unwrap_or(bad.len()).min(bad.len());
                for byte in &bad[..skip] {
                    out.push_str(&format!("\\x{byte:02x}"));
                }
                rest = &bad[skip..];
            }
        }
    }
}

/// The conflict list of `git diff --name-only -z --diff-filter=U` output: at
/// most 200 paths (design §11.5 step 3).
#[must_use]
pub fn conflict_files(out: &[u8]) -> Vec<String> {
    nul_fields(out)
        .into_iter()
        .take(MAX_CONFLICT_PATHS)
        .map(path_text)
        .collect()
}

/// Step 3: the rebase of the captured source onto the captured base, by
/// commit id, in the temporary worktree (120 s). A conflict gives the files
/// and runs `git rebase --abort`.
///
/// # Errors
/// An `internal` [`Refusal`] (an exit other than 0 or 1, exit 1 with no
/// unmerged path, a timeout); the rebase is aborted when it stopped.
pub async fn rebase(options: &GitOptions, temp: &Path, base_oid: &str) -> Result<Rebased, Refusal> {
    let net = net_git(options, temp, WRITE_TIMEOUT);
    let args = [
        "-c",
        "rebase.autoStash=false",
        "-c",
        "rebase.updateRefs=false",
        "-c",
        "core.editor=true",
        "rebase",
        "--no-autosquash",
        "--no-fork-point",
        base_oid,
    ];
    let outcome = net.status(&args, &[], None).await;
    let (exit, stderr) = match outcome {
        Ok((0, _, _)) => {
            let here = read_git(options, temp, READ_TIMEOUT);
            let oid = commit_of(&here, "HEAD")
                .await?
                .ok_or_else(|| internal("HEAD is not a commit after the rebase"))?;
            return Ok(Rebased::Done { rebased_oid: oid });
        }
        Ok((exit, _, stderr)) => (exit, stderr),
        Err(GitError::Timeout { .. }) => {
            abort(options, temp).await;
            return Err(internal("the rebase took more than 120 s"));
        }
        Err(e) => {
            abort(options, temp).await;
            return Err(internal(&e.to_string()));
        }
    };
    let files = if exit == 1 {
        let args = ["diff", "--name-only", "-z", "--diff-filter=U"];
        let git = read_git(options, temp, READ_TIMEOUT);
        match git.bounded(&args, CONFLICT_CAP).await {
            Ok((0, out, false)) => conflict_files(&out),
            _ => Vec::new(),
        }
    } else {
        Vec::new()
    };
    abort(options, temp).await;
    if files.is_empty() {
        return Err(internal(&format!("git rebase failed ({exit}): {stderr}")));
    }
    Ok(Rebased::Conflict { files })
}

/// `git rebase --abort`; a failure is logged (the temporary folder is
/// deleted afterwards).
async fn abort(options: &GitOptions, temp: &Path) {
    let git = net_git(options, temp, WRITE_TIMEOUT);
    match git
        .status_stderr(&["rebase", "--abort"], &[], STDERR_CAP)
        .await
    {
        Ok((0, _)) => {}
        Ok((exit, stderr)) => {
            tracing::warn!(exit, "git rebase --abort failed: {}", redact(&stderr));
        }
        Err(e) => tracing::warn!("git rebase --abort failed: {}", redact(&e.to_string())),
    }
}

/// The proof of design §11.5 step 4 (decision 50): after a good check, `HEAD`
/// is `rebased_oid` and `git status --porcelain=v1 -z --untracked-files=no
/// --ignore-submodules=none` prints nothing. `false`: the check changed the
/// tested commit or a tracked file.
///
/// # Errors
/// An `internal` [`Refusal`] when Git could not answer.
pub async fn tested_unchanged(
    options: &GitOptions,
    temp: &Path,
    rebased_oid: &str,
) -> Result<bool, Refusal> {
    let git = read_git(options, temp, READ_TIMEOUT);
    if commit_of(&git, "HEAD").await?.as_deref() != Some(rebased_oid) {
        return Ok(false);
    }
    let args = [
        "status",
        "--porcelain=v1",
        "-z",
        "--untracked-files=no",
        "--ignore-submodules=none",
    ];
    match git.bounded(&args, STATUS_CAP).await {
        Ok((0, out, false)) => Ok(out.is_empty()),
        Ok((0, _, true)) => Ok(false),
        Ok((exit, _, _)) => Err(internal(&format!("git status failed ({exit})"))),
        Err(e) => Err(internal(&e.to_string())),
    }
}

#[cfg(test)]
#[path = "land_tests.rs"]
mod tests;
