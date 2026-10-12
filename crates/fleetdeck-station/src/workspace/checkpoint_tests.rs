//! Checkpoint and restore tests with real `git` in temporary repositories
//! (PLAN §6.6 "Snapshot and restore limits", "Checkpoint scope").

use std::path::{Path, PathBuf};
use std::process::Command;

use super::*;

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .current_dir(dir)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

fn write(dir: &Path, path: &str, text: &str) {
    let p = dir.join(path);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

fn read(dir: &Path, path: &str) -> String {
    std::fs::read_to_string(dir.join(path)).unwrap()
}

/// A repository with `a.txt`, `src/lib.rs` and an ignored `target/`.
fn repo() -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("repo");
    std::fs::create_dir(&dir).unwrap();
    git(&dir, &["init", "-q", "-b", "main"]);
    write(&dir, "a.txt", "one\n");
    write(&dir, "src/lib.rs", "fn a() {}\n");
    write(&dir, ".gitignore", "target/\n");
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "init"]);
    (tmp, dir)
}

fn key(turn: &str, phase: Phase) -> CheckpointKey {
    CheckpointKey {
        incarnation: "INC1".into(),
        session: "S1".into(),
        turn: turn.into(),
        phase,
    }
}

async fn open(dir: &Path) -> Worktree {
    Worktree::open(dir, GitOptions::isolated()).await.unwrap()
}

async fn published(wt: &Worktree, k: &CheckpointKey) -> Checkpoint {
    match wt.capture(k, Limits::default()).await.unwrap() {
        Captured::Published(c) => c,
        other => panic!("expected a published checkpoint, got {other:?}"),
    }
}

fn index_bytes(dir: &Path) -> Vec<u8> {
    std::fs::read(dir.join(".git/index")).unwrap()
}

#[tokio::test]
async fn a_checkpoint_holds_tracked_edits_and_excludes_untracked_and_ignored_files() {
    let (_t, dir) = repo();
    write(&dir, "a.txt", "edited, not staged\n");
    write(&dir, "new.txt", "untracked\n");
    write(&dir, "target/out.bin", "ignored\n");
    let wt = open(&dir).await;
    let c = published(&wt, &key("T1", Phase::Start)).await;
    let files = git(&dir, &["ls-tree", "-r", "--name-only", &c.tree]);
    assert_eq!(
        files.lines().collect::<Vec<_>>(),
        [".gitignore", "a.txt", "src/lib.rs"]
    );
    assert_eq!(
        git(&dir, &["cat-file", "blob", &format!("{}:a.txt", c.tree)]),
        "edited, not staged\n"
    );
    assert_eq!(c.scope, SCOPE_TRACKED);
}

#[tokio::test]
async fn a_capture_never_writes_the_real_index_and_keeps_staged_changes_staged() {
    let (_t, dir) = repo();
    write(&dir, "a.txt", "staged\n");
    git(&dir, &["add", "a.txt"]);
    write(&dir, "a.txt", "staged then edited\n");
    let before = index_bytes(&dir);
    let wt = open(&dir).await;
    published(&wt, &key("T1", Phase::Start)).await;
    assert_eq!(index_bytes(&dir), before, "the real index is unchanged");
    assert_eq!(git(&dir, &["diff", "--cached", "--name-only"]), "a.txt\n");
    let leftovers: Vec<_> = std::fs::read_dir(dir.join(".git"))
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with("fleetdeck-ckpt-")
        })
        .collect();
    assert!(leftovers.is_empty(), "the private index is removed");
}

#[tokio::test]
async fn the_ref_is_hidden_and_namespaced_by_incarnation_worktree_session_and_turn() {
    let (_t, dir) = repo();
    let wt = open(&dir).await;
    let c = published(&wt, &key("T7", Phase::Completion)).await;
    assert_eq!(c.ref_name, format!("{REF_ROOT}/INC1/{}/S1/T7/end", wt.id()));
    assert!(!c.ref_name.starts_with("refs/heads") && !c.ref_name.starts_with("refs/tags"));
    assert_eq!(git(&dir, &["branch", "--list"]).lines().count(), 1);
    assert_eq!(git(&dir, &["tag", "--list"]), "");
}

#[tokio::test]
async fn a_second_capture_of_the_same_turn_returns_the_first_checkpoint_unchanged() {
    let (_t, dir) = repo();
    let wt = open(&dir).await;
    let k = key("T1", Phase::Completion);
    let first = published(&wt, &k).await;
    write(&dir, "a.txt", "later writes of another turn\n");
    let again = wt.capture(&k, Limits::default()).await.unwrap();
    assert_eq!(again, Captured::Existing(first.clone()));
    assert_eq!(wt.read(&k).await.unwrap(), Some(first));
}

#[tokio::test]
async fn bad_key_parts_are_rejected() {
    let (_t, dir) = repo();
    let wt = open(&dir).await;
    for bad in ["", "a/b", "..", "x y", "a.lock"] {
        let mut k = key("T1", Phase::Start);
        k.turn = bad.into();
        assert!(matches!(
            wt.capture(&k, Limits::default()).await,
            Err(CheckpointError::BadKey(_))
        ));
    }
}

#[tokio::test]
async fn two_worktrees_of_one_repository_have_separate_ids_and_reject_each_other() {
    let (tmp, dir) = repo();
    let other = tmp.path().join("repo--fd-b");
    git(
        &dir,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "fd/b",
            other.to_str().unwrap(),
        ],
    );
    let main = open(&dir).await;
    let linked = open(&other).await;
    assert_ne!(main.id(), linked.id());
    let c = published(&main, &key("T1", Phase::Start)).await;
    assert!(matches!(
        linked.read_ref(&c.ref_name).await,
        Err(CheckpointError::OtherWorktree(_))
    ));
    assert!(matches!(
        linked.plan_restore(&c, Limits::default()).await,
        Err(CheckpointError::OtherWorktree(_))
    ));
    // The linked worktree's own checkpoint works and leaves its index alone.
    write(&other, "a.txt", "b\n");
    let own = published(&linked, &key("T1", Phase::Start)).await;
    assert_eq!(
        git(
            &other,
            &["cat-file", "blob", &format!("{}:a.txt", own.tree)]
        ),
        "b\n"
    );
}

#[tokio::test]
async fn a_submodule_makes_the_checkpoint_unavailable() {
    let (_t, dir) = repo();
    let head = git(&dir, &["rev-parse", "HEAD"]);
    let entry = format!("160000,{},vendor/sub", head.trim());
    git(&dir, &["update-index", "--add", "--cacheinfo", &entry]);
    let wt = open(&dir).await;
    let got = wt
        .capture(&key("T1", Phase::Start), Limits::default())
        .await
        .unwrap();
    assert_eq!(
        got,
        Captured::Unavailable(Unavailable::Submodule("vendor/sub".into()))
    );
    assert!(
        wt.read(&key("T1", Phase::Start)).await.unwrap().is_none(),
        "nothing published"
    );
}

#[tokio::test]
async fn filters_skip_worktree_entries_and_size_limits_make_the_checkpoint_unavailable() {
    let (_t, dir) = repo();
    let wt = open(&dir).await;
    let k = key("T1", Phase::Start);

    let small = Limits {
        max_file_bytes: 3,
        ..Limits::default()
    };
    let got = wt.capture(&k, small).await.unwrap();
    assert!(matches!(
        got,
        Captured::Unavailable(Unavailable::FileTooLarge { .. })
    ));
    let total = Limits {
        max_total_bytes: 5,
        ..Limits::default()
    };
    let got = wt.capture(&k, total).await.unwrap();
    assert!(matches!(
        got,
        Captured::Unavailable(Unavailable::TooLarge { .. })
    ));
    let few = Limits {
        max_files: 2,
        ..Limits::default()
    };
    assert_eq!(
        wt.capture(&k, few).await.unwrap(),
        Captured::Unavailable(Unavailable::TooManyFiles(3))
    );

    write(&dir, ".gitattributes", "*.bin filter=lfs\n");
    write(&dir, "big.bin", "x");
    git(&dir, &["add", ".gitattributes"]);
    git(
        &dir,
        &[
            "update-index",
            "--add",
            "--cacheinfo",
            &format!(
                "100644,{},big.bin",
                git(&dir, &["hash-object", "-w", "big.bin"]).trim()
            ),
        ],
    );
    let got = wt.capture(&k, Limits::default()).await.unwrap();
    assert!(
        matches!(got, Captured::Unavailable(Unavailable::Filter { ref path, .. }) if path == "big.bin")
    );
    git(&dir, &["rm", "-q", "--cached", "big.bin", ".gitattributes"]);

    git(&dir, &["update-index", "--skip-worktree", "a.txt"]);
    let got = wt.capture(&k, Limits::default()).await.unwrap();
    assert_eq!(
        got,
        Captured::Unavailable(Unavailable::SkipWorktree("a.txt".into()))
    );
    assert!(wt.read(&k).await.unwrap().is_none());
}

/// Regression (round-3 review): Git does not look at an assume-unchanged
/// file, so `add -u` would silently leave out its unstaged edit and the
/// checkpoint would claim content that is not on disk.
#[tokio::test]
async fn an_assume_unchanged_entry_makes_the_checkpoint_unavailable_instead_of_dropping_its_edit() {
    let (_t, dir) = repo();
    let wt = open(&dir).await;
    let k = key("T1", Phase::Start);

    git(&dir, &["update-index", "--assume-unchanged", "a.txt"]);
    write(&dir, "a.txt", "edited but hidden from git\n");
    // The precondition of the bug: Git itself reports no change.
    assert_eq!(git(&dir, &["status", "--porcelain", "--", "a.txt"]), "");
    let before = index_bytes(&dir);

    let got = wt.capture(&k, Limits::default()).await.unwrap();
    assert_eq!(
        got,
        Captured::Unavailable(Unavailable::AssumeUnchanged("a.txt".into()))
    );
    assert_eq!(got_code(&got), Some("assume_unchanged"));
    assert!(wt.read(&k).await.unwrap().is_none(), "nothing is published");
    assert_eq!(index_bytes(&dir), before, "the real index is untouched");

    // A combined skip-worktree + assume-unchanged entry (`s`) also fails.
    git(&dir, &["update-index", "--skip-worktree", "a.txt"]);
    assert_eq!(
        wt.capture(&k, Limits::default()).await.unwrap(),
        Captured::Unavailable(Unavailable::SkipWorktree("a.txt".into()))
    );

    // Without the flags the same edit is in the checkpoint.
    git(&dir, &["update-index", "--no-skip-worktree", "a.txt"]);
    git(&dir, &["update-index", "--no-assume-unchanged", "a.txt"]);
    let cp = published(&wt, &k).await;
    let shown = git(&dir, &["show", &format!("{}:a.txt", cp.commit)]);
    assert_eq!(shown, "edited but hidden from git\n");
}

fn got_code(c: &Captured) -> Option<&'static str> {
    match c {
        Captured::Unavailable(why) => Some(why.code()),
        _ => None,
    }
}

#[tokio::test]
async fn the_diff_between_start_and_completion_comes_from_the_checkpoints() {
    let (_t, dir) = repo();
    let wt = open(&dir).await;
    let start = published(&wt, &key("T1", Phase::Start)).await;
    write(&dir, "a.txt", "two\n");
    std::fs::remove_file(dir.join("src/lib.rs")).unwrap();
    let end = published(&wt, &key("T1", Phase::Completion)).await;
    write(&dir, "a.txt", "a later turn\n");
    let diff = wt.diff(&start, &end, 1 << 20).await.unwrap();
    assert!(diff.patch.contains("-one\n+two\n"), "{}", diff.patch);
    assert!(diff.patch.contains("deleted file mode"));
    assert!(
        !diff.patch.contains("a later turn"),
        "later writes are not in it"
    );
    assert!(!diff.truncated);
    let cut = wt.diff(&start, &end, 10).await.unwrap();
    assert!(cut.truncated);
}

#[tokio::test]
async fn a_restore_brings_back_tracked_content_and_keeps_untracked_ignored_files_and_the_index() {
    let (_t, dir) = repo();
    write(&dir, "a.txt", "staged\n");
    git(&dir, &["add", "a.txt"]);
    let wt = open(&dir).await;
    let start = published(&wt, &key("T1", Phase::Start)).await;
    // The turn: edits, a deletion, a new tracked file, untracked and ignored files.
    write(&dir, "a.txt", "agent edit\n");
    std::fs::remove_file(dir.join("src/lib.rs")).unwrap();
    write(&dir, "added.rs", "new\n");
    git(&dir, &["add", "added.rs"]);
    write(&dir, "notes.txt", "untracked\n");
    write(&dir, "target/out.bin", "ignored\n");
    let index = index_bytes(&dir);

    let plan = wt
        .plan_restore(&start, Limits::default())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(plan.remove, ["added.rs"]);
    assert_eq!(index_bytes(&dir), index, "planning changes nothing");
    let report = wt.apply_restore(&plan).await.unwrap();
    assert!(report.matches);
    assert_eq!(read(&dir, "a.txt"), "staged\n");
    assert_eq!(read(&dir, "src/lib.rs"), "fn a() {}\n");
    assert!(!dir.join("added.rs").exists());
    assert_eq!(read(&dir, "notes.txt"), "untracked\n");
    assert_eq!(read(&dir, "target/out.bin"), "ignored\n");
    assert_eq!(index_bytes(&dir), index, "the real index is preserved");
}

#[tokio::test]
async fn a_restore_onto_an_untracked_file_is_rejected_before_any_change() {
    let (_t, dir) = repo();
    write(&dir, "later.rs", "tracked in the checkpoint\n");
    git(&dir, &["add", "later.rs"]);
    let wt = open(&dir).await;
    let start = published(&wt, &key("T1", Phase::Start)).await;
    git(&dir, &["rm", "-q", "--cached", "later.rs"]);
    write(&dir, "later.rs", "now an untracked file of the owner\n");
    write(&dir, "a.txt", "edit\n");
    let refs = git(&dir, &["for-each-ref"]);

    let got = wt.plan_restore(&start, Limits::default()).await.unwrap();
    assert_eq!(got, Err(Rejected::UntrackedConflict("later.rs".into())));
    assert_eq!(
        read(&dir, "later.rs"),
        "now an untracked file of the owner\n"
    );
    assert_eq!(read(&dir, "a.txt"), "edit\n");
    assert_eq!(
        git(&dir, &["for-each-ref"]),
        refs,
        "checkpoint refs are unchanged"
    );
}

#[tokio::test]
async fn a_restore_below_a_symlink_is_rejected() {
    let (tmp, dir) = repo();
    let wt = open(&dir).await;
    let start = published(&wt, &key("T1", Phase::Start)).await;
    let outside = tmp.path().join("outside");
    std::fs::create_dir(&outside).unwrap();
    std::fs::rename(dir.join("src"), tmp.path().join("moved")).unwrap();
    std::os::unix::fs::symlink(&outside, dir.join("src")).unwrap();
    let got = wt.plan_restore(&start, Limits::default()).await.unwrap();
    assert_eq!(got, Err(Rejected::SymlinkAncestor("src/lib.rs".into())));
    assert!(
        std::fs::read_dir(&outside).unwrap().next().is_none(),
        "nothing written outside"
    );
}

#[tokio::test]
async fn a_file_to_directory_change_is_restored() {
    let (_t, dir) = repo();
    let wt = open(&dir).await;
    // Checkpoint with `doc/x` below a directory; the turn turns `doc` into a file.
    write(&dir, "doc/x", "x\n");
    git(&dir, &["add", "doc/x"]);
    let start = published(&wt, &key("T1", Phase::Start)).await;
    git(&dir, &["rm", "-q", "-f", "doc/x"]);
    write(&dir, "doc", "a file now\n");
    git(&dir, &["add", "doc"]);
    let plan = wt
        .plan_restore(&start, Limits::default())
        .await
        .unwrap()
        .unwrap();
    let report = wt.apply_restore(&plan).await.unwrap();
    assert!(report.matches);
    assert_eq!(read(&dir, "doc/x"), "x\n");
}

#[tokio::test]
async fn a_repository_without_commits_can_be_checkpointed() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().to_path_buf();
    git(&dir, &["init", "-q"]);
    write(&dir, "f", "1\n");
    git(&dir, &["add", "f"]);
    let wt = open(&dir).await;
    let c = published(&wt, &key("T1", Phase::Start)).await;
    assert_eq!(git(&dir, &["ls-tree", "--name-only", &c.tree]), "f\n");
}

#[tokio::test]
async fn a_directory_outside_git_is_not_a_worktree() {
    let tmp = tempfile::tempdir().unwrap();
    assert!(matches!(
        Worktree::open(tmp.path(), GitOptions::isolated()).await,
        Err(CheckpointError::NotAWorktree(_))
    ));
}
