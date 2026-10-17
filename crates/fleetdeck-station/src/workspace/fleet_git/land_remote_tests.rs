//! The remote work of a land publication, with real Git and a bare remote on
//! the same machine.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::Command;

use super::*;
use crate::workspace::branches::parse_git_version;

const WAIT: Duration = Duration::from_secs(30);

struct Repos {
    _dir: tempfile::TempDir,
    bare: PathBuf,
    home: PathBuf,
    url: String,
    options: GitOptions,
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .current_dir(dir)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@example.com")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@example.com")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

fn commit(dir: &Path, file: &str, text: &str) -> String {
    fs::write(dir.join(file), format!("{text}\n")).unwrap();
    git(dir, &["add", "."]);
    git(dir, &["commit", "-m", text]);
    git(dir, &["rev-parse", "HEAD"])
}

impl Repos {
    fn new() -> Option<Self> {
        let out = Command::new("git").arg("--version").output().unwrap();
        if parse_git_version(&String::from_utf8_lossy(&out.stdout)).is_none_or(|v| v < (2, 45)) {
            eprintln!("skipped: Fleet Git needs Git 2.45 or newer");
            return None;
        }
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let (bare, home) = (root.join("remote.git"), root.join("work"));
        fs::create_dir(&bare).unwrap();
        fs::create_dir(&home).unwrap();
        git(&bare, &["init", "--bare", "-b", "main"]);
        git(&home, &["init", "-b", "main"]);
        commit(&home, "a.txt", "one");
        let url = format!("file://{}", bare.display());
        git(&home, &["remote", "add", "origin", &url]);
        git(&home, &["push", "origin", "main"]);
        let program = root.join("git-wrapper");
        fs::write(
            &program,
            "#!/bin/sh\nexport GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1\nexec git \"$@\"\n",
        )
        .unwrap();
        fs::set_permissions(&program, fs::Permissions::from_mode(0o755)).unwrap();
        Some(Self {
            _dir: dir,
            bare,
            home,
            url,
            options: GitOptions {
                program,
                ..GitOptions::isolated()
            },
        })
    }
}

async fn run(push: &LandPush) -> PushEnd {
    push.spawn().unwrap().finish().await
}

#[tokio::test]
async fn a_branch_push_applies_and_the_read_finds_the_tip() {
    let Some(r) = Repos::new() else { return };
    git(&r.home, &["checkout", "-q", "-b", "fd/alpha/s1"]);
    let oid = commit(&r.home, "b.txt", "two");
    let push = LandPush::branch(
        &r.options,
        &r.home,
        WAIT,
        &r.url,
        "alpha",
        "fd/alpha/s1",
        &oid,
        "",
    )
    .unwrap();
    assert_eq!(run(&push).await.outcome, PushOutcome::Applied);
    let tip = remote_tip(&r.options, &r.home, &r.url, "refs/heads/fd/alpha/s1", WAIT).await;
    assert_eq!(tip, RemoteTip::Answered(Some(oid.clone())));
    let absent = remote_tip(&r.options, &r.home, &r.url, "fd/alpha/none", WAIT).await;
    assert_eq!(absent, RemoteTip::Answered(None));
    // The same commit again is up to date and applied.
    assert_eq!(run(&push).await.outcome, PushOutcome::Applied);
}

#[tokio::test]
async fn a_lease_that_does_not_hold_is_rejected_with_push_rejected() {
    let Some(r) = Repos::new() else { return };
    git(&r.home, &["checkout", "-q", "-b", "fd/alpha/s1"]);
    let one = commit(&r.home, "b.txt", "two");
    git(&r.home, &["push", "origin", "fd/alpha/s1"]);
    let two = commit(&r.home, "c.txt", "three");
    // The lease says "must not exist", but the ref exists.
    let push = LandPush::branch(
        &r.options,
        &r.home,
        WAIT,
        &r.url,
        "alpha",
        "fd/alpha/s1",
        &two,
        "",
    )
    .unwrap();
    let end = run(&push).await;
    assert_eq!(end.outcome, PushOutcome::Rejected(code::PUSH_REJECTED));
    assert_eq!(end.failure.code, code::PUSH_REJECTED);
    assert!(
        end.failure.text.starts_with("Push was rejected: "),
        "{}",
        end.failure.text
    );
    // With the right lease it applies.
    let push = LandPush::branch(
        &r.options,
        &r.home,
        WAIT,
        &r.url,
        "alpha",
        "fd/alpha/s1",
        &two,
        &one,
    )
    .unwrap();
    assert_eq!(run(&push).await.outcome, PushOutcome::Applied);
}

/// The default push of a land with the captured base as its lease.
fn default_push(r: &Repos, oid: &str, base: &str) -> LandPush {
    LandPush::default(&r.options, &r.home, WAIT, &r.url, "main", oid, base).unwrap()
}

#[tokio::test]
async fn a_default_push_is_a_fast_forward_from_the_captured_base() {
    let Some(r) = Repos::new() else { return };
    let base = git(&r.home, &["rev-parse", "HEAD"]);
    let oid = commit(&r.home, "b.txt", "two");
    let push = default_push(&r, &oid, &base);
    assert_eq!(run(&push).await.outcome, PushOutcome::Applied);
    assert_eq!(git(&r.bare, &["rev-parse", "refs/heads/main"]), oid);
    // A commit that is not a fast forward is refused: the remote is no longer
    // at the base.
    git(&r.home, &["reset", "-q", "--hard", "HEAD~1"]);
    let other = commit(&r.home, "c.txt", "other");
    let end = run(&default_push(&r, &other, &base)).await;
    assert!(matches!(end.outcome, PushOutcome::Rejected(_)), "{end:?}");
    assert_eq!(git(&r.bare, &["rev-parse", "refs/heads/main"]), oid);
}

#[tokio::test]
async fn a_default_push_with_a_base_that_the_remote_left_is_rejected() {
    let Some(r) = Repos::new() else { return };
    let first = git(&r.home, &["rev-parse", "HEAD"]);
    let base = commit(&r.home, "b.txt", "two");
    git(&r.home, &["push", "origin", "main"]);
    let oid = commit(&r.home, "c.txt", "three");
    // Another actor reset the remote branch to an ancestor of the base after
    // the capture: the ordinary push would be a fast forward from there.
    git(&r.bare, &["update-ref", "refs/heads/main", &first]);
    let end = run(&default_push(&r, &oid, &base)).await;
    assert_eq!(end.outcome, PushOutcome::Rejected(code::PUSH_REJECTED));
    assert_eq!(end.failure.code, code::PUSH_REJECTED);
    assert_eq!(git(&r.bare, &["rev-parse", "refs/heads/main"]), first);
    // The branch deleted after the capture is not made again.
    git(&r.bare, &["update-ref", "-d", "refs/heads/main"]);
    let end = run(&default_push(&r, &oid, &base)).await;
    assert_eq!(end.outcome, PushOutcome::Rejected(code::PUSH_REJECTED));
    assert_eq!(
        remote_tip(&r.options, &r.home, &r.url, "main", WAIT).await,
        RemoteTip::Answered(None)
    );
}

#[tokio::test]
async fn a_default_push_names_the_lease_of_its_base() {
    let Some(r) = Repos::new() else { return };
    let base = git(&r.home, &["rev-parse", "HEAD"]);
    let oid = commit(&r.home, "b.txt", "two");
    let push = default_push(&r, &oid, &base);
    let lease = format!("--force-with-lease=refs/heads/main:{base}");
    assert!(push.args.contains(&lease), "{:?}", push.args);
    assert!(!push.args.iter().any(|a| a.starts_with('+')));
}

#[tokio::test]
async fn a_refused_refspec_builds_no_push() {
    let Some(r) = Repos::new() else { return };
    let oid = "a".repeat(40);
    let built = |slug: &str, name: &str, oid: &str, lease: &str| {
        LandPush::branch(&r.options, &r.home, WAIT, &r.url, slug, name, oid, lease).err()
    };
    assert_eq!(
        built("alpha", "main", &oid, ""),
        Some(RefspecError::NotOwnNamespace)
    );
    assert_eq!(
        built("alpha", "fd/alpha/x", "abc", ""),
        Some(RefspecError::InvalidOid)
    );
    assert_eq!(
        built("alpha", "fd/alpha/x", &oid, "zz"),
        Some(RefspecError::InvalidOid)
    );
    for bad in ["", "-x", "a b", "a..b", "x:y", "a.lock"] {
        let got = LandPush::default(&r.options, &r.home, WAIT, &r.url, bad, &oid, &oid).err();
        assert_eq!(got, Some(RefspecError::InvalidRef), "{bad}");
    }
    // The base is a commit id: an empty or a bad one builds no push.
    for bad in ["", "zz", "abc"] {
        let got = LandPush::default(&r.options, &r.home, WAIT, &r.url, "main", &oid, bad).err();
        assert_eq!(got, Some(RefspecError::InvalidOid), "{bad}");
    }
}

#[tokio::test]
async fn a_read_of_an_unreachable_remote_gives_no_answer() {
    let Some(r) = Repos::new() else { return };
    let missing = format!("file://{}", r.bare.join("nope").display());
    let tip = remote_tip(&r.options, &r.home, &missing, "refs/heads/main", WAIT).await;
    assert_eq!(tip, RemoteTip::NoAnswer);
}

#[tokio::test]
async fn the_reads_of_the_local_repository() {
    let Some(r) = Repos::new() else { return };
    let base = git(&r.home, &["rev-parse", "HEAD"]);
    git(&r.home, &["checkout", "-q", "-b", "fd/alpha/s1"]);
    let tip = commit(&r.home, "b.txt", "two");
    assert_eq!(
        local_tip(&r.options, &r.home, "fd/alpha/s1", WAIT).await,
        Some(tip.clone())
    );
    assert_eq!(
        local_tip(&r.options, &r.home, "fd/alpha/none", WAIT).await,
        None
    );
    assert_eq!(
        is_ancestor(&r.options, &r.home, &base, &tip, WAIT).await,
        Some(true)
    );
    assert_eq!(
        is_ancestor(&r.options, &r.home, &tip, &base, WAIT).await,
        Some(false)
    );
    assert_eq!(
        is_ancestor(&r.options, &r.home, &"b".repeat(40), &tip, WAIT).await,
        None
    );
    assert!(fetch_default(&r.options, &r.home, "origin").await);
    assert!(!fetch_default(&r.options, &r.home, "nothing").await);
}
