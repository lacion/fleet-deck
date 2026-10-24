//! The branches of other stations and the fetch of the radar home (FG-10,
//! design §13.1, O31): `project.branches` lists the `fd/*` branches that the
//! remote holds, and `project.fetch` fetches them. The listing runs no fetch.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use fleetdeck_proto::fleet_git::{
    FetchAnswer, REMOTE_BRANCH_SUBJECT_MAX, REMOTE_BRANCHES_MAX, RemoteBranch, RemoteBranches, code,
};

use super::fetch::{default_branch, fetch_remote};
use super::identity::{READ_TIMEOUT, Refusal, read, read_git};
use super::radar::{RadarEnv, commit_of, fetch_wip};
use super::redact::redact;
use crate::workspace::Git;

/// The time limit of the whole `project.branches` answer (decision H14: the
/// deck waits 20 s).
const ANSWER_TIMEOUT: Duration = Duration::from_secs(18);
/// The most bytes of the listing of the refs (design §2.4).
const LIST_CAP: usize = 512 * 1024;
/// The most bytes of the answer of one `git rev-list --count`.
const COUNT_CAP: usize = 4 * 1024;

/// When the last fetch of each home ended, in Unix milliseconds (decision Q2:
/// kept in memory, lost at a restart).
static FETCHED: Mutex<Option<HashMap<String, i64>>> = Mutex::new(None);

/// Notes that a fetch of the home `repo_dir` ended at `at_ms` (FG-10). The
/// stamp never goes back: a radar fetch that started before a manual fetch
/// can end after it, and its older stamp must not replace the newer one.
pub fn note_fetched(repo_dir: &Path, at_ms: i64) {
    let key = repo_dir.to_string_lossy().into_owned();
    FETCHED
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get_or_insert_with(HashMap::new)
        .entry(key)
        .and_modify(|v| *v = (*v).max(at_ms))
        .or_insert(at_ms);
}

fn fetched_at(repo_dir: &Path) -> Option<i64> {
    let key = repo_dir.to_string_lossy();
    FETCHED
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .as_ref()
        .and_then(|m| m.get(key.as_ref()).copied())
}

/// `project.fetch` (design §13.1): one fetch of `push_remote` and, with
/// `wip_remote`, one fetch of the WIP URL. The first failure ends the run and
/// is the `code` and `text` of the answer; `fetched_at` is then none.
pub async fn fetch_project(env: &RadarEnv<'_>, now_ms: i64) -> FetchAnswer {
    let settings = &env.view.settings;
    let mut failure = fetch_remote(env.options, env.repo_dir, &settings.push_remote)
        .await
        .err();
    if failure.is_none()
        && let Some(url) = &settings.wip_remote
    {
        failure = fetch_wip(env, url).await.err();
    }
    match failure {
        Some(refusal) => FetchAnswer {
            fetched_at: None,
            code: Some(refusal.code.to_owned()),
            text: Some(refusal.text),
        },
        None => {
            note_fetched(env.repo_dir, now_ms);
            FetchAnswer {
                fetched_at: Some(now_ms),
                code: None,
                text: None,
            }
        }
    }
}

/// `project.branches` (design §13.1): the `fd/*` branches of the remote of the
/// home, newest tip first, at most [`REMOTE_BRANCHES_MAX`]. No fetch runs.
///
/// # Errors
/// A [`Refusal`] with the code `internal` when Git cannot list the refs, or
/// when the answer takes more than 18 s.
pub async fn list_branches(env: &RadarEnv<'_>) -> Result<RemoteBranches, Refusal> {
    list_with(env, ANSWER_TIMEOUT).await
}

async fn list_with(env: &RadarEnv<'_>, total: Duration) -> Result<RemoteBranches, Refusal> {
    match tokio::time::timeout(total, list(env)).await {
        Ok(result) => result,
        Err(_) => Err(Refusal::new(
            code::INTERNAL,
            "The branch list took more than 18 s.",
        )),
    }
}

/// One listed ref before the counts.
struct Tip {
    name: String,
    slug: Option<String>,
    callsign: String,
    oid: String,
    at: i64,
    subject: String,
    reference: String,
}

fn internal(detail: &str) -> Refusal {
    Refusal::new(
        code::INTERNAL,
        format!("Fleet Git could not read the branches: {detail}"),
    )
}

/// The commit refs below `prefix` with two or three name components after
/// `fd/`'s parent: `fd/<callsign>` (legacy) and `fd/<slug>/<callsign>`.
fn parse(out: &str, prefix: &str) -> Vec<Tip> {
    let mut tips = Vec::new();
    for line in out.lines() {
        let mut fields = line.splitn(5, '\0');
        let (Some(reference), Some(oid), Some(kind), Some(time), Some(subject)) = (
            fields.next(),
            fields.next(),
            fields.next(),
            fields.next(),
            fields.next(),
        ) else {
            continue;
        };
        let Some(rest) = reference.strip_prefix(prefix) else {
            continue;
        };
        let name = format!("fd/{rest}");
        let parts: Vec<&str> = rest.split('/').collect();
        let (slug, callsign) = match parts.as_slice() {
            [callsign] => (None, (*callsign).to_owned()),
            [slug, callsign] => (Some((*slug).to_owned()), (*callsign).to_owned()),
            _ => continue,
        };
        let Ok(seconds) = time.parse::<i64>() else {
            continue;
        };
        if kind != "commit" {
            continue;
        }
        tips.push(Tip {
            name,
            slug,
            callsign,
            oid: oid.to_owned(),
            at: seconds.saturating_mul(1000),
            subject: subject.chars().take(REMOTE_BRANCH_SUBJECT_MAX).collect(),
            reference: reference.to_owned(),
        });
    }
    tips
}

/// `(ahead, behind)` of `reference` against `base` (`git rev-list
/// --left-right --count base...reference`: left is behind, right is ahead).
async fn counts(git: &Git, base: &str, reference: &str) -> Option<(u32, u32)> {
    let range = format!("{base}...{reference}");
    let args = [
        "rev-list",
        "--left-right",
        "--count",
        "--end-of-options",
        &range,
    ];
    let (exit, out, _) = read(git, &args, COUNT_CAP).await.ok()?;
    if exit != 0 {
        return None;
    }
    let mut numbers = out.split_whitespace().map(str::parse::<u32>);
    let behind = numbers.next()?.ok()?;
    let ahead = numbers.next()?.ok()?;
    Some((ahead, behind))
}

async fn list(env: &RadarEnv<'_>) -> Result<RemoteBranches, Refusal> {
    let settings = &env.view.settings;
    let remote = settings.push_remote.as_str();
    // The listing prefix keeps `fd/` out: the name is rebuilt as `fd/<rest>`.
    let prefix = if settings.wip_remote.is_some() {
        "refs/fleetdeck/wip/fd/".to_owned()
    } else {
        format!("refs/remotes/{remote}/fd/")
    };
    let git = read_git(env.options, env.repo_dir, READ_TIMEOUT);
    let args = [
        "for-each-ref",
        "--format=%(refname)%00%(objectname)%00%(objecttype)%00%(committerdate:unix)%00%(contents:subject)",
        "--",
        &prefix,
    ];
    let (exit, out, err) = read(&git, &args, LIST_CAP)
        .await
        .map_err(|e| internal(&e))?;
    if exit != 0 {
        return Err(internal(&redact(&err)));
    }
    let mut tips = parse(&out, &prefix);
    tips.sort_by(|a, b| b.at.cmp(&a.at).then_with(|| a.name.cmp(&b.name)));
    let truncated = tips.len() > REMOTE_BRANCHES_MAX;
    tips.truncate(REMOTE_BRANCHES_MAX);
    // No default branch resolves before the first fetch (decision Q6).
    let resolved = default_branch(&git, remote, settings).await;
    let default_name = resolved
        .clone()
        .or_else(|| settings.default_branch.clone())
        .unwrap_or_else(|| "main".to_owned());
    let base = format!("refs/remotes/{remote}/{default_name}");
    let default_oid = match resolved {
        Some(_) => commit_of(&git, &base).await,
        None => None,
    };
    let mut branches = Vec::with_capacity(tips.len());
    for tip in tips {
        let (ahead, behind) = if default_oid.is_some() {
            match counts(&git, &base, &tip.reference).await {
                Some(pair) => pair,
                None => {
                    tracing::warn!(branch = %tip.name, "the commits of a branch were not counted");
                    (0, 0)
                }
            }
        } else {
            (0, 0)
        };
        branches.push(RemoteBranch {
            name: tip.name,
            slug: tip.slug,
            callsign: tip.callsign,
            head: tip.oid,
            ahead,
            behind,
            at: tip.at,
            subject: tip.subject,
        });
    }
    Ok(RemoteBranches {
        remote: remote.to_owned(),
        default_branch: default_name,
        default_oid,
        fetched_at: fetched_at(env.repo_dir),
        branches,
        truncated,
    })
}

#[cfg(test)]
#[path = "remote_branches_tests.rs"]
mod tests;
