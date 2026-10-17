//! `project.ensure_home` and `project.home_status` (design §8.4, §8.9).

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use fleetdeck_proto::fleet_git::{LocalRemotes, RemoteIdentity};
use fleetdeck_testkit::Det;

use super::*;
use crate::store::OfflineApprovalPolicy;
use crate::store::git_state::tests::store as new_store;
use crate::workspace::branches::parse_git_version;

struct Rig {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    program: PathBuf,
    store: Store,
    policy: Arc<Policy>,
    /// A bare repository with one commit on `main`.
    bare: PathBuf,
}

fn git_new_enough() -> bool {
    let out = Command::new("git").arg("--version").output().unwrap();
    parse_git_version(&String::from_utf8_lossy(&out.stdout)).is_some_and(|v| v >= (2, 45))
}

impl Rig {
    /// `None` (with a printed reason) when the Git of the machine is older than 2.45.
    async fn new() -> Option<Self> {
        Self::with_policy(|p| p.with_allow_local_remotes(true)).await
    }

    async fn with_policy(f: impl FnOnce(Policy) -> Policy) -> Option<Self> {
        if !git_new_enough() {
            eprintln!("skipped: Fleet Git needs Git 2.45 or newer");
            return None;
        }
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let program = root.join("git-wrapper");
        fs::write(
            &program,
            "#!/bin/sh\nexport GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1\nexec git \"$@\"\n",
        )
        .unwrap();
        fs::set_permissions(&program, fs::Permissions::from_mode(0o755)).unwrap();
        let policy = Policy::new(
            vec![root.clone()],
            4,
            OfflineApprovalPolicy::Wait,
            BTreeMap::new(),
        );
        let rig = Self {
            store: new_store(&Det::new()).await,
            policy: Arc::new(f(policy)),
            bare: root.join("remote.git"),
            _tmp: tmp,
            root,
            program,
        };
        rig.make_bare(&rig.bare, true);
        Some(rig)
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

    /// A bare repository at `path`, with one commit when `commit`.
    fn make_bare(&self, path: &Path, commit: bool) {
        fs::create_dir_all(path).unwrap();
        self.git(path, &["init", "--bare", "-b", "main"]);
        if commit {
            let work = self.root.join(format!(
                "work-{}",
                path.file_name().unwrap().to_string_lossy()
            ));
            fs::create_dir(&work).unwrap();
            self.git(&work, &["init", "-b", "main"]);
            fs::write(work.join("a.txt"), "a\n").unwrap();
            self.git(&work, &["add", "."]);
            self.git(&work, &["commit", "-m", "one"]);
            self.git(&work, &["push", &path.to_string_lossy(), "main"]);
        }
    }

    fn url(&self) -> String {
        format!("file://{}", self.bare.display())
    }

    fn projects(&self) -> PathBuf {
        self.root.join("projects")
    }

    fn options(&self) -> GitOptions {
        GitOptions {
            program: self.program.clone(),
            timeout: Duration::from_secs(60),
            isolated: true,
            confine: None,
        }
    }

    fn params(&self, project: &str, name: &str, attempt: &str, url: &str) -> EnsureHomeParams {
        let mut p = EnsureHomeParams {
            project_id: project.to_owned(),
            project_name: name.to_owned(),
            folder: home_folder_name(name),
            url: url.to_owned(),
            remote: remote_identity(url, LocalRemotes::Allow).unwrap(),
            remote_name: "origin".to_owned(),
            generation: 1,
            attempt_id: attempt.to_owned(),
            request_digest: String::new(),
            retry_of: None,
        };
        sign(&mut p);
        p
    }

    async fn ensure(&self, p: EnsureHomeParams) -> Result<HomeStatus, HomeRefusal> {
        ensure_home(&self.store, Arc::clone(&self.policy), self.options(), p).await
    }

    /// The status of `attempt` once the clone ended.
    async fn settle(&self, project: &str, attempt: &str) -> HomeStatus {
        for _ in 0..600 {
            let params = HomeStatusParams {
                project_id: project.to_owned(),
                attempt_id: attempt.to_owned(),
            };
            let status = home_status(&self.store, params).await.unwrap();
            if status.state != HomeAttemptState::Cloning {
                return status;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("the clone did not end");
    }

    /// A complete clone of the bare repository at `dest`.
    fn clone_to(&self, dest: &Path) {
        fs::create_dir_all(dest.parent().unwrap()).unwrap();
        let parent = dest.parent().unwrap();
        self.git(parent, &["clone", &self.url(), &dest.to_string_lossy()]);
    }
}

fn sign(p: &mut EnsureHomeParams) {
    p.request_digest = ensure_home_digest(p);
}

fn invalid_text(r: Result<HomeStatus, HomeRefusal>) -> String {
    let err = r.unwrap_err();
    assert_eq!(err.code, code::INVALID_PARAMS, "{}", err.text);
    err.text
}

#[tokio::test]
async fn a_clone_goes_into_clone_root_and_ends_ready_with_a_verified_path() {
    let Some(rig) = Rig::new().await else { return };
    let dest = rig.projects().join("app");
    let first = rig
        .ensure(rig.params("p1", "app", "a1", &rig.url()))
        .await
        .unwrap();
    // The answer comes at once, with the reserved folder.
    assert_eq!(first.state, HomeAttemptState::Cloning);
    assert_eq!(first.reserved_path.as_deref(), dest.to_str());
    assert_eq!(first.path, None);
    assert_eq!((first.attempt_id.as_str(), first.generation), ("a1", 1));
    let done = rig.settle("p1", "a1").await;
    assert_eq!(done.state, HomeAttemptState::Ready, "{done:?}");
    assert_eq!(done.path.as_deref(), dest.to_str());
    assert_eq!(done.reserved_path, None);
    assert!(dest.join(".git").exists() && dest.join("a.txt").exists());
    assert_eq!(rig.git(&dest, &["remote"]), "origin");
    // clone_root was made with mode 0700.
    let mode = fs::metadata(rig.projects()).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o700);
}

#[tokio::test]
async fn an_existing_complete_clone_is_adopted_and_no_clone_runs() {
    let Some(rig) = Rig::new().await else { return };
    let dest = rig.projects().join("app");
    rig.clone_to(&dest);
    let marker = dest.join(".git").join("HEAD");
    let before = fs::metadata(&marker).unwrap().modified().unwrap();
    let status = rig
        .ensure(rig.params("p1", "app", "a1", &rig.url()))
        .await
        .unwrap();
    assert_eq!(status.state, HomeAttemptState::Adopted, "{status:?}");
    assert_eq!(status.path.as_deref(), dest.to_str());
    assert_eq!(fs::metadata(&marker).unwrap().modified().unwrap(), before);
}

#[tokio::test]
async fn a_folder_that_is_not_a_complete_clone_is_a_conflict() {
    let Some(rig) = Rig::new().await else { return };
    let repo = "localhost/".to_owned() + rig.bare.strip_prefix("/").unwrap().to_str().unwrap();
    // An empty folder.
    fs::create_dir_all(rig.projects().join("one")).unwrap();
    let one = rig
        .ensure(rig.params("p1", "one", "a1", &rig.url()))
        .await
        .unwrap();
    assert_eq!(one.state, HomeAttemptState::Failed);
    assert_eq!(one.code.as_deref(), Some(code::HOME_CONFLICT));
    let shown = rig.projects().join("one");
    assert_eq!(
        one.message.unwrap(),
        format!(
            "{} exists and is not a complete clone of {repo}.",
            shown.display()
        )
    );
    assert_eq!(one.reserved_path.as_deref(), shown.to_str());
    // A clone of another repository.
    let other = rig.root.join("other.git");
    rig.make_bare(&other, true);
    let two = rig.projects().join("two");
    rig.git(
        &rig.root,
        &[
            "clone",
            &format!("file://{}", other.display()),
            &two.to_string_lossy(),
        ],
    );
    let status = rig
        .ensure(rig.params("p2", "two", "a2", &rig.url()))
        .await
        .unwrap();
    assert_eq!(status.state, HomeAttemptState::Failed);
    assert_eq!(status.code.as_deref(), Some(code::HOME_CONFLICT));
    // A link at the folder is not a real folder.
    let real = rig.root.join("realclone");
    rig.clone_to(&real);
    std::os::unix::fs::symlink(&real, rig.projects().join("three")).unwrap();
    let status = rig
        .ensure(rig.params("p3", "three", "a3", &rig.url()))
        .await
        .unwrap();
    assert_eq!(status.state, HomeAttemptState::Failed);
    assert_eq!(status.code.as_deref(), Some(code::HOME_CONFLICT));
}

#[tokio::test]
async fn a_repeat_with_the_same_attempt_returns_the_stored_result() {
    let Some(rig) = Rig::new().await else { return };
    let p = rig.params("p1", "app", "a1", &rig.url());
    rig.ensure(p.clone()).await.unwrap();
    let done = rig.settle("p1", "a1").await;
    assert_eq!(done.state, HomeAttemptState::Ready);
    let again = rig.ensure(p.clone()).await.unwrap();
    assert_eq!(again, done);
    // The same generation without a retry returns the same attempt.
    let mut second = rig.params("p1", "app", "a2", &rig.url());
    second.generation = 1;
    sign(&mut second);
    assert_eq!(rig.ensure(second).await.unwrap(), done);
    // Another digest under the same attempt id.
    let mut other = p;
    other.generation = 1;
    other.remote_name = "upstream".to_owned();
    sign(&mut other);
    let err = rig.ensure(other).await.unwrap_err();
    assert_eq!(err.code, code::ATTEMPT_REUSED);
    assert_eq!(err.text, "This attempt id was used for another request.");
}

#[tokio::test]
async fn a_second_project_for_the_same_folder_is_destination_reserved() {
    let Some(rig) = Rig::new().await else { return };
    rig.ensure(rig.params("p1", "app", "a1", &rig.url()))
        .await
        .unwrap();
    let err = rig
        .ensure(rig.params("p2", "app", "b1", &rig.url()))
        .await
        .unwrap_err();
    assert_eq!(err.code, code::DESTINATION_RESERVED);
    assert!(
        err.text
            .contains("is reserved by project p1 on this station."),
        "{}",
        err.text
    );
    // The folder is held after the clone too.
    rig.settle("p1", "a1").await;
    let err = rig
        .ensure(rig.params("p2", "app", "b2", &rig.url()))
        .await
        .unwrap_err();
    assert_eq!(err.code, code::DESTINATION_RESERVED);
}

#[tokio::test]
async fn a_failed_clone_is_retried_after_the_cause_is_fixed() {
    let Some(rig) = Rig::new().await else { return };
    // The remote is a folder that is not a repository yet.
    let broken = rig.root.join("broken.git");
    fs::create_dir(&broken).unwrap();
    let url = format!("file://{}", broken.display());
    let mut p = rig.params("p1", "app", "a1", &url);
    rig.ensure(p.clone()).await.unwrap();
    let failed = rig.settle("p1", "a1").await;
    assert_eq!(failed.state, HomeAttemptState::Failed);
    assert_eq!(failed.code.as_deref(), Some(code::CLONE_FAILED));
    assert!(failed.message.unwrap().starts_with("The clone failed: "));
    let dest = rig.projects().join("app");
    assert_eq!(failed.reserved_path.as_deref(), dest.to_str());
    // Fix the remote, then retry with the folder gone: the old attempt is cleared.
    fs::remove_dir(&broken).unwrap();
    rig.make_bare(&broken, true);
    p.attempt_id = "a2".to_owned();
    p.retry_of = Some("a1".to_owned());
    sign(&mut p);
    let again = rig.ensure(p).await.unwrap();
    assert_eq!(again.state, HomeAttemptState::Cloning);
    let done = rig.settle("p1", "a2").await;
    assert_eq!(done.state, HomeAttemptState::Ready, "{done:?}");
    let old = rig
        .store
        .home_attempt("p1".into(), "a1".into())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(old.state, HomeAttemptState::Cleared);
}

#[tokio::test]
async fn a_retry_of_an_interrupted_attempt_never_adopts_the_folder_that_is_there() {
    let Some(rig) = Rig::new().await else { return };
    let dest = rig.projects().join("app");
    // The station stopped during a clone: the folder holds a complete clone
    // of the right origin, and the attempt is `interrupted`.
    let p = rig.params("p1", "app", "a1", &rig.url());
    rig.store
        .home_begin(NewHomeAttempt {
            attempt_id: "a1".into(),
            project_id: "p1".into(),
            generation: 1,
            request_digest: p.request_digest.clone(),
            retry_of: None,
            dest_path: dest.to_string_lossy().into_owned(),
            url: rig.url(),
            remote_name: "origin".into(),
        })
        .await
        .unwrap();
    rig.clone_to(&dest);
    assert_eq!(rig.store.home_interrupt_cloning().await.unwrap(), 1);
    let status = home_status(
        &rig.store,
        HomeStatusParams {
            project_id: "p1".into(),
            attempt_id: "a1".into(),
        },
    )
    .await
    .unwrap();
    assert_eq!(status.state, HomeAttemptState::Interrupted);
    assert_eq!(status.reserved_path.as_deref(), dest.to_str());
    assert_eq!(status.code.as_deref(), Some(code::CLONE_INTERRUPTED));
    // The retry is refused; nothing is adopted and no new attempt exists.
    let mut retry = p;
    retry.attempt_id = "a2".into();
    retry.retry_of = Some("a1".into());
    sign(&mut retry);
    let err = rig.ensure(retry.clone()).await.unwrap_err();
    assert_eq!(err.code, code::INCOMPLETE_FOLDER);
    assert!(
        err.text
            .contains("holds an incomplete clone. Remove it, then retry.")
    );
    assert!(
        rig.store
            .home_attempt("p1".into(), "a2".into())
            .await
            .unwrap()
            .is_none()
    );
    // The owner removes the folder: the retry clones.
    fs::remove_dir_all(&dest).unwrap();
    let again = rig.ensure(retry).await.unwrap();
    assert_eq!(again.state, HomeAttemptState::Cloning);
    let done = rig.settle("p1", "a2").await;
    assert_eq!(done.state, HomeAttemptState::Ready, "{done:?}");
}

#[tokio::test]
async fn a_folder_removed_after_a_failure_is_cleared_for_the_next_generation() {
    let Some(rig) = Rig::new().await else { return };
    let dest = rig.projects().join("app");
    let broken = rig.root.join("broken.git");
    fs::create_dir(&broken).unwrap();
    let url = format!("file://{}", broken.display());
    rig.ensure(rig.params("p1", "app", "a1", &url))
        .await
        .unwrap();
    assert_eq!(rig.settle("p1", "a1").await.state, HomeAttemptState::Failed);
    // The generation 2 clones into the same folder (it is absent).
    let mut g2 = rig.params("p1", "app", "a2", &rig.url());
    g2.generation = 2;
    sign(&mut g2);
    rig.ensure(g2).await.unwrap();
    let old = rig
        .store
        .home_attempt("p1".into(), "a1".into())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(old.state, HomeAttemptState::Cleared);
    assert_eq!(old.code.as_deref(), Some(code::DESTINATION_CLEARED));
    assert_eq!(rig.settle("p1", "a2").await.state, HomeAttemptState::Ready);
    assert!(dest.is_dir());
}

#[tokio::test]
async fn a_remote_name_other_than_origin_is_the_name_of_the_clone() {
    let Some(rig) = Rig::new().await else { return };
    let mut p = rig.params("p1", "app", "a1", &rig.url());
    let origin_digest = p.request_digest.clone();
    p.remote_name = "upstream".to_owned();
    sign(&mut p);
    assert_ne!(
        p.request_digest, origin_digest,
        "the digest covers remote_name"
    );
    rig.ensure(p).await.unwrap();
    let done = rig.settle("p1", "a1").await;
    assert_eq!(done.state, HomeAttemptState::Ready, "{done:?}");
    let dest = rig.projects().join("app");
    assert_eq!(rig.git(&dest, &["remote"]), "upstream");
}

#[tokio::test]
async fn an_adopted_folder_without_the_remote_name_fails() {
    let Some(rig) = Rig::new().await else { return };
    let dest = rig.projects().join("app");
    rig.clone_to(&dest);
    let mut p = rig.params("p1", "app", "a1", &rig.url());
    p.remote_name = "upstream".to_owned();
    sign(&mut p);
    let status = rig.ensure(p).await.unwrap();
    assert_eq!(status.state, HomeAttemptState::Failed);
    assert_eq!(status.code.as_deref(), Some(code::HOME_CONFLICT));
    assert_eq!(
        status.message.unwrap(),
        format!("{} exists but has no remote upstream.", dest.display())
    );
}

#[tokio::test]
async fn every_validation_refusal_has_its_text_in_order() {
    let Some(rig) = Rig::new().await else { return };
    let base = rig.params("p1", "app", "a1", &rig.url());
    let with = |f: &dyn Fn(&mut EnsureHomeParams)| {
        let mut p = base.clone();
        f(&mut p);
        sign(&mut p);
        p
    };
    // 1. The project id.
    let long_id = "x".repeat(129);
    for id in ["", long_id.as_str(), "a\nb"] {
        let p = with(&|p| id.clone_into(&mut p.project_id));
        assert_eq!(
            invalid_text(rig.ensure(p).await),
            "The project id is not valid."
        );
    }
    // 2. The project name.
    let long_name = "n".repeat(201);
    for name in ["", long_name.as_str()] {
        let p = with(&|p| name.clone_into(&mut p.project_name));
        assert_eq!(
            invalid_text(rig.ensure(p).await),
            "The project name is not valid."
        );
    }
    // 3. The folder.
    let p = with(&|p| "other".clone_into(&mut p.folder));
    assert_eq!(
        invalid_text(rig.ensure(p).await),
        "The folder name does not match the project name."
    );
    // 4. The remote name.
    let p = with(&|p| "-x".clone_into(&mut p.remote_name));
    assert_eq!(
        invalid_text(rig.ensure(p).await),
        "The push remote must be a remote name, for example origin."
    );
    // 5. The URL: a password, and a local remote that this station refuses.
    let p = with(&|p| "https://u:pw@h.example/a/b.git".clone_into(&mut p.url));
    assert!(invalid_text(rig.ensure(p).await).contains("credentials"));
    let closed = Rig::with_policy(|p| p).await.unwrap();
    let local = closed.params("p1", "app", "a1", &closed.url());
    assert_eq!(
        invalid_text(closed.ensure(local).await),
        "Local remotes are allowed only on stations with allow_local_remotes = true."
    );
    // 6. The identity of the remote: another one, and an error of the identity.
    let p = with(&|p| {
        p.remote = RemoteIdentity {
            repo: "x/y".to_owned(),
            dest: "https://x:443/y".to_owned(),
        };
    });
    assert_eq!(
        invalid_text(rig.ensure(p).await),
        "The remote identity does not match the URL."
    );
    let p = with(&|p| "https://h.example/a%2Fb.git".clone_into(&mut p.url));
    assert_eq!(
        invalid_text(rig.ensure(p).await),
        "This remote URL has an escaped path. Use a plain path."
    );
    // 7. The digest.
    let mut p = base.clone();
    p.request_digest = "0".repeat(64);
    let err = rig.ensure(p).await.unwrap_err();
    assert_eq!(err.code, code::ATTEMPT_REUSED);
    // Nothing was stored by a refusal.
    assert!(
        rig.store
            .home_attempt("p1".into(), "a1".into())
            .await
            .unwrap()
            .is_none()
    );
    // The order: an invalid id wins over an invalid name.
    let p = with(&|p| {
        p.project_id.clear();
        p.project_name.clear();
    });
    assert_eq!(
        invalid_text(rig.ensure(p).await),
        "The project id is not valid."
    );
}

#[tokio::test]
async fn an_unknown_attempt_gets_the_link_error() {
    let Some(rig) = Rig::new().await else { return };
    let err = home_status(
        &rig.store,
        HomeStatusParams {
            project_id: "p1".into(),
            attempt_id: "nope".into(),
        },
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, code::UNKNOWN_ATTEMPT);
    assert_eq!(err.text, "The station does not know this home attempt.");
    // An attempt of another project is unknown too.
    rig.ensure(rig.params("p1", "app", "a1", &rig.url()))
        .await
        .unwrap();
    let err = home_status(
        &rig.store,
        HomeStatusParams {
            project_id: "p2".into(),
            attempt_id: "a1".into(),
        },
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, code::UNKNOWN_ATTEMPT);
}

#[tokio::test]
async fn a_clone_root_that_cannot_be_made_is_a_link_error_and_stores_nothing() {
    let Some(rig) = Rig::with_policy(|p| {
        let missing = p.clone_root().join("deeper/still");
        p.with_allow_local_remotes(true).with_clone_root(missing)
    })
    .await
    else {
        return;
    };
    let err = rig
        .ensure(rig.params("p1", "app", "a1", &rig.url()))
        .await
        .unwrap_err();
    assert_eq!(err.code, code::CLONE_ROOT_MISSING);
    assert!(
        err.text.starts_with("The clone folder ")
            && err.text.ends_with(" does not exist and cannot be made."),
        "{}",
        err.text
    );
    assert!(
        rig.store
            .home_attempt("p1".into(), "a1".into())
            .await
            .unwrap()
            .is_none()
    );
    // A clone folder outside the allowed roots is refused too.
    let outside = tempfile::tempdir().unwrap();
    let out_root = outside.path().to_path_buf();
    let Some(rig) =
        Rig::with_policy(|p| p.with_allow_local_remotes(true).with_clone_root(out_root)).await
    else {
        return;
    };
    let err = rig
        .ensure(rig.params("p1", "app", "a1", &rig.url()))
        .await
        .unwrap_err();
    assert_eq!(err.code, code::CLONE_ROOT_MISSING);
}

#[tokio::test]
async fn the_default_clone_root_is_made_below_the_allowed_root() {
    let Some(rig) = Rig::new().await else { return };
    // `projects` is the default clone_root: its parent is the allowed root.
    assert!(!rig.projects().exists());
    rig.ensure(rig.params("p1", "app", "a1", &rig.url()))
        .await
        .unwrap();
    assert!(rig.projects().is_dir());
    rig.settle("p1", "a1").await;
}

#[tokio::test]
async fn the_git_version_is_the_text_after_git_version() {
    let Some(rig) = Rig::new().await else { return };
    let version = git_version(&rig.options()).await.unwrap();
    assert!(
        version.chars().next().unwrap().is_ascii_digit(),
        "{version}"
    );
    assert!(!version.starts_with("git version"));
}
