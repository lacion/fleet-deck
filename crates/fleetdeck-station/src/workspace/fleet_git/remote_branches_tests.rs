//! `project.branches` and `project.fetch` on the station (design §13.3), with
//! real Git.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use fleetdeck_proto::fleet_git::{
    GitProjectView, GitSettings, REMOTE_BRANCHES_MAX, RemoteIdentity, code,
};

use super::*;
use crate::workspace::GitOptions;
use crate::workspace::branches::parse_git_version;

struct Rig {
    _dir: tempfile::TempDir,
    root: PathBuf,
    /// The bare remote.
    bare: PathBuf,
    /// The home: a clone of the remote.
    home: PathBuf,
}

impl Rig {
    fn new() -> Option<Self> {
        let out = Command::new("git").arg("--version").output().unwrap();
        if parse_git_version(&String::from_utf8_lossy(&out.stdout)).is_none_or(|v| v < (2, 45)) {
            eprintln!("skipped: Fleet Git needs Git 2.45 or newer");
            return None;
        }
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let rig = Self {
            bare: root.join("remote.git"),
            home: root.join("home"),
            _dir: dir,
            root,
        };
        fs::create_dir(&rig.bare).unwrap();
        rig.git(&rig.bare, &["init", "--bare", "-b", "main"]);
        fs::create_dir(&rig.home).unwrap();
        rig.git(&rig.home, &["init", "-b", "main"]);
        rig.git(
            &rig.home,
            &["remote", "add", "origin", &rig.bare.to_string_lossy()],
        );
        Some(rig)
    }

    fn git(&self, dir: &Path, args: &[&str]) -> String {
        self.git_at(dir, args, "2001-09-09T01:46:40Z")
    }

    fn git_at(&self, dir: &Path, args: &[&str], date: &str) -> String {
        let out = Command::new("git")
            .current_dir(dir)
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@example.com")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@example.com")
            .env("GIT_AUTHOR_DATE", date)
            .env("GIT_COMMITTER_DATE", date)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_owned()
    }

    /// A commit with `message` made at the Unix time `at`.
    fn commit(&self, message: &str, at: i64) -> String {
        let date = format!("{at} +0000");
        fs::write(self.home.join("f.txt"), format!("{message}\n")).unwrap();
        self.git_at(&self.home, &["add", "."], &date);
        self.git_at(&self.home, &["commit", "-q", "-m", message], &date);
        self.git(&self.home, &["rev-parse", "HEAD"])
    }

    /// `main` with a base commit, pushed to the remote.
    fn base(&self) -> String {
        let oid = self.commit("base", 1_000_000_000);
        self.git(&self.home, &["push", "-q", "origin", "main"]);
        oid
    }

    /// Sets the remote-tracking ref `refs/remotes/origin/<name>` to `oid`.
    fn track(&self, name: &str, oid: &str) {
        self.git(
            &self.home,
            &["update-ref", &format!("refs/remotes/origin/{name}"), oid],
        );
    }

    fn view(&self, wip: Option<String>) -> GitProjectView {
        GitProjectView {
            project_id: "p1".to_owned(),
            project_name: "app".to_owned(),
            settings_revision: 1,
            settings: GitSettings {
                sync: true,
                wip_remote: wip,
                ..GitSettings::default()
            },
            remote: RemoteIdentity {
                repo: "h/acme/app".to_owned(),
                dest: "https://h:443/acme/app".to_owned(),
            },
            forge: None,
            clone_url: "https://h/acme/app.git".to_owned(),
            home_generation: 1,
            home_path: self.home.to_string_lossy().into_owned(),
            radar_home: true,
        }
    }

    async fn list(&self, view: &GitProjectView) -> RemoteBranches {
        let options = GitOptions::isolated();
        let env = RadarEnv {
            options: &options,
            repo_dir: &self.home,
            view,
            allow_local_remotes: true,
        };
        list_branches(&env).await.unwrap()
    }

    async fn fetch(&self, view: &GitProjectView, now: i64) -> FetchAnswer {
        let options = GitOptions::isolated();
        let env = RadarEnv {
            options: &options,
            repo_dir: &self.home,
            view,
            allow_local_remotes: true,
        };
        fetch_project(&env, now).await
    }
}

fn names(list: &RemoteBranches) -> Vec<&str> {
    list.branches.iter().map(|b| b.name.as_str()).collect()
}

#[tokio::test]
async fn only_two_and_three_component_commit_refs_are_listed_newest_first() {
    let Some(rig) = Rig::new() else { return };
    let base = rig.base();
    let old = rig.commit("old", 1_000_000_100);
    let new = rig.commit("new", 1_000_000_200);
    rig.track("fd/alpha/old", &old);
    rig.track("fd/alpha/new", &new);
    rig.track("fd/legacy", &old);
    // Four components: skipped. A tip that is not a commit: skipped.
    rig.track("fd/alpha/sub/deep", &new);
    let tree = rig.git(&rig.home, &["rev-parse", "HEAD^{tree}"]);
    rig.track("fd/alpha/tree", &tree);
    // A ref outside `fd/` is not listed.
    rig.track("other/alpha/x", &new);
    let list = rig.list(&rig.view(None)).await;
    assert_eq!(names(&list), ["fd/alpha/new", "fd/alpha/old", "fd/legacy"]);
    assert!(!list.truncated);
    let first = &list.branches[0];
    assert_eq!(first.slug.as_deref(), Some("alpha"));
    assert_eq!(first.callsign, "new");
    assert_eq!(first.head, new);
    // The commit time of the tip in milliseconds.
    assert_eq!(first.at, 1_000_000_200_000);
    assert_eq!(first.subject, "new");
    let legacy = &list.branches[2];
    assert_eq!(
        (legacy.slug.as_deref(), legacy.callsign.as_str()),
        (None, "legacy")
    );
    assert_eq!(list.remote, "origin");
    assert_eq!(list.default_branch, "main");
    assert_eq!(list.default_oid.as_deref(), Some(base.as_str()));
}

#[tokio::test]
async fn ahead_and_behind_count_against_the_default_branch() {
    let Some(rig) = Rig::new() else { return };
    rig.base();
    rig.git(&rig.home, &["checkout", "-q", "-b", "work", "main"]);
    rig.commit("w1", 1_000_000_100);
    let tip = rig.commit("w2", 1_000_000_200);
    rig.git(
        &rig.home,
        &["push", "-q", "origin", "work:refs/heads/fd/alpha/one"],
    );
    rig.git(&rig.home, &["checkout", "-q", "main"]);
    // `main` moves on: it has one commit that the branch lacks.
    fs::write(rig.home.join("g.txt"), "g\n").unwrap();
    rig.git(&rig.home, &["add", "."]);
    rig.git(&rig.home, &["commit", "-q", "-m", "m1"]);
    rig.git(&rig.home, &["push", "-q", "origin", "main"]);
    let list = rig.list(&rig.view(None)).await;
    assert_eq!(names(&list), ["fd/alpha/one"]);
    let one = &list.branches[0];
    assert_eq!((one.ahead, one.behind), (2, 1));
    assert_eq!(one.head, tip);
}

#[tokio::test]
async fn more_than_200_branches_are_cut_and_flagged() {
    let Some(rig) = Rig::new() else { return };
    rig.base();
    let tip = rig.commit("tip", 1_000_000_100);
    let mut input = String::new();
    for n in 0..=REMOTE_BRANCHES_MAX {
        input.push_str(&format!(
            "create refs/remotes/origin/fd/alpha/b{n:03} {tip}\n"
        ));
    }
    let mut child = Command::new("git")
        .current_dir(&rig.home)
        .args(["update-ref", "--stdin"])
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    assert!(child.wait().unwrap().success());
    let list = rig.list(&rig.view(None)).await;
    assert_eq!(list.branches.len(), REMOTE_BRANCHES_MAX);
    assert!(list.truncated);
    // The same time: the name order breaks the tie.
    assert_eq!(list.branches[0].name, "fd/alpha/b000");
    assert_eq!(list.branches[199].name, "fd/alpha/b199");
}

#[tokio::test]
async fn the_listing_runs_no_fetch_and_the_fetch_fills_it() {
    let Some(rig) = Rig::new() else { return };
    rig.base();
    let tip = rig.commit("late", 1_000_000_100);
    // Pushed by another form of the URL: Git keeps no tracking ref for it.
    let url = format!("file://{}", rig.bare.display());
    rig.git(
        &rig.home,
        &[
            "push",
            "-q",
            &url,
            &format!("{tip}:refs/heads/fd/bravo/late"),
        ],
    );
    let view = rig.view(None);
    let before = rig.list(&view).await;
    assert!(before.branches.is_empty());
    assert_eq!(before.fetched_at, None);
    let answer = rig.fetch(&view, 4242).await;
    assert_eq!(answer.fetched_at, Some(4242));
    assert_eq!((answer.code, answer.text), (None, None));
    let after = rig.list(&view).await;
    assert_eq!(names(&after), ["fd/bravo/late"]);
    assert_eq!(after.fetched_at, Some(4242));
}

#[tokio::test]
async fn with_a_wip_remote_the_listing_and_the_fetch_use_the_wip_refs() {
    let Some(rig) = Rig::new() else { return };
    rig.base();
    let tip = rig.commit("wip work", 1_000_000_100);
    let wip = rig.root.join("wip.git");
    fs::create_dir(&wip).unwrap();
    rig.git(&wip, &["init", "--bare", "-b", "main"]);
    let url = format!("file://{}", wip.display());
    rig.git(
        &rig.home,
        &[
            "push",
            "-q",
            &url,
            &format!("{tip}:refs/heads/fd/alpha/one"),
        ],
    );
    // A remote-tracking ref of `push_remote` is not listed with a WIP remote.
    rig.track("fd/alpha/ignored", &tip);
    let view = rig.view(Some(url));
    assert!(rig.list(&view).await.branches.is_empty());
    let answer = rig.fetch(&view, 7).await;
    assert_eq!(answer.code, None, "{:?}", answer.text);
    let list = rig.list(&view).await;
    assert_eq!(names(&list), ["fd/alpha/one"]);
    // The count is against `<push_remote>/<default>`.
    assert!(list.default_oid.is_some());
}

#[tokio::test]
async fn without_a_default_branch_the_counts_are_zero_and_a_long_subject_is_cut() {
    let Some(rig) = Rig::new() else { return };
    // Nothing was pushed: `origin/main` does not exist.
    let subject = "s".repeat(300);
    let oid = rig.commit(&subject, 1_000_000_100);
    rig.track("fd/alpha/one", &oid);
    let list = rig.list(&rig.view(None)).await;
    assert_eq!(list.default_branch, "main");
    assert_eq!(list.default_oid, None);
    let one = &list.branches[0];
    assert_eq!((one.ahead, one.behind), (0, 0));
    assert_eq!(one.subject.chars().count(), REMOTE_BRANCH_SUBJECT_MAX);
}

#[tokio::test]
async fn a_failed_fetch_is_the_code_and_text_of_the_answer() {
    let Some(rig) = Rig::new() else { return };
    rig.base();
    rig.git(
        &rig.home,
        &[
            "remote",
            "set-url",
            "origin",
            &rig.root.join("missing.git").to_string_lossy(),
        ],
    );
    let answer = rig.fetch(&rig.view(None), 9).await;
    assert_eq!(answer.fetched_at, None);
    assert_eq!(answer.code.as_deref(), Some(code::FETCH_FAILED));
    assert!(answer.text.unwrap().starts_with("The fetch failed: "));
}

#[tokio::test]
async fn a_listing_past_its_time_limit_is_an_internal_error() {
    let Some(rig) = Rig::new() else { return };
    rig.base();
    let view = rig.view(None);
    let options = GitOptions::isolated();
    let env = RadarEnv {
        options: &options,
        repo_dir: &rig.home,
        view: &view,
        allow_local_remotes: true,
    };
    let refusal = list_with(&env, Duration::ZERO).await.unwrap_err();
    assert_eq!(refusal.code, code::INTERNAL);
    assert_eq!(refusal.text, "The branch list took more than 18 s.");
}
