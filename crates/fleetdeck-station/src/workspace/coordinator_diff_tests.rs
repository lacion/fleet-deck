//! The coordinator diff (ADR-008 §4.4, test 13) on real Git and the store:
//! no byte of the old side leaves the station.

use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use fleetdeck_proto::event::{Event, FileChange, ProviderKind};
use fleetdeck_proto::files::{CoordinatorDiffQuery, DiffScope, SessionDiff, code};
use fleetdeck_proto::member_deny::MemberDeny;
use fleetdeck_testkit::Det;

use super::{Hooks, rewrite, run_in};
use crate::manager::policy::Policy;
use crate::store::{NewEvent, NewSession, OfflineApprovalPolicy, Store, StoreConfig};
use crate::workspace::files::FileError;
use crate::workspace::private_paths::private_paths_tests::{NEEDLE, private_of};
use crate::workspace::{GitOptions, PrivatePaths};

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

/// A text that only the old side of the repository holds.
const OLD_TOKEN: &str = "OLD-SIDE-TOKEN-4711";

struct Rig {
    _tmp: tempfile::TempDir,
    /// The directory of the repository.
    dir: PathBuf,
    /// The canonical temp root (the repository is below it).
    root: PathBuf,
    temp: tempfile::TempDir,
    store: Store,
    policy: Policy,
    private: PrivatePaths,
}

/// A repository whose base commit holds the files that `files` names, and a
/// session in it with that commit as its base. The session has a worktree
/// of its own unless `shared`.
async fn rig(files: &[(&str, &str)], shared: bool) -> Rig {
    let det = Det::new();
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let dir = root.join("repo");
    std::fs::create_dir(&dir).unwrap();
    git(&dir, &["init", "-q", "-b", "main"]);
    for (path, text) in files {
        let file = dir.join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, text).unwrap();
    }
    git(&dir, &["add", "-f", "."]);
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
    let mut events = vec![NewEvent::from(Event::SessionBase {
        git_ref: Some("main".to_owned()),
        oid: base,
        inferred: false,
    })];
    if !shared {
        events.push(NewEvent::from(Event::SessionNamed {
            callsign: "ace".to_owned(),
            worktree: Some(dir.display().to_string()),
        }));
    }
    store.append("s1".into(), events).await.unwrap();
    let policy = Policy::new(
        vec![root.clone()],
        4,
        OfflineApprovalPolicy::Wait,
        BTreeMap::new(),
    );
    Rig {
        _tmp: tmp,
        dir,
        root,
        temp: tempfile::tempdir().unwrap(),
        store,
        policy,
        private: PrivatePaths::default(),
    }
}

impl Rig {
    async fn diff(&self) -> Result<SessionDiff, FileError> {
        self.diff_with(Hooks::default()).await
    }

    async fn diff_with(&self, hooks: Hooks) -> Result<SessionDiff, FileError> {
        run_in(
            &self.store,
            &self.policy,
            &self.private,
            &GitOptions::isolated(),
            CoordinatorDiffQuery {
                session_id: "s1".into(),
            },
            self.temp.path(),
            hooks,
        )
        .await
    }

    fn write(&self, path: &str, text: &str) {
        let file = self.dir.join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, text).unwrap();
    }

    fn temp_is_empty(&self) -> bool {
        std::fs::read_dir(self.temp.path()).unwrap().count() == 0
    }
}

/// Every `@@` line of the patch has exactly the rebuilt form.
fn assert_clean_hunk_headers(patch: &str) {
    for line in patch.lines().filter(|l| l.starts_with("@@")) {
        let ok = line
            .strip_prefix("@@ -")
            .and_then(|r| r.strip_suffix(" @@"))
            .and_then(|r| r.split_once(" +"))
            .is_some_and(|(old, new)| {
                [old, new].iter().all(|side| {
                    side.split_once(',')
                        .is_some_and(|(a, b)| a.parse::<u64>().is_ok() && b.parse::<u64>().is_ok())
                })
            });
        assert!(ok, "a hunk header with more than numbers: {line:?}");
    }
}

#[tokio::test]
async fn the_diff_has_no_byte_of_the_old_side_not_even_in_a_hunk_header_or_a_git_metadata_line() {
    // `src.rs`: a function line far above the change (git would put it into
    // the `@@` header), an old line with a token, and a mode change.
    let old_src = format!(
        "SECRETFUNC_old_context(\n  l1\n  l2\n  l3\n  l4\n  l5\n  l6\n  l7 {OLD_TOKEN}\n  l8\n  l9\n  l10\n  l11\n"
    );
    let r = rig(
        &[
            ("src.rs", &old_src),
            ("config.txt", "password=hunter2\nsecond=hunter3\n"),
            ("gone.txt", &format!("{OLD_TOKEN} gone\nlast\n")),
            ("tool.sh", "#!/bin/sh\n"),
        ],
        false,
    )
    .await;
    // The same function line shows up in the header of a plain Git diff:
    // the test is about something real.
    r.write("src.rs", &old_src.replace(OLD_TOKEN, "new line"));
    assert!(
        git(&r.dir, &["diff", "-U1"]).contains("@@ SECRETFUNC_old_context("),
        "the repository has the history that the test needs"
    );
    // A committed credential under an ordinary name, replaced by a safe file.
    r.write("config.txt", "safe\n");
    std::fs::remove_file(r.dir.join("gone.txt")).unwrap();
    std::fs::set_permissions(
        r.dir.join("tool.sh"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    r.write("added.txt", "brand new\n");

    let d = r.diff().await.unwrap();
    for secret in [OLD_TOKEN, "hunter2", "hunter3", "SECRETFUNC"] {
        assert!(
            !d.patch.contains(secret),
            "{secret} left the station:\n{}",
            d.patch
        );
    }
    for metadata in [
        "index ",
        "old mode",
        "new mode",
        "similarity",
        "--- ",
        "+++ ",
        "diff --git",
    ] {
        assert!(!d.patch.contains(metadata), "{metadata}:\n{}", d.patch);
    }
    assert_clean_hunk_headers(&d.patch);
    // The removed lines are counted, the added lines are shown.
    assert!(
        d.patch
            .contains("diff --coordinator src.rs\n@@ -5,7 +5,7 @@\n"),
        "{}",
        d.patch
    );
    assert!(
        d.patch.contains("-[1 lines removed]\n+  l7 new line\n"),
        "{}",
        d.patch
    );
    assert!(
        d.patch.contains(
            "diff --coordinator config.txt\n@@ -1,2 +1,1 @@\n-[2 lines removed]\n+safe\n"
        ),
        "{}",
        d.patch
    );
    assert!(
        d.patch
            .contains("diff --coordinator gone.txt\n(deleted file)\n-[2 lines removed]\n"),
        "{}",
        d.patch
    );
    assert!(
        d.patch
            .contains("diff --coordinator added.txt\n@@ -0,0 +1,1 @@\n+brand new\n"),
        "{}",
        d.patch
    );
    // The mode change alone has no line to show.
    assert!(
        d.patch.contains("diff --coordinator tool.sh\n"),
        "{}",
        d.patch
    );
    // The summary counts the real lines.
    assert_eq!(d.summary.insertions, 1 + 1 + 1);
    assert_eq!(d.summary.deletions, 1 + 2 + 2);
    assert_eq!(d.scope, DiffScope::Worktree);
    assert!(r.temp_is_empty());
}

#[tokio::test]
async fn a_path_with_a_private_name_on_either_side_is_dropped() {
    let r = rig(
        &[
            (".ssh/id", "key\n"),
            ("keep.txt", "keep\n"),
            ("server.pem", "pem\n"),
            ("old.txt", "text\n"),
        ],
        false,
    )
    .await;
    // The old name is private; the new name is plain.
    git(&r.dir, &["mv", ".ssh/id", "plain.txt"]);
    // A file of the member deny list, changed.
    r.write("server.pem", "changed pem\n");
    // A plain file that is renamed to a private name.
    git(&r.dir, &["mv", "old.txt", "renamed.pem"]);
    r.write("keep.txt", "kept and changed\n");
    let d = r.diff().await.unwrap();
    assert!(d.patch.contains("keep.txt"), "{}", d.patch);
    for hidden in ["plain.txt", ".ssh", "server.pem", "renamed.pem", "old.txt"] {
        assert!(!d.patch.contains(hidden), "{hidden}:\n{}", d.patch);
    }
    assert_eq!(d.summary.files_changed, 1);
}

#[tokio::test]
async fn the_member_deny_list_of_the_station_hides_more_paths() {
    let mut r = rig(&[("notes.txt", "a\n"), ("plain.txt", "b\n")], false).await;
    r.policy
        .set_member_deny(MemberDeny::new(&["notes.*".to_owned()]));
    r.write("notes.txt", "changed\n");
    r.write("plain.txt", "changed\n");
    let d = r.diff().await.unwrap();
    assert!(!d.patch.contains("notes.txt"), "{}", d.patch);
    assert!(d.patch.contains("plain.txt"), "{}", d.patch);
}

#[tokio::test]
async fn a_tracked_file_that_is_a_hard_link_to_a_private_file_is_hidden() {
    // The repository is below the home of the station: the private files of
    // the layout are in the registry.
    let tmp_home = tempfile::tempdir().unwrap();
    let home = tmp_home.path().canonicalize().unwrap();
    let mut r = rig(&[("link.txt", "plain\n"), ("other.txt", "plain\n")], false).await;
    r.private = private_of(&home);
    let credential = home.join(".ssh/id_ed25519");
    std::fs::remove_file(r.dir.join("link.txt")).unwrap();
    std::fs::hard_link(&credential, r.dir.join("link.txt")).unwrap();
    // A second, plain hard link between two files of the repository: the
    // link count alone hides it (a link to a credential cannot be told apart
    // cheaply, DL-173).
    std::fs::remove_file(r.dir.join("other.txt")).unwrap();
    let outside = r.root.join("outside.txt");
    std::fs::write(&outside, format!("{NEEDLE} outside\n")).unwrap();
    std::fs::hard_link(&outside, r.dir.join("other.txt")).unwrap();
    let d = r.diff().await.unwrap();
    // The registry knows the inode of the first link: its name is dropped or
    // shown as hidden. The second link is hidden by its link count.
    let listed = d.patch.contains("diff --coordinator link.txt\n");
    assert!(
        !listed
            || d.patch
                .contains("diff --coordinator link.txt\n(hidden: private file)\n"),
        "{}",
        d.patch
    );
    assert!(
        d.patch
            .contains("diff --coordinator other.txt\n(hidden: private file)\n"),
        "{}",
        d.patch
    );
    assert!(!d.patch.contains(NEEDLE), "{}", d.patch);
    assert_eq!(d.summary.files_changed, 0);
}

#[tokio::test]
async fn a_path_that_is_replaced_after_the_check_cannot_change_the_bytes() {
    let r = rig(&[("a.txt", "one\n")], false).await;
    r.write("a.txt", "one\nchecked bytes\n");
    let secret = r.root.join("secret.txt");
    std::fs::write(&secret, format!("{NEEDLE} secret\n")).unwrap();
    let dir = r.dir.clone();
    let swapped = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let seen = Arc::clone(&swapped);
    let hooks = Hooks {
        before_open: Arc::new(|_| {}),
        // After the identity check, before the read: the path now names a
        // hard link to a credential.
        after_open: Arc::new(move |rel| {
            let link = dir.join("swap.tmp");
            std::fs::hard_link(&secret, &link).unwrap();
            std::fs::rename(&link, dir.join(rel)).unwrap();
            seen.store(true, std::sync::atomic::Ordering::SeqCst);
        }),
    };
    let d = r.diff_with(hooks).await.unwrap();
    assert!(swapped.load(std::sync::atomic::Ordering::SeqCst));
    assert!(d.patch.contains("+checked bytes\n"), "{}", d.patch);
    assert!(!d.patch.contains(NEEDLE), "{}", d.patch);
}

#[tokio::test]
async fn an_identity_check_that_cannot_run_fails_the_whole_query_and_the_temp_directory_is_removed()
{
    if rustix::process::geteuid().is_root() {
        return; // a file mode does not stop root
    }
    let r = rig(&[("a.txt", "one\n"), ("b.txt", "two\n")], false).await;
    r.write("a.txt", "one\nchanged\n");
    r.write("b.txt", "two\nchanged\n");
    let dir = r.dir.clone();
    let hooks = Hooks {
        // The file cannot be opened any more: the identity of the open file
        // cannot be checked.
        before_open: Arc::new(move |rel| {
            if rel == "b.txt" {
                std::fs::set_permissions(dir.join(rel), std::fs::Permissions::from_mode(0o000))
                    .unwrap();
            }
        }),
        after_open: Arc::new(|_| {}),
    };
    let e = r.diff_with(hooks).await.unwrap_err();
    assert_eq!(e.code, code::PRIVACY_UNKNOWN, "{e}");
    assert!(r.temp_is_empty(), "the temporary directory stays");
}

#[tokio::test]
async fn a_rename_a_binary_file_a_link_and_a_large_file_have_their_own_forms() {
    let long: String = (0..60).map(|i| format!("line {i}\n")).collect();
    let r = rig(
        &[("old.txt", &long), ("bin.dat", "x\n"), ("big.txt", "x\n")],
        false,
    )
    .await;
    git(&r.dir, &["mv", "old.txt", "new.txt"]);
    r.write("new.txt", &long.replace("line 30\n", "line thirty\n"));
    std::fs::write(r.dir.join("bin.dat"), [0u8, 1, 2]).unwrap();
    std::os::unix::fs::symlink("/etc/hostname", r.dir.join("link.txt")).unwrap();
    std::fs::write(r.dir.join("big.txt"), vec![b'a'; 300 * 1024]).unwrap();
    let d = r.diff().await.unwrap();
    assert!(
        d.patch.contains(
            "diff --coordinator new.txt\n(renamed) old.txt -> new.txt\n@@ -28,7 +28,7 @@\n"
        ),
        "{}",
        d.patch
    );
    assert!(
        d.patch.contains("-[1 lines removed]\n+line thirty\n"),
        "{}",
        d.patch
    );
    assert!(
        d.patch
            .contains("diff --coordinator bin.dat\n(binary file)\n"),
        "{}",
        d.patch
    );
    assert!(
        d.patch
            .contains("diff --coordinator big.txt\n(hidden: diff unavailable)\n"),
        "{}",
        d.patch
    );
    // A symlink is not followed: its target is nowhere in the patch.
    assert!(!d.patch.contains("hostname"), "{}", d.patch);
    assert!(d.truncated);
    assert_clean_hunk_headers(&d.patch);
}

#[tokio::test]
async fn a_session_in_a_shared_folder_gets_only_the_files_that_it_changed() {
    let r = rig(&[("mine.txt", "a\n"), ("theirs.txt", "b\n")], true).await;
    r.write("mine.txt", "a\nmore\n");
    r.write("theirs.txt", "b\nmore\n");
    // No file changed that the session reported: an empty diff.
    let d = r.diff().await.unwrap();
    assert_eq!(d.scope, DiffScope::SessionFiles);
    assert!(d.patch.is_empty(), "{}", d.patch);
    let event = Event::FileChanged {
        path: "mine.txt".to_owned(),
        change: FileChange::Modified,
        node_id: None,
    };
    r.store
        .append("s1".into(), vec![NewEvent::from(event)])
        .await
        .unwrap();
    let d = r.diff().await.unwrap();
    assert!(d.patch.contains("mine.txt"), "{}", d.patch);
    assert!(!d.patch.contains("theirs.txt"), "{}", d.patch);
}

#[tokio::test]
async fn the_refusals_of_the_session_diff_keep_their_codes() {
    let r = rig(&[("a.txt", "one\n")], false).await;
    let e = run_in(
        &r.store,
        &r.policy,
        &r.private,
        &GitOptions::isolated(),
        CoordinatorDiffQuery {
            session_id: "nope".into(),
        },
        r.temp.path(),
        Hooks::default(),
    )
    .await
    .unwrap_err();
    assert_eq!(e.code, code::UNKNOWN_SESSION);
    // A session without a base commit.
    r.store
        .create_session(NewSession {
            session_id: "s2".into(),
            provider: ProviderKind::Claude,
            provider_instance: "claude".into(),
            cwd: r.dir.display().to_string(),
            worktree: None,
            branch: None,
            mode: None,
            title: None,
            offline_approval_policy: OfflineApprovalPolicy::Wait,
        })
        .await
        .unwrap();
    let e = run_in(
        &r.store,
        &r.policy,
        &r.private,
        &GitOptions::isolated(),
        CoordinatorDiffQuery {
            session_id: "s2".into(),
        },
        r.temp.path(),
        Hooks::default(),
    )
    .await
    .unwrap_err();
    assert_eq!(e.code, code::NO_BASE);
    assert!(r.temp_is_empty());
}

#[test]
fn the_rewrite_rebuilds_the_headers_counts_the_removed_runs_and_keeps_the_new_side() {
    let output = "diff --git a/a b/b\nindex 1111111..2222222 100644\n--- a/a\n+++ b/b\n\
                  @@ -3,6 +3,5 @@ fn old_function_context\n ctx1\n-gone 1\n-gone 2\n\\ No newline at end of file\n+added\n\\ No newline at end of file\n ctx2\r\n-gone 3\n ctx3\n";
    let out = rewrite(output);
    assert_eq!(
        out.text,
        "@@ -3,6 +3,5 @@\n ctx1\n-[2 lines removed]\n+added\n\\ No newline at end of file\n ctx2\r\n-[1 lines removed]\n ctx3\n"
    );
    assert_eq!((out.added, out.removed), (1, 3));
    // A header without a count has the count 1; a header that does not parse
    // drops its hunk.
    assert_eq!(rewrite("@@ -1 +1 @@\n+x\n").text, "@@ -1,1 +1,1 @@\n+x\n");
    assert_eq!(rewrite("@@ nonsense @@\n+x\n").text, "");
}
