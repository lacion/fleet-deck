//! The Git work of a land (FG-8, design §11.5 steps 1 to 4) with real Git and
//! a bare remote on the same machine.

use std::ffi::OsString;
use std::fs;
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use super::*;
use crate::workspace::branches::parse_git_version;
use crate::workspace::fleet_git::{ensure_dir, temp_path};

struct Repos {
    _dir: tempfile::TempDir,
    root: PathBuf,
    bare: PathBuf,
    home: PathBuf,
    /// A `git` wrapper without the user configuration, with an identity.
    options: GitOptions,
    /// The same without an identity.
    anonymous: GitOptions,
}

fn wrapper(path: &Path, identity: bool) -> GitOptions {
    let ident = if identity {
        "export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@example.com \
         GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@example.com"
    } else {
        "unset GIT_AUTHOR_NAME GIT_AUTHOR_EMAIL GIT_COMMITTER_NAME GIT_COMMITTER_EMAIL EMAIL"
    };
    fs::write(
        path,
        format!(
            "#!/bin/sh\nexport GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1\n{ident}\nexec git \"$@\"\n"
        ),
    )
    .unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    GitOptions {
        program: path.to_path_buf(),
        ..GitOptions::isolated()
    }
}

impl Repos {
    /// A bare remote with `main` (one commit), and a home with `origin` set to
    /// it. `None` when Git is older than 2.45.
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
        let repos = Self {
            options: wrapper(&root.join("git-id"), true),
            anonymous: wrapper(&root.join("git-anon"), false),
            _dir: dir,
            root,
            bare,
            home,
        };
        repos.git(&repos.bare, &["init", "--bare", "-b", "main"]);
        repos.git(&repos.home, &["init", "-b", "main"]);
        repos.commit("a.txt", "one");
        let url = format!("file://{}", repos.bare.display());
        repos.git(&repos.home, &["remote", "add", "origin", &url]);
        repos.git(&repos.home, &["push", "origin", "main"]);
        Some(repos)
    }

    fn git(&self, dir: &Path, args: &[&str]) -> String {
        let out = Command::new(&self.options.program)
            .current_dir(dir)
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_owned()
    }

    /// A commit in the home that writes `file`.
    fn commit(&self, file: &str, text: &str) -> String {
        fs::write(self.home.join(file), format!("{text}\n")).unwrap();
        self.git(&self.home, &["add", "."]);
        self.git(&self.home, &["commit", "-m", text]);
        self.git(&self.home, &["rev-parse", "HEAD"])
    }

    /// The session branch `fd/alpha/x` with one commit on top of `main`; the
    /// home is back on `main`.
    fn branch(&self, file: &str, text: &str) -> String {
        self.git(&self.home, &["checkout", "-b", "fd/alpha/x"]);
        let oid = self.commit(file, text);
        self.git(&self.home, &["checkout", "main"]);
        oid
    }

    /// A new commit on the remote `main`, made in a second clone.
    fn move_remote(&self, file: &OsString, text: &str) -> String {
        let other = self.root.join("other");
        if !other.exists() {
            let url = format!("file://{}", self.bare.display());
            self.git(&self.root, &["clone", &url, "other"]);
        }
        fs::write(other.join(file), format!("{text}\n")).unwrap();
        self.git(&other, &["add", "."]);
        self.git(&other, &["commit", "-m", text]);
        self.git(&other, &["push", "origin", "main"]);
        self.git(&other, &["rev-parse", "HEAD"])
    }

    async fn captured(&self) -> Result<Captured, Refusal> {
        capture(
            &self.options,
            &self.home,
            "origin",
            None,
            "fd/alpha/x",
            None,
        )
        .await
    }

    /// The temporary worktree of a captured land.
    async fn temp(&self, captured: &Captured) -> PathBuf {
        let root = self.root.clone();
        tokio::task::spawn_blocking(move || ensure_dir(&root))
            .await
            .unwrap()
            .unwrap();
        let temp = temp_path(&self.root, "land-0123456789", 1);
        add_worktree(&self.options, &self.home, &temp, &captured.source_oid)
            .await
            .unwrap();
        temp
    }
}

#[tokio::test]
async fn the_capture_fixes_the_default_branch_and_both_commits() {
    let Some(repos) = Repos::new() else { return };
    let source = repos.branch("b.txt", "work");
    let main = repos.git(&repos.home, &["rev-parse", "main"]);
    let got = repos.captured().await.unwrap();
    assert_eq!(got.default_ref, "main");
    assert_eq!(got.base_oid, main);
    assert_eq!(got.source_oid, source);
}

#[tokio::test]
async fn a_branch_without_new_commits_is_nothing_to_land() {
    let Some(repos) = Repos::new() else { return };
    repos.git(&repos.home, &["branch", "fd/alpha/x"]);
    let refusal = repos.captured().await.unwrap_err();
    assert_eq!(refusal.code, code::NOTHING_TO_LAND);
    assert_eq!(
        refusal.text,
        "The branch has no commits that are not on origin/main."
    );
}

#[tokio::test]
async fn a_default_branch_that_the_remote_lacks_is_refused() {
    let Some(repos) = Repos::new() else { return };
    repos.branch("b.txt", "work");
    let refusal = capture(
        &repos.options,
        &repos.home,
        "origin",
        Some("trunk"),
        "fd/alpha/x",
        None,
    )
    .await
    .unwrap_err();
    assert_eq!(refusal.code, code::NO_DEFAULT_BRANCH);
}

#[tokio::test]
async fn a_failed_fetch_is_fetch_failed_with_a_redacted_line() {
    let Some(repos) = Repos::new() else { return };
    repos.branch("b.txt", "work");
    fs::remove_dir_all(&repos.bare).unwrap();
    let refusal = repos.captured().await.unwrap_err();
    assert_eq!(refusal.code, code::FETCH_FAILED);
    assert!(
        refusal.text.starts_with("The fetch failed: "),
        "{}",
        refusal.text
    );
}

#[tokio::test]
async fn the_rebase_puts_the_session_commits_on_the_captured_base() {
    let Some(repos) = Repos::new() else { return };
    let source = repos.branch("b.txt", "work");
    let moved = repos.move_remote(&OsString::from("c.txt"), "elsewhere");
    let got = repos.captured().await.unwrap();
    assert_eq!(got.base_oid, moved);
    let temp = repos.temp(&got).await;
    assert!(has_identity(&repos.options, &temp).await);
    let Rebased::Done { rebased_oid } = rebase(&repos.options, &temp, &got.base_oid).await.unwrap()
    else {
        panic!("a clean rebase");
    };
    assert_ne!(rebased_oid, source);
    let parent = repos.git(&temp, &["rev-parse", "HEAD^"]);
    assert_eq!(parent, moved);
    assert!(
        tested_unchanged(&repos.options, &temp, &rebased_oid)
            .await
            .unwrap()
    );
    // The session branch of the home did not move.
    assert_eq!(repos.git(&repos.home, &["rev-parse", "fd/alpha/x"]), source);
}

#[tokio::test]
async fn a_conflict_lists_its_files_aborts_the_rebase_and_changes_no_ref() {
    let Some(repos) = Repos::new() else { return };
    let odd = OsString::from_vec(b"bad\xffname.txt".to_vec());
    let newline = OsString::from("a\nb.txt");
    // The session adds three files; the remote adds the same names with other
    // text.
    repos.git(&repos.home, &["checkout", "-b", "fd/alpha/x"]);
    for name in [&OsString::from("plain.txt"), &odd, &newline] {
        fs::write(repos.home.join(name), "session\n").unwrap();
    }
    repos.git(&repos.home, &["add", "."]);
    repos.git(&repos.home, &["commit", "-m", "session"]);
    repos.git(&repos.home, &["checkout", "main"]);
    let remote_before = {
        for name in [&OsString::from("plain.txt"), &odd, &newline] {
            repos.move_remote(name, "remote");
        }
        repos.git(&repos.bare, &["for-each-ref"])
    };
    let got = repos.captured().await.unwrap();
    let temp = repos.temp(&got).await;
    let Rebased::Conflict { files } = rebase(&repos.options, &temp, &got.base_oid).await.unwrap()
    else {
        panic!("a conflict");
    };
    let mut files = files;
    files.sort();
    assert_eq!(files, vec!["a\nb.txt", "bad\\xffname.txt", "plain.txt"]);
    // `git rebase --abort` ran: the admin entry has no rebase state.
    let admin = repos.git(&temp, &["rev-parse", "--absolute-git-dir"]);
    assert!(!Path::new(&admin).join("rebase-merge").exists());
    assert!(!Path::new(&admin).join("rebase-apply").exists());
    assert_eq!(repos.git(&repos.bare, &["for-each-ref"]), remote_before);
}

#[tokio::test]
async fn git_without_an_identity_is_reported() {
    let Some(repos) = Repos::new() else { return };
    assert!(has_identity(&repos.options, &repos.home).await);
    assert!(!has_identity(&repos.anonymous, &repos.home).await);
}

#[tokio::test]
async fn a_check_that_edits_a_file_or_moves_head_is_a_change() {
    let Some(repos) = Repos::new() else { return };
    repos.branch("b.txt", "work");
    let got = repos.captured().await.unwrap();
    let temp = repos.temp(&got).await;
    let Rebased::Done { rebased_oid } = rebase(&repos.options, &temp, &got.base_oid).await.unwrap()
    else {
        panic!("a clean rebase");
    };
    // An untracked file is allowed.
    fs::write(temp.join("build.out"), "x").unwrap();
    assert!(
        tested_unchanged(&repos.options, &temp, &rebased_oid)
            .await
            .unwrap()
    );
    // A tracked file that changed is not.
    fs::write(temp.join("b.txt"), "edited\n").unwrap();
    assert!(
        !tested_unchanged(&repos.options, &temp, &rebased_oid)
            .await
            .unwrap()
    );
    repos.git(&temp, &["commit", "-am", "more"]);
    assert!(
        !tested_unchanged(&repos.options, &temp, &rebased_oid)
            .await
            .unwrap()
    );
}

#[test]
fn the_conflict_list_has_at_most_200_paths_of_4096_bytes() {
    let mut out = Vec::new();
    for n in 0..250 {
        out.extend_from_slice(format!("f{n}").as_bytes());
        out.push(0);
    }
    assert_eq!(conflict_files(&out).len(), 200);
    let mut long = vec![b'x'; 5000];
    long.push(0);
    assert_eq!(conflict_files(&long)[0].len(), 4096);
}

#[test]
fn a_byte_that_is_not_utf8_is_written_as_hex() {
    assert_eq!(conflict_files(b"a\xff\xfeb\0"), vec!["a\\xff\\xfeb"]);
    assert_eq!(conflict_files("é\0".as_bytes()), vec!["é"]);
}
