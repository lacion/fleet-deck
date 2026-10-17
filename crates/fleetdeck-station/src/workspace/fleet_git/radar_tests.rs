//! The radar run on the station (design §10.2, §10.7), with real Git.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use fleetdeck_proto::fleet_git::{
    GitProjectView, GitSettings, RADAR_DEFAULT_KEY, RadarBranch, RadarResult, RemoteIdentity, code,
};

use super::*;
use crate::workspace::branches::parse_git_version;

struct Rig {
    _dir: tempfile::TempDir,
    root: PathBuf,
    program: PathBuf,
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
        let program = root.join("git-wrapper");
        let rig = Self {
            bare: root.join("remote.git"),
            home: root.join("home"),
            _dir: dir,
            root,
            program,
        };
        rig.script(&rig.program, "exec git \"$@\"");
        fs::create_dir(&rig.bare).unwrap();
        rig.git(&rig.bare, &["init", "--bare", "-b", "main"]);
        fs::create_dir(&rig.home).unwrap();
        rig.git(&rig.home, &["init", "-b", "main"]);
        rig.git(
            &rig.home,
            &["remote", "add", "origin", &rig.bare.to_string_lossy()],
        );
        let text: String = (1..=20).map(|n| format!("line {n}\n")).collect();
        rig.write(&rig.home, "a.txt", &text);
        rig.write(&rig.home, "b.txt", "b\n");
        rig.write(&rig.home, "c.txt", "c\n");
        rig.commit(&rig.home, "base");
        rig.git(&rig.home, &["push", "origin", "main"]);
        Some(rig)
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

    fn options(&self) -> GitOptions {
        GitOptions {
            program: self.program.clone(),
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

    fn write(&self, repo: &Path, file: &str, text: &str) {
        fs::write(repo.join(file), text).unwrap();
    }

    fn commit(&self, repo: &Path, message: &str) {
        self.git(repo, &["add", "."]);
        self.git(repo, &["commit", "-m", message]);
    }

    /// A branch `name` off `main` that rewrites `file` with `text`, pushed to
    /// `target` (a remote name or a URL) as `refs/heads/fd/alpha/<name>`.
    fn branch(&self, name: &str, file: &str, text: &str, target: &str) {
        self.git(&self.home, &["checkout", "-q", "-b", "tmp", "main"]);
        self.write(&self.home, file, text);
        self.commit(&self.home, name);
        self.git(
            &self.home,
            &["push", target, &format!("tmp:refs/heads/fd/alpha/{name}")],
        );
        self.git(&self.home, &["checkout", "-q", "main"]);
        self.git(&self.home, &["branch", "-q", "-D", "tmp"]);
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

    async fn run(&self, view: &GitProjectView, keys: &[&str]) -> RadarResult {
        self.run_with_limits(view, keys, LIMITS).await
    }

    async fn run_with_limits(
        &self,
        view: &GitProjectView,
        keys: &[&str],
        limits: Limits,
    ) -> RadarResult {
        let options = self.options();
        let env = RadarEnv {
            options: &options,
            repo_dir: &self.home,
            view,
            allow_local_remotes: true,
        };
        run_with(&env, &branches(keys), 777, limits).await
    }
}

/// Branches `fd/alpha/<key>`, in the given order (newest first).
fn branches(keys: &[&str]) -> Vec<RadarBranch> {
    keys.iter()
        .map(|k| RadarBranch {
            key: (*k).to_owned(),
            ref_name: format!("fd/alpha/{k}"),
        })
        .collect()
}

fn lines(n: usize, edit: &[(usize, &str)]) -> String {
    (1..=n)
        .map(|i| {
            let text = edit
                .iter()
                .find(|(at, _)| *at == i)
                .map_or(format!("line {i}"), |(_, t)| (*t).to_owned());
            format!("{text}\n")
        })
        .collect()
}

fn pair<'a>(
    result: &'a RadarResult,
    a: &str,
    b: &str,
) -> &'a fleetdeck_proto::fleet_git::RadarPair {
    result
        .pairs
        .iter()
        .find(|p| p.a == a && p.b == b)
        .unwrap_or_else(|| panic!("no pair {a} {b}: {:?}", result.pairs))
}

#[tokio::test]
async fn two_branches_that_change_the_same_lines_conflict_with_that_file() {
    let Some(rig) = Rig::new() else { return };
    rig.branch("one", "a.txt", &lines(20, &[(5, "from one")]), "origin");
    rig.branch("two", "a.txt", &lines(20, &[(5, "from two")]), "origin");
    let result = rig.run(&rig.view(None), &["two", "one"]).await;
    assert_eq!(result.code, None, "{:?}", result.text);
    assert!(result.base_oid.is_some());
    assert_eq!(result.checked_at, 777);
    let both = pair(&result, "two", "one");
    assert!(both.conflict);
    assert_eq!(both.files, ["a.txt"]);
    assert!(!both.files_truncated);
    // Each branch is clean against the default branch.
    assert!(!pair(&result, "one", RADAR_DEFAULT_KEY).conflict);
    assert!(!pair(&result, "two", RADAR_DEFAULT_KEY).conflict);
    assert_eq!(result.skipped, 0);
}

#[tokio::test]
async fn two_branches_that_change_different_files_do_not_conflict() {
    let Some(rig) = Rig::new() else { return };
    rig.branch("one", "b.txt", "one\n", "origin");
    rig.branch("two", "c.txt", "two\n", "origin");
    let result = rig.run(&rig.view(None), &["two", "one"]).await;
    let both = pair(&result, "two", "one");
    assert!(!both.conflict);
    assert!(both.files.is_empty());
}

#[tokio::test]
async fn a_branch_that_changes_other_lines_of_the_same_file_does_not_conflict() {
    let Some(rig) = Rig::new() else { return };
    rig.branch("one", "a.txt", &lines(20, &[(2, "from one")]), "origin");
    rig.branch("two", "a.txt", &lines(20, &[(18, "from two")]), "origin");
    let result = rig.run(&rig.view(None), &["two", "one"]).await;
    assert!(!pair(&result, "two", "one").conflict);
}

#[tokio::test]
async fn a_branch_that_conflicts_with_the_default_branch_is_found() {
    let Some(rig) = Rig::new() else { return };
    rig.branch("one", "a.txt", &lines(20, &[(5, "from one")]), "origin");
    rig.write(&rig.home, "a.txt", &lines(20, &[(5, "from main")]));
    rig.commit(&rig.home, "main moves");
    rig.git(&rig.home, &["push", "origin", "main"]);
    let result = rig.run(&rig.view(None), &["one"]).await;
    let base = pair(&result, "one", RADAR_DEFAULT_KEY);
    assert!(base.conflict);
    assert_eq!(base.files, ["a.txt"]);
}

#[tokio::test]
async fn the_pairs_run_against_the_default_first_then_by_the_newer_branch_and_stop_at_fifty() {
    let Some(rig) = Rig::new() else { return };
    let keys: Vec<String> = (0..11).map(|n| format!("b{n}")).collect();
    for key in &keys {
        rig.branch(key, "b.txt", &format!("{key}\n"), "origin");
    }
    let keys: Vec<&str> = keys.iter().map(String::as_str).collect();
    let result = rig.run(&rig.view(None), &keys).await;
    // 11 against the default branch and 55 pairs: 66, the first 50 run.
    assert_eq!(result.pairs.len(), 50);
    assert_eq!(result.skipped, 16);
    for (i, key) in keys.iter().enumerate() {
        assert_eq!(
            (result.pairs[i].a.as_str(), result.pairs[i].b.as_str()),
            (*key, RADAR_DEFAULT_KEY)
        );
    }
    assert_eq!(
        (result.pairs[11].a.as_str(), result.pairs[11].b.as_str()),
        ("b0", "b1")
    );
    assert_eq!(
        (result.pairs[12].a.as_str(), result.pairs[12].b.as_str()),
        ("b0", "b2")
    );
    assert_eq!(
        (result.pairs[21].a.as_str(), result.pairs[21].b.as_str()),
        ("b1", "b2")
    );
    // b0 x b1 conflict on b.txt (both rewrite it).
    assert!(result.pairs[11].conflict);
}

#[tokio::test]
async fn a_run_that_hits_its_time_limit_reports_the_rest_as_skipped() {
    let Some(rig) = Rig::new() else { return };
    rig.branch("one", "b.txt", "one\n", "origin");
    rig.branch("two", "c.txt", "two\n", "origin");
    let limits = Limits {
        pair: Duration::from_secs(20),
        run: Duration::ZERO,
    };
    let result = rig
        .run_with_limits(&rig.view(None), &["two", "one"], limits)
        .await;
    assert_eq!(result.code, None);
    assert!(result.pairs.is_empty());
    assert_eq!(result.skipped, 3);
    assert!(result.base_oid.is_some());
}

#[tokio::test]
async fn a_branch_that_is_not_in_the_fetched_refs_is_left_out_and_not_counted() {
    let Some(rig) = Rig::new() else { return };
    rig.branch("one", "b.txt", "one\n", "origin");
    let result = rig.run(&rig.view(None), &["ghost", "one"]).await;
    assert_eq!(result.pairs.len(), 1);
    assert_eq!(result.pairs[0].a, "one");
    assert_eq!(result.skipped, 0);
}

#[tokio::test]
async fn a_pair_whose_merge_tree_fails_is_skipped() {
    let Some(rig) = Rig::new() else { return };
    rig.branch("one", "b.txt", "one\n", "origin");
    // An orphan branch has no merge base with the others.
    rig.git(&rig.home, &["checkout", "-q", "--orphan", "lone"]);
    rig.git(&rig.home, &["rm", "-rfq", "."]);
    rig.write(&rig.home, "z.txt", "z\n");
    rig.commit(&rig.home, "orphan");
    rig.git(
        &rig.home,
        &["push", "origin", "lone:refs/heads/fd/alpha/lone"],
    );
    rig.git(&rig.home, &["checkout", "-q", "main"]);
    let result = rig.run(&rig.view(None), &["lone", "one"]).await;
    assert_eq!(result.code, None);
    assert!(
        result
            .pairs
            .iter()
            .all(|p| !(p.a == "lone" || p.b == "lone")),
        "{:?}",
        result.pairs
    );
    // lone x default, and lone x one: both skipped; one x default ran.
    assert_eq!(result.skipped, 2);
    assert_eq!(result.pairs.len(), 1);
}

#[tokio::test]
async fn a_fetch_that_fails_gives_the_code_and_no_pair() {
    let Some(rig) = Rig::new() else { return };
    let mut view = rig.view(None);
    view.settings.push_remote = "nowhere".to_owned();
    let result = rig.run(&view, &["one"]).await;
    assert_eq!(result.code.as_deref(), Some(code::FETCH_FAILED));
    assert!(
        result
            .text
            .as_deref()
            .is_some_and(|t| t.starts_with("The fetch failed: ")),
        "{:?}",
        result.text
    );
    assert!(result.pairs.is_empty());
    assert_eq!(result.base_oid, None);
}

#[tokio::test]
async fn a_remote_without_a_default_branch_gives_no_default_branch() {
    let Some(rig) = Rig::new() else { return };
    let empty = rig.root.join("empty.git");
    fs::create_dir(&empty).unwrap();
    rig.git(&empty, &["init", "--bare", "-b", "main"]);
    rig.git(
        &rig.home,
        &["remote", "add", "bare2", &empty.to_string_lossy()],
    );
    let mut view = rig.view(None);
    view.settings.push_remote = "bare2".to_owned();
    let result = rig.run(&view, &[]).await;
    assert_eq!(result.code.as_deref(), Some(code::NO_DEFAULT_BRANCH));
}

#[tokio::test]
async fn a_git_older_than_2_45_gives_git_too_old() {
    let Some(rig) = Rig::new() else { return };
    let old = rig.root.join("git-old");
    rig.script(&old, "echo 'git version 2.30.0'");
    let options = GitOptions {
        program: old,
        ..rig.options()
    };
    let view = rig.view(None);
    let env = RadarEnv {
        options: &options,
        repo_dir: &rig.home,
        view: &view,
        allow_local_remotes: true,
    };
    let result = run_radar(&env, &[], 5).await;
    assert_eq!(result.code.as_deref(), Some(code::GIT_TOO_OLD));
    assert_eq!(result.text.as_deref(), Some("Git is older than 2.45."));
}

#[tokio::test]
async fn with_a_wip_remote_the_branches_come_from_the_wip_refs() {
    let Some(rig) = Rig::new() else { return };
    let wip = rig.root.join("wip.git");
    fs::create_dir(&wip).unwrap();
    rig.git(&wip, &["init", "--bare", "-b", "main"]);
    let url = format!("file://{}", wip.display());
    // The branches exist only on the WIP remote.
    rig.branch("one", "a.txt", &lines(20, &[(5, "from one")]), &url);
    rig.branch("two", "a.txt", &lines(20, &[(5, "from two")]), &url);
    let result = rig.run(&rig.view(Some(url)), &["two", "one"]).await;
    assert_eq!(result.code, None, "{:?}", result.text);
    assert!(pair(&result, "two", "one").conflict);
    // The default branch comes from `push_remote`.
    assert!(!pair(&result, "one", RADAR_DEFAULT_KEY).conflict);
}

#[tokio::test]
async fn a_wip_url_that_is_a_local_remote_is_refused_unless_the_station_allows_it() {
    let Some(rig) = Rig::new() else { return };
    let wip = rig.root.join("wip.git");
    fs::create_dir(&wip).unwrap();
    rig.git(&wip, &["init", "--bare", "-b", "main"]);
    let view = rig.view(Some(format!("file://{}", wip.display())));
    let options = rig.options();
    let env = RadarEnv {
        options: &options,
        repo_dir: &rig.home,
        view: &view,
        allow_local_remotes: false,
    };
    let result = run_radar(&env, &branches(&["one"]), 5).await;
    assert_eq!(result.code.as_deref(), Some(code::REMOTE_LOCAL_REFUSED));
    assert!(result.pairs.is_empty());
}

#[test]
fn only_plain_session_branches_are_valid() {
    let ok = |n: &str| branch_name_ok(n);
    assert!(ok("fd/alpha/falcon"));
    assert!(ok("fd/alpha/falcon-2"));
    for bad in [
        "main",
        "fd/alpha",
        "fd//falcon",
        "fd/alpha/../x",
        "fd/alpha/a b",
        "fd/alpha/a:b",
        "fd/alpha/x.lock",
        "fd/alpha/.hidden",
        "fd/alpha/end.",
        "other/alpha/x",
        "fd/alpha/a\nb",
    ] {
        assert!(!ok(bad), "{bad:?}");
    }
}

#[test]
fn the_branch_list_is_checked() {
    let good = branches(&["a", "b"]);
    assert_eq!(check_branches(&good), None);
    let many: Vec<RadarBranch> = (0..51)
        .map(|n| RadarBranch {
            key: format!("k{n}"),
            ref_name: format!("fd/alpha/k{n}"),
        })
        .collect();
    assert_eq!(check_branches(&many), Some("At most 50 branches."));
    assert!(check_branches(&branches(&["a", "a"])).is_some());
    let default_key = [RadarBranch {
        key: RADAR_DEFAULT_KEY.to_owned(),
        ref_name: "fd/alpha/a".to_owned(),
    }];
    assert!(check_branches(&default_key).is_some());
    let bad_ref = [RadarBranch {
        key: "k".to_owned(),
        ref_name: "main".to_owned(),
    }];
    assert_eq!(
        check_branches(&bad_ref),
        Some("The branch name is not valid.")
    );
}

#[test]
fn one_run_of_a_home_at_a_time() {
    let path = Path::new("/radar-test/home-a");
    let first = RadarGuard::try_acquire(path).unwrap();
    assert!(RadarGuard::try_acquire(path).is_none());
    assert!(RadarGuard::try_acquire(Path::new("/radar-test/home-b")).is_some());
    drop(first);
    assert!(RadarGuard::try_acquire(path).is_some());
}

#[test]
fn the_conflict_files_stop_at_the_empty_line_and_at_one_hundred() {
    let (files, cut) = conflict_files("tree\na.txt\nb.txt\na.txt\n\nCONFLICT message\n");
    assert_eq!(files, ["a.txt", "b.txt"]);
    assert!(!cut);
    let many: String = (0..120).map(|n| format!("f{n}\n")).collect();
    let (files, cut) = conflict_files(&format!("tree\n{many}"));
    assert_eq!(files.len(), 100);
    assert!(cut);
}
