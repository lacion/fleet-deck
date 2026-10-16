//! The checked staging of FDV2-003: a tracked path that is replaced by a
//! hard link to a private file never enters a snapshot, a checkpoint, a
//! restore or a diff. Real `git` in temporary repositories.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use fleetdeck_proto::event::{Event, ProviderKind};
use fleetdeck_proto::files::{SessionDiffQuery, TurnDiffQuery};
use fleetdeck_testkit::Det;

use super::*;
use crate::manager::policy::Policy;
use crate::store::{
    NewEvent, NewSession, NewTurnCheckpoint, OfflineApprovalPolicy, RowSide, Store, StoreConfig,
    Which,
};
use crate::workspace::restore::Rejected;
use crate::workspace::stage::{AfterOpen, BeforeWalk, IndexTrust, check_change_count};

const SECRET: &str = "SECRET-CREDENTIAL-BYTES-7f3a9c";

fn git_env(dir: &Path, args: &[&str], env: &[(&str, &OsStr)]) -> String {
    let mut cmd = Command::new("git");
    cmd.current_dir(dir)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t");
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.output().unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

fn git(dir: &Path, args: &[&str]) -> String {
    git_env(dir, args, &[])
}

fn write(dir: &Path, path: &str, text: &str) {
    let p = dir.join(path);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

/// A repository with tracked `a.txt`, `b.txt` and `src/lib.rs`, and a
/// credential file `cred/secret` outside it.
fn world() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let dir = root.join("repo");
    std::fs::create_dir(&dir).unwrap();
    git(&dir, &["init", "-q", "-b", "main"]);
    write(&dir, "a.txt", "one\n");
    write(&dir, "b.txt", "two\n");
    write(&dir, "src/lib.rs", "fn a() {}\n");
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "init"]);
    let cred = root.join("cred/secret");
    write(&root, "cred/secret", &format!("{SECRET}\n"));
    (tmp, dir, cred)
}

async fn open(dir: &Path) -> Worktree {
    Worktree::open(dir, GitOptions::isolated()).await.unwrap()
}

async fn snapshot(wt: &Worktree, private: &PrivatePaths) -> Snapshot {
    wt.snapshot_tree(Limits::default(), private)
        .await
        .unwrap()
        .unwrap()
}

fn blob(dir: &Path, tree: &str, path: &str) -> String {
    git(dir, &["cat-file", "blob", &format!("{tree}:{path}")])
}

/// True when some Git object of the repository holds `needle`.
fn some_object_holds(dir: &Path, needle: &str) -> bool {
    let out = Command::new("git")
        .current_dir(dir)
        .args(["cat-file", "--batch-all-objects", "--batch"])
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .unwrap();
    assert!(out.status.success());
    out.stdout
        .windows(needle.len())
        .any(|w| w == needle.as_bytes())
}

fn replace_with_link(dir: &Path, path: &str, target: &Path) {
    std::fs::remove_file(dir.join(path)).unwrap();
    std::fs::hard_link(target, dir.join(path)).unwrap();
}

#[tokio::test]
async fn a_tracked_path_replaced_by_a_link_to_a_registered_credential_keeps_its_old_blob() {
    let (_t, dir, cred) = world();
    let private = PrivatePaths::default();
    private.register(&cred);
    replace_with_link(&dir, "a.txt", &cred);
    // The original is gone: only the registry knows the file (nlink is 1).
    std::fs::remove_file(&cred).unwrap();
    write(&dir, "b.txt", "two, edited\n");
    let wt = open(&dir).await;
    let snap = snapshot(&wt, &private).await;
    assert_eq!(snap.hidden, ["a.txt"]);
    assert_eq!(blob(&dir, &snap.tree, "a.txt"), "one\n");
    assert_eq!(blob(&dir, &snap.tree, "b.txt"), "two, edited\n");
    assert!(
        !some_object_holds(&dir, SECRET),
        "no Git object holds the secret"
    );
}

#[tokio::test]
async fn a_tracked_path_replaced_by_a_link_to_an_unregistered_file_is_left_out_too() {
    let (_t, dir, cred) = world();
    // Not registered, and the original stays: the link count is 2.
    replace_with_link(&dir, "a.txt", &cred);
    let wt = open(&dir).await;
    let snap = snapshot(&wt, &PrivatePaths::default()).await;
    assert_eq!(snap.hidden, ["a.txt"]);
    assert_eq!(blob(&dir, &snap.tree, "a.txt"), "one\n");
    assert!(!some_object_holds(&dir, SECRET));
}

#[tokio::test]
async fn a_capture_records_the_capture_version_and_leaves_out_the_private_file() {
    let (_t, dir, cred) = world();
    let private = PrivatePaths::default();
    private.register(&cred);
    replace_with_link(&dir, "a.txt", &cred);
    let wt = open(&dir).await;
    let key = CheckpointKey {
        incarnation: "INC1".into(),
        session: "S1".into(),
        turn: "T1".into(),
        phase: Phase::Start,
    };
    let Captured::Published(c) = wt.capture(&key, Limits::default(), &private).await.unwrap()
    else {
        panic!("not published");
    };
    assert_eq!(c.capture, CAPTURE_VERIFIED);
    let body = git(&dir, &["cat-file", "commit", &c.commit]);
    assert!(
        body.contains("scope: tracked\ncapture: 2\n"),
        "the capture line follows the scope line:\n{body}"
    );
    assert_eq!(wt.read(&key).await.unwrap().unwrap().capture, 2);
    assert_eq!(blob(&dir, &c.tree, "a.txt"), "one\n");
    assert!(!some_object_holds(&dir, SECRET));
}

#[tokio::test]
async fn the_bytes_come_from_the_opened_file_when_the_path_is_replaced_after_the_open() {
    let (_t, dir, cred) = world();
    write(&dir, "a.txt", "opened content\n");
    let wt = open(&dir).await;
    let index = PrivateIndex::copy_of(wt.git_dir()).unwrap();
    let swap = dir.clone();
    let secret = cred.clone();
    let hook: AfterOpen = Arc::new(move |rel: &str| {
        if rel == "a.txt" {
            // The path now names a link to a credential.
            std::fs::remove_file(swap.join("a.txt")).unwrap();
            std::fs::hard_link(&secret, swap.join("a.txt")).unwrap();
        }
    });
    let hidden = wt
        .stage_changes_with(
            &index,
            Limits::default(),
            &PrivatePaths::default(),
            IndexTrust::Untrusted,
            hook,
            Arc::new(|_| {}),
        )
        .await
        .unwrap()
        .unwrap();
    assert!(hidden.is_empty(), "the opened file was not private");
    let env = [("GIT_INDEX_FILE", index.path().as_os_str())];
    let tree = wt.git().call(&["write-tree"], &env, None).await.unwrap();
    let tree = String::from_utf8_lossy(&tree).trim().to_owned();
    assert_eq!(blob(&dir, &tree, "a.txt"), "opened content\n");
    assert!(!some_object_holds(&dir, SECRET));
}

/// The tree that `git add -u` makes of the changes in `dir`.
fn tree_of_add_u(dir: &Path) -> String {
    let copy = dir.join(".git/parity.index");
    std::fs::copy(dir.join(".git/index"), &copy).unwrap();
    let env = [("GIT_INDEX_FILE", copy.as_os_str())];
    git_env(dir, &["add", "-u", "--", ":/"], &env);
    let tree = git_env(dir, &["write-tree"], &env);
    std::fs::remove_file(copy).unwrap();
    tree.trim().to_owned()
}

#[tokio::test]
async fn edits_deletions_modes_and_symlinks_stage_like_git_add_u() {
    let (_t, dir, _cred) = world();
    write(&dir, "gone.txt", "will be deleted\n");
    write(&dir, "tool.sh", "#!/bin/sh\n");
    write(&dir, "nested/deep/file.txt", "deep\n");
    std::os::unix::fs::symlink("a.txt", dir.join("link")).unwrap();
    std::os::unix::fs::symlink("b.txt", dir.join("becomes-link")).unwrap();
    write(&dir, "becomes-link-target", "x\n");
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "more"]);
    // The changes: an edit, a deletion, a mode change, a re-pointed link,
    // a file that became a link, a link that became a file, a nested edit.
    write(&dir, "a.txt", "one, edited\nwith two lines\n");
    std::fs::remove_file(dir.join("gone.txt")).unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir.join("tool.sh"), std::fs::Permissions::from_mode(0o755))
            .unwrap();
    }
    std::fs::remove_file(dir.join("link")).unwrap();
    std::os::unix::fs::symlink("src/lib.rs", dir.join("link")).unwrap();
    std::fs::remove_file(dir.join("src/lib.rs")).unwrap();
    std::os::unix::fs::symlink("../a.txt", dir.join("src/lib.rs")).unwrap();
    std::fs::remove_file(dir.join("becomes-link")).unwrap();
    write(&dir, "becomes-link", "now a file\n");
    write(&dir, "nested/deep/file.txt", "deeper\n");
    let wt = open(&dir).await;
    let snap = snapshot(&wt, &PrivatePaths::default()).await;
    assert!(snap.hidden.is_empty());
    assert_eq!(snap.tree, tree_of_add_u(&dir));
    // Spot checks of what the tree holds.
    let ls = git(&dir, &["ls-tree", "-r", &snap.tree]);
    assert!(ls.contains("100755"), "{ls}");
    assert!(ls.contains("120000"), "{ls}");
    assert!(!ls.contains("gone.txt"), "{ls}");
}

#[tokio::test]
async fn a_tracked_directory_replaced_by_a_symlink_is_a_deletion_not_a_read_through_the_link() {
    let (_t, dir, _cred) = world();
    let outside = dir.parent().unwrap().join("outside");
    write(&outside, "lib.rs", "outside content\n");
    std::fs::remove_dir_all(dir.join("src")).unwrap();
    std::os::unix::fs::symlink(&outside, dir.join("src")).unwrap();
    let wt = open(&dir).await;
    let snap = snapshot(&wt, &PrivatePaths::default()).await;
    let ls = git(&dir, &["ls-tree", "-r", "--name-only", &snap.tree]);
    assert!(!ls.contains("src/lib.rs"), "{ls}");
    assert!(!some_object_holds(&dir, "outside content"));
}

#[tokio::test]
async fn a_tracked_path_that_became_a_fifo_makes_the_snapshot_unavailable() {
    let (_t, dir, _cred) = world();
    std::fs::remove_file(dir.join("b.txt")).unwrap();
    let made = Command::new("mkfifo")
        .arg(dir.join("b.txt"))
        .status()
        .unwrap();
    assert!(made.success());
    let wt = open(&dir).await;
    let got = wt
        .snapshot_tree(Limits::default(), &PrivatePaths::default())
        .await
        .unwrap();
    assert_eq!(got, Err(Unavailable::NotRegular("b.txt".into())));
    assert_eq!(Unavailable::NotRegular(String::new()).code(), "not_regular");
}

#[test]
fn more_than_five_thousand_changed_paths_are_too_many() {
    assert_eq!(check_change_count(0), Ok(()));
    assert_eq!(check_change_count(5000), Ok(()));
    assert_eq!(
        check_change_count(5001),
        Err(Unavailable::TooManyChanges(5001))
    );
    assert_eq!(Unavailable::TooManyChanges(5001).code(), "too_many_changes");
}

/// A checkpoint commit of the old capture: the tree of `source` and a
/// body without the `capture:` line, as a station before the fix wrote it.
fn legacy_commit(wt: &Worktree, dir: &Path, key: &CheckpointKey, tree: &str) -> String {
    let message = format!(
        "fleetdeck checkpoint\n\nworktree-id: {}\nincarnation: {}\nsession: {}\nturn: {}\nphase: {}\nscope: tracked\n",
        wt.id(),
        key.incarnation,
        key.session,
        key.turn,
        key.phase.as_str()
    );
    let msg = dir.join(".git/legacy-message");
    std::fs::write(&msg, message).unwrap();
    let commit = git(
        dir,
        &[
            "commit-tree",
            "--no-gpg-sign",
            tree,
            "-F",
            msg.to_str().unwrap(),
        ],
    );
    let commit = commit.trim().to_owned();
    let ref_name = wt.ref_name(key).unwrap();
    git(dir, &["update-ref", &ref_name, &commit]);
    commit
}

fn key(turn: &str, phase: Phase) -> CheckpointKey {
    CheckpointKey {
        incarnation: "INC1".into(),
        session: "S1".into(),
        turn: turn.into(),
        phase,
    }
}

#[tokio::test]
async fn a_revert_to_a_capture_one_checkpoint_is_refused_for_everyone() {
    let (_t, dir, _cred) = world();
    let wt = open(&dir).await;
    let tree = git(&dir, &["rev-parse", "HEAD^{tree}"]).trim().to_owned();
    legacy_commit(&wt, &dir, &key("T1", Phase::Start), &tree);
    let old = wt.read(&key("T1", Phase::Start)).await.unwrap().unwrap();
    assert_eq!(old.capture, 1, "a commit without the line is capture 1");
    write(&dir, "a.txt", "changed\n");
    let got = wt
        .plan_restore(&old, Limits::default(), &PrivatePaths::default())
        .await
        .unwrap();
    assert_eq!(got, Err(Rejected::OldCapture));
    assert_eq!(
        Rejected::OldCapture.code(),
        crate::manager::revert_run::reason::CHECKPOINT_UNVERIFIED
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("a.txt")).unwrap(),
        "changed\n"
    );
}

#[tokio::test]
async fn a_restore_never_writes_over_a_private_file_or_reads_it() {
    let (_t, dir, cred) = world();
    let private = PrivatePaths::default();
    private.register(&cred);
    // The start checkpoint has an edit of `a.txt`; b.txt changes later.
    write(&dir, "a.txt", "start edit\n");
    let wt = open(&dir).await;
    let Captured::Published(start) = wt
        .capture(&key("T1", Phase::Start), Limits::default(), &private)
        .await
        .unwrap()
    else {
        panic!("not published");
    };
    write(&dir, "b.txt", "two, later\n");
    replace_with_link(&dir, "a.txt", &cred);
    let plan = wt
        .plan_restore(&start, Limits::default(), &private)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(plan.hidden, ["a.txt"]);
    assert_eq!(plan.write, ["b.txt"], "the private path is not restored");
    let report = wt.apply_restore(&plan, &private).await.unwrap();
    assert!(report.matches);
    assert_eq!(std::fs::read_to_string(dir.join("b.txt")).unwrap(), "two\n");
    assert_eq!(
        std::fs::read_to_string(dir.join("a.txt")).unwrap(),
        format!("{SECRET}\n"),
        "the private path is left as it was"
    );
    assert!(!some_object_holds(&dir, SECRET));
}

fn policy(root: &Path) -> Policy {
    Policy::new(
        vec![root.to_path_buf()],
        4,
        OfflineApprovalPolicy::Wait,
        BTreeMap::new(),
    )
}

async fn store_with_session(det: &Det, dir: &Path) -> Store {
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
    store
}

#[tokio::test]
async fn the_session_diff_does_not_contain_the_bytes_of_a_private_file() {
    let det = Det::new();
    let (t, dir, cred) = world();
    let private = PrivatePaths::default();
    private.register(&cred);
    let base = git(&dir, &["rev-parse", "HEAD"]).trim().to_owned();
    let store = store_with_session(&det, &dir).await;
    let events = [
        Event::SessionBase {
            git_ref: Some("main".to_owned()),
            oid: base,
            inferred: false,
        },
        Event::SessionNamed {
            callsign: "ace".to_owned(),
            worktree: Some(dir.display().to_string()),
        },
    ];
    for event in events {
        store
            .append("s1".into(), vec![NewEvent::from(event)])
            .await
            .unwrap();
    }
    replace_with_link(&dir, "a.txt", &cred);
    write(&dir, "b.txt", "two, edited\n");
    let root = t.path().canonicalize().unwrap();
    let diff = crate::workspace::session_diff::run(
        &store,
        &policy(&root),
        &private,
        &GitOptions::isolated(),
        SessionDiffQuery {
            session_id: "s1".into(),
            max_bytes: None,
        },
    )
    .await
    .unwrap();
    assert!(diff.patch.contains("+two, edited"), "{}", diff.patch);
    assert!(!diff.patch.contains(SECRET), "{}", diff.patch);
    assert!(!diff.patch.contains("a.txt"), "{}", diff.patch);
    assert!(!some_object_holds(&dir, SECRET));
}

/// A finalized turn of `s1` whose two checkpoints are of capture
/// `capture` (1: the old commit body; 2: a real capture).
async fn finalized_turn(det: &Det, dir: &Path, legacy: bool) -> Store {
    let edit = |dir: &Path| write(dir, "a.txt", "one, turn edit\n");
    finalized_turn_with(det, dir, legacy, &PrivatePaths::default(), &edit).await
}

/// [`finalized_turn`] with the edit that the turn makes and the private
/// paths of the captures.
async fn finalized_turn_with(
    det: &Det,
    dir: &Path,
    legacy: bool,
    private: &PrivatePaths,
    edit: &dyn Fn(&Path),
) -> Store {
    let store = store_with_session(det, dir).await;
    let wt = open(dir).await;
    let planned = store
        .plan_turn_checkpoint(NewTurnCheckpoint {
            app_turn: "T1".into(),
            session_id: "s1".into(),
            process: Some("proc-1".to_owned().into()),
            input_id: Some("in-1".into()),
            turn_id: None,
            worktree_id: Some(wt.id().to_owned()),
            worktree_root: Some(wt.root().display().to_string()),
            scope: SCOPE_TRACKED.to_owned(),
            start: RowSide::Capturing,
            generation: None,
        })
        .await
        .unwrap();
    let incarnation = store.incarnation().as_str().to_owned();
    let key = |phase| CheckpointKey {
        incarnation: incarnation.clone(),
        session: "s1".into(),
        turn: "T1".into(),
        phase,
    };
    let mut commits = Vec::new();
    for (phase, which) in [
        (Phase::Start, Which::Start),
        (Phase::Completion, Which::End),
    ] {
        if phase == Phase::Completion {
            edit(dir);
            store
                .record_checkpoint_side(planned.clone(), Which::End, RowSide::Capturing)
                .await
                .unwrap();
        }
        let k = key(phase);
        let commit = if legacy {
            let snap = snapshot(&wt, private).await;
            legacy_commit(&wt, dir, &k, &snap.tree)
        } else {
            match wt.capture(&k, Limits::default(), private).await.unwrap() {
                Captured::Published(c) => c.commit,
                other => panic!("{other:?}"),
            }
        };
        store
            .record_checkpoint_side(planned.clone(), which, RowSide::Available(commit.clone()))
            .await
            .unwrap();
        if phase == Phase::Start {
            store
                .bind_checkpoint_turn(planned.clone(), "turn-1".into())
                .await
                .unwrap();
        }
        commits.push(commit);
    }
    store.finalize_turn(planned, None).await.unwrap();
    store
}

fn turn_query() -> TurnDiffQuery {
    // The wire form of a query by provider turn id.
    serde_json::from_value(serde_json::json!({ "session_id": "s1", "turn_id": "turn-1" })).unwrap()
}

#[tokio::test]
async fn a_member_turn_diff_of_a_capture_one_turn_is_refused_and_the_owner_gets_it() {
    use crate::manager::files::diff_as;
    let det = Det::new();
    let (t, dir, _cred) = world();
    let store = finalized_turn(&det, &dir, true).await;
    let root = t.path().canonicalize().unwrap();
    let p = policy(&root);
    let private = PrivatePaths::default();
    let git = GitOptions::isolated();
    let member = diff_as(&store, &p, &private, &git, turn_query(), true)
        .await
        .unwrap_err();
    assert_eq!(member.code, "forbidden");
    assert_eq!(
        member.message,
        "this turn was recorded before a security fix; only the owner can view it"
    );
    let owner = diff_as(&store, &p, &private, &git, turn_query(), false)
        .await
        .unwrap();
    assert!(owner.patch.contains("+one, turn edit"), "{}", owner.patch);
}

#[tokio::test]
async fn a_member_turn_diff_of_a_capture_two_turn_is_served() {
    use crate::manager::files::diff_as;
    let det = Det::new();
    let (t, dir, _cred) = world();
    let store = finalized_turn(&det, &dir, false).await;
    let root = t.path().canonicalize().unwrap();
    let member = diff_as(
        &store,
        &policy(&root),
        &PrivatePaths::default(),
        &GitOptions::isolated(),
        turn_query(),
        true,
    )
    .await
    .unwrap();
    assert!(member.patch.contains("+one, turn edit"), "{}", member.patch);
}

#[tokio::test]
async fn files_that_were_only_touched_are_not_changes() {
    let (_t, dir, _cred) = world();
    let later = std::time::SystemTime::now() + std::time::Duration::from_secs(30);
    for path in ["a.txt", "b.txt", "src/lib.rs"] {
        std::fs::File::options()
            .write(true)
            .open(dir.join(path))
            .unwrap()
            .set_modified(later)
            .unwrap();
    }
    // Without a refresh Git lists them: the file times differ.
    assert!(
        !git(&dir, &["diff-files", "--name-only"]).is_empty(),
        "the touched files differ in file times"
    );
    let wt = open(&dir).await;
    let snap = snapshot(&wt, &PrivatePaths::default()).await;
    assert!(snap.hidden.is_empty());
    assert_eq!(snap.tree, git(&dir, &["rev-parse", "HEAD^{tree}"]).trim());
    // After the refresh no path differs, so none counts toward the limit.
    let index = PrivateIndex::copy_of(wt.git_dir()).unwrap();
    let hidden = wt
        .stage_changes(
            &index,
            Limits::default(),
            &PrivatePaths::default(),
            IndexTrust::Untrusted,
        )
        .await
        .unwrap()
        .unwrap();
    assert!(hidden.is_empty());
    let env = [("GIT_INDEX_FILE", index.path().as_os_str())];
    assert_eq!(
        git_env(&dir, &["diff-files", "--name-only"], &env),
        "",
        "the refresh left no stat-only difference"
    );
}

/// True when a blob that the tree `tree` reaches holds `needle`.
fn tree_holds(dir: &Path, tree: &str, needle: &str) -> bool {
    let names = git(dir, &["ls-tree", "-r", "--name-only", tree]);
    names
        .lines()
        .any(|path| blob(dir, tree, path).contains(needle))
}

#[tokio::test]
async fn a_credential_blob_that_the_agent_staged_is_not_kept_from_the_copied_index() {
    let (_t, dir, cred) = world();
    let private = PrivatePaths::default();
    private.register(&cred);
    replace_with_link(&dir, "a.txt", &cred);
    std::fs::remove_file(&cred).unwrap();
    // The agent stages the link: the real index holds the credential blob
    // and `diff-files` shows the path as clean.
    git(&dir, &["add", "-u"]);
    assert_eq!(git(&dir, &["diff-files", "--name-only"]), "");
    write(&dir, "b.txt", "two, edited\n");
    let wt = open(&dir).await;
    let snap = snapshot(&wt, &private).await;
    assert_eq!(snap.hidden, ["a.txt"]);
    assert_eq!(blob(&dir, &snap.tree, "a.txt"), "one\n", "the HEAD blob");
    assert_eq!(blob(&dir, &snap.tree, "b.txt"), "two, edited\n");
    assert!(!tree_holds(&dir, &snap.tree, SECRET));
    let key = key("T1", Phase::Start);
    let Captured::Published(c) = wt.capture(&key, Limits::default(), &private).await.unwrap()
    else {
        panic!("not published");
    };
    assert!(!tree_holds(&dir, &c.tree, SECRET));
}

#[tokio::test]
async fn a_staged_credential_at_a_path_that_head_lacks_is_removed_and_hidden() {
    let (_t, dir, cred) = world();
    let private = PrivatePaths::default();
    private.register(&cred);
    std::fs::hard_link(&cred, dir.join("new.txt")).unwrap();
    std::fs::remove_file(&cred).unwrap();
    git(&dir, &["add", "new.txt"]);
    let wt = open(&dir).await;
    let snap = snapshot(&wt, &private).await;
    assert_eq!(snap.hidden, ["new.txt"]);
    let names = git(&dir, &["ls-tree", "-r", "--name-only", &snap.tree]);
    assert!(!names.contains("new.txt"), "{names}");
    assert!(!tree_holds(&dir, &snap.tree, SECRET));
}

#[tokio::test]
async fn a_staged_credential_in_a_repository_without_a_commit_is_hidden() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let dir = root.join("repo");
    std::fs::create_dir(&dir).unwrap();
    git(&dir, &["init", "-q", "-b", "main"]);
    write(&root, "cred/secret", &format!("{SECRET}\n"));
    let cred = root.join("cred/secret");
    let private = PrivatePaths::default();
    private.register(&cred);
    write(&dir, "ok.txt", "fine\n");
    std::fs::hard_link(&cred, dir.join("key.txt")).unwrap();
    std::fs::remove_file(&cred).unwrap();
    git(&dir, &["add", "."]);
    let wt = open(&dir).await;
    let snap = snapshot(&wt, &private).await;
    assert_eq!(snap.hidden, ["key.txt"]);
    assert_eq!(blob(&dir, &snap.tree, "ok.txt"), "fine\n");
    assert!(!tree_holds(&dir, &snap.tree, SECRET));
}

#[tokio::test]
async fn a_file_that_the_agent_staged_is_captured_from_the_work_tree_bytes() {
    let (_t, dir, _cred) = world();
    write(&dir, "b.txt", "two, staged\n");
    git(&dir, &["add", "-u"]);
    write(&dir, "b.txt", "two, after the staging\n");
    write(&dir, "a.txt", "one, staged and kept\n");
    git(&dir, &["add", "-u"]);
    let wt = open(&dir).await;
    let snap = snapshot(&wt, &PrivatePaths::default()).await;
    assert!(snap.hidden.is_empty());
    assert_eq!(blob(&dir, &snap.tree, "b.txt"), "two, after the staging\n");
    assert_eq!(blob(&dir, &snap.tree, "a.txt"), "one, staged and kept\n");
    // A path that the agent staged and then deleted is gone from the tree.
    std::fs::remove_file(dir.join("a.txt")).unwrap();
    let snap = snapshot(&wt, &PrivatePaths::default()).await;
    let names = git(&dir, &["ls-tree", "-r", "--name-only", &snap.tree]);
    assert!(!names.contains("a.txt"), "{names}");
}

#[tokio::test]
async fn the_turn_diffs_against_a_benign_start_do_not_contain_a_staged_credential() {
    use crate::manager::files::diff_as;
    let det = Det::new();
    let (t, dir, cred) = world();
    let private = PrivatePaths::default();
    private.register(&cred);
    let secret = cred.clone();
    let edit = move |dir: &Path| {
        replace_with_link(dir, "a.txt", &secret);
        std::fs::remove_file(&secret).unwrap();
        git(dir, &["add", "-u"]);
        write(dir, "b.txt", "two, turn edit\n");
    };
    let store = finalized_turn_with(&det, &dir, false, &private, &edit).await;
    let root = t.path().canonicalize().unwrap();
    let p = policy(&root);
    let git_options = GitOptions::isolated();
    for member in [true, false] {
        let diff = diff_as(&store, &p, &private, &git_options, turn_query(), member)
            .await
            .unwrap();
        assert!(diff.patch.contains("+two, turn edit"), "{}", diff.patch);
        assert!(!diff.patch.contains(SECRET), "{}", diff.patch);
        assert!(!diff.patch.contains("a.txt"), "{}", diff.patch);
    }
}

#[tokio::test]
async fn an_ancestor_swapped_for_a_symlink_before_the_walk_never_reads_outside() {
    let (_t, dir, _cred) = world();
    let outside = dir.parent().unwrap().join("outside");
    write(&outside, "lib.rs", "outside content\n");
    write(&dir, "src/lib.rs", "fn a() { changed }\n");
    let wt = open(&dir).await;
    let index = PrivateIndex::copy_of(wt.git_dir()).unwrap();
    let swap = dir.clone();
    let target = outside.clone();
    let hook: BeforeWalk = Arc::new(move |rel: &str| {
        if rel == "src/lib.rs" {
            std::fs::remove_dir_all(swap.join("src")).unwrap();
            std::os::unix::fs::symlink(&target, swap.join("src")).unwrap();
        }
    });
    let hidden = wt
        .stage_changes_with(
            &index,
            Limits::default(),
            &PrivatePaths::default(),
            IndexTrust::Untrusted,
            Arc::new(|_| {}),
            hook,
        )
        .await
        .unwrap()
        .unwrap();
    assert!(hidden.is_empty());
    let env = [("GIT_INDEX_FILE", index.path().as_os_str())];
    let tree = wt.git().call(&["write-tree"], &env, None).await.unwrap();
    let tree = String::from_utf8_lossy(&tree).trim().to_owned();
    let names = git(&dir, &["ls-tree", "-r", "--name-only", &tree]);
    assert!(!names.contains("src/lib.rs"), "{names}");
    assert!(!some_object_holds(&dir, "outside content"));
}
