use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use super::*;
use crate::workspace::branches::parse_git_version;

struct Env {
    _dir: tempfile::TempDir,
    root: PathBuf,
    program: PathBuf,
    group: Arc<AtomicU32>,
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
            group: Arc::new(AtomicU32::new(0)),
        };
        env.script(&env.program, "exec git \"$@\"");
        Some(env)
    }

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

    /// A bare remote with the branches `branches` (all at one commit), the
    /// remote HEAD on the first one, and a clone `home` of it.
    fn remote_and_home(&self, branches: &[&str]) -> (PathBuf, PathBuf) {
        let bare = self.root.join("remote.git");
        fs::create_dir(&bare).unwrap();
        self.git(&bare, &["init", "--bare", "-b", branches[0]]);
        let seed = self.root.join("seed");
        fs::create_dir(&seed).unwrap();
        self.git(&seed, &["init", "-b", branches[0]]);
        self.commit(&seed, "one");
        self.git(&seed, &["remote", "add", "origin", bare.to_str().unwrap()]);
        for branch in branches {
            self.git(
                &seed,
                &["push", "-q", "origin", &format!("HEAD:refs/heads/{branch}")],
            );
        }
        let home = self.root.join("home");
        self.git(
            &self.root,
            &[
                "clone",
                "-q",
                bare.to_str().unwrap(),
                home.to_str().unwrap(),
            ],
        );
        // A newer Git creates a missing `origin/HEAD` in a fetch; the tests
        // set it themselves.
        self.git(
            &home,
            &["config", "remote.origin.followRemoteHEAD", "never"],
        );
        (bare, home)
    }

    fn commit(&self, repo: &Path, text: &str) -> String {
        fs::write(repo.join("a.txt"), format!("{text}\n")).unwrap();
        self.git(repo, &["add", "."]);
        self.git(repo, &["commit", "-m", text]);
        self.git(repo, &["rev-parse", "HEAD"])
    }

    async fn start(
        &self,
        home: &Path,
        settings: &GitSettings,
        owner_base: Option<&str>,
    ) -> Result<StartPoint, Refusal> {
        fetch_for_start(
            &self.options(&self.program),
            &self.group,
            home,
            "origin",
            settings,
            "alpha",
            "falcon",
            owner_base,
        )
        .await
    }
}

fn settings(default_branch: Option<&str>) -> GitSettings {
    GitSettings {
        sync: true,
        default_branch: default_branch.map(str::to_owned),
        ..GitSettings::default()
    }
}

#[tokio::test]
async fn an_old_local_head_starts_from_the_fetched_remote_commit() {
    let Some(env) = Env::new() else { return };
    let (bare, home) = env.remote_and_home(&["main"]);
    // The remote moves on; the clone is one commit behind.
    let seed = env.root.join("seed");
    let newer = env.commit(&seed, "two");
    env.git(&seed, &["push", "-q", "origin", "HEAD:refs/heads/main"]);
    assert_ne!(env.git(&home, &["rev-parse", "origin/main"]), newer);
    let got = env.start(&home, &settings(None), None).await.unwrap();
    assert_eq!(
        got,
        StartPoint {
            rev: Some("origin/main".to_owned()),
            stale: None
        }
    );
    assert_eq!(env.git(&home, &["rev-parse", "origin/main"]), newer);
    assert!(bare.exists());
}

#[tokio::test]
async fn the_start_point_order_is_owner_base_setting_remote_head_main_master() {
    let Some(env) = Env::new() else { return };
    let (bare, home) = env.remote_and_home(&["main", "master", "dev"]);
    let rev = |p: &StartPoint| p.rev.clone();

    // The owner's base wins, as given.
    let got = env
        .start(&home, &settings(Some("dev")), Some("abc123"))
        .await
        .unwrap();
    assert_eq!((rev(&got), got.stale), (Some("abc123".to_owned()), None));

    // The setting wins over the remote HEAD (`main`).
    let got = env
        .start(&home, &settings(Some("dev")), None)
        .await
        .unwrap();
    assert_eq!(rev(&got), Some("origin/dev".to_owned()));

    // A setting that names no branch of the remote falls through.
    let got = env
        .start(&home, &settings(Some("nope")), None)
        .await
        .unwrap();
    assert_eq!(rev(&got), Some("origin/main".to_owned()));

    // The remote HEAD: another default branch than `main`.
    env.git(&home, &["remote", "set-head", "origin", "dev"]);
    let got = env.start(&home, &settings(None), None).await.unwrap();
    assert_eq!(rev(&got), Some("origin/dev".to_owned()));

    // No remote HEAD: `main`, then `master`.
    env.git(&home, &["remote", "set-head", "origin", "-d"]);
    let got = env.start(&home, &settings(None), None).await.unwrap();
    assert_eq!(rev(&got), Some("origin/main".to_owned()));
    env.git(&bare, &["symbolic-ref", "HEAD", "refs/heads/master"]);
    env.git(&bare, &["update-ref", "-d", "refs/heads/main"]);
    let got = env.start(&home, &settings(None), None).await.unwrap();
    assert_eq!(rev(&got), Some("origin/master".to_owned()));
}

#[tokio::test]
async fn a_remote_without_a_default_branch_starts_from_head() {
    let Some(env) = Env::new() else { return };
    let (_bare, home) = env.remote_and_home(&["trunk"]);
    env.git(&home, &["remote", "set-head", "origin", "-d"]);
    let got = env.start(&home, &settings(None), None).await.unwrap();
    assert_eq!(got.rev, None);
    let stale = got.stale.unwrap();
    assert_eq!(stale.code, code::NO_DEFAULT_BRANCH);
    assert_eq!(
        stale.reason,
        "The remote has no default branch; this session started from the local HEAD."
    );
}

#[tokio::test]
async fn a_gone_remote_starts_from_head_with_the_redacted_reason() {
    let Some(env) = Env::new() else { return };
    let (bare, home) = env.remote_and_home(&["main"]);
    fs::remove_dir_all(&bare).unwrap();
    let got = env.start(&home, &settings(None), None).await.unwrap();
    assert_eq!(got.rev, None);
    let stale = got.stale.unwrap();
    assert_eq!(stale.code, code::FETCH_FAILED);
    assert!(
        stale
            .reason
            .starts_with("Fetch failed; this session started from the local HEAD: "),
        "{}",
        stale.reason
    );
    // The owner's base still wins when the fetch failed.
    let got = env
        .start(&home, &settings(None), Some("v1.2"))
        .await
        .unwrap();
    assert_eq!(got.rev.as_deref(), Some("v1.2"));
    assert_eq!(got.stale.unwrap().code, code::FETCH_FAILED);
}

#[tokio::test]
async fn a_fetch_that_takes_too_long_is_a_timeout() {
    let Some(env) = Env::new() else { return };
    let (_bare, home) = env.remote_and_home(&["main"]);
    let slow = env.root.join("git-slow");
    env.script(
        &slow,
        "for a in \"$@\"; do if [ \"$a\" = fetch ]; then exec sleep 5; fi; done\nexec git \"$@\"",
    );
    let got = start_point(
        &env.options(&slow),
        &env.group,
        &home,
        "origin",
        &settings(None),
        ("alpha", "falcon"),
        None,
        Duration::from_millis(400),
    )
    .await
    .unwrap();
    assert_eq!(got.rev, None);
    let stale = got.stale.unwrap();
    assert_eq!(stale.code, code::FETCH_TIMEOUT);
    assert_eq!(
        stale.reason,
        "Fetch took more than 30 s; this session started from the local HEAD."
    );
}

#[tokio::test]
async fn the_fetch_command_is_the_hardened_one() {
    let Some(env) = Env::new() else { return };
    let (_bare, home) = env.remote_and_home(&["main"]);
    let log = env.root.join("argv.log");
    let logger = env.root.join("git-logger");
    env.script(
        &logger,
        &format!("echo \"$*\" >> {}\nexec git \"$@\"", log.display()),
    );
    let got = fetch_for_start(
        &env.options(&logger),
        &env.group,
        &home,
        "origin",
        &settings(None),
        "alpha",
        "falcon",
        None,
    )
    .await
    .unwrap();
    assert_eq!(got.rev.as_deref(), Some("origin/main"));
    let text = fs::read_to_string(&log).unwrap();
    assert!(
        text.lines().any(|l| l.contains("protocol.ext.allow=never")
            && l.contains(
                "fetch --prune --no-tags --no-recurse-submodules --no-write-fetch-head -- origin"
            )),
        "{text}"
    );
}

#[tokio::test]
async fn a_blocking_fd_slug_branch_fails_the_start() {
    let Some(env) = Env::new() else { return };
    let (_bare, home) = env.remote_and_home(&["main"]);
    env.git(&home, &["branch", "fd/alpha"]);
    let err = env.start(&home, &settings(None), None).await.unwrap_err();
    assert_eq!(err.code, code::BRANCH_NAMESPACE_BLOCKED);
    assert_eq!(
        err.text,
        "The branch fd/alpha blocks the session branch fd/alpha/falcon. Rename it, for example: git branch -m fd/alpha fd-alpha."
    );
}

#[tokio::test]
async fn a_remote_branch_named_fd_slug_blocks_after_the_fetch() {
    let Some(env) = Env::new() else { return };
    let (_bare, home) = env.remote_and_home(&["main"]);
    let seed = env.root.join("seed");
    env.git(&seed, &["push", "-q", "origin", "HEAD:refs/heads/fd/alpha"]);
    // The clone has not fetched it yet: the fetch of the start finds it.
    let err = env.start(&home, &settings(None), None).await.unwrap_err();
    assert_eq!(err.code, code::BRANCH_NAMESPACE_BLOCKED);
    assert!(
        env.git(&home, &["for-each-ref", "refs/remotes/origin/fd/alpha"])
            .contains("refs/remotes/origin/fd/alpha")
    );
}

#[tokio::test]
async fn refs_below_fd_slug_do_not_block() {
    let Some(env) = Env::new() else { return };
    let (_bare, home) = env.remote_and_home(&["main"]);
    let seed = env.root.join("seed");
    env.git(
        &seed,
        &["push", "-q", "origin", "HEAD:refs/heads/fd/alpha/raven"],
    );
    env.git(&home, &["branch", "fd/alpha/otter"]);
    let got = env.start(&home, &settings(None), None).await.unwrap();
    assert_eq!(got.rev.as_deref(), Some("origin/main"));
}

#[tokio::test]
async fn the_namespace_check_runs_also_when_the_fetch_failed() {
    let Some(env) = Env::new() else { return };
    let (bare, home) = env.remote_and_home(&["main"]);
    env.git(&home, &["branch", "fd/alpha"]);
    fs::remove_dir_all(&bare).unwrap();
    let err = env.start(&home, &settings(None), None).await.unwrap_err();
    assert_eq!(err.code, code::BRANCH_NAMESPACE_BLOCKED);
}

#[tokio::test]
async fn an_old_git_fails_the_start() {
    let Some(env) = Env::new() else { return };
    let (_bare, home) = env.remote_and_home(&["main"]);
    let old = env.root.join("old-git");
    fs::write(&old, "#!/bin/sh\necho 'git version 2.30.0'\n").unwrap();
    fs::set_permissions(&old, fs::Permissions::from_mode(0o755)).unwrap();
    let err = fetch_for_start(
        &env.options(&old),
        &env.group,
        &home,
        "origin",
        &settings(None),
        "alpha",
        "falcon",
        None,
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, code::GIT_TOO_OLD);
    assert_eq!(
        err.text,
        "Fleet Git needs Git 2.45 or newer on this station."
    );
}

#[tokio::test]
async fn a_second_fetch_of_the_same_repository_waits_for_the_first() {
    let dir = tempfile::tempdir().unwrap();
    let (one, other) = (dir.path().join("one"), dir.path().join("other"));
    let held = fetch_lock(&one).await;
    // Another repository does not wait.
    drop(fetch_lock(&other).await);
    let mut waiting = Box::pin(fetch_lock(&one));
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(50), &mut waiting)
            .await
            .is_err()
    );
    drop(held);
    let _second = tokio::time::timeout(std::time::Duration::from_secs(5), waiting)
        .await
        .unwrap();
}
