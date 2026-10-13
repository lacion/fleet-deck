//! The worktree inventory (DL-142) with real `git` in temporary
//! repositories.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use fleetdeck_proto::event::SessionState;
use fleetdeck_proto::link::{WorktreeInfo, WorktreeVerdict};
use fleetdeck_proto::session::SessionSummary;

use super::worktrees::scan::{blob_oid, parse_debug, scan};
use super::worktrees::{NOT_CHECKED, OUTSIDE_ROOTS, git_options, list, parse_worktrees};
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
    let mut out = list(
        vec![s],
        &[],
        policy,
        &git_options(),
        Duration::from_secs(60),
    )
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

/// A script in `dir` that writes the marker file `ran` next to it.
fn marker_script(dir: &Path) -> (PathBuf, PathBuf) {
    use std::os::unix::fs::PermissionsExt;
    let marker = dir.join("ran");
    let script = dir.join("probe.sh");
    std::fs::write(
        &script,
        format!("#!/bin/sh\ntouch '{}'\ncat\n", marker.display()),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    (script, marker)
}

#[tokio::test]
async fn a_repository_with_git_filters_is_inspected_without_running_them() {
    let f = fixture();
    let tools = tempfile::tempdir().unwrap();
    let (script, marker) = marker_script(tools.path());
    std::fs::write(f.main.join(".gitattributes"), "* filter=x\n").unwrap();
    git(&f.main, &["add", ".gitattributes"]);
    git(&f.main, &["commit", "-q", "-m", "attributes"]);
    git(&f.wt, &["reset", "-q", "--hard", "main"]);
    for key in ["clean", "smudge", "process"] {
        git(
            &f.main,
            &[
                "config",
                &format!("filter.x.{key}"),
                script.to_str().unwrap(),
            ],
        );
    }
    let p = policy(f.root.path());
    let info = one(&p, session("s1", &f.wt, SessionState::Exited)).await;
    assert_eq!(info.verdict, WorktreeVerdict::Clean, "{info:?}");
    std::fs::write(f.wt.join("a.txt"), "changed\n").unwrap();
    let info = one(&p, session("s1", &f.wt, SessionState::Exited)).await;
    assert_eq!(info.verdict, WorktreeVerdict::HasWork, "{info:?}");
    assert_eq!(info.dirty, Some(1));
    assert!(!marker.exists(), "a filter ran");
}

#[tokio::test]
async fn a_configured_fsmonitor_command_does_not_run() {
    let f = fixture();
    let tools = tempfile::tempdir().unwrap();
    let (script, marker) = marker_script(tools.path());
    git(
        &f.main,
        &["config", "core.fsmonitor", script.to_str().unwrap()],
    );
    std::fs::write(f.wt.join("a.txt"), "changed\n").unwrap();
    let info = one(
        &policy(f.root.path()),
        session("s1", &f.wt, SessionState::Exited),
    )
    .await;
    assert_eq!(info.verdict, WorktreeVerdict::HasWork, "{info:?}");
    assert_eq!(info.dirty, Some(1));
    assert!(!marker.exists(), "the fsmonitor command ran");
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
        &[],
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
        &[],
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
fn the_porcelain_list_gives_the_main_checkout_first() {
    let text = "worktree /r/repo\nHEAD aaa\nbranch refs/heads/main\n\nworktree /r/repo--fd-x\nHEAD bbb\nbranch refs/heads/fd/x\n";
    let entries = parse_worktrees(text);
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].path, "/r/repo");
    assert_eq!(entries[0].head.as_deref(), Some("aaa"));
}

async fn dirty_of(f: &Fixture) -> WorktreeInfo {
    one(
        &policy(f.root.path()),
        session("s1", &f.wt, SessionState::Exited),
    )
    .await
}

#[tokio::test]
async fn a_staged_edit_and_a_staged_deletion_are_work() {
    let f = fixture();
    std::fs::write(f.wt.join("a.txt"), "changed\n").unwrap();
    git(&f.wt, &["add", "a.txt"]);
    let info = dirty_of(&f).await;
    assert_eq!(info.verdict, WorktreeVerdict::HasWork, "{info:?}");
    assert_eq!(info.dirty, Some(1));
    git(&f.wt, &["reset", "-q", "--hard"]);
    assert_eq!(dirty_of(&f).await.verdict, WorktreeVerdict::Clean);
    git(&f.wt, &["rm", "-q", "a.txt"]);
    let info = dirty_of(&f).await;
    assert_eq!(info.verdict, WorktreeVerdict::HasWork, "{info:?}");
    assert_eq!(info.dirty, Some(1));
}

#[tokio::test]
async fn an_equal_size_rewrite_with_the_old_mtime_is_work() {
    let f = fixture();
    let file = f.wt.join("a.txt");
    let mtime = std::fs::metadata(&file).unwrap().modified().unwrap();
    std::fs::write(&file, "ONE\n").unwrap();
    std::fs::File::options()
        .write(true)
        .open(&file)
        .unwrap()
        .set_modified(mtime)
        .unwrap();
    let info = dirty_of(&f).await;
    assert_eq!(info.verdict, WorktreeVerdict::HasWork, "{info:?}");
    assert_eq!(info.dirty, Some(1));
}

#[tokio::test]
async fn a_changed_executable_bit_is_work() {
    use std::os::unix::fs::PermissionsExt;
    let f = fixture();
    let file = f.wt.join("a.txt");
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755)).unwrap();
    let info = dirty_of(&f).await;
    assert_eq!(info.verdict, WorktreeVerdict::HasWork, "{info:?}");
    assert_eq!(info.dirty, Some(1));
}

#[tokio::test]
async fn a_touch_of_a_clean_file_is_not_work() {
    let f = fixture();
    let later = std::time::SystemTime::now() + Duration::from_secs(5);
    std::fs::File::options()
        .write(true)
        .open(f.wt.join("a.txt"))
        .unwrap()
        .set_modified(later)
        .unwrap();
    let info = dirty_of(&f).await;
    assert_eq!(info.verdict, WorktreeVerdict::Clean, "{info:?}");
    assert_eq!(info.dirty, Some(0));
}

#[tokio::test]
async fn an_unborn_branch_with_a_staged_file_is_work() {
    let f = fixture();
    let orphan = f.wt.parent().unwrap().join("repo--fd-orphan");
    git(
        &f.main,
        &[
            "worktree",
            "add",
            "-q",
            "--orphan",
            "-b",
            "fd/orphan",
            orphan.to_str().unwrap(),
        ],
    );
    std::fs::write(orphan.join("new.txt"), "x\n").unwrap();
    git(&orphan, &["add", "new.txt"]);
    let info = one(
        &policy(f.root.path()),
        session("s1", &orphan, SessionState::Exited),
    )
    .await;
    assert_eq!(info.verdict, WorktreeVerdict::HasWork, "{info:?}");
    assert_eq!(info.dirty, Some(1));
    assert_eq!(info.unpushed, Some(0));
    assert_eq!(info.compared_with, None);
}

#[tokio::test]
async fn a_new_commit_inside_a_submodule_is_work() {
    let f = fixture();
    let upstream = tempfile::tempdir().unwrap();
    let sub_src = upstream.path().canonicalize().unwrap().join("lib");
    std::fs::create_dir(&sub_src).unwrap();
    git(&sub_src, &["init", "-q", "-b", "main"]);
    std::fs::write(sub_src.join("l.txt"), "lib\n").unwrap();
    git(&sub_src, &["add", "."]);
    git(&sub_src, &["commit", "-q", "-m", "lib"]);
    git(
        &f.main,
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            "-q",
            sub_src.to_str().unwrap(),
            "lib",
        ],
    );
    git(&f.main, &["commit", "-q", "-m", "submodule"]);
    git(&f.wt, &["reset", "-q", "--hard", "main"]);
    git(
        &f.wt,
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "update",
            "-q",
            "--init",
        ],
    );
    let info = dirty_of(&f).await;
    assert_eq!(info.verdict, WorktreeVerdict::Clean, "{info:?}");
    let inner = f.wt.join("lib");
    std::fs::write(inner.join("l.txt"), "more\n").unwrap();
    git(&inner, &["commit", "-q", "-a", "-m", "inside"]);
    let info = dirty_of(&f).await;
    assert_eq!(info.verdict, WorktreeVerdict::HasWork, "{info:?}");
    assert_eq!(info.dirty, Some(1));
}

#[test]
fn the_debug_listing_is_parsed_with_its_stat_blocks() {
    let text = b"100644 78981922613b2afb6025042ff6bd878ac1994e85 0\ta.txt\0  ctime: 1:2\n  mtime: 3:4\n  dev: 5\tino: 6\n  uid: 7\tgid: 8\n  size: 9\tflags: 0\n100755 e69de29bb2d1d6434b8b29ae775ad8c2e48c5391 0\tx\ny\0  ctime: 11:12\n  mtime: 13:14\n  dev: 15\tino: 16\n  uid: 17\tgid: 18\n  size: 19\tflags: 0\n";
    let entries = parse_debug(text).unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].mode, 0o100_644);
    assert_eq!(entries[0].oid, "78981922613b2afb6025042ff6bd878ac1994e85");
    assert_eq!(entries[0].stage, 0);
    assert_eq!(entries[0].path, b"a.txt");
    assert_eq!((entries[0].ctime, entries[0].mtime), ((1, 2), (3, 4)));
    assert_eq!((entries[0].ino, entries[0].size), (6, 9));
    assert_eq!(entries[1].mode, 0o100_755);
    assert_eq!(entries[1].path, b"x\ny");
    assert_eq!((entries[1].ino, entries[1].size), (16, 19));
    assert!(parse_debug(b"").unwrap().is_empty());
    assert!(parse_debug(b"100644 abc 0\ta\0  ctime: 1:2\n").is_err());
}

#[test]
fn the_blob_id_is_the_git_hash_of_the_content() {
    assert_eq!(
        blob_oid(false, b"a\n"),
        "78981922613b2afb6025042ff6bd878ac1994e85"
    );
    assert_eq!(
        blob_oid(false, b""),
        "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391"
    );
    assert_eq!(blob_oid(true, b"a\n").len(), 64);
}

#[test]
fn the_scan_future_can_move_between_threads() {
    fn is_send<T: Send>(_: &T) {}
    let git = crate::workspace::Git::new(".", git_options());
    let fut = scan(&git, Path::new("."), 0);
    is_send(&fut);
}
