//! Tests of the fetch, the worktree and the publication ref of a move (FG-9,
//! design §12.3 step 4, M8 and M9), with real Git (skipped below Git 2.45).

use std::collections::HashSet;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::AtomicU32;
use std::time::Duration;

use fleetdeck_proto::fleet_git::{LocalRemotes, MovePublication, code, remote_identity};

use super::{
    PushDestination, add_move_worktree, check_publication, check_publication_values,
    fetch_publication, pick_publish_ref, private_ref,
};
use crate::workspace::GitOptions;
use crate::workspace::branches::parse_git_version;

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
        fs::write(
            &program,
            "#!/bin/sh\nexport GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1\nexec git \"$@\"\n",
        )
        .unwrap();
        fs::set_permissions(&program, fs::Permissions::from_mode(0o755)).unwrap();
        Some(Self {
            _dir: dir,
            root,
            program,
        })
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

    fn bare(&self, name: &str) -> PathBuf {
        let path = self.root.join(name);
        fs::create_dir(&path).unwrap();
        self.git(&path, &["init", "--bare", "-b", "main"]);
        path
    }

    fn commit(&self, repo: &Path, text: &str) -> String {
        fs::write(repo.join("a.txt"), format!("{text}\n")).unwrap();
        self.git(repo, &["add", "."]);
        self.git(repo, &["commit", "-m", text]);
        self.git(repo, &["rev-parse", "HEAD"])
    }

    /// A home with one commit and the remote `origin` at `url`.
    fn home(&self, url: &str) -> PathBuf {
        let path = self.root.join("home");
        fs::create_dir(&path).unwrap();
        self.git(&path, &["init", "-b", "main"]);
        self.commit(&path, "base");
        self.git(&path, &["remote", "add", "origin", url]);
        path
    }

    /// A source repository that pushed `branch` (one commit on top of one
    /// base commit) to `bare`; returns `(tip, base)`.
    fn publish(&self, bare: &Path, branch: &str) -> (String, String) {
        let seed = self.root.join(format!("seed-{}", branch.replace('/', "-")));
        fs::create_dir(&seed).unwrap();
        self.git(&seed, &["init", "-b", "main"]);
        let base = self.commit(&seed, "one");
        let tip = self.commit(&seed, "two");
        self.git(
            &seed,
            &[
                "push",
                "-q",
                bare.to_str().unwrap(),
                &format!("HEAD:refs/heads/{branch}"),
            ],
        );
        (tip, base)
    }
}

fn file_url(path: &Path) -> String {
    format!("file://{}", path.display())
}

fn dest_of(url: &str) -> String {
    remote_identity(url, LocalRemotes::Allow).unwrap().dest
}

fn publication(branch: &str, dest: &str, oid: &str) -> MovePublication {
    MovePublication {
        local_branch: branch.to_owned(),
        dest: dest.to_owned(),
        ref_name: branch.to_owned(),
        oid: oid.to_owned(),
    }
}

fn group() -> Arc<AtomicU32> {
    Arc::new(AtomicU32::new(0))
}

/// The set of the three trees of an M8 test: a project remote, a WIP
/// remote that holds the publication, the home, and the publication.
struct Moved {
    env: Env,
    home: PathBuf,
    wip_url: String,
    publication: MovePublication,
    base: String,
}

fn moved() -> Option<Moved> {
    let env = Env::new()?;
    let remote = env.bare("remote.git");
    let wip = env.bare("wip.git");
    let home = env.home(&file_url(&remote));
    let wip_url = file_url(&wip);
    let (tip, base) = env.publish(&wip, "fd/alpha/cs");
    let publication = publication("fd/alpha/cs", &dest_of(&wip_url), &tip);
    Some(Moved {
        env,
        home,
        wip_url,
        publication,
        base,
    })
}

fn wip(url: &str) -> PushDestination {
    PushDestination::Wip {
        url: url.to_owned(),
    }
}

#[tokio::test]
async fn the_publication_is_fetched_from_the_wip_url_into_the_private_ref_and_the_ref_is_deleted_after_worktree_add()
 {
    let Some(m) = moved() else { return };
    let options = m.env.options();
    let url = check_publication(
        &options,
        &m.home,
        &wip(&m.wip_url),
        &dest_of(&m.wip_url),
        &m.publication,
        true,
        "bravo",
    )
    .await
    .unwrap();
    assert_eq!(url.url, m.wip_url);
    fetch_publication(&options, &group(), &m.home, &url.url, &m.publication, "m1")
        .await
        .unwrap();
    let private = private_ref("m1");
    assert_eq!(
        m.env.git(&m.home, &["rev-parse", &private]),
        m.publication.oid
    );
    let path = m.env.root.join("home--fd-cs");
    add_move_worktree(
        &options,
        &m.home,
        &path,
        &m.publication,
        "m1",
        "bravo",
        "origin",
    )
    .await
    .unwrap();
    // The private ref is gone after `worktree add`.
    assert_eq!(
        m.env
            .git(&m.home, &["for-each-ref", "refs/fleetdeck/move/"]),
        ""
    );
}

#[tokio::test]
async fn a_publication_on_another_destination_gives_move_publication_mismatch() {
    let Some(m) = moved() else { return };
    let other = m.env.bare("other.git");
    let other_dest = dest_of(&file_url(&other));
    let mut publication = m.publication.clone();
    publication.dest = other_dest.clone();
    let err = check_publication(
        &m.env.options(),
        &m.home,
        &wip(&m.wip_url),
        &dest_of(&m.wip_url),
        &publication,
        true,
        "bravo",
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, code::MOVE_PUBLICATION_MISMATCH);
    assert!(err.text.contains(&other_dest), "{}", err.text);
    assert!(
        err.text.contains("bravo pushes this project to"),
        "{}",
        err.text
    );
}

#[tokio::test]
async fn a_moved_ref_gives_move_branch_moved() {
    let Some(m) = moved() else { return };
    let mut publication = m.publication.clone();
    publication.oid = m.base.clone();
    let err = fetch_publication(
        &m.env.options(),
        &group(),
        &m.home,
        &m.wip_url,
        &publication,
        "m2",
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, code::MOVE_BRANCH_MOVED);
}

#[tokio::test]
async fn a_fetch_of_a_missing_ref_gives_fetch_failed() {
    let Some(m) = moved() else { return };
    let mut publication = m.publication.clone();
    publication.ref_name = "fd/alpha/other".to_owned();
    let err = fetch_publication(
        &m.env.options(),
        &group(),
        &m.home,
        &m.wip_url,
        &publication,
        "m3",
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, code::FETCH_FAILED);
}

#[tokio::test]
async fn the_target_worktree_is_on_the_source_branch() {
    let Some(m) = moved() else { return };
    let options = m.env.options();
    fetch_publication(
        &options,
        &group(),
        &m.home,
        &m.wip_url,
        &m.publication,
        "m4",
    )
    .await
    .unwrap();
    let path = m.env.root.join("home--fd-cs");
    add_move_worktree(
        &options,
        &m.home,
        &path,
        &m.publication,
        "m4",
        "bravo",
        "origin",
    )
    .await
    .unwrap();
    assert_eq!(
        m.env.git(&path, &["symbolic-ref", "HEAD"]),
        "refs/heads/fd/alpha/cs"
    );
    assert_eq!(m.env.git(&path, &["rev-parse", "HEAD"]), m.publication.oid);
}

#[tokio::test]
async fn a_local_branch_with_the_source_name_gives_move_branch_exists() {
    let Some(m) = moved() else { return };
    let options = m.env.options();
    fetch_publication(
        &options,
        &group(),
        &m.home,
        &m.wip_url,
        &m.publication,
        "m5",
    )
    .await
    .unwrap();
    m.env.git(&m.home, &["branch", "fd/alpha/cs"]);
    let path = m.env.root.join("home--fd-cs");
    let err = add_move_worktree(
        &options,
        &m.home,
        &path,
        &m.publication,
        "m5",
        "bravo",
        "origin",
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, code::MOVE_BRANCH_EXISTS);
    assert!(
        err.text.contains("fd/alpha/cs") && err.text.contains("bravo"),
        "{}",
        err.text
    );
    assert!(!path.exists(), "no worktree is made");
}

#[tokio::test]
async fn a_namespace_block_gives_branch_namespace_blocked() {
    let Some(m) = moved() else { return };
    let options = m.env.options();
    fetch_publication(
        &options,
        &group(),
        &m.home,
        &m.wip_url,
        &m.publication,
        "m6",
    )
    .await
    .unwrap();
    // A local branch named `fd/alpha` blocks `fd/alpha/cs` (a ref D/F conflict).
    m.env.git(&m.home, &["branch", "fd/alpha"]);
    let path = m.env.root.join("home--fd-cs");
    let err = add_move_worktree(
        &options,
        &m.home,
        &path,
        &m.publication,
        "m6",
        "bravo",
        "origin",
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, code::BRANCH_NAMESPACE_BLOCKED);
}

#[tokio::test]
async fn the_publish_ref_is_the_first_free_name_and_takes_2_when_the_remote_has_the_first() {
    let Some(env) = Env::new() else { return };
    let bare = env.bare("remote.git");
    let home = env.home(&file_url(&bare));
    let url = file_url(&bare);
    let used = HashSet::new();
    let first = pick_publish_ref(&env.options(), &home, &url, "bravo", "cs", &used)
        .await
        .unwrap();
    assert_eq!(first, "fd/bravo/cs");
    env.publish(&bare, "fd/bravo/cs");
    let second = pick_publish_ref(&env.options(), &home, &url, "bravo", "cs", &used)
        .await
        .unwrap();
    assert_eq!(second, "fd/bravo/cs-2");
    // A ref below the name takes it too (a ref D/F conflict).
    env.publish(&bare, "fd/bravo/cs-2/x");
    let third = pick_publish_ref(&env.options(), &home, &url, "bravo", "cs", &used)
        .await
        .unwrap();
    assert_eq!(third, "fd/bravo/cs-3");
}

#[tokio::test]
async fn a_used_name_in_git_pushes_is_skipped() {
    let Some(env) = Env::new() else { return };
    let bare = env.bare("remote.git");
    let home = env.home(&file_url(&bare));
    let used: HashSet<String> = ["fd/bravo/cs".to_owned()].into();
    let got = pick_publish_ref(
        &env.options(),
        &home,
        &file_url(&bare),
        "bravo",
        "cs",
        &used,
    )
    .await
    .unwrap();
    assert_eq!(got, "fd/bravo/cs-2");
}

#[tokio::test]
async fn all_99_names_taken_gives_move_ref_exhausted() {
    let Some(env) = Env::new() else { return };
    let bare = env.bare("remote.git");
    let home = env.home(&file_url(&bare));
    let mut used: HashSet<String> = (2..=99).map(|n| format!("fd/bravo/cs-{n}")).collect();
    used.insert("fd/bravo/cs".to_owned());
    let err = pick_publish_ref(
        &env.options(),
        &home,
        &file_url(&bare),
        "bravo",
        "cs",
        &used,
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, code::MOVE_REF_EXHAUSTED);
    assert!(err.text.contains("fd/bravo/cs-99"), "{}", err.text);
}

#[tokio::test]
async fn a_failed_remote_list_gives_fetch_failed_and_picks_no_name() {
    let Some(env) = Env::new() else { return };
    let bare = env.bare("remote.git");
    let home = env.home(&file_url(&bare));
    let missing = file_url(&env.root.join("missing.git"));
    let err = pick_publish_ref(
        &env.options(),
        &home,
        &missing,
        "bravo",
        "cs",
        &HashSet::new(),
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, code::FETCH_FAILED);
}

#[test]
fn the_values_from_the_source_are_checked() {
    let good = publication("fd/alpha/cs", "file:///x", &"a".repeat(40));
    assert!(check_publication_values(&good).is_ok());
    for bad in [
        publication("main", "file:///x", &"a".repeat(40)),
        publication("fd/alpha/../x", "file:///x", &"a".repeat(40)),
        publication("fd/alpha/cs", "", &"a".repeat(40)),
        publication("fd/alpha/cs", "file:///x", "HEAD"),
        publication("fd/alpha/cs", "file:///x", &"A".repeat(40)),
    ] {
        let err = check_publication_values(&bad).unwrap_err();
        assert_eq!(err.code, code::INVALID_PARAMS, "{bad:?}");
    }
    let mut bad_ref = good.clone();
    bad_ref.ref_name = "refs/heads/x y".to_owned();
    assert!(check_publication_values(&bad_ref).is_err());
}
