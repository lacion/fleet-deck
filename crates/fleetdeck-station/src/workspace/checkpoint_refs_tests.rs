//! The checkpoint refs of one session (R1.5) with real `git`.

use std::path::Path;
use std::process::Command;

use super::{delete_session_refs, of_session, session_refs};
use crate::workspace::{Git, GitOptions};

fn git(dir: &Path, args: &[&str]) {
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
}

#[test]
fn only_a_ref_with_the_session_in_the_session_place_matches() {
    let base = "refs/fleetdeck/checkpoints";
    assert!(of_session(&format!("{base}/i/w/s1/t1/start"), "s1"));
    assert!(of_session(&format!("{base}/i/w/s1/t1/end"), "s1"));
    assert!(!of_session(&format!("{base}/i/w/s10/t1/end"), "s1"));
    assert!(!of_session(&format!("{base}/s1/w/x/t1/end"), "s1"));
    assert!(!of_session(&format!("{base}/i/w/s1/t1"), "s1"));
    assert!(!of_session(&format!("{base}/i/w/s1/t1/end/more"), "s1"));
    assert!(!of_session("refs/heads/s1", "s1"));
}

#[tokio::test]
async fn the_refs_of_one_session_are_deleted_and_the_others_stay() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    git(dir, &["init", "-q", "-b", "main"]);
    std::fs::write(dir.join("a.txt"), "one\n").unwrap();
    git(dir, &["add", "."]);
    git(dir, &["commit", "-q", "-m", "init"]);
    let base = "refs/fleetdeck/checkpoints/i/w";
    for name in [
        format!("{base}/s1/t1/start"),
        format!("{base}/s1/t1/end"),
        format!("{base}/s1/t2/start"),
        format!("{base}/s2/t1/start"),
    ] {
        git(dir, &["update-ref", &name, "HEAD"]);
    }
    git(dir, &["update-ref", "refs/heads/s1", "HEAD"]);
    let g = Git::new(dir, GitOptions::isolated());
    assert_eq!(session_refs(&g, "s1").await.unwrap().len(), 3);
    assert_eq!(delete_session_refs(&g, "s1").await.unwrap(), 3);
    assert!(session_refs(&g, "s1").await.unwrap().is_empty());
    assert_eq!(session_refs(&g, "s2").await.unwrap().len(), 1);
    // A second call finds nothing, and a branch of the same name stays.
    assert_eq!(delete_session_refs(&g, "s1").await.unwrap(), 0);
    let heads = Command::new("git")
        .args(["branch", "--list", "s1"])
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&heads.stdout).contains("s1"));
}
