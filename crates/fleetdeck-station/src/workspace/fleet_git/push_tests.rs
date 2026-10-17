use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use super::*;
use crate::workspace::branches::parse_git_version;

const SLUG: &str = "alpha";
const REF: &str = "fd/alpha/falcon";
const OID: &str = "0123456789abcdef0123456789abcdef01234567";

// ------------------------------------------------------------------ pure rules

#[test]
fn the_refspec_names_the_commit_and_the_own_ref() {
    assert_eq!(
        refspec_for(SLUG, REF, OID).unwrap(),
        format!("{OID}:refs/heads/{REF}")
    );
    let sha256 = "a".repeat(64);
    assert!(refspec_for(SLUG, "fd/alpha/falcon-2", &sha256).is_ok());
    assert!(refspec_for(SLUG, "fd/alpha/team/falcon", OID).is_ok());
}

#[test]
fn the_refspec_refuses_every_ref_outside_the_own_namespace() {
    for bad in [
        "main",
        "refs/heads/fd/alpha/falcon",
        "fd/alpha",
        "fd/alpha/",
        "fd/alphabet/falcon",
        "fd/bravo/falcon",
        "fd/falcon",
        "x/fd/alpha/falcon",
    ] {
        assert!(refspec_for(SLUG, bad, OID).is_err(), "{bad}");
    }
    assert_eq!(
        refspec_for(SLUG, "main", OID),
        Err(RefspecError::NotOwnNamespace)
    );
    for bad in [
        "fd/alpha/a..b",
        "fd/alpha/a b",
        "fd/alpha//b",
        "fd/alpha/.hidden",
        "fd/alpha/a.lock",
        "fd/alpha/a:b",
        "fd/alpha/a*",
        "fd/alpha/a~1",
        "fd/alpha/a@{1}",
        "fd/alpha/a\\b",
        "fd/alpha/a/",
        "fd/alpha/a.",
    ] {
        assert_eq!(
            refspec_for(SLUG, bad, OID),
            Err(RefspecError::InvalidRef),
            "{bad}"
        );
    }
    for slug in ["", "Alpha", "al/pha"] {
        let publish = format!("fd/{slug}/falcon");
        assert_eq!(
            refspec_for(slug, &publish, OID),
            Err(RefspecError::InvalidRef),
            "{slug}"
        );
    }
    for oid in [
        "",
        "abc",
        "ABCDEF0123456789abcdef0123456789abcdef01",
        "zz23456789abcdef0123456789abcdef01234567",
    ] {
        assert_eq!(
            refspec_for(SLUG, REF, oid),
            Err(RefspecError::InvalidOid),
            "{oid}"
        );
    }
}

#[test]
fn the_backoff_is_one_two_four_eight_then_ten_minutes() {
    let minutes: Vec<u64> = (1..=8).map(|n| backoff_delay(n).as_secs() / 60).collect();
    assert_eq!(minutes, [1, 2, 4, 8, 10, 10, 10, 10]);
    assert_eq!(backoff_delay(0), Duration::from_secs(60));
    assert_eq!(backoff_delay(u32::MAX), Duration::from_secs(600));
}

fn line(flag: char, reference: &str, summary: &str) -> String {
    format!("To file:///x.git\n{flag}\trefs/heads/x:refs/heads/{reference}\t{summary}\nDone\n")
}

#[test]
fn the_porcelain_flag_of_the_exact_ref_classifies_the_push() {
    for (flag, summary) in [
        (' ', "abc..def"),
        ('+', "abc...def (forced update)"),
        ('*', "[new branch]"),
        ('=', "[up to date]"),
    ] {
        let text = line(flag, REF, summary);
        assert_eq!(
            push_outcome(0, &text, false, REF),
            PushOutcome::Applied,
            "{flag:?}"
        );
        // The same line with a failing exit code is not a proof.
        assert_eq!(
            push_outcome(1, &text, false, REF),
            PushOutcome::Unknown,
            "{flag:?}"
        );
    }
    for summary in [
        "[rejected] (stale info)",
        "[rejected] (fetch first)",
        "[remote rejected] (hook declined)",
    ] {
        let text = line('!', REF, summary);
        assert_eq!(
            push_outcome(1, &text, false, REF),
            PushOutcome::Rejected(code::PUSH_REJECTED)
        );
    }
}

#[test]
fn every_other_push_result_is_unknown() {
    let applied = line('*', REF, "[new branch]");
    // A timeout beats a line.
    assert_eq!(push_outcome(0, &applied, true, REF), PushOutcome::Unknown);
    // A transport or authentication error.
    assert_eq!(push_outcome(128, "", false, REF), PushOutcome::Unknown);
    // A signal.
    assert_eq!(push_outcome(-1, "", false, REF), PushOutcome::Unknown);
    // Exit 0 without the line of the ref, or a line for another ref.
    assert_eq!(push_outcome(0, "Done\n", false, REF), PushOutcome::Unknown);
    let other = line('*', "fd/alpha/other", "[new branch]");
    assert_eq!(push_outcome(0, &other, false, REF), PushOutcome::Unknown);
    // A delete flag is not a push of the commit.
    assert_eq!(
        push_outcome(0, &line('-', REF, "[deleted]"), false, REF),
        PushOutcome::Unknown
    );
}

#[test]
fn a_failure_has_the_code_and_text_of_the_design() {
    let auth = failure(
        PushOutcome::Unknown,
        "git@h: Permission denied (publickey).",
        "",
    );
    assert_eq!(auth.code, code::PUSH_AUTH);
    assert_eq!(
        auth.text,
        "Push needs credentials on this station: git@h: Permission denied (publickey)."
    );
    let failed = failure(PushOutcome::Unknown, "fatal: something else", "");
    assert_eq!(failed.code, code::PUSH_FAILED);
    assert_eq!(failed.text, "Push failed: fatal: something else");
    let rejected = failure(
        PushOutcome::Rejected(code::PUSH_REJECTED),
        "error: failed to push",
        "",
    );
    assert_eq!(rejected.code, code::PUSH_REJECTED);
    assert_eq!(rejected.text, "Push was rejected: error: failed to push");
}

// ----------------------------------------------------------------- with Git

struct Env {
    _dir: tempfile::TempDir,
    root: PathBuf,
    program: PathBuf,
}

impl Env {
    fn new() -> Option<Self> {
        let out = Command::new("git").arg("--version").output().unwrap();
        if parse_git_version(&String::from_utf8_lossy(&out.stdout)).is_none_or(|v| v < (2, 45)) {
            eprintln!("skipped: Fleet Git needs Git 2.45 or newer");
            return None;
        }
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let program = root.join("git-wrapper");
        let env = Self {
            _dir: dir,
            root,
            program,
        };
        env.script(&env.program, "exec git \"$@\"");
        Some(env)
    }

    /// A shell program that runs `body` with the isolated Git environment.
    fn script(&self, path: &Path, body: &str) {
        fs::write(
            path,
            format!(
                "#!/bin/sh\nexport GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1\n{body}\n"
            ),
        )
        .unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn options(&self, program: &Path) -> GitOptions {
        GitOptions {
            program: program.to_path_buf(),
            timeout: Duration::from_secs(60),
            isolated: true,
            confine: None,
        }
    }

    fn git(&self, dir: &Path, args: &[&str]) -> String {
        let out = Command::new(&self.program)
            .current_dir(dir)
            .args(args)
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

    fn bare(&self, name: &str) -> PathBuf {
        let path = self.root.join(name);
        fs::create_dir(&path).unwrap();
        self.git(&path, &["init", "--bare", "-b", "main"]);
        path
    }

    /// A repository with one commit.
    fn repo(&self, name: &str) -> PathBuf {
        let path = self.root.join(name);
        fs::create_dir(&path).unwrap();
        self.git(&path, &["init", "-b", "main"]);
        self.commit(&path, "one");
        path
    }

    fn commit(&self, repo: &Path, text: &str) -> String {
        fs::write(repo.join("a.txt"), format!("{text}\n")).unwrap();
        self.git(repo, &["add", "."]);
        self.git(repo, &["commit", "-m", text]);
        self.git(repo, &["rev-parse", "HEAD"])
    }

    fn refs(&self, bare: &Path) -> Vec<String> {
        self.git(bare, &["for-each-ref", "--format=%(refname) %(objectname)"])
            .lines()
            .map(str::to_owned)
            .collect()
    }

    fn job(&self, repo: &Path, bare: &Path, oid: &str, lease: &str) -> PushJob {
        PushJob {
            repo_dir: repo.to_path_buf(),
            url: format!("file://{}", bare.display()),
            slug: SLUG.to_owned(),
            publish_ref: REF.to_owned(),
            oid: oid.to_owned(),
            lease: lease.to_owned(),
        }
    }
}

fn is_applied(result: &PushResult) -> bool {
    matches!(result, PushResult::Applied)
}

#[tokio::test]
async fn a_first_push_with_an_empty_lease_then_a_second_with_the_lease() {
    let Some(env) = Env::new() else { return };
    let bare = env.bare("remote.git");
    let repo = env.repo("work");
    let writer = PushWriter::new(env.options(&env.program));
    let first = env.git(&repo, &["rev-parse", "HEAD"]);
    let got = writer
        .submit(&env.job(&repo, &bare, &first, ""))
        .await
        .unwrap();
    assert!(is_applied(&got), "{got:?}");
    assert_eq!(env.refs(&bare), [format!("refs/heads/{REF} {first}")]);

    let second = env.commit(&repo, "two");
    let got = writer
        .submit(&env.job(&repo, &bare, &second, &first))
        .await
        .unwrap();
    assert!(is_applied(&got), "{got:?}");
    assert_eq!(env.refs(&bare), [format!("refs/heads/{REF} {second}")]);
}

#[tokio::test]
async fn a_revert_then_a_push_succeeds_with_the_lease() {
    let Some(env) = Env::new() else { return };
    let bare = env.bare("remote.git");
    let repo = env.repo("work");
    let writer = PushWriter::new(env.options(&env.program));
    let first = env.git(&repo, &["rev-parse", "HEAD"]);
    let second = env.commit(&repo, "two");
    assert!(is_applied(
        &writer
            .submit(&env.job(&repo, &bare, &second, ""))
            .await
            .unwrap()
    ));
    // The session rewrites its history: the lease is the commit pushed last.
    env.git(&repo, &["reset", "--hard", &first]);
    let got = writer
        .submit(&env.job(&repo, &bare, &first, &second))
        .await
        .unwrap();
    assert!(is_applied(&got), "{got:?}");
    assert_eq!(env.refs(&bare), [format!("refs/heads/{REF} {first}")]);
}

#[tokio::test]
async fn a_push_over_a_moved_remote_ref_is_rejected_and_confirmed_not_applied() {
    let Some(env) = Env::new() else { return };
    let bare = env.bare("remote.git");
    let repo = env.repo("work");
    let writer = PushWriter::new(env.options(&env.program));
    let first = env.git(&repo, &["rev-parse", "HEAD"]);
    assert!(is_applied(
        &writer
            .submit(&env.job(&repo, &bare, &first, ""))
            .await
            .unwrap()
    ));
    // Another machine moves the ref.
    let other = env.root.join("other");
    env.git(
        &env.root,
        &[
            "clone",
            "-q",
            bare.to_str().unwrap(),
            other.to_str().unwrap(),
        ],
    );
    env.git(
        &other,
        &["checkout", "-q", "-b", "x", &format!("origin/{REF}")],
    );
    let moved = env.commit(&other, "elsewhere");
    env.git(
        &other,
        &["push", "-q", "origin", &format!("x:refs/heads/{REF}")],
    );
    let second = env.commit(&repo, "two");
    let got = writer
        .submit(&env.job(&repo, &bare, &second, &first))
        .await
        .unwrap();
    match got {
        PushResult::NotApplied(failure) => {
            assert_eq!(failure.code, code::PUSH_REJECTED);
            assert!(
                failure.text.starts_with("Push was rejected: "),
                "{}",
                failure.text
            );
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(env.refs(&bare), [format!("refs/heads/{REF} {moved}")]);
}

#[tokio::test]
async fn the_recovery_read_takes_a_remote_tip_equal_to_the_attempt() {
    let Some(env) = Env::new() else { return };
    let bare = env.bare("remote.git");
    let repo = env.repo("work");
    let writer = PushWriter::new(env.options(&env.program));
    let first = env.git(&repo, &["rev-parse", "HEAD"]);
    assert!(is_applied(
        &writer
            .submit(&env.job(&repo, &bare, &first, ""))
            .await
            .unwrap()
    ));
    // The restart of a `pushing` row: the same attempt values.
    let same = env.job(&repo, &bare, &first, "");
    assert_eq!(writer.recover(&same).await, PushResult::Applied);
    // Another tip, or no ref: confirmed not applied.
    let second = env.commit(&repo, "two");
    let newer = env.job(&repo, &bare, &second, &first);
    assert!(matches!(
        writer.recover(&newer).await,
        PushResult::NotApplied(_)
    ));
    env.git(&bare, &["update-ref", "-d", &format!("refs/heads/{REF}")]);
    assert!(matches!(
        writer.recover(&same).await,
        PushResult::NotApplied(_)
    ));
}

#[tokio::test]
async fn a_remote_that_does_not_answer_leaves_the_outcome_unknown() {
    let Some(env) = Env::new() else { return };
    let repo = env.repo("work");
    let writer = PushWriter::new(env.options(&env.program));
    let oid = env.git(&repo, &["rev-parse", "HEAD"]);
    let gone = env.root.join("gone.git");
    let got = writer
        .submit(&env.job(&repo, &gone, &oid, ""))
        .await
        .unwrap();
    match got {
        PushResult::Unknown(failure) => assert_eq!(failure.code, code::PUSH_FAILED),
        other => panic!("{other:?}"),
    }
    assert!(matches!(
        writer.recover(&env.job(&repo, &gone, &oid, "")).await,
        PushResult::Unknown(_)
    ));
}

#[tokio::test]
async fn a_ref_outside_the_own_namespace_is_never_pushed() {
    let Some(env) = Env::new() else { return };
    let bare = env.bare("remote.git");
    let repo = env.repo("work");
    let writer = PushWriter::new(env.options(&env.program));
    let oid = env.git(&repo, &["rev-parse", "HEAD"]);
    for publish in ["main", "fd/bravo/falcon", "refs/heads/fd/alpha/falcon"] {
        let mut job = env.job(&repo, &bare, &oid, "");
        publish.clone_into(&mut job.publish_ref);
        assert!(writer.submit(&job).await.is_err(), "{publish}");
    }
    let mut job = env.job(&repo, &bare, &oid, "not-an-oid");
    assert_eq!(
        writer.submit(&job).await.unwrap_err(),
        RefspecError::InvalidOid
    );
    job.lease.clear();
    assert!(env.refs(&bare).is_empty());
}

#[tokio::test]
async fn a_pre_push_hook_does_not_run() {
    let Some(env) = Env::new() else { return };
    let bare = env.bare("remote.git");
    let repo = env.repo("work");
    let marker = env.root.join("hook-ran");
    let hook = repo.join(".git/hooks/pre-push");
    fs::create_dir_all(hook.parent().unwrap()).unwrap();
    fs::write(
        &hook,
        format!("#!/bin/sh\ntouch {}\nexit 0\n", marker.display()),
    )
    .unwrap();
    fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
    let writer = PushWriter::new(env.options(&env.program));
    let oid = env.git(&repo, &["rev-parse", "HEAD"]);
    let got = writer
        .submit(&env.job(&repo, &bare, &oid, ""))
        .await
        .unwrap();
    assert!(is_applied(&got), "{got:?}");
    assert!(!marker.exists(), "the pre-push hook ran");
}

#[tokio::test]
async fn the_push_command_names_the_verified_url_and_the_commit() {
    let Some(env) = Env::new() else { return };
    let bare = env.bare("remote.git");
    let repo = env.repo("work");
    let log = env.root.join("argv.log");
    let logger = env.root.join("git-logger");
    env.script(
        &logger,
        &format!(
            "for a in \"$@\"; do printf '%s\\n' \"$a\" >> {}; done\nexec git \"$@\"",
            log.display()
        ),
    );
    let writer = PushWriter::new(env.options(&logger));
    let oid = env.git(&repo, &["rev-parse", "HEAD"]);
    let job = env.job(&repo, &bare, &oid, "");
    assert!(is_applied(&writer.submit(&job).await.unwrap()));
    let args: Vec<String> = fs::read_to_string(&log)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect();
    for expected in [
        "push",
        "--porcelain",
        "--no-verify",
        "--no-follow-tags",
        "--recurse-submodules=no",
        &format!("--force-with-lease=refs/heads/{REF}:"),
        "--",
        &job.url,
        &format!("{oid}:refs/heads/{REF}"),
        "protocol.ext.allow=never",
    ] {
        assert!(
            args.iter().any(|a| a == expected),
            "{expected} is not in {args:?}"
        );
    }
    assert!(!args.iter().any(|a| a == "origin"), "{args:?}");
    assert!(
        !args
            .iter()
            .any(|a| a == "--all" || a == "--mirror" || a == "--tags"),
        "{args:?}"
    );
}

#[tokio::test]
async fn a_push_that_applies_and_then_times_out_is_found_by_the_recovery_read() {
    let Some(env) = Env::new() else { return };
    let bare = env.bare("remote.git");
    let repo = env.repo("work");
    let slow = env.root.join("git-slow");
    env.script(
        &slow,
        "for a in \"$@\"; do if [ \"$a\" = push ]; then git \"$@\"; exec sleep 5; fi; done\nexec git \"$@\"",
    );
    let writer = PushWriter::new(env.options(&slow));
    let oid = env.git(&repo, &["rev-parse", "HEAD"]);
    let got = writer
        .submit_within(
            &env.job(&repo, &bare, &oid, ""),
            Duration::from_millis(1500),
        )
        .await
        .unwrap();
    assert!(is_applied(&got), "{got:?}");
    assert_eq!(env.refs(&bare), [format!("refs/heads/{REF} {oid}")]);
}

#[tokio::test]
async fn a_push_that_times_out_without_applying_reports_the_timeout() {
    let Some(env) = Env::new() else { return };
    let bare = env.bare("remote.git");
    let repo = env.repo("work");
    let hang = env.root.join("git-hang");
    env.script(
        &hang,
        "for a in \"$@\"; do if [ \"$a\" = push ]; then exec sleep 5; fi; done\nexec git \"$@\"",
    );
    let writer = PushWriter::new(env.options(&hang));
    let oid = env.git(&repo, &["rev-parse", "HEAD"]);
    let got = writer
        .submit_within(&env.job(&repo, &bare, &oid, ""), Duration::from_millis(500))
        .await
        .unwrap();
    match got {
        PushResult::NotApplied(failure) => {
            assert_eq!(failure.code, code::PUSH_TIMEOUT);
            assert_eq!(failure.text, "Push took more than 60 s.");
        }
        other => panic!("{other:?}"),
    }
    assert!(env.refs(&bare).is_empty());
}

#[tokio::test]
async fn jobs_run_one_after_the_other() {
    let Some(env) = Env::new() else { return };
    let bare = env.bare("remote.git");
    let repo = env.repo("work");
    let log = env.root.join("order.log");
    let logger = env.root.join("git-order");
    env.script(
        &logger,
        &format!(
            "for a in \"$@\"; do if [ \"$a\" = push ]; then echo start >> {log}; sleep 0.3; git \"$@\"; rc=$?; echo end >> {log}; exit $rc; fi; done\nexec git \"$@\"",
            log = log.display()
        ),
    );
    let writer = PushWriter::new(env.options(&logger));
    let oid = env.git(&repo, &["rev-parse", "HEAD"]);
    let first = env.job(&repo, &bare, &oid, "");
    let mut second = env.job(&repo, &bare, &oid, "");
    "fd/alpha/raven".clone_into(&mut second.publish_ref);
    let (a, b) = tokio::join!(writer.submit(&first), writer.submit(&second));
    assert!(is_applied(&a.unwrap()) && is_applied(&b.unwrap()));
    let order: Vec<String> = fs::read_to_string(&log)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect();
    assert_eq!(order, ["start", "end", "start", "end"]);
}
