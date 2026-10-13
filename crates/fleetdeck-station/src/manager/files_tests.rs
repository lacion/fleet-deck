//! The read-only file service (DL-088) on real directories and real Git:
//! symlinks are never followed (also when swapped in), FIFOs never block,
//! `.git` and credential directories stay hidden, every answer is bounded,
//! roots pass the allowed-roots policy, and per-turn diffs come from the
//! checkpoint refs only.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use fleetdeck_proto::TurnId;
use fleetdeck_proto::event::InputState;
use fleetdeck_proto::files::{
    FileKind, FileListQuery, FileReadQuery, FileRoot, FileSearchQuery, SearchBackend, SearchMode,
    TurnDiffQuery, code,
};
use fleetdeck_testkit::Det;

use super::{diff, list, read, search};
use crate::manager::policy::Policy;
use crate::manager::turns::turns_tests::{
    S1, completed, dispatch, git, input, repo, setup, started,
};
use crate::store::{OfflineApprovalPolicy, Store};
use crate::workspace::{GitOptions, PrivatePaths};

pub(super) fn policy(root: &Path) -> Policy {
    Policy::new(
        vec![root.to_path_buf()],
        4,
        OfflineApprovalPolicy::Wait,
        BTreeMap::new(),
    )
}

pub(super) fn ws(path: &Path) -> FileRoot {
    FileRoot::Workspace(path.display().to_string())
}

pub(super) fn session() -> FileRoot {
    FileRoot::Session(S1.into())
}

/// A plain directory (no Git) with a file, a sub-directory, a symlink to a
/// file outside, a symlinked directory, a FIFO and hidden directories.
fn tree() -> (tempfile::TempDir, PathBuf, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("secret.txt"), "outside secret\n").unwrap();
    std::fs::create_dir(root.join("src")).unwrap();
    std::fs::write(
        root.join("src/main.rs"),
        "fn main() {\n    hello_world();\n}\n",
    )
    .unwrap();
    std::fs::write(root.join("README.md"), "Hello from the readme\n").unwrap();
    std::fs::write(
        root.join("bin.dat"),
        [0u8, 1, 2, 3, b'h', b'e', b'l', b'l', b'o'],
    )
    .unwrap();
    std::os::unix::fs::symlink(outside.path().join("secret.txt"), root.join("link.txt")).unwrap();
    std::os::unix::fs::symlink(outside.path(), root.join("linkdir")).unwrap();
    // `mkfifo(1)` exists on Linux and macOS; rustix has no `mkfifoat` on macOS.
    let made = std::process::Command::new("mkfifo")
        .arg("-m")
        .arg("600")
        .arg(root.join("pipe"))
        .status()
        .unwrap();
    assert!(made.success());
    std::fs::create_dir_all(root.join(".ssh")).unwrap();
    std::fs::write(root.join(".ssh/id_hello"), "hello key\n").unwrap();
    std::fs::create_dir_all(root.join(".GIT")).unwrap();
    std::fs::write(root.join(".GIT/config"), "hello config\n").unwrap();
    std::fs::create_dir_all(root.join("node_modules/dep")).unwrap();
    std::fs::write(root.join("node_modules/dep/hello.js"), "hello\n").unwrap();
    (tmp, root, outside)
}

pub(super) async fn store_with_session(det: &Det, cwd: &Path) -> Store {
    setup(det, cwd).await.0
}

#[tokio::test]
async fn list_shows_dirs_first_marks_symlinks_and_hides_credential_dirs() {
    let det = Det::new();
    let (_t, root, _o) = tree();
    let s = store_with_session(&det, &root).await;
    let p = policy(&root);
    let listing = list(
        &s,
        &p,
        &PrivatePaths::default(),
        FileListQuery {
            root: ws(&root),
            path: String::new(),
        },
    )
    .await
    .unwrap();
    let names: Vec<(&str, FileKind)> = listing
        .entries
        .iter()
        .map(|e| (e.name.as_str(), e.kind))
        .collect();
    assert_eq!(names[0], ("node_modules", FileKind::Dir));
    assert_eq!(names[1], ("src", FileKind::Dir));
    assert!(names.contains(&("link.txt", FileKind::Symlink)));
    assert!(names.contains(&("linkdir", FileKind::Symlink)));
    assert!(names.contains(&("pipe", FileKind::Other)));
    assert!(
        !names
            .iter()
            .any(|(n, _)| n.eq_ignore_ascii_case(".git") || *n == ".ssh")
    );
    let readme = listing
        .entries
        .iter()
        .find(|e| e.name == "README.md")
        .unwrap();
    assert_eq!(readme.size, Some(22));
    assert!(!listing.truncated);
    // The session root is the session's working directory.
    let by_session = list(
        &s,
        &p,
        &PrivatePaths::default(),
        FileListQuery {
            root: session(),
            path: "src".into(),
        },
    )
    .await
    .unwrap();
    assert_eq!(by_session.entries.len(), 1);
    assert_eq!(by_session.path, "src");
}

#[tokio::test]
async fn paths_that_escape_follow_a_symlink_or_name_a_hidden_dir_are_refused() {
    let det = Det::new();
    let (_t, root, _o) = tree();
    let s = store_with_session(&det, &root).await;
    let p = policy(&root);
    let read_code = |path: &str| {
        let (s, p, root) = (s.clone(), p.clone(), root.clone());
        let path = path.to_owned();
        async move {
            read(
                &s,
                &p,
                &PrivatePaths::default(),
                FileReadQuery {
                    root: ws(&root),
                    path,
                    max_bytes: None,
                },
            )
            .await
            .unwrap_err()
            .code
        }
    };
    for bad in [
        "../etc/passwd",
        "/etc/passwd",
        "src/../README.md",
        "./README.md",
        "a\\b",
        "a\0b",
        "src//main.rs",
    ] {
        assert_eq!(read_code(bad).await, code::INVALID_PATH, "{bad:?}");
    }
    assert_eq!(read_code("link.txt").await, code::NOT_FOUND);
    assert_eq!(read_code("linkdir/secret.txt").await, code::NOT_FOUND);
    assert_eq!(read_code(".ssh/id_hello").await, code::NOT_FOUND);
    assert_eq!(read_code(".Git/config").await, code::NOT_FOUND);
    assert_eq!(read_code("missing.txt").await, code::NOT_FOUND);
    assert_eq!(read_code("src").await, code::WRONG_KIND);
    let list_err = list(
        &s,
        &p,
        &PrivatePaths::default(),
        FileListQuery {
            root: ws(&root),
            path: "linkdir".into(),
        },
    )
    .await
    .unwrap_err();
    assert_eq!(list_err.code, code::NOT_FOUND);
    let list_err = list(
        &s,
        &p,
        &PrivatePaths::default(),
        FileListQuery {
            root: ws(&root),
            path: "README.md".into(),
        },
    )
    .await
    .unwrap_err();
    assert_eq!(list_err.code, code::WRONG_KIND);
}

#[tokio::test]
async fn a_fifo_is_refused_without_blocking() {
    let det = Det::new();
    let (_t, root, _o) = tree();
    let s = store_with_session(&det, &root).await;
    let p = policy(&root);
    let started = std::time::Instant::now();
    let err = read(
        &s,
        &p,
        &PrivatePaths::default(),
        FileReadQuery {
            root: ws(&root),
            path: "pipe".into(),
            max_bytes: None,
        },
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, code::WRONG_KIND);
    assert!(started.elapsed() < std::time::Duration::from_secs(2));
}

#[tokio::test]
async fn a_read_is_bounded_and_binary_content_is_not_returned() {
    let det = Det::new();
    let (_t, root, _o) = tree();
    let s = store_with_session(&det, &root).await;
    let p = policy(&root);
    let q = |path: &str, max| FileReadQuery {
        root: ws(&root),
        path: path.into(),
        max_bytes: max,
    };
    let full = read(&s, &p, &PrivatePaths::default(), q("README.md", None))
        .await
        .unwrap();
    assert_eq!(full.text.as_deref(), Some("Hello from the readme\n"));
    assert!(!full.truncated && !full.binary);
    let cut = read(&s, &p, &PrivatePaths::default(), q("README.md", Some(5)))
        .await
        .unwrap();
    assert_eq!(cut.text.as_deref(), Some("Hello"));
    assert!(cut.truncated);
    assert_eq!(cut.size, 22);
    let bin = read(&s, &p, &PrivatePaths::default(), q("bin.dat", None))
        .await
        .unwrap();
    assert!(bin.binary);
    assert_eq!(bin.text, None);
    // A huge max_bytes is capped.
    let capped = read(
        &s,
        &p,
        &PrivatePaths::default(),
        q("README.md", Some(u64::MAX)),
    )
    .await
    .unwrap();
    assert!(!capped.truncated);
}

#[tokio::test]
async fn roots_outside_the_policy_or_unknown_sessions_are_refused() {
    let det = Det::new();
    let (_t, root, outside) = tree();
    let s = store_with_session(&det, &root).await;
    let p = policy(&root.join("src"));
    // The session cwd (root) is outside the narrowed policy.
    let err = list(
        &s,
        &p,
        &PrivatePaths::default(),
        FileListQuery {
            root: session(),
            path: String::new(),
        },
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, code::OUTSIDE_ROOTS);
    let err = list(
        &s,
        &p,
        &PrivatePaths::default(),
        FileListQuery {
            root: ws(outside.path()),
            path: String::new(),
        },
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, code::OUTSIDE_ROOTS);
    // A symlink to an outside directory is not a way in.
    let err = list(
        &s,
        &policy(&root),
        &PrivatePaths::default(),
        FileListQuery {
            root: ws(&root.join("linkdir")),
            path: String::new(),
        },
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, code::OUTSIDE_ROOTS);
    let err = list(
        &s,
        &policy(&root),
        &PrivatePaths::default(),
        FileListQuery {
            root: ws(&root.join("gone")),
            path: String::new(),
        },
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, code::MISSING_WORKSPACE);
    let err = list(
        &s,
        &policy(&root),
        &PrivatePaths::default(),
        FileListQuery {
            root: FileRoot::Session("nope".into()),
            path: String::new(),
        },
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, code::UNKNOWN_SESSION);
}

#[tokio::test]
async fn a_walk_search_skips_symlinks_hidden_and_pruned_dirs() {
    let det = Det::new();
    let (_t, root, _o) = tree();
    let s = store_with_session(&det, &root).await;
    let p = policy(&root);
    let git = GitOptions::isolated();
    let q = |text: &str, mode| FileSearchQuery {
        root: ws(&root),
        query: text.into(),
        mode,
        max_hits: None,
    };
    let names = search(
        &s,
        &p,
        &PrivatePaths::default(),
        &git,
        q("MAIN", SearchMode::Name),
    )
    .await
    .unwrap();
    assert_eq!(names.backend, SearchBackend::Walk);
    let paths: Vec<&str> = names.hits.iter().map(|h| h.path.as_str()).collect();
    assert_eq!(paths, ["src/main.rs"]);
    let content = search(
        &s,
        &p,
        &PrivatePaths::default(),
        &git,
        q("hello", SearchMode::Content),
    )
    .await
    .unwrap();
    let hits: Vec<(&str, Option<u32>)> = content
        .hits
        .iter()
        .map(|h| (h.path.as_str(), h.line))
        .collect();
    // Not the symlinked secret, not .ssh/.GIT, not node_modules, not binary.
    assert_eq!(hits, [("README.md", Some(1)), ("src/main.rs", Some(2))]);
    assert_eq!(content.hits[1].text.as_deref(), Some("    hello_world();"));
    let one = search(
        &s,
        &p,
        &PrivatePaths::default(),
        &git,
        FileSearchQuery {
            max_hits: Some(1),
            ..q("hello", SearchMode::Content)
        },
    )
    .await
    .unwrap();
    assert_eq!(one.hits.len(), 1);
    assert!(one.truncated);
    for bad in ["h", ""] {
        let err = search(
            &s,
            &p,
            &PrivatePaths::default(),
            &git,
            q(bad, SearchMode::Name),
        )
        .await
        .unwrap_err();
        assert_eq!(err.code, code::INVALID_QUERY);
    }
}

#[tokio::test]
async fn a_git_search_covers_tracked_and_untracked_files_but_not_ignored_ones() {
    let det = Det::new();
    let (_t, dir) = repo();
    std::fs::write(dir.join(".gitignore"), "ignored.txt\n").unwrap();
    git(&dir, &["add", ".gitignore"]);
    git(&dir, &["commit", "-q", "-m", "ignore"]);
    std::fs::write(dir.join("ignored.txt"), "needle ignored\n").unwrap();
    std::fs::write(dir.join("new.txt"), "a needle here\n").unwrap();
    std::fs::write(dir.join("a.txt"), "one\nneedle tracked\n").unwrap();
    std::os::unix::fs::symlink("/etc/hostname", dir.join("hostlink")).unwrap();
    let s = store_with_session(&det, &dir).await;
    let p = policy(&dir);
    let git_options = GitOptions::isolated();
    let q = |text: &str, mode| FileSearchQuery {
        root: session(),
        query: text.into(),
        mode,
        max_hits: None,
    };
    let found = search(
        &s,
        &p,
        &PrivatePaths::default(),
        &git_options,
        q("NEEDLE", SearchMode::Content),
    )
    .await
    .unwrap();
    assert_eq!(found.backend, SearchBackend::Git);
    let hits: Vec<(&str, Option<u32>)> = found
        .hits
        .iter()
        .map(|h| (h.path.as_str(), h.line))
        .collect();
    assert_eq!(hits, [("a.txt", Some(2)), ("new.txt", Some(1))]);
    let names = search(
        &s,
        &p,
        &PrivatePaths::default(),
        &git_options,
        q(".txt", SearchMode::Name),
    )
    .await
    .unwrap();
    let paths: Vec<&str> = names.hits.iter().map(|h| h.path.as_str()).collect();
    assert_eq!(paths, ["a.txt", "b.txt", "new.txt"]);
    let none = search(
        &s,
        &p,
        &PrivatePaths::default(),
        &git_options,
        q("absent-text", SearchMode::Content),
    )
    .await
    .unwrap();
    assert!(none.hits.is_empty() && !none.truncated);
}

/// A Git search while the root's ancestor is exchanged with a symlink to
/// another repository outside the allowed roots: Git runs in the directory
/// that the root descriptor holds, so it never searches the other
/// repository (DL-092).
#[cfg(target_os = "linux")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_git_search_never_runs_in_a_repository_swapped_in_above_the_root() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let det = Det::new();
    let top = tempfile::tempdir().unwrap();
    let top_path = std::fs::canonicalize(top.path()).unwrap();
    for (dir, text) in [
        ("base/ws", "needle inside\n"),
        ("evil/ws", "needle OUTSIDE\n"),
    ] {
        let dir = top_path.join(dir);
        std::fs::create_dir_all(&dir).unwrap();
        git(&dir, &["init", "-q", "-b", "main"]);
        std::fs::write(dir.join("a.txt"), text).unwrap();
        git(&dir, &["add", "."]);
        git(&dir, &["commit", "-q", "-m", "init"]);
    }
    std::os::unix::fs::symlink(top_path.join("evil"), top_path.join("baselink")).unwrap();
    let root = top_path.join("base/ws");
    let s = store_with_session(&det, &root).await;
    // Only `base` is allowed; `evil` is outside the roots.
    let p = policy(&top_path.join("base"));
    let stop = Arc::new(AtomicBool::new(false));
    let swapper = {
        let (a, b, stop) = (
            top_path.join("base"),
            top_path.join("baselink"),
            Arc::clone(&stop),
        );
        std::thread::spawn(move || {
            let mut swaps = 0u64;
            while !stop.load(Ordering::Relaxed) {
                rustix::fs::renameat_with(
                    rustix::fs::CWD,
                    &a,
                    rustix::fs::CWD,
                    &b,
                    rustix::fs::RenameFlags::EXCHANGE,
                )
                .unwrap();
                swaps += 1;
            }
            swaps
        })
    };
    let git_options = GitOptions::isolated();
    let (mut answers, mut rounds) = (0u32, 0u32);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(4);
    while std::time::Instant::now() < deadline || rounds < 30 {
        rounds += 1;
        let query = FileSearchQuery {
            root: session(),
            query: "needle".into(),
            mode: SearchMode::Content,
            max_hits: None,
        };
        match search(&s, &p, &PrivatePaths::default(), &git_options, query).await {
            Ok(found) => {
                for hit in &found.hits {
                    let text = hit.text.as_deref().unwrap_or_default();
                    assert!(
                        !text.contains("OUTSIDE"),
                        "searched the swapped-in repository"
                    );
                }
                if found.backend == SearchBackend::Git && !found.hits.is_empty() {
                    answers += 1;
                }
            }
            Err(e) => assert!(
                [
                    code::NOT_FOUND,
                    code::OUTSIDE_ROOTS,
                    code::MISSING_WORKSPACE,
                    "internal"
                ]
                .contains(&e.code),
                "{e}"
            ),
        }
    }
    stop.store(true, Ordering::Relaxed);
    let swaps = swapper.join().unwrap();
    assert!(swaps > 100, "only {swaps} swaps");
    assert!(
        answers > 0,
        "no Git answer from the real root in {rounds} rounds"
    );
}

#[tokio::test]
async fn a_turn_diff_comes_from_the_checkpoint_refs_and_fails_closed() {
    let det = Det::new();
    let (_t, dir) = repo();
    let (s, turns) = setup(&det, &dir).await;
    let p = policy(&dir);
    let git_options = GitOptions::isolated();
    dispatch(&turns, "in-1").await;
    turns.observe(&[started("turn-1")]).await;
    std::fs::write(dir.join("a.txt"), "one\nmore\n").unwrap();
    turns
        .observe(&[
            input("in-1", Some("turn-1"), InputState::Started),
            completed("turn-1"),
        ])
        .await;
    Arc::clone(&turns).capture_due().await;
    let q = |turn: &str, max| TurnDiffQuery {
        session_id: S1.into(),
        turn_id: TurnId::from(turn),
        max_bytes: max,
    };
    let d = diff(
        &s,
        &p,
        &PrivatePaths::default(),
        &git_options,
        q("turn-1", None),
    )
    .await
    .unwrap();
    assert_eq!(d.summary.files_changed, 1);
    assert!(d.patch.contains("+more"), "{}", d.patch);
    assert!(!d.truncated);
    assert_ne!(d.from_commit, d.to_commit);
    // A later workspace change does not alter the recorded diff.
    std::fs::write(dir.join("b.txt"), "changed later\n").unwrap();
    let again = diff(
        &s,
        &p,
        &PrivatePaths::default(),
        &git_options,
        q("turn-1", None),
    )
    .await
    .unwrap();
    assert_eq!(again.patch, d.patch);
    let cut = diff(
        &s,
        &p,
        &PrivatePaths::default(),
        &git_options,
        q("turn-1", Some(10)),
    )
    .await
    .unwrap();
    assert!(cut.truncated);
    let err = diff(
        &s,
        &p,
        &PrivatePaths::default(),
        &git_options,
        q("turn-9", None),
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, code::NO_DIFF);
    let err = diff(
        &s,
        &policy(&dir.join("sub-none")),
        &PrivatePaths::default(),
        &git_options,
        q("turn-1", None),
    )
    .await;
    assert!(err.is_err());
    let err = diff(
        &s,
        &p,
        &PrivatePaths::default(),
        &git_options,
        TurnDiffQuery {
            session_id: "nope".into(),
            turn_id: TurnId::from("turn-1"),
            max_bytes: None,
        },
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, code::UNKNOWN_SESSION);
    // A deleted completion ref makes the diff unavailable, not a guess.
    let refs = std::process::Command::new("git")
        .args(["for-each-ref", "--format=%(refname)", "refs/fleetdeck/"])
        .current_dir(&dir)
        .output()
        .unwrap();
    let names = String::from_utf8(refs.stdout).unwrap();
    let end = names
        .lines()
        .find(|l| l.ends_with("/end"))
        .unwrap()
        .to_owned();
    git(&dir, &["update-ref", "-d", &end]);
    let err = diff(
        &s,
        &p,
        &PrivatePaths::default(),
        &git_options,
        q("turn-1", None),
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, code::NO_DIFF);
}
