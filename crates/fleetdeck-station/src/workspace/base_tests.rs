//! The base commit of a session (R1.1) with real `git`.

use std::path::Path;
use std::process::Command;

use super::{at_head, inferred, planned};
use crate::store::WorkspacePlan;
use crate::workspace::GitOptions;

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .current_dir(dir)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}");
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

/// A repository with two commits on `main`; returns the first commit.
fn repo() -> (tempfile::TempDir, std::path::PathBuf, String) {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().canonicalize().unwrap().join("repo");
    std::fs::create_dir(&dir).unwrap();
    git(&dir, &["init", "-q", "-b", "main"]);
    std::fs::write(dir.join("a.txt"), "one\n").unwrap();
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "first"]);
    let first = git(&dir, &["rev-parse", "HEAD"]);
    std::fs::write(dir.join("a.txt"), "two\n").unwrap();
    git(&dir, &["commit", "-q", "-am", "second"]);
    (tmp, dir, first)
}

fn plan(repo: &Path, name: &str, branch: &str, base: Option<&str>) -> WorkspacePlan {
    WorkspacePlan::Worktree {
        repo: repo.display().to_string(),
        path: repo.with_file_name(name).display().to_string(),
        branch: branch.to_owned(),
        base: base.map(str::to_owned),
    }
}

#[tokio::test]
async fn a_start_in_place_has_the_head_and_its_branch() {
    let (_t, dir, _) = repo();
    let o = GitOptions::isolated();
    let head = git(&dir, &["rev-parse", "HEAD"]);
    let b = planned(&o, None, &dir.display().to_string(), None)
        .await
        .unwrap();
    assert_eq!(
        (b.git_ref.as_deref(), b.oid.as_str(), b.inferred),
        (Some("main"), head.as_str(), false)
    );
    git(&dir, &["checkout", "-q", "--detach"]);
    let b = at_head(&o, &dir).await.unwrap();
    assert_eq!(b.git_ref, None);
    assert_eq!(b.oid, head);
}

#[tokio::test]
async fn a_worktree_start_reads_the_base_the_branch_or_head_before_the_worktree_exists() {
    let (_t, dir, first) = repo();
    let o = GitOptions::isolated();
    let head = git(&dir, &["rev-parse", "HEAD"]);
    // A base name.
    let p = plan(&dir, "wt-a", "fd/a", Some(&first));
    let b = planned(&o, Some(&p), "", None).await.unwrap();
    assert_eq!(
        (b.git_ref.as_deref(), b.oid.as_str()),
        (Some(first.as_str()), first.as_str())
    );
    // No base, a new branch: the HEAD of the repository and its branch.
    let p = plan(&dir, "wt-b", "fd/b", None);
    let b = planned(&o, Some(&p), "", None).await.unwrap();
    assert_eq!(
        (b.git_ref.as_deref(), b.oid.as_str()),
        (Some("main"), head.as_str())
    );
    // An existing branch: its tip.
    git(&dir, &["branch", "old", &first]);
    let p = plan(&dir, "wt-c", "old", None);
    let b = planned(&o, Some(&p), "", None).await.unwrap();
    assert_eq!(
        (b.git_ref.as_deref(), b.oid.as_str()),
        (Some("old"), first.as_str())
    );
    // A base that is not a commit gives no base.
    let p = plan(&dir, "wt-d", "fd/d", Some("no-such-ref"));
    assert_eq!(planned(&o, Some(&p), "", None).await, None);
    // A folder that exists already (a repeated effect) gives none.
    std::fs::create_dir(dir.with_file_name("wt-e")).unwrap();
    let p = plan(&dir, "wt-e", "fd/e", None);
    assert_eq!(planned(&o, Some(&p), "", None).await, None);
}

#[tokio::test]
async fn a_directory_without_git_has_no_base() {
    let tmp = tempfile::tempdir().unwrap();
    let o = GitOptions::isolated();
    // The temporary directory may lie inside a repository on a developer
    // machine; a fresh empty repository has no commit, so no base either.
    git(tmp.path(), &["init", "-q", "-b", "main"]);
    assert_eq!(at_head(&o, tmp.path()).await, None);
    assert_eq!(inferred(&o, tmp.path()).await, None);
}

#[tokio::test]
async fn an_inferred_base_is_the_merge_base_with_origin_or_the_head() {
    let (_t, dir, first) = repo();
    let o = GitOptions::isolated();
    let head = git(&dir, &["rev-parse", "HEAD"]);
    // No origin: HEAD.
    let b = inferred(&o, &dir).await.unwrap();
    assert_eq!(
        (b.git_ref.as_deref(), b.oid.as_str(), b.inferred),
        (Some("main"), head.as_str(), true)
    );
    // With `origin/HEAD` at the first commit: the merge base.
    git(&dir, &["update-ref", "refs/remotes/origin/main", &first]);
    git(
        &dir,
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/main",
        ],
    );
    let b = inferred(&o, &dir).await.unwrap();
    assert_eq!(
        (b.git_ref.as_deref(), b.oid.as_str(), b.inferred),
        (Some("origin/HEAD"), first.as_str(), true)
    );
}
