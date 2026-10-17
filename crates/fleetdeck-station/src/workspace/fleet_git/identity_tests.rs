use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use fleetdeck_proto::fleet_git::{
    GitProjectView, GitSettings, HomeCheckState, LocalRemotes, RemoteIdentity, code,
    remote_identity,
};

use super::*;
use crate::workspace::branches::parse_git_version;

/// A temporary folder with a Git that ignores the user configuration.
struct Env {
    _dir: tempfile::TempDir,
    root: PathBuf,
    program: PathBuf,
}

fn git_new_enough() -> bool {
    let out = Command::new("git").arg("--version").output().unwrap();
    parse_git_version(&String::from_utf8_lossy(&out.stdout)).is_some_and(|v| v >= (2, 45))
}

impl Env {
    /// None (and a printed reason) when the Git of the machine is older than 2.45.
    fn new() -> Option<Self> {
        if !git_new_enough() {
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
        self.options_for(self.program.clone())
    }

    fn options_for(&self, program: PathBuf) -> GitOptions {
        GitOptions {
            program,
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

    /// A clone-like home: one commit, the remote `origin` at `url`.
    fn home(&self, name: &str, url: &str) -> PathBuf {
        let path = self.root.join(name);
        fs::create_dir(&path).unwrap();
        self.git(&path, &["init", "-b", "main"]);
        fs::write(path.join("a.txt"), "a\n").unwrap();
        self.git(&path, &["add", "."]);
        self.git(&path, &["commit", "-m", "one"]);
        self.git(&path, &["remote", "add", "origin", url]);
        path
    }
}

fn file_url(path: &Path) -> String {
    format!("file://{}", path.display())
}

fn identity(url: &str) -> RemoteIdentity {
    remote_identity(url, LocalRemotes::Allow).unwrap()
}

fn view(project: RemoteIdentity, remote: &str) -> GitProjectView {
    GitProjectView {
        project_id: "p1".to_owned(),
        project_name: "app".to_owned(),
        settings_revision: 1,
        settings: GitSettings {
            sync: true,
            push_remote: remote.to_owned(),
            ..GitSettings::default()
        },
        remote: project,
        forge: None,
        clone_url: "https://example.com/acme/app.git".to_owned(),
        home_generation: 3,
        home_path: String::new(),
        radar_home: false,
    }
}

async fn check(
    env: &Env,
    home: &Path,
    project: &RemoteIdentity,
    allow: bool,
) -> fleetdeck_proto::fleet_git::HomeCheck {
    check_home(
        &env.options(),
        home,
        &view(project.clone(), "origin"),
        allow,
    )
    .await
}

#[tokio::test]
async fn a_home_with_the_project_remote_is_verified() {
    let Some(env) = Env::new() else { return };
    let remote = env.bare("remote.git");
    let url = file_url(&remote);
    let home = env.home("home", &url);
    let got = check(&env, &home, &identity(&url), true).await;
    assert_eq!(got.state, HomeCheckState::Verified, "{got:?}");
    assert_eq!(got.project_id, "p1");
    assert_eq!(got.generation, 3);
    assert_eq!(
        got.canonical_path,
        Some(home.to_string_lossy().into_owned())
    );
    assert_eq!(got.code, None);
}

#[tokio::test]
async fn a_local_remote_is_refused_without_the_setting() {
    let Some(env) = Env::new() else { return };
    let remote = env.bare("remote.git");
    let url = file_url(&remote);
    let home = env.home("home", &url);
    let got = check(&env, &home, &identity(&url), false).await;
    assert_eq!(got.state, HomeCheckState::Refused);
    assert_eq!(got.code.as_deref(), Some(code::REMOTE_LOCAL_REFUSED));
}

#[tokio::test]
async fn a_local_remote_must_be_bare_and_canonical() {
    let Some(env) = Env::new() else { return };
    let plain = env.root.join("plain");
    fs::create_dir(&plain).unwrap();
    env.git(&plain, &["init", "-b", "main"]);
    let url = file_url(&plain);
    let home = env.home("home", &url);
    let got = check(&env, &home, &identity(&url), true).await;
    assert_eq!(got.state, HomeCheckState::Refused);
    assert_eq!(got.code.as_deref(), Some(code::REMOTE_LOCAL_REFUSED));

    let remote = env.bare("remote.git");
    symlink(&remote, env.root.join("link.git")).unwrap();
    let url = file_url(&env.root.join("link.git"));
    let home = env.home("home2", &url);
    let got = check(&env, &home, &identity(&url), true).await;
    assert_eq!(got.state, HomeCheckState::Refused);
    assert_eq!(got.code.as_deref(), Some(code::REMOTE_PATH_INVALID));
}

#[tokio::test]
async fn a_missing_folder_and_a_plain_folder_are_not_homes() {
    let Some(env) = Env::new() else { return };
    let project = identity("https://example.com/acme/app.git");
    let got = check(&env, &env.root.join("none"), &project, true).await;
    assert_eq!(got.state, HomeCheckState::Missing);
    assert_eq!(got.canonical_path, None);

    let plain = env.root.join("plain");
    fs::create_dir(&plain).unwrap();
    let got = check(&env, &plain, &project, true).await;
    assert_eq!(got.state, HomeCheckState::NotARepo);

    // A folder inside a repository is not the top of it.
    let remote = env.bare("remote.git");
    let home = env.home("home", &file_url(&remote));
    fs::create_dir(home.join("sub")).unwrap();
    let got = check(&env, &home.join("sub"), &project, true).await;
    assert_eq!(got.state, HomeCheckState::NotARepo);
}

#[tokio::test]
async fn a_home_without_the_remote_has_no_remote() {
    let Some(env) = Env::new() else { return };
    let remote = env.bare("remote.git");
    let url = file_url(&remote);
    let home = env.home("home", &url);
    env.git(&home, &["remote", "rename", "origin", "upstream"]);
    let got = check(&env, &home, &identity(&url), true).await;
    assert_eq!(got.state, HomeCheckState::NoRemote);
    assert_eq!(got.code.as_deref(), Some(code::NO_REMOTE));
}

#[tokio::test]
async fn two_pushurls_or_two_urls_are_ambiguous() {
    let Some(env) = Env::new() else { return };
    let remote = env.bare("remote.git");
    let second = env.bare("second.git");
    let url = file_url(&remote);
    let project = identity(&url);

    let home = env.home("home", &url);
    env.git(&home, &["config", "--add", "remote.origin.pushurl", &url]);
    env.git(
        &home,
        &[
            "config",
            "--add",
            "remote.origin.pushurl",
            &file_url(&second),
        ],
    );
    let got = check(&env, &home, &project, true).await;
    assert_eq!(got.state, HomeCheckState::RemoteAmbiguous);
    assert_eq!(got.code.as_deref(), Some(code::REMOTE_AMBIGUOUS));

    let home = env.home("home2", &url);
    env.git(
        &home,
        &["config", "--add", "remote.origin.url", &file_url(&second)],
    );
    let got = check(&env, &home, &project, true).await;
    assert_eq!(got.state, HomeCheckState::RemoteAmbiguous);
}

#[tokio::test]
async fn a_pushurl_to_another_repository_is_a_mismatch() {
    let Some(env) = Env::new() else { return };
    let remote = env.bare("remote.git");
    let other = env.bare("other.git");
    let url = file_url(&remote);
    let home = env.home("home", &url);
    env.git(
        &home,
        &["config", "remote.origin.pushurl", &file_url(&other)],
    );
    let got = check(&env, &home, &identity(&url), true).await;
    assert_eq!(got.state, HomeCheckState::Mismatch);
    assert_eq!(got.seen, Some(identity(&file_url(&other))));
}

#[tokio::test]
async fn another_project_destination_is_a_mismatch() {
    let Some(env) = Env::new() else { return };
    let remote = env.bare("remote.git");
    let home = env.home("home", &file_url(&remote));
    let got = check(
        &env,
        &home,
        &identity("https://example.com/acme/app.git"),
        true,
    )
    .await;
    assert_eq!(got.state, HomeCheckState::Mismatch);
    assert_eq!(got.seen, Some(identity(&file_url(&remote))));
}

#[tokio::test]
async fn a_rewrite_rule_that_matches_the_push_url_again_is_refused() {
    let Some(env) = Env::new() else { return };
    let remote = env.bare("remote.git");
    let url = file_url(&remote);
    let home = env.home("home", &url);
    let key = format!("url.{url}.pushInsteadOf");
    env.git(&home, &["config", &key, &url]);
    let got = check(&env, &home, &identity(&url), true).await;
    assert_eq!(got.state, HomeCheckState::Refused);
    assert_eq!(got.code.as_deref(), Some(code::REMOTE_REWRITTEN));
}

#[tokio::test]
async fn an_escaped_path_is_refused_and_no_git_push_url_is_used() {
    let Some(env) = Env::new() else { return };
    let remote = env.bare("remote.git");
    let project = identity(&file_url(&remote));
    for url in ["ssh://alice@h/~/repo%2Fother", "alice@h:repo%2Fother"] {
        let home = env.home(&format!("home-{}", url.len()), url);
        let got = check(&env, &home, &project, true).await;
        assert_eq!(got.state, HomeCheckState::Refused, "{url}");
        assert_eq!(got.code.as_deref(), Some(code::REMOTE_UNSUPPORTED), "{url}");
    }
}

#[tokio::test]
async fn an_old_git_is_refused() {
    let Some(env) = Env::new() else { return };
    let old = env.root.join("old-git");
    fs::write(&old, "#!/bin/sh\necho 'git version 2.30.0'\n").unwrap();
    fs::set_permissions(&old, fs::Permissions::from_mode(0o755)).unwrap();
    let remote = env.bare("remote.git");
    let url = file_url(&remote);
    let home = env.home("home", &url);
    let got = check_home(
        &env.options_for(old),
        &home,
        &view(identity(&url), "origin"),
        true,
    )
    .await;
    assert_eq!(got.state, HomeCheckState::Refused);
    assert_eq!(got.code.as_deref(), Some(code::GIT_TOO_OLD));
}

fn remote_destination(project: &RemoteIdentity) -> PushDestination {
    PushDestination::Remote {
        name: "origin".to_owned(),
        project: project.clone(),
        clone_url: "https://example.com/acme/app.git".to_owned(),
    }
}

#[tokio::test]
async fn the_push_remote_gives_its_verified_url() {
    let Some(env) = Env::new() else { return };
    let remote = env.bare("remote.git");
    let url = file_url(&remote);
    let home = env.home("home", &url);
    let project = identity(&url);
    let got = effective_push_url(
        &env.options(),
        &home,
        &remote_destination(&project),
        &project.dest,
        true,
    )
    .await
    .unwrap();
    assert_eq!(
        got,
        VerifiedUrl {
            url,
            dest: project.dest.clone()
        }
    );

    // A lease never moves to another destination.
    let err = effective_push_url(
        &env.options(),
        &home,
        &remote_destination(&project),
        "ssh://alice@h:22/~/repo",
        true,
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, code::REMOTE_MISMATCH);
    assert!(err.text.contains("no longer points to"), "{}", err.text);
}

#[tokio::test]
async fn a_second_pushurl_refuses_the_push_remote() {
    let Some(env) = Env::new() else { return };
    let remote = env.bare("remote.git");
    let second = env.bare("second.git");
    let url = file_url(&remote);
    let home = env.home("home", &url);
    env.git(&home, &["config", "--add", "remote.origin.pushurl", &url]);
    env.git(
        &home,
        &[
            "config",
            "--add",
            "remote.origin.pushurl",
            &file_url(&second),
        ],
    );
    let project = identity(&url);
    let err = effective_push_url(
        &env.options(),
        &home,
        &remote_destination(&project),
        &project.dest,
        true,
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, code::REMOTE_AMBIGUOUS);
}

#[tokio::test]
async fn a_missing_push_remote_names_the_fix() {
    let Some(env) = Env::new() else { return };
    let remote = env.bare("remote.git");
    let url = file_url(&remote);
    let home = env.home("home", &url);
    env.git(&home, &["remote", "remove", "origin"]);
    let project = identity(&url);
    let err = effective_push_url(
        &env.options(),
        &home,
        &remote_destination(&project),
        &project.dest,
        true,
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, code::NO_REMOTE);
    assert!(
        err.text
            .contains("git remote add origin https://example.com/acme/app.git"),
        "{}",
        err.text
    );
}

#[tokio::test]
async fn the_wip_url_is_verified_against_the_rewrite_rules() {
    let Some(env) = Env::new() else { return };
    let remote = env.bare("remote.git");
    let wip = env.bare("wip.git");
    let other = env.bare("other.git");
    let home = env.home("home", &file_url(&remote));
    let wip_url = file_url(&wip);
    let target = identity(&wip_url).dest;
    let wip_dest = PushDestination::Wip {
        url: wip_url.clone(),
    };

    let got = effective_push_url(&env.options(), &home, &wip_dest, &target, true)
        .await
        .unwrap();
    assert_eq!(got.url, wip_url);

    // Another destination than the lease's: refused.
    let err = effective_push_url(&env.options(), &home, &wip_dest, "https://h:443/x", true)
        .await
        .unwrap_err();
    assert_eq!(err.code, code::REMOTE_REWRITTEN);

    // A rule that rewrites the WIP URL to another repository: refused.
    let key = format!("url.{}.pushInsteadOf", file_url(&other));
    env.git(&home, &["config", &key, &wip_url]);
    let err = effective_push_url(&env.options(), &home, &wip_dest, &target, true)
        .await
        .unwrap_err();
    assert_eq!(err.code, code::REMOTE_REWRITTEN);
    assert!(err.text.contains("rewrites"), "{}", err.text);
}

#[tokio::test]
async fn a_wip_url_that_a_rule_would_rewrite_again_is_refused() {
    let Some(env) = Env::new() else { return };
    let remote = env.bare("remote.git");
    let wip = env.bare("wip.git");
    let home = env.home("home", &file_url(&remote));
    let wip_url = file_url(&wip);
    let key = format!("url.{wip_url}.insteadOf");
    env.git(&home, &["config", &key, &wip_url]);
    let err = effective_push_url(
        &env.options(),
        &home,
        &PushDestination::Wip {
            url: wip_url.clone(),
        },
        &identity(&wip_url).dest,
        true,
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, code::REMOTE_REWRITTEN);
}

#[tokio::test]
async fn an_escaped_wip_url_and_a_local_wip_url_without_the_setting_are_refused() {
    let Some(env) = Env::new() else { return };
    let remote = env.bare("remote.git");
    let wip = env.bare("wip.git");
    let home = env.home("home", &file_url(&remote));
    let err = effective_push_url(
        &env.options(),
        &home,
        &PushDestination::Wip {
            url: "alice@h:repo%2Fother".to_owned(),
        },
        "ssh://alice@h:22/~/repo",
        true,
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, code::REMOTE_UNSUPPORTED);
    let wip_url = file_url(&wip);
    let err = effective_push_url(
        &env.options(),
        &home,
        &PushDestination::Wip {
            url: wip_url.clone(),
        },
        &identity(&wip_url).dest,
        false,
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, code::REMOTE_LOCAL_REFUSED);
}
