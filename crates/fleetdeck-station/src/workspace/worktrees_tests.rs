//! The worktree inventory (DL-142) with real `git` in temporary
//! repositories.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use fleetdeck_proto::event::SessionState;
use fleetdeck_proto::link::{WorktreeInfo, WorktreeVerdict};
use fleetdeck_proto::session::SessionSummary;

use super::worktrees::{
    Count, NOT_CHECKED, OUTSIDE_ROOTS, count_entries, git_options, list, parse_worktrees,
};
use crate::manager::policy::Policy;
use crate::store::OfflineApprovalPolicy;

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
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

/// A temporary root with the main checkout `repo` (one commit) and the
/// worktree `repo--fd-x` on branch `fd/x`.
struct Fixture {
    root: tempfile::TempDir,
    main: PathBuf,
    wt: PathBuf,
}

fn fixture() -> Fixture {
    let root = tempfile::tempdir().unwrap();
    let base = root.path().canonicalize().unwrap();
    let main = base.join("repo");
    std::fs::create_dir(&main).unwrap();
    git(&main, &["init", "-q", "-b", "main"]);
    std::fs::write(main.join("a.txt"), "one\n").unwrap();
    std::fs::write(main.join(".gitignore"), "target/\n").unwrap();
    git(&main, &["add", "."]);
    git(&main, &["commit", "-q", "-m", "init"]);
    let wt = base.join("repo--fd-x");
    git(
        &main,
        &["worktree", "add", "-q", "-b", "fd/x", wt.to_str().unwrap()],
    );
    Fixture { root, main, wt }
}

fn policy(root: &Path) -> Policy {
    Policy::new(
        vec![root.to_path_buf()],
        1,
        OfflineApprovalPolicy::Wait,
        BTreeMap::new(),
    )
}

fn session(id: &str, path: &Path, state: SessionState) -> SessionSummary {
    let mut s = SessionSummary::new(id.into());
    s.worktree = Some(path.display().to_string());
    s.state = state;
    s
}

async fn one(policy: &Policy, s: SessionSummary) -> WorktreeInfo {
    let mut out = list(vec![s], policy, &git_options(), Duration::from_secs(60))
        .await
        .worktrees;
    assert_eq!(out.len(), 1);
    out.remove(0)
}

#[tokio::test]
async fn a_clean_worktree_is_clean_and_registered() {
    let f = fixture();
    let info = one(
        &policy(f.root.path()),
        session("s1", &f.wt, SessionState::Exited),
    )
    .await;
    assert_eq!(info.verdict, WorktreeVerdict::Clean, "{info:?}");
    assert!(info.exists && info.registered && !info.live);
    assert_eq!(info.dirty, Some(0));
    assert_eq!(info.ignored, Some(0));
    assert_eq!(info.unpushed, Some(0));
    assert_eq!(info.compared_with.as_deref(), Some("main"));
    assert_eq!(info.branch.as_deref(), Some("fd/x"));
    assert_eq!(info.main.as_deref(), Some(f.main.to_str().unwrap()));
    assert_eq!(info.note, None);
    let commit = info.last_commit.unwrap();
    assert_eq!(commit.subject, "init");
    assert!(commit.at > 1_000_000_000_000);
}

#[tokio::test]
async fn an_untracked_file_is_work_also_when_the_repo_hides_untracked_files() {
    let f = fixture();
    git(&f.main, &["config", "status.showUntrackedFiles", "no"]);
    std::fs::write(f.wt.join("new.txt"), "x\n").unwrap();
    std::fs::create_dir(f.wt.join("target")).unwrap();
    std::fs::write(f.wt.join("target/out"), "x\n").unwrap();
    let info = one(
        &policy(f.root.path()),
        session("s1", &f.wt, SessionState::Exited),
    )
    .await;
    assert_eq!(info.verdict, WorktreeVerdict::HasWork, "{info:?}");
    assert_eq!(info.dirty, Some(1));
    assert_eq!(info.ignored, Some(1));
}

#[tokio::test]
async fn a_commit_that_main_does_not_hold_is_unpushed() {
    let f = fixture();
    std::fs::write(f.wt.join("b.txt"), "two\n").unwrap();
    git(&f.wt, &["add", "."]);
    git(&f.wt, &["commit", "-q", "-m", "work"]);
    let info = one(
        &policy(f.root.path()),
        session("s1", &f.wt, SessionState::Exited),
    )
    .await;
    assert_eq!(info.verdict, WorktreeVerdict::HasWork, "{info:?}");
    assert_eq!(info.unpushed, Some(1));
    assert_eq!(info.dirty, Some(0));
    assert_eq!(info.compared_with.as_deref(), Some("main"));
    assert_eq!(info.last_commit.unwrap().subject, "work");
}

#[tokio::test]
async fn with_a_remote_ref_the_commits_are_compared_with_the_remotes() {
    let f = fixture();
    std::fs::write(f.wt.join("b.txt"), "two\n").unwrap();
    git(&f.wt, &["add", "."]);
    git(&f.wt, &["commit", "-q", "-m", "work"]);
    git(&f.main, &["update-ref", "refs/remotes/origin/main", "main"]);
    let p = policy(f.root.path());
    let info = one(&p, session("s1", &f.wt, SessionState::Exited)).await;
    assert_eq!(info.compared_with.as_deref(), Some("remotes"));
    assert_eq!(info.unpushed, Some(1));
    git(&f.wt, &["update-ref", "refs/remotes/origin/fd/x", "HEAD"]);
    let info = one(&p, session("s1", &f.wt, SessionState::Exited)).await;
    assert_eq!(info.unpushed, Some(0));
    assert_eq!(info.verdict, WorktreeVerdict::Clean, "{info:?}");
}

#[tokio::test]
async fn a_removed_directory_is_missing() {
    let f = fixture();
    std::fs::remove_dir_all(&f.wt).unwrap();
    let info = one(
        &policy(f.root.path()),
        session("s1", &f.wt, SessionState::Exited),
    )
    .await;
    assert_eq!(info.verdict, WorktreeVerdict::Missing);
    assert!(!info.exists);
    assert_eq!(info.branch, None);
    assert_eq!(info.dirty, None);
}

#[tokio::test]
async fn a_path_outside_the_roots_is_unknown_and_never_given_to_git() {
    let f = fixture();
    let other = tempfile::tempdir().unwrap();
    let info = one(
        &policy(other.path()),
        session("s1", &f.wt, SessionState::Exited),
    )
    .await;
    assert_eq!(info.verdict, WorktreeVerdict::Unknown);
    assert_eq!(info.note.as_deref(), Some(OUTSIDE_ROOTS));
    assert!(info.exists);
    assert_eq!((info.branch, info.dirty, info.unpushed), (None, None, None));
}

#[tokio::test]
async fn a_live_session_is_live_even_with_work() {
    let f = fixture();
    std::fs::write(f.wt.join("new.txt"), "x\n").unwrap();
    let info = one(
        &policy(f.root.path()),
        session("s1", &f.wt, SessionState::Running),
    )
    .await;
    assert_eq!(info.verdict, WorktreeVerdict::Live);
    assert!(info.live);
    assert_eq!(info.dirty, Some(1));
}

#[tokio::test]
async fn a_directory_that_git_does_not_know_is_unknown() {
    let f = fixture();
    let plain = f.root.path().canonicalize().unwrap().join("plain");
    std::fs::create_dir(&plain).unwrap();
    let info = one(
        &policy(f.root.path()),
        session("s1", &plain, SessionState::Exited),
    )
    .await;
    assert_eq!(info.verdict, WorktreeVerdict::Unknown, "{info:?}");
    assert!(!info.registered);
    assert!(info.note.is_some());
}

#[tokio::test]
async fn sessions_without_a_worktree_are_left_out_and_the_newest_comes_first() {
    let f = fixture();
    let mut old = session("s1", &f.wt, SessionState::Exited);
    old.updated_at = 10;
    let mut new = session("s2", &f.wt, SessionState::Exited);
    new.updated_at = 20;
    let mut none = SessionSummary::new("s3".into());
    none.updated_at = 30;
    let out = list(
        vec![old, none, new],
        &policy(f.root.path()),
        &git_options(),
        Duration::from_secs(60),
    )
    .await;
    let ids: Vec<_> = out
        .worktrees
        .iter()
        .map(|w| w.session_id.as_str())
        .collect();
    assert_eq!(ids, ["s2", "s1"]);
}

#[tokio::test]
async fn a_passed_deadline_leaves_every_worktree_unchecked() {
    let f = fixture();
    let out = list(
        vec![
            session("s1", &f.wt, SessionState::Exited),
            session("s2", &f.wt, SessionState::Exited),
        ],
        &policy(f.root.path()),
        &git_options(),
        Duration::ZERO,
    )
    .await;
    assert_eq!(out.worktrees.len(), 2);
    for w in out.worktrees {
        assert_eq!(w.verdict, WorktreeVerdict::Unknown);
        assert_eq!(w.note.as_deref(), Some(NOT_CHECKED));
    }
}

#[test]
fn the_status_count_skips_the_second_path_of_a_rename() {
    let text = "R  new.txt\0old.txt\0 M a.txt\0?? b.txt\0!! target/\0";
    assert_eq!(count_entries(text, Count::Changed), 3);
    assert_eq!(count_entries(text, Count::Ignored), 1);
    assert_eq!(count_entries("", Count::Changed), 0);
}

#[test]
fn the_porcelain_list_gives_the_main_checkout_first() {
    let text = "worktree /r/repo\nHEAD aaa\nbranch refs/heads/main\n\nworktree /r/repo--fd-x\nHEAD bbb\nbranch refs/heads/fd/x\n";
    let entries = parse_worktrees(text);
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].path, "/r/repo");
    assert_eq!(entries[0].head.as_deref(), Some("aaa"));
}
