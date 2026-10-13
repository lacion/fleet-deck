//! Regression tests for the files.v1 review sa-13931f53 (DL-098): Git only
//! names search candidates and their content opens through descriptors
//! (finding 1), a private directory never serves as a root (finding 2),
//! checkpoint diffs leave private paths out (finding 3), the diff budget
//! bounds the `git` output itself (finding 4), and a timed-out blocking
//! worker keeps its slot until it ends (finding 5).

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::time::Duration;

use fleetdeck_proto::TurnId;
use fleetdeck_proto::event::InputState;
use fleetdeck_proto::files::{
    FileListQuery, FileReadQuery, FileSearchQuery, SearchBackend, SearchMode, TurnDiffQuery, code,
};
use fleetdeck_testkit::Det;

use super::files_tests::{policy, session, store_with_session, ws};
use super::{Slot, blocking, diff, list, read, search};
use crate::manager::turns::turns_tests::{
    S1, completed, dispatch, git, input, repo, setup, started,
};
use crate::workspace::files::private_root;
use crate::workspace::{GitOptions, PrivatePaths};

/// A content search of a Git worktree while a tracked child directory is
/// exchanged with a symlink to a directory outside the allowed roots: Git
/// lists `src/needle.txt`, but the content opens from the root descriptor
/// without following a symlink, so no hit ever holds the outside text.
#[cfg(target_os = "linux")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_git_content_search_never_reads_a_child_directory_swapped_for_a_symlink() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let det = Det::new();
    let (_t, dir) = repo();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("needle.txt"), "needle OUTSIDE_SECRET\n").unwrap();
    std::fs::create_dir(dir.join("src")).unwrap();
    std::fs::write(dir.join("src/needle.txt"), "needle inside\n").unwrap();
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "src"]);
    std::os::unix::fs::symlink(outside.path(), dir.join("srclink")).unwrap();
    let s = store_with_session(&det, &dir).await;
    let p = policy(&dir);
    let stop = Arc::new(AtomicBool::new(false));
    let swapper = {
        let (a, b, stop) = (dir.join("src"), dir.join("srclink"), Arc::clone(&stop));
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
    let deadline = std::time::Instant::now() + Duration::from_secs(4);
    while std::time::Instant::now() < deadline || rounds < 40 {
        rounds += 1;
        let query = FileSearchQuery {
            root: session(),
            query: "needle".into(),
            mode: SearchMode::Content,
            max_hits: None,
        };
        let found = search(&s, &p, &PrivatePaths::default(), &git_options, query)
            .await
            .unwrap();
        assert_eq!(found.backend, SearchBackend::Git);
        for hit in &found.hits {
            let text = hit.text.as_deref().unwrap_or_default();
            assert!(
                !text.contains("OUTSIDE"),
                "read through the symlink: {hit:?}"
            );
        }
        if found
            .hits
            .iter()
            .any(|h| h.text.as_deref() == Some("needle inside"))
        {
            answers += 1;
        }
    }
    stop.store(true, Ordering::Relaxed);
    let swaps = swapper.join().unwrap();
    assert!(swaps > 100, "only {swaps} swaps");
    assert!(answers > 0, "no inside hit in {rounds} rounds");
}

/// A private directory as the root, or a root inside one, is refused for
/// both root forms and for a case variant; the content stays unread.
#[tokio::test]
async fn a_private_directory_never_serves_as_a_root() {
    let det = Det::new();
    let tmp = tempfile::tempdir().unwrap();
    let top = tmp.path().canonicalize().unwrap();
    for private in [".ssh", "proj/.SSH", ".git", "proj/.Aws/sub"] {
        let dir = top.join(private);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("id_test"), "PRIVATE_ROOT_SECRET\n").unwrap();
        let p = policy(&top);
        // The workspace form.
        let err = read(
            &store_with_session(&det, &top).await,
            &p,
            &PrivatePaths::default(),
            FileReadQuery {
                root: ws(&dir),
                path: "id_test".into(),
                max_bytes: None,
            },
        )
        .await
        .unwrap_err();
        assert_eq!(err.code, code::OUTSIDE_ROOTS, "{private}: {err}");
        // The session form: the session works in the private directory.
        let s = store_with_session(&det, &dir).await;
        let err = read(
            &s,
            &p,
            &PrivatePaths::default(),
            FileReadQuery {
                root: session(),
                path: "id_test".into(),
                max_bytes: None,
            },
        )
        .await
        .unwrap_err();
        assert_eq!(err.code, code::OUTSIDE_ROOTS, "{private}: {err}");
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
        assert_eq!(err.code, code::OUTSIDE_ROOTS, "{private}: {err}");
        let err = search(
            &s,
            &p,
            &PrivatePaths::default(),
            &GitOptions::isolated(),
            FileSearchQuery {
                root: session(),
                query: "PRIVATE".into(),
                mode: SearchMode::Content,
                max_hits: None,
            },
        )
        .await
        .unwrap_err();
        assert_eq!(err.code, code::OUTSIDE_ROOTS, "{private}: {err}");
    }
    // The allowed root itself and a public child still serve.
    std::fs::create_dir_all(top.join("proj/src")).unwrap();
    std::fs::write(top.join("proj/src/a.txt"), "public\n").unwrap();
    let s = store_with_session(&det, &top.join("proj/src")).await;
    let content = read(
        &s,
        &policy(&top),
        &PrivatePaths::default(),
        FileReadQuery {
            root: session(),
            path: "a.txt".into(),
            max_bytes: None,
        },
    )
    .await
    .unwrap();
    assert_eq!(content.text.as_deref(), Some("public\n"));
}

#[test]
fn a_private_root_check_uses_the_segments_below_the_allowed_root() {
    let allowed = vec![Path::new("/home/u").to_path_buf()];
    assert!(!private_root(Path::new("/home/u"), &allowed));
    assert!(!private_root(Path::new("/home/u/proj/src"), &allowed));
    assert!(private_root(Path::new("/home/u/.ssh"), &allowed));
    assert!(private_root(Path::new("/home/u/p/.SSH/k"), &allowed));
    assert!(private_root(Path::new("/home/u/p/.Git"), &allowed));
    // No allowed root holds the path: fail closed.
    assert!(private_root(Path::new("/srv/x"), &allowed));
    // A public dot directory above the allowed root does not count.
    let inner = vec![Path::new("/home/u/.config/work").to_path_buf()];
    assert!(!private_root(Path::new("/home/u/.config/work/app"), &inner));
    // A private allowed root, or one inside a private directory, never
    // masks it (finding 1 of sa-fbed2a6d).
    let private = vec![Path::new("/home/u/.SSH").to_path_buf()];
    assert!(private_root(Path::new("/home/u/.SSH"), &private));
    assert!(private_root(Path::new("/home/u/.SSH/keys"), &private));
    let below = vec![Path::new("/home/u/.ssh/work").to_path_buf()];
    assert!(private_root(Path::new("/home/u/.ssh/work/app"), &below));
}

/// Checkpoints for one turn that changes `a.txt`, a tracked private file,
/// moves `b.txt` into a private directory and moves a private file out.
async fn private_turn(dir: &Path) -> (crate::store::Store, crate::manager::policy::Policy) {
    std::fs::create_dir_all(dir.join(".ssh")).unwrap();
    std::fs::create_dir_all(dir.join("sub/.SSH")).unwrap();
    std::fs::write(dir.join(".ssh/id_test"), "OLD_DIFF_PRIVATE_SECRET\n").unwrap();
    std::fs::write(dir.join("sub/.SSH/key"), "OLD_NESTED_SECRET\n").unwrap();
    std::fs::write(dir.join(".ssh/moved_out"), "MOVED_OUT_TEXT\n").unwrap();
    git(dir, &["add", "-f", "."]);
    git(dir, &["commit", "-q", "-m", "private"]);
    let det = Det::new();
    let (s, turns) = setup(&det, dir).await;
    dispatch(&turns, "in-1").await;
    turns.observe(&[started("turn-1")]).await;
    std::fs::write(dir.join("a.txt"), "one\npublic change\n").unwrap();
    std::fs::write(dir.join(".ssh/id_test"), "NEW_DIFF_PRIVATE_SECRET\n").unwrap();
    std::fs::write(dir.join("sub/.SSH/key"), "NEW_NESTED_SECRET\n").unwrap();
    git(dir, &["mv", "b.txt", ".ssh/b.txt"]);
    git(dir, &["mv", ".ssh/moved_out", "public.txt"]);
    turns
        .observe(&[
            input("in-1", Some("turn-1"), InputState::Started),
            completed("turn-1"),
        ])
        .await;
    Arc::clone(&turns).capture_due().await;
    (s, policy(dir))
}

#[tokio::test]
async fn a_turn_diff_leaves_out_private_paths_in_the_patch_and_the_summary() {
    let (_t, dir) = repo();
    let (s, p) = private_turn(&dir).await;
    let d = diff(
        &s,
        &p,
        &PrivatePaths::default(),
        &GitOptions::isolated(),
        TurnDiffQuery {
            session_id: S1.into(),
            turn_id: TurnId::from("turn-1"),
            max_bytes: None,
        },
    )
    .await
    .unwrap();
    for secret in ["PRIVATE_SECRET", "NESTED_SECRET", ".ssh", ".SSH", "id_test"] {
        assert!(!d.patch.contains(secret), "{secret} in:\n{}", d.patch);
    }
    // The public sides: a.txt changed, b.txt left (its private side is
    // hidden), public.txt arrived (its private side is hidden).
    assert!(d.patch.contains("+public change"), "{}", d.patch);
    assert!(d.patch.contains("b/public.txt"), "{}", d.patch);
    assert!(d.patch.contains("a/b.txt"), "{}", d.patch);
    assert!(!d.patch.contains("rename"), "{}", d.patch);
    assert_eq!(d.summary.files_changed, 3, "{:?}", d.summary);
    // The `turn.finalized` summary has the same three public paths only.
    let finalized = crate::manager::turns::turns_tests::finalized(&s).await;
    let summary = finalized.last().and_then(|f| f.3).unwrap();
    assert_eq!(summary.files_changed, 3, "{summary:?}");
}

#[tokio::test]
async fn a_large_diff_stops_at_the_budget() {
    let (_t, dir) = repo();
    let det = Det::new();
    let (s, turns) = setup(&det, &dir).await;
    dispatch(&turns, "in-1").await;
    turns.observe(&[started("turn-1")]).await;
    // About 6 MiB of new lines: far above the budget.
    let big: String = (0..200_000)
        .map(|i| format!("line {i:08} of the big file\n"))
        .collect();
    std::fs::write(dir.join("big.txt"), &big).unwrap();
    git(&dir, &["add", "big.txt"]);
    turns
        .observe(&[
            input("in-1", Some("turn-1"), InputState::Started),
            completed("turn-1"),
        ])
        .await;
    Arc::clone(&turns).capture_due().await;
    let d = diff(
        &s,
        &policy(&dir),
        &PrivatePaths::default(),
        &GitOptions::isolated(),
        TurnDiffQuery {
            session_id: S1.into(),
            turn_id: TurnId::from("turn-1"),
            max_bytes: Some(4096),
        },
    )
    .await
    .unwrap();
    assert!(d.truncated);
    assert!(d.patch.len() <= 4096 + 64, "{}", d.patch.len());
    assert!(d.patch.contains("line 00000000"), "{}", d.patch);
}

/// A blocking worker that outlives its deadline keeps its slot: the
/// caller gets `timeout`, a new operation gets `busy` until the worker
/// really ends, and then the slot is free again.
#[tokio::test]
async fn a_timed_out_worker_keeps_its_slot_until_it_ends() {
    static TEST_SLOTS: AtomicUsize = AtomicUsize::new(0);
    let slot = Slot::take(&TEST_SLOTS, 1).unwrap();
    let err = blocking(Duration::from_millis(50), slot, || {
        std::thread::sleep(Duration::from_millis(400));
        Ok(())
    })
    .await
    .unwrap_err();
    assert_eq!(err.code, code::TIMEOUT);
    let busy = Slot::take(&TEST_SLOTS, 1).err().unwrap();
    assert_eq!(busy.code, code::BUSY);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let free = loop {
        if let Ok(slot) = Slot::take(&TEST_SLOTS, 1) {
            break slot;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the slot stays taken"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    drop(free);
}
