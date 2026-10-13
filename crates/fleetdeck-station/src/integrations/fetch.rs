//! The remotes of a repository and `prs.fetch_head` (R3.7, R3.10, DL-164).
//!
//! A review session works on the real head of a pull request or merge
//! request, also for a fork. The station fetches the forge's own head ref
//! (`refs/pull/<n>/head` on GitHub, `refs/merge-requests/<n>/head` on
//! GitLab) into `refs/fleetdeck/pr/<n>`, with its own Git credentials and
//! without a prompt.

use std::path::Path;
use std::time::Duration;

use fleetdeck_proto::integrations::{ForgeBinding, ForgeKind, PrFetchHead, PrHead, code};

use super::exec::{Fail, MESSAGE_CHARS};
use crate::workspace::provision::scrub;
use crate::workspace::{Git, GitError, GitOptions};

/// The time limit of the fetch.
pub const FETCH_TIMEOUT: Duration = Duration::from_secs(60);

/// True when `url` names the project of `forge`: the same host (without a
/// port) and the same path, without a leading `/` or a trailing `.git`,
/// compared without case. The forms are `git@host:group/proj`,
/// `ssh://[user@]host[:port]/group/proj`, and `http(s)://[user@]host[:port]/group/proj`.
#[must_use]
pub fn remote_names_project(url: &str, forge: &ForgeBinding) -> bool {
    let Some(named) = fleetdeck_core::git::normalize_remote(url) else {
        return false;
    };
    let host = forge.host.split(':').next().unwrap_or("");
    named.eq_ignore_ascii_case(&format!("{host}/{}", forge.repo))
}

fn git(cwd: &Path, isolated: bool) -> Git {
    Git::new(
        cwd,
        GitOptions {
            program: "git".into(),
            timeout: FETCH_TIMEOUT,
            isolated,
        },
    )
}

fn failed(e: &GitError) -> Fail {
    match e {
        GitError::Timeout { .. } => Fail::new(code::TIMEOUT, scrub(&e.to_string())),
        _ => Fail::new(code::FAILED, scrub(&e.to_string())),
    }
}

/// The `(name, url)` of every remote URL of the repository at `cwd`.
///
/// # Errors
/// [`Fail`] when `git` does not run.
pub async fn remote_urls(cwd: &Path) -> Result<Vec<(String, String)>, Fail> {
    let (exit, out, stderr) = git(cwd, true)
        .status(&["config", "--get-regexp", r"^remote\..*\.url$"], &[], None)
        .await
        .map_err(|e| failed(&e))?;
    match exit {
        // Exit code 1: no remote.
        0 | 1 => {}
        _ => return Err(Fail::new(code::FAILED, last_line(&stderr))),
    }
    Ok(String::from_utf8_lossy(&out)
        .lines()
        .filter_map(|line| {
            let (key, url) = line.split_once(' ')?;
            let name = key.strip_prefix("remote.")?.strip_suffix(".url")?;
            Some((name.to_owned(), url.trim().to_owned()))
        })
        .collect())
}

/// The last line of `git` stderr, scrubbed and cut.
fn last_line(stderr: &str) -> String {
    let line = stderr
        .lines()
        .rfind(|l| !l.trim().is_empty())
        .unwrap_or("git failed");
    scrub(line).chars().take(MESSAGE_CHARS).collect()
}

/// The remote whose URL names the project, else `origin`.
async fn remote_of(cwd: &Path, forge: &ForgeBinding) -> Result<String, Fail> {
    let remotes = remote_urls(cwd).await?;
    if let Some((name, _)) = remotes
        .iter()
        .find(|(_, url)| remote_names_project(url, forge))
    {
        return Ok(name.clone());
    }
    if remotes.iter().any(|(name, _)| name == "origin") {
        return Ok("origin".to_owned());
    }
    Err(Fail::new(
        code::INVALID_PARAMS,
        format!(
            "no remote of this repository names {}/{}",
            forge.host, forge.repo
        ),
    ))
}

/// Fetches the head of the pull request into `refs/fleetdeck/pr/<number>`
/// of the repository at `cwd` and names the commit.
///
/// # Errors
/// `invalid_params` when no remote fits; `timeout`; `failed` with the last
/// line of the `git` error.
pub async fn fetch_head(q: &PrFetchHead, cwd: &Path) -> Result<PrHead, Fail> {
    let remote = remote_of(cwd, &q.forge).await?;
    let src = match q.forge.kind {
        ForgeKind::Github => format!("refs/pull/{}/head", q.number),
        ForgeKind::Gitlab => format!("refs/merge-requests/{}/head", q.number),
    };
    let ref_name = format!("refs/fleetdeck/pr/{}", q.number);
    let refspec = format!("+{src}:{ref_name}");
    // Like a clone, the fetch needs the owner's credential helpers and SSH
    // settings, and it must never prompt.
    let git = git(cwd, false);
    let (exit, _, stderr) = git
        .status(
            &[
                "-c",
                "protocol.ext.allow=never",
                "-c",
                "core.sshCommand=ssh -o BatchMode=yes -o ConnectTimeout=10",
                "fetch",
                "--no-tags",
                "--no-write-fetch-head",
                "--",
                &remote,
                &refspec,
            ],
            &[],
            None,
        )
        .await
        .map_err(|e| failed(&e))?;
    if exit != 0 {
        return Err(Fail::new(code::FAILED, last_line(&stderr)));
    }
    let commit = format!("{ref_name}^{{commit}}");
    let oid = git
        .text(&["rev-parse", "--verify", &commit])
        .await
        .map_err(|e| failed(&e))?;
    let oid = oid.trim();
    let hex = oid.bytes().all(|b| b.is_ascii_hexdigit());
    if !hex || !(oid.len() == 40 || oid.len() == 64) {
        return Err(Fail::bad_answer("git gave no commit id for the head"));
    }
    Ok(PrHead {
        ref_name,
        oid: oid.to_owned(),
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use std::path::PathBuf;
    use std::process::Command;

    use super::*;

    fn run(dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .current_dir(dir)
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(args)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .output()
            .unwrap();
        assert!(out.status.success(), "{args:?}: {out:?}");
        String::from_utf8(out.stdout).unwrap().trim().to_owned()
    }

    fn forge(kind: ForgeKind, host: &str, repo: &str) -> ForgeBinding {
        ForgeBinding {
            kind,
            host: host.into(),
            repo: repo.into(),
        }
    }

    /// A bare repository with `ref_name` on one commit, and a clone of it
    /// whose `origin` is the bare repository. Returns `(clone, oid)`.
    fn fixture(root: &Path, ref_name: &str) -> (PathBuf, String) {
        let work = root.join("work");
        std::fs::create_dir(&work).unwrap();
        run(&work, &["init", "-q"]);
        std::fs::write(work.join("a.txt"), "one\n").unwrap();
        run(&work, &["add", "a.txt"]);
        run(&work, &["commit", "-qm", "one"]);
        let oid = run(&work, &["rev-parse", "HEAD"]);
        let bare = root.join("up.git");
        std::fs::create_dir(&bare).unwrap();
        run(&bare, &["init", "-q", "--bare"]);
        run(
            &work,
            &[
                "push",
                "-q",
                bare.to_str().unwrap(),
                &format!("HEAD:{ref_name}"),
            ],
        );
        let clone = root.join("clone");
        std::fs::create_dir(&clone).unwrap();
        run(&clone, &["init", "-q"]);
        run(&clone, &["remote", "add", "origin", bare.to_str().unwrap()]);
        (clone, oid)
    }

    fn query(forge: ForgeBinding, number: u64, path: &Path) -> PrFetchHead {
        PrFetchHead {
            forge,
            number,
            path: path.to_string_lossy().into_owned(),
        }
    }

    #[test]
    fn a_remote_url_names_the_project_in_every_form() {
        let gl = forge(ForgeKind::Gitlab, "gitlab.com", "group/sub/proj");
        for url in [
            "git@gitlab.com:group/sub/proj.git",
            "git@gitlab.com:group/sub/proj",
            "ssh://git@gitlab.com:2222/group/sub/proj.git",
            "ssh://gitlab.com/group/sub/proj",
            "https://gitlab.com/group/sub/proj.git",
            "https://oauth2@gitlab.com:8443/group/sub/proj",
            "http://gitlab.com/group/sub/proj.git",
            "git@GitLab.com:Group/Sub/Proj.git",
        ] {
            assert!(remote_names_project(url, &gl), "{url}");
        }
        for url in [
            "git@gitlab.example:group/sub/proj.git",
            "git@gitlab.com:group/sub/other.git",
            "git@gitlab.com:group/proj.git",
            "https://gitlab.com/group/sub/proj/extra",
            "/srv/git/group/sub/proj.git",
            "file:///srv/git/group/sub/proj.git",
            "",
        ] {
            assert!(!remote_names_project(url, &gl), "{url}");
        }
        // The port of the binding is ignored too.
        let with_port = forge(ForgeKind::Github, "github.com:443", "Acme/App");
        assert!(remote_names_project(
            "git@github.com:acme/app.git",
            &with_port
        ));
    }

    #[tokio::test]
    async fn the_head_of_a_pull_request_is_fetched_into_a_local_ref() {
        let tmp = tempfile::tempdir().unwrap();
        let (clone, oid) = fixture(tmp.path(), "refs/pull/7/head");
        let q = query(
            forge(ForgeKind::Github, "github.com", "acme/app"),
            7,
            &clone,
        );
        let head = fetch_head(&q, &clone).await.unwrap();
        assert_eq!(head.ref_name, "refs/fleetdeck/pr/7");
        assert_eq!(head.oid, oid);
        assert_eq!(run(&clone, &["rev-parse", "refs/fleetdeck/pr/7"]), oid);
    }

    #[tokio::test]
    async fn a_merge_request_head_comes_from_the_gitlab_ref() {
        let tmp = tempfile::tempdir().unwrap();
        let (clone, oid) = fixture(tmp.path(), "refs/merge-requests/3/head");
        let q = query(
            forge(ForgeKind::Gitlab, "gitlab.com", "group/sub/proj"),
            3,
            &clone,
        );
        assert_eq!(fetch_head(&q, &clone).await.unwrap().oid, oid);
    }

    #[tokio::test]
    async fn a_missing_pull_request_is_a_failed_fetch() {
        let tmp = tempfile::tempdir().unwrap();
        let (clone, _) = fixture(tmp.path(), "refs/pull/7/head");
        let q = query(
            forge(ForgeKind::Github, "github.com", "acme/app"),
            9,
            &clone,
        );
        let err = fetch_head(&q, &clone).await.unwrap_err();
        assert_eq!(err.code, code::FAILED, "{}", err.message);
        assert!(err.message.contains("refs/pull/9/head"), "{}", err.message);
        assert!(!err.message.contains('\n'));
    }

    #[tokio::test]
    async fn the_remote_that_names_the_project_wins_and_origin_is_the_fallback() {
        let tmp = tempfile::tempdir().unwrap();
        let (clone, _) = fixture(tmp.path(), "refs/pull/7/head");
        let gh = forge(ForgeKind::Github, "github.com", "acme/app");
        assert_eq!(remote_of(&clone, &gh).await.unwrap(), "origin");
        run(
            &clone,
            &["remote", "add", "up", "git@github.com:Acme/App.git"],
        );
        assert_eq!(remote_of(&clone, &gh).await.unwrap(), "up");
        run(&clone, &["remote", "remove", "origin"]);
        let other = forge(ForgeKind::Github, "github.com", "acme/other");
        let err = remote_of(&clone, &other).await.unwrap_err();
        assert_eq!(err.code, code::INVALID_PARAMS);
        assert_eq!(
            err.message,
            "no remote of this repository names github.com/acme/other"
        );
    }

    #[tokio::test]
    async fn a_repository_without_a_remote_has_no_urls() {
        let tmp = tempfile::tempdir().unwrap();
        run(tmp.path(), &["init", "-q"]);
        assert!(remote_urls(tmp.path()).await.unwrap().is_empty());
    }
}
