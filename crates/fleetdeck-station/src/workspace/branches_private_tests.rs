//! The branch diff leaves out the hidden names and the private paths, and
//! Git runs in the pinned session folder (DL-191, PRE-PR blockers).

use std::path::{Path, PathBuf};
use std::process::Command;

use fleetdeck_proto::SessionId;

use super::*;

const TS: i64 = 1_700_000_000;

fn git(dir: &Path, args: &[&str]) -> String {
    let mut cmd = Command::new("git");
    cmd.current_dir(dir)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .env("GIT_AUTHOR_DATE", format!("{TS} +0000"))
        .env("GIT_COMMITTER_DATE", format!("{TS} +0000"));
    let out = cmd.output().unwrap();
    assert!(out.status.success(), "git {args:?}");
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

/// Writes the `files` (creating directories) and commits them.
fn commit_files(dir: &Path, files: &[(&str, &str)], msg: &str) -> String {
    for (path, content) in files {
        let full = dir.join(path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(full, content).unwrap();
    }
    git(dir, &["add", "-f", "."]);
    git(dir, &["commit", "-q", "-m", msg]);
    git(dir, &["rev-parse", "HEAD"])
}

fn opts() -> GitOptions {
    GitOptions::isolated()
}

fn sid() -> SessionId {
    "s1".into()
}

/// A repository on `main` with the `base` files committed.
fn repo(base: &[(&str, &str)]) -> (tempfile::TempDir, PathBuf) {
    repo_named("repo", base)
}

fn repo_named(name: &str, base: &[(&str, &str)]) -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().canonicalize().unwrap().join(name);
    std::fs::create_dir(&dir).unwrap();
    git(&dir, &["init", "-q", "-b", "main"]);
    commit_files(&dir, base, "base");
    (tmp, dir)
}

/// Commits `files` on the new branch `name` and returns to `main`; the tip.
fn branch_with(dir: &Path, name: &str, files: &[(&str, &str)]) -> String {
    git(dir, &["checkout", "-q", "-b", name]);
    let tip = commit_files(dir, files, "branch");
    git(dir, &["checkout", "-q", "main"]);
    tip
}

/// The paths that a patch names (`diff --git a/<path> b/<path>` lines).
fn patch_paths(patch: &str) -> Vec<&str> {
    patch
        .lines()
        .filter_map(|l| l.strip_prefix("diff --git a/"))
        .map(|rest| rest.split(" b/").next().unwrap())
        .collect()
}

#[tokio::test]
async fn hidden_names_are_left_out_of_the_branch_diff() {
    let (_tmp, dir) = repo(&[("a.txt", "one\n")]);
    let tip = branch_with(
        &dir,
        "feat",
        &[
            ("notes.txt", "notes\n"),
            (".ssh/id_ed25519", "KEY\n"),
            ("sub/.aws/credentials", "SECRET\n"),
        ],
    );
    let diff = branch_diff(&opts(), &dir, &sid(), &tip, &PrivatePaths::default())
        .await
        .unwrap();
    assert_eq!(patch_paths(&diff.patch), ["notes.txt"], "{}", diff.patch);
    assert!(!diff.patch.contains("KEY") && !diff.patch.contains("SECRET"));
    assert_eq!(diff.summary.files_changed, 1);
}

#[tokio::test]
async fn a_private_file_is_left_out_of_the_branch_diff() {
    let (_tmp, dir) = repo(&[("base.txt", "base\n")]);
    let tip = branch_with(
        &dir,
        "feat",
        &[("secret.toml", "token = 1\n"), ("a.txt", "aaa\n")],
    );
    let mut private = PrivatePaths::default();
    private.exact.push(dir.join("secret.toml"));
    let diff = branch_diff(&opts(), &dir, &sid(), &tip, &private)
        .await
        .unwrap();
    assert_eq!(patch_paths(&diff.patch), ["a.txt"], "{}", diff.patch);
    assert!(!diff.patch.contains("token"));
    assert_eq!(diff.summary.files_changed, 1);
}

#[tokio::test]
async fn a_private_file_is_left_out_when_the_repository_name_ends_in_a_space() {
    let (_tmp, dir) = repo_named("repo ", &[("base.txt", "base\n")]);
    let tip = branch_with(
        &dir,
        "feat",
        &[("secret.toml", "token = 1\n"), ("a.txt", "aaa\n")],
    );
    let mut private = PrivatePaths::default();
    private.exact.push(dir.join("secret.toml"));
    let diff = branch_diff(&opts(), &dir, &sid(), &tip, &private)
        .await
        .unwrap();
    assert_eq!(patch_paths(&diff.patch), ["a.txt"], "{}", diff.patch);
    assert!(!diff.patch.contains("token"));
    assert_eq!(diff.summary.files_changed, 1);
}

#[test]
fn the_top_keeps_its_spaces_and_needs_its_newline() {
    assert_eq!(top_of(b"/w/repo \n"), Some(PathBuf::from("/w/repo ")));
    assert_eq!(top_of(b"/w/repo"), None);
    assert_eq!(top_of(b"\n"), None);
    assert_eq!(top_of(b"/a\n/b\n"), None);
}

#[tokio::test]
async fn a_session_folder_below_the_top_sees_the_whole_repository_but_no_hidden_name() {
    let (_tmp, dir) = repo(&[("a.txt", "one\n"), ("sub/x.txt", "x\n")]);
    let tip = branch_with(
        &dir,
        "feat",
        &[
            ("top.txt", "top\n"),
            ("sub/x.txt", "x\nchanged\n"),
            (".ssh/k", "KEY\n"),
        ],
    );
    let sub = dir.join("sub");
    let diff = branch_diff(&opts(), &sub, &sid(), &tip, &PrivatePaths::default())
        .await
        .unwrap();
    assert_eq!(
        patch_paths(&diff.patch),
        ["sub/x.txt", "top.txt"],
        "{}",
        diff.patch
    );
    assert!(!diff.patch.contains("KEY"));
    assert_eq!(diff.summary.files_changed, 2);
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn git_runs_in_the_pinned_folder_after_a_swap() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let make = |name: &str, branch: &str| {
        let dir = root.join(name);
        std::fs::create_dir(&dir).unwrap();
        git(&dir, &["init", "-q", "-b", "main"]);
        commit_files(&dir, &[("a.txt", "one\n")], "base");
        branch_with(&dir, branch, &[("b.txt", "bee\n")]);
        dir
    };
    let a_dir = make("a", "alpha");
    let b_dir = make("b", "evil");
    let (_fd, pinned) = pinned_git(&opts(), &a_dir).unwrap();
    // A workspace process moves the folder and puts a link to B in its place.
    std::fs::rename(&a_dir, root.join("a_moved")).unwrap();
    std::os::unix::fs::symlink(&b_dir, &a_dir).unwrap();
    let list = list(&pinned).await.unwrap();
    let names: Vec<&str> = list.branches.iter().map(|b| b.name.as_str()).collect();
    assert_eq!(names, ["alpha"]);
}

#[test]
fn the_branch_pathspec_reads_every_entry_from_the_top() {
    assert_eq!(with_top(":(exclude,literal)a"), ":(top,exclude,literal)a");
    assert_eq!(with_top("plain/path"), "");
    let top = Path::new("/r");
    let mut private = PrivatePaths::default();
    private.exact.push(PathBuf::from("/r/secret.toml"));
    let spec = branch_pathspec(top, &private);
    assert_eq!(spec[0], "--");
    assert_eq!(spec[1], ":/");
    let hidden = &spec[2..16];
    assert_eq!(hidden.len(), 14);
    assert!(
        hidden
            .iter()
            .all(|s| s.starts_with(":(top,exclude,icase,glob)**/"))
    );
    assert!(hidden.contains(&":(top,exclude,icase,glob)**/.ssh/**".to_owned()));
    let entry = private.excludes_for(top);
    assert_eq!(entry.len(), 1);
    assert_eq!(&spec[16..], [with_top(&entry[0])]);
    assert!(spec[16].starts_with(":(top,exclude,literal"));
    assert!(spec[16].ends_with(")secret.toml"));
}
