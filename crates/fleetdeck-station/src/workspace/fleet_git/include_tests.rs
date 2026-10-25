use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use super::*;
use crate::workspace::branches::parse_git_version;

struct Env {
    _dir: tempfile::TempDir,
    /// The main checkout of the home.
    home: PathBuf,
    /// The new worktree.
    wt: PathBuf,
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
        let (home, wt) = (root.join("home"), root.join("wt"));
        fs::create_dir(&home).unwrap();
        fs::create_dir(&wt).unwrap();
        let env = Self {
            _dir: dir,
            home,
            wt,
            program,
        };
        env.git(&["init", "-b", "main"]);
        fs::write(
            env.home.join(".gitignore"),
            ".env\n.env.*\n*.local\nbig*.bin\nsecret/\n",
        )
        .unwrap();
        fs::write(env.home.join("tracked.txt"), "tracked\n").unwrap();
        env.git(&["add", "."]);
        env.git(&["commit", "-m", "one"]);
        Some(env)
    }

    fn git(&self, args: &[&str]) {
        let out = Command::new(&self.program)
            .current_dir(&self.home)
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
    }

    fn options(&self) -> GitOptions {
        GitOptions {
            program: self.program.clone(),
            timeout: Duration::from_secs(60),
            isolated: true,
            confine: None,
        }
    }

    fn write(&self, rel: &str, content: &[u8], mode: u32) {
        let path = self.home.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, content).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
    }

    async fn copy(&self, patterns: &[&str]) -> Vec<SkippedFile> {
        let patterns: Vec<String> = patterns.iter().map(|p| (*p).to_owned()).collect();
        copy_includes(&self.home, &self.wt, &patterns, &self.options()).await
    }
}

fn reasons(skipped: &[SkippedFile]) -> Vec<(&str, &str)> {
    skipped
        .iter()
        .map(|s| (s.path.as_str(), s.reason.as_str()))
        .collect()
}

fn mode_of(path: &Path) -> u32 {
    fs::metadata(path).unwrap().permissions().mode() & 0o777
}

#[test]
fn the_glob_matches_inside_one_segment_and_hides_dot_names() {
    assert!(glob("*.local", "a.local"));
    assert!(glob("*", "anything"));
    assert!(glob("a?c", "abc"));
    assert!(glob("a*c*e", "abcde"));
    assert!(glob(".env*", ".env.local"));
    assert!(glob(".*", ".env"));
    assert!(!glob("*.local", ".hidden.local"));
    assert!(glob(".*.local", ".hidden.local"));
    assert!(!glob("a?c", "ac"));
    assert!(!glob("a*c", "abd"));
    assert!(glob("", ""));
}

#[tokio::test]
async fn env_files_copy_with_their_modes() {
    let Some(env) = Env::new() else { return };
    env.write(".env", b"A=1\n", 0o600);
    env.write("config/app.local", b"b\n", 0o640);
    env.write("config/deep/x.local", b"c\n", 0o644);
    let skipped = env
        .copy(&[".env", "config/*.local", "config/*/*.local"])
        .await;
    assert!(skipped.is_empty(), "{skipped:?}");
    assert_eq!(fs::read(env.wt.join(".env")).unwrap(), b"A=1\n");
    assert_eq!(mode_of(&env.wt.join(".env")), 0o600);
    assert_eq!(mode_of(&env.wt.join("config/app.local")), 0o640);
    assert_eq!(
        fs::read(env.wt.join("config/deep/x.local")).unwrap(),
        b"c\n"
    );
    assert_eq!(mode_of(&env.wt.join("config")), 0o700);
}

#[tokio::test]
async fn a_match_that_is_not_ignored_or_is_tracked_is_not_copied() {
    let Some(env) = Env::new() else { return };
    env.write("plain.txt", b"x\n", 0o644);
    let skipped = env.copy(&["plain.txt", "tracked.txt"]).await;
    assert_eq!(
        reasons(&skipped),
        [("plain.txt", "not_ignored"), ("tracked.txt", "not_ignored")]
    );
    assert!(!env.wt.join("plain.txt").exists());
    assert!(!env.wt.join("tracked.txt").exists());
}

#[tokio::test]
async fn a_failing_check_ignore_skips_every_match() {
    let Some(env) = Env::new() else { return };
    env.write(".env", b"A=1\n", 0o600);
    // Not a repository any more: `git check-ignore` fails with 128.
    fs::remove_dir_all(env.home.join(".git")).unwrap();
    let skipped = env.copy(&[".env"]).await;
    assert_eq!(reasons(&skipped), [(".env", "not_ignored")]);
    assert!(!env.wt.join(".env").exists());
}

#[tokio::test]
async fn a_symlink_and_a_fifo_are_skipped() {
    let Some(env) = Env::new() else { return };
    env.write("target.txt", b"secret\n", 0o644);
    symlink(env.home.join("target.txt"), env.home.join("link.local")).unwrap();
    assert!(
        Command::new("mkfifo")
            .arg(env.home.join("pipe.local"))
            .status()
            .unwrap()
            .success()
    );
    let skipped = env.copy(&["link.local", "pipe.local"]).await;
    assert_eq!(
        reasons(&skipped),
        [("link.local", "symlink"), ("pipe.local", "not_regular")]
    );
    assert!(!env.wt.join("link.local").exists());
    assert!(!env.wt.join("pipe.local").exists());
}

#[tokio::test]
async fn a_folder_that_is_a_link_is_not_followed_in_the_source() {
    let Some(env) = Env::new() else { return };
    let outside = env.home.parent().unwrap().join("outside");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("x.local"), "outside\n").unwrap();
    symlink(&outside, env.home.join("secret")).unwrap();
    let skipped = env.copy(&["secret/x.local", "*/x.local"]).await;
    assert!(skipped.is_empty(), "{skipped:?}");
    assert!(!env.wt.join("secret").exists());
}

#[tokio::test]
async fn a_large_file_and_the_total_are_limited() {
    let Some(env) = Env::new() else { return };
    env.write("big.bin", &vec![b'x'; 1024 * 1024 + 1], 0o644);
    env.write("bigok.bin", &vec![b'y'; 1024 * 1024], 0o644);
    let skipped = env.copy(&["big.bin", "bigok.bin"]).await;
    assert_eq!(reasons(&skipped), [("big.bin", "too_large")]);
    assert!(!env.wt.join("big.bin").exists());
    assert_eq!(
        fs::metadata(env.wt.join("bigok.bin")).unwrap().len(),
        1024 * 1024
    );
    fs::remove_file(env.wt.join("bigok.bin")).unwrap();

    // 11 files of 1 MiB: ten fit in 10 MiB; the 11th and every later file do not.
    for n in 0..11 {
        env.write(&format!("bigs{n:02}.bin"), &vec![b'z'; 1024 * 1024], 0o644);
    }
    env.write("later.local", b"small\n", 0o644);
    let skipped = env.copy(&["bigs*.bin", "later.local"]).await;
    assert_eq!(
        reasons(&skipped),
        [
            ("bigs10.bin", "total_too_large"),
            ("later.local", "total_too_large")
        ]
    );
    assert!(env.wt.join("bigs09.bin").exists());
    assert!(!env.wt.join("bigs10.bin").exists());
    assert!(!env.wt.join("later.local").exists());
}

#[tokio::test]
async fn an_existing_destination_file_is_never_overwritten() {
    let Some(env) = Env::new() else { return };
    env.write(".env", b"from home\n", 0o600);
    fs::write(env.wt.join(".env"), "already here\n").unwrap();
    let skipped = env.copy(&[".env"]).await;
    assert_eq!(reasons(&skipped), [(".env", "exists")]);
    assert_eq!(fs::read(env.wt.join(".env")).unwrap(), b"already here\n");
}

#[tokio::test]
async fn a_link_swapped_into_a_destination_folder_is_not_followed() {
    let Some(env) = Env::new() else { return };
    env.write("config/app.local", b"x\n", 0o644);
    let outside = env.home.parent().unwrap().join("outside");
    fs::create_dir(&outside).unwrap();
    symlink(&outside, env.wt.join("config")).unwrap();
    let skipped = env.copy(&["config/*.local"]).await;
    assert_eq!(reasons(&skipped), [("config/app.local", "exists")]);
    assert!(fs::read_dir(&outside).unwrap().next().is_none());
}

#[tokio::test]
async fn hidden_names_match_only_a_dot_pattern() {
    let Some(env) = Env::new() else { return };
    env.write(".hidden.local", b"h\n", 0o644);
    env.write("shown.local", b"s\n", 0o644);
    let skipped = env.copy(&["*.local"]).await;
    assert!(skipped.is_empty(), "{skipped:?}");
    assert!(env.wt.join("shown.local").exists());
    assert!(!env.wt.join(".hidden.local").exists());
    let skipped = env.copy(&[".*.local"]).await;
    assert!(skipped.is_empty(), "{skipped:?}");
    assert!(env.wt.join(".hidden.local").exists());
}

#[tokio::test]
async fn a_dotdot_or_absolute_pattern_matches_nothing_outside() {
    let Some(env) = Env::new() else { return };
    let outside = env.home.parent().unwrap().join("outside.local");
    fs::write(&outside, "outside\n").unwrap();
    let absolute = outside.to_string_lossy().into_owned();
    let skipped = env
        .copy(&["../outside.local", &absolute, "a/../../outside.local", ""])
        .await;
    assert_eq!(skipped.len(), 4, "{skipped:?}");
    assert!(skipped.iter().all(|s| s.reason == "unreadable"));
    assert!(!env.wt.join("outside.local").exists());
}

#[tokio::test]
async fn nothing_below_dot_git_is_copied() {
    let Some(env) = Env::new() else { return };
    let skipped = env.copy(&[".git/config", ".git"]).await;
    assert_eq!(reasons(&skipped), [(".git", "not_ignored")]);
    assert!(!env.wt.join(".git").exists());
}

#[tokio::test]
async fn more_than_200_matches_give_one_entry_and_200_copies() {
    let Some(env) = Env::new() else { return };
    for n in 0..201 {
        env.write(&format!("f{n:03}.local"), b"x\n", 0o644);
    }
    let skipped = env.copy(&["*.local"]).await;
    assert_eq!(reasons(&skipped), [("*.local", "too_many_matches")]);
    let copied = fs::read_dir(&env.wt).unwrap().count();
    assert_eq!(copied, 200);
}

#[tokio::test]
async fn the_result_has_at_most_50_entries() {
    let Some(env) = Env::new() else { return };
    for n in 0..60 {
        env.write(&format!("n{n:02}.txt"), b"x\n", 0o644);
    }
    let skipped = env.copy(&["n*.txt"]).await;
    assert_eq!(skipped.len(), 50);
    assert!(skipped.iter().all(|s| s.reason == "not_ignored"));
}

#[tokio::test]
async fn a_pattern_without_a_match_and_an_unopenable_root_are_handled() {
    let Some(env) = Env::new() else { return };
    assert!(
        env.copy(&["missing/*.local", "nothing.local"])
            .await
            .is_empty()
    );
    let got = copy_includes(
        &env.home.join("gone"),
        &env.wt,
        &["x".to_owned()],
        &env.options(),
    )
    .await;
    assert_eq!(reasons(&got), [(".", "unreadable")]);
}
