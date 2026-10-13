//! The session diff (R1.2) on real Git and the store.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use fleetdeck_proto::event::{Event, FileChange, ProviderKind};
use fleetdeck_proto::files::{DiffScope, FileContent, SessionDiffQuery, code};
use fleetdeck_testkit::Det;

use super::{new_file_patch, repo_path, run};
use crate::manager::policy::Policy;
use crate::store::{NewEvent, NewSession, OfflineApprovalPolicy, Store, StoreConfig};
use crate::workspace::GitOptions;

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
    assert!(out.status.success(), "git {args:?}");
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

fn policy(root: &Path) -> Policy {
    Policy::new(
        vec![root.to_path_buf()],
        4,
        OfflineApprovalPolicy::Wait,
        BTreeMap::new(),
    )
}

struct Rig {
    _tmp: tempfile::TempDir,
    dir: PathBuf,
    base: String,
    store: Store,
    policy: Policy,
}

/// A repository with `a.txt`, `b.txt` and a `.gitignore` (for `ignored.txt`)
/// committed, and a session in it
/// with the first commit as its base (unless `with_base` is false).
async fn rig(with_base: bool) -> Rig {
    let det = Det::new();
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let dir = root.join("repo");
    std::fs::create_dir(&dir).unwrap();
    git(&dir, &["init", "-q", "-b", "main"]);
    std::fs::write(dir.join("a.txt"), "one\n").unwrap();
    std::fs::write(dir.join("b.txt"), "two\n").unwrap();
    std::fs::write(dir.join(".gitignore"), "ignored.txt\n").unwrap();
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "init"]);
    let base = git(&dir, &["rev-parse", "HEAD"]);
    let store = Store::open_in_memory(det.clock(), det.ids(), StoreConfig::default())
        .await
        .unwrap();
    store.recover(Vec::new()).await.unwrap();
    store
        .create_session(NewSession {
            session_id: "s1".into(),
            provider: ProviderKind::Claude,
            provider_instance: "claude".into(),
            cwd: dir.display().to_string(),
            worktree: None,
            branch: None,
            mode: None,
            title: None,
            offline_approval_policy: OfflineApprovalPolicy::Wait,
        })
        .await
        .unwrap();
    if with_base {
        let event = Event::SessionBase {
            git_ref: Some("main".to_owned()),
            oid: base.clone(),
            inferred: false,
        };
        store
            .append("s1".into(), vec![NewEvent::from(event)])
            .await
            .unwrap();
    }
    let policy = policy(&root);
    Rig {
        _tmp: tmp,
        dir,
        base,
        store,
        policy,
    }
}

impl Rig {
    /// The station gives the session a worktree of its own.
    async fn own_worktree(&self) {
        let named = Event::SessionNamed {
            callsign: "ace".to_owned(),
            worktree: Some(self.dir.display().to_string()),
        };
        self.store
            .append("s1".into(), vec![NewEvent::from(named)])
            .await
            .unwrap();
    }

    /// The session reports that it changed `path`.
    async fn reports(&self, path: &str) {
        let event = Event::FileChanged {
            path: path.to_owned(),
            change: FileChange::Modified,
            node_id: None,
        };
        self.store
            .append("s1".into(), vec![NewEvent::from(event)])
            .await
            .unwrap();
    }

    async fn diff(
        &self,
        max: Option<u64>,
    ) -> Result<fleetdeck_proto::files::SessionDiff, crate::workspace::files::FileError> {
        run(
            &self.store,
            &self.policy,
            &GitOptions::isolated(),
            SessionDiffQuery {
                session_id: "s1".into(),
                max_bytes: max,
            },
        )
        .await
    }
}

#[tokio::test]
async fn the_diff_has_the_edits_after_the_base_commit_and_the_untracked_files() {
    let r = rig(true).await;
    r.own_worktree().await;
    // An edit that is committed after the base, an uncommitted edit, a
    // deletion, an untracked text file and an untracked binary file.
    std::fs::write(r.dir.join("a.txt"), "one\nmore\n").unwrap();
    git(&r.dir, &["commit", "-q", "-am", "edit a"]);
    std::fs::write(r.dir.join("b.txt"), "two\nthree\n").unwrap();
    std::fs::write(r.dir.join("new.txt"), "hello\nworld").unwrap();
    std::fs::write(r.dir.join("bin.dat"), [0u8, 1, 2, 3]).unwrap();
    let d = r.diff(None).await.unwrap();
    assert_eq!(d.base_oid, r.base);
    assert!(d.patch.contains("+more"), "{}", d.patch);
    assert!(d.patch.contains("+three"), "{}", d.patch);
    assert!(d.patch.contains("diff --git a/new.txt b/new.txt\nnew file mode 100644\n--- /dev/null\n+++ b/new.txt\n@@ -0,0 +1,2 @@\n+hello\n+world\n\\ No newline at end of file\n"), "{}", d.patch);
    assert!(
        d.patch
            .contains("Binary files /dev/null and b/bin.dat differ"),
        "{}",
        d.patch
    );
    // 2 tracked files, 2 untracked files.
    assert_eq!(d.summary.files_changed, 4);
    assert_eq!(d.summary.insertions, 1 + 1 + 2);
    assert_eq!(d.summary.deletions, 0);
    // The binary file is left out of the lines: the view says so.
    assert!(d.truncated);
}

#[tokio::test]
async fn the_diff_leaves_out_ignored_and_private_paths_and_does_not_follow_a_link() {
    let r = rig(true).await;
    r.own_worktree().await;
    std::fs::write(r.dir.join("ignored.txt"), "secret\n").unwrap();
    std::fs::create_dir(r.dir.join(".ssh")).unwrap();
    std::fs::write(r.dir.join(".ssh/key"), "private\n").unwrap();
    std::os::unix::fs::symlink("/etc/hostname", r.dir.join("link.txt")).unwrap();
    std::fs::write(r.dir.join("plain.txt"), "plain\n").unwrap();
    let d = r.diff(None).await.unwrap();
    assert!(d.patch.contains("plain.txt"), "{}", d.patch);
    assert!(!d.patch.contains("ignored.txt"), "{}", d.patch);
    assert!(!d.patch.contains("private"), "{}", d.patch);
    assert!(!d.patch.contains(".ssh"), "{}", d.patch);
    assert!(!d.patch.contains("link.txt"), "{}", d.patch);
}

#[tokio::test]
async fn a_small_byte_limit_cuts_the_patch_and_marks_it() {
    let r = rig(true).await;
    r.own_worktree().await;
    std::fs::write(r.dir.join("a.txt"), "one\nmore\nlines\nand more lines\n").unwrap();
    let d = r.diff(Some(10)).await.unwrap();
    assert!(d.truncated);
    assert!(d.patch.ends_with("[diff truncated]\n"));
}

#[tokio::test]
async fn a_session_without_a_base_or_unknown_or_with_a_lost_base_is_refused() {
    let r = rig(false).await;
    assert_eq!(r.diff(None).await.unwrap_err().code, code::NO_BASE);
    let err = run(
        &r.store,
        &r.policy,
        &GitOptions::isolated(),
        SessionDiffQuery {
            session_id: "nope".into(),
            max_bytes: None,
        },
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, code::UNKNOWN_SESSION);
    // A base commit that the repository does not have.
    let lost = rig(true).await;
    let event = Event::SessionBase {
        git_ref: None,
        oid: "0123456789abcdef0123456789abcdef01234567".to_owned(),
        inferred: true,
    };
    lost.store
        .append("s1".into(), vec![NewEvent::from(event)])
        .await
        .unwrap();
    assert_eq!(lost.diff(None).await.unwrap_err().code, code::NO_BASE);
}

#[test]
fn a_new_file_patch_shows_text_lines_and_a_header_for_other_content() {
    let text = |t: &str, truncated| FileContent {
        path: "f".to_owned(),
        size: t.len() as u64,
        binary: false,
        text: Some(t.to_owned()),
        truncated,
    };
    let (p, n) = new_file_patch("f", &text("a\nb\n", false));
    assert_eq!(n, 2);
    assert!(p.ends_with("@@ -0,0 +1,2 @@\n+a\n+b\n"), "{p}");
    let (p, n) = new_file_patch("f", &text("", false));
    assert_eq!(
        (p.as_str(), n),
        ("diff --git a/f b/f\nnew file mode 100644\n", 0)
    );
    let (p, n) = new_file_patch("f", &text("a\n", true));
    assert!(p.contains("Binary files /dev/null and b/f differ"), "{p}");
    assert_eq!(n, 0);
}

#[tokio::test]
async fn a_session_in_a_shared_folder_shows_only_the_files_that_it_changed() {
    let r = rig(true).await;
    // The session edits `a.txt` (absolute path) and writes `mine.txt`
    // (relative path); someone else edits `b.txt` and writes `theirs.txt`.
    std::fs::write(r.dir.join("a.txt"), "one\nmine\n").unwrap();
    std::fs::write(r.dir.join("mine.txt"), "mine\n").unwrap();
    std::fs::write(r.dir.join("b.txt"), "two\ntheirs\n").unwrap();
    std::fs::write(r.dir.join("theirs.txt"), "theirs\n").unwrap();
    r.reports(&r.dir.join("a.txt").display().to_string()).await;
    r.reports("mine.txt").await;
    let d = r.diff(None).await.unwrap();
    assert_eq!(d.scope, DiffScope::SessionFiles);
    assert!(d.patch.contains("+mine"), "{}", d.patch);
    assert!(d.patch.contains("a/mine.txt"), "{}", d.patch);
    assert!(!d.patch.contains("theirs"), "{}", d.patch);
    assert!(!d.patch.contains("b.txt"), "{}", d.patch);
    assert_eq!(d.summary.files_changed, 2);
    assert_eq!(d.summary.insertions, 2);
}

#[tokio::test]
async fn a_session_in_a_shared_folder_that_changed_no_file_has_an_empty_diff() {
    let r = rig(true).await;
    std::fs::write(r.dir.join("b.txt"), "two\ntheirs\n").unwrap();
    // A path outside the folder does not count.
    r.reports("/etc/hostname").await;
    r.reports("../outside.txt").await;
    let d = r.diff(None).await.unwrap();
    assert_eq!(d.scope, DiffScope::SessionFiles);
    assert!(d.patch.is_empty());
    assert_eq!(
        (
            d.summary.files_changed,
            d.summary.insertions,
            d.summary.deletions
        ),
        (0, 0, 0)
    );
    assert!(!d.truncated);
}

#[tokio::test]
async fn a_session_with_a_worktree_of_its_own_keeps_the_full_diff() {
    let r = rig(true).await;
    r.own_worktree().await;
    std::fs::write(r.dir.join("a.txt"), "one\nmine\n").unwrap();
    std::fs::write(r.dir.join("b.txt"), "two\nmore\n").unwrap();
    let d = r.diff(None).await.unwrap();
    assert_eq!(d.scope, DiffScope::Worktree);
    assert_eq!(d.summary.files_changed, 2);
    assert!(
        d.patch.contains("+mine") && d.patch.contains("+more"),
        "{}",
        d.patch
    );
}

#[tokio::test]
async fn the_files_of_a_shared_folder_session_go_to_git_in_groups() {
    let r = rig(true).await;
    // More tracked files than one pathspec group; the session changed all of
    // them, and someone else changed `b.txt`.
    for n in 0..=super::MAX_PATHSPEC {
        std::fs::write(r.dir.join(format!("f{n:03}.txt")), "x\n").unwrap();
    }
    git(&r.dir, &["add", "."]);
    git(&r.dir, &["commit", "-q", "-m", "many"]);
    std::fs::write(r.dir.join("b.txt"), "two\ntheirs\n").unwrap();
    for n in 0..=super::MAX_PATHSPEC {
        r.reports(&format!("f{n:03}.txt")).await;
    }
    let d = r.diff(None).await.unwrap();
    assert_eq!(d.summary.files_changed as usize, super::MAX_PATHSPEC + 1);
    assert!(d.patch.contains("f000.txt") && d.patch.contains("f500.txt"));
    assert!(!d.patch.contains("theirs"), "{}", d.patch);
}

#[test]
fn a_reported_path_becomes_a_path_from_the_top_of_the_worktree() {
    let top = Path::new("/w/repo");
    let dir = Path::new("/w/repo/sub");
    let cwd = Some(Path::new("/link/repo/sub"));
    let of = |raw| repo_path(raw, dir, cwd, top);
    assert_eq!(of("/w/repo/a/b.txt").as_deref(), Some("a/b.txt"));
    assert_eq!(of("b.txt").as_deref(), Some("sub/b.txt"));
    // Through the stored (not canonical) folder.
    assert_eq!(of("/link/repo/sub/c.txt").as_deref(), Some("sub/c.txt"));
    assert_eq!(of("/elsewhere/a.txt"), None);
    assert_eq!(of("../../outside.txt"), None);
    assert_eq!(of("/w/repo/.git/config"), None);
    assert_eq!(of("/w/repo"), None);
}
