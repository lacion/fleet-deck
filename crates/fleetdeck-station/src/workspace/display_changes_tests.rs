//! `files.display_changes` on real Git and the store (R13 P2b, §5D items 13
//! and 14), and the parser of the section list.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use fleetdeck_proto::display_changes::{
    DisplayChanges, DisplayChangesQuery, MAX_DISPLAY_PATCH_FILE,
};
use fleetdeck_proto::event::{DiffSummary, Event, ProviderKind};
use fleetdeck_proto::files::{DiffScope, code};
use fleetdeck_proto::member_deny::MemberDeny;
use fleetdeck_testkit::Det;

use super::{build, run, section_path, sections};
use crate::manager::policy::Policy;
use crate::store::{NewEvent, NewSession, OfflineApprovalPolicy, Store, StoreConfig};
use crate::workspace::files::FileError;
use crate::workspace::private_paths::private_paths_tests::{
    NEEDLE, NORMAL, PRIVATE_FILES, layout, private_of,
};
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

struct Rig {
    _tmp: tempfile::TempDir,
    dir: PathBuf,
    store: Store,
    policy: Policy,
    private: PrivatePaths,
}

/// A repository with `a.txt` and `b.txt` committed and a session with its own
/// worktree and the first commit as its base (unless `with_base` is false).
async fn rig(with_base: bool) -> Rig {
    let det = Det::new();
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let dir = root.join("repo");
    std::fs::create_dir(&dir).unwrap();
    git(&dir, &["init", "-q", "-b", "main"]);
    std::fs::write(dir.join("a.txt"), "one\n").unwrap();
    std::fs::write(dir.join("b.txt"), "two\n").unwrap();
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
    let mut events = vec![NewEvent::from(Event::SessionNamed {
        callsign: "ace".to_owned(),
        worktree: Some(dir.display().to_string()),
    })];
    if with_base {
        events.push(NewEvent::from(Event::SessionBase {
            git_ref: Some("main".to_owned()),
            oid: base,
            inferred: false,
        }));
    }
    store.append("s1".into(), events).await.unwrap();
    let policy = Policy::new(vec![root], 4, OfflineApprovalPolicy::Wait, BTreeMap::new());
    Rig {
        _tmp: tmp,
        dir,
        store,
        policy,
        private: PrivatePaths::default(),
    }
}

impl Rig {
    async fn changes(&self, patch: bool, max: Option<u64>) -> Result<DisplayChanges, FileError> {
        run(
            &self.store,
            &self.policy,
            &self.private,
            &GitOptions::isolated(),
            DisplayChangesQuery {
                session_id: "s1".into(),
                patch,
                max_bytes: max,
            },
        )
        .await
    }
}

fn file<'a>(
    c: &'a DisplayChanges,
    path: &str,
) -> &'a fleetdeck_proto::display_changes::DisplayFileChange {
    c.files
        .iter()
        .find(|f| f.path.as_deref() == Some(path))
        .unwrap_or_else(|| panic!("{path} in {:?}", c.files))
}

#[tokio::test]
async fn the_statistics_cover_edits_new_files_renames_and_binary_files() {
    let r = rig(true).await;
    std::fs::write(r.dir.join("a.txt"), "one\nmore\nlines\n").unwrap();
    std::fs::create_dir(r.dir.join("sub")).unwrap();
    std::fs::rename(r.dir.join("b.txt"), r.dir.join("sub/renamed.txt")).unwrap();
    std::fs::write(r.dir.join("new.txt"), "hello\nworld\n").unwrap();
    std::fs::write(r.dir.join("bin.dat"), [0u8, 1, 2, 3]).unwrap();
    git(&r.dir, &["add", "-A"]);
    let c = r.changes(false, None).await.unwrap();
    assert_eq!(c.scope, DiffScope::Worktree);
    let a = file(&c, "a.txt");
    assert_eq!((a.added, a.removed, a.binary), (Some(2), Some(0), false));
    let n = file(&c, "new.txt");
    assert_eq!((n.added, n.removed), (Some(2), Some(0)));
    let rn = file(&c, "sub/renamed.txt");
    assert_eq!((rn.added, rn.removed), (Some(0), Some(0)));
    let bin = file(&c, "bin.dat");
    assert!(bin.binary);
    assert_eq!((bin.added, bin.removed), (None, None));
    assert!(c.files.iter().all(|f| !f.hidden && f.patch.is_none()));
    assert_eq!(c.summary.files_changed, 4);
}

#[tokio::test]
async fn a_denied_file_becomes_an_anonymous_marker_and_leaves_the_summary() {
    let r = rig(true).await;
    std::fs::write(r.dir.join("a.txt"), "one\nmore\n").unwrap();
    std::fs::write(r.dir.join(".env"), "TOKEN=sentinel-token-123\n").unwrap();
    std::fs::write(r.dir.join("secrets.txt"), "password=sentinel-pass-456\n").unwrap();
    for patch in [false, true] {
        let c = r.changes(patch, None).await.unwrap();
        let text = serde_json::to_string(&c).unwrap();
        for hidden in ["sentinel", ".env", "secrets", "TOKEN", "password"] {
            assert!(!text.contains(hidden), "{hidden} in {text}");
        }
        let markers: Vec<_> = c.files.iter().filter(|f| f.hidden).collect();
        assert_eq!(markers.len(), 2, "{text}");
        for m in markers {
            assert!(m.path.is_none() && m.patch.is_none() && !m.too_large && !m.binary);
            assert!(m.added.is_none() && m.removed.is_none());
        }
        // The markers come after the visible files; only `a.txt` is counted.
        assert!(c.files[0].path.is_some() && c.files[1].hidden && c.files[2].hidden);
        assert_eq!(c.summary.files_changed, 1);
        assert_eq!((c.summary.insertions, c.summary.deletions), (1, 0));
    }
}

#[tokio::test]
async fn a_private_path_is_not_listed_and_has_no_marker() {
    let mut r = rig(true).await;
    layout(&r.dir);
    r.private = private_of(&r.dir);
    for file in PRIVATE_FILES.iter().chain(NORMAL) {
        std::fs::write(r.dir.join(file), format!("{NEEDLE} changed {file}\n")).unwrap();
    }
    let c = r.changes(true, None).await.unwrap();
    let text = serde_json::to_string(&c).unwrap();
    for name in ["station.toml", "jira_token", "id_ed25519", "hosts.yml"] {
        assert!(!text.contains(name), "{name} in {text}");
    }
    for name in NORMAL {
        assert!(
            c.files.iter().any(|f| f.path.as_deref() == Some(*name)),
            "{name}"
        );
    }
    assert!(c.files.iter().all(|f| !f.hidden || f.path.is_none()));
}

#[tokio::test]
async fn the_patch_comes_only_when_asked_and_a_large_section_is_marked() {
    let r = rig(true).await;
    std::fs::write(r.dir.join("a.txt"), "one\nmore\n").unwrap();
    let big: String = (0..8000).map(|i| format!("line number {i}\n")).collect();
    assert!(big.len() as u64 > MAX_DISPLAY_PATCH_FILE);
    std::fs::write(r.dir.join("big.txt"), big).unwrap();
    let without = r.changes(false, None).await.unwrap();
    assert!(
        without
            .files
            .iter()
            .all(|f| f.patch.is_none() && !f.too_large)
    );
    let with = r.changes(true, None).await.unwrap();
    let a = file(&with, "a.txt");
    let patch = a.patch.as_deref().unwrap();
    assert!(patch.starts_with("diff --git a/a.txt b/a.txt"), "{patch}");
    assert!(
        patch.contains("+more") && !patch.contains("big.txt"),
        "{patch}"
    );
    let big = file(&with, "big.txt");
    assert!(big.too_large && big.patch.is_none());
    assert_eq!(big.added, Some(8000));
}

#[tokio::test]
async fn a_small_byte_limit_marks_the_answer_as_cut() {
    let r = rig(true).await;
    std::fs::write(r.dir.join("a.txt"), "one\nmore\nlines\nand more lines\n").unwrap();
    std::fs::write(r.dir.join("b.txt"), "two\nmore\nlines\nand more lines\n").unwrap();
    let full = r.changes(true, None).await.unwrap();
    assert!(!full.truncated);
    assert_eq!(full.files.len(), 2);
    let cut = r.changes(true, Some(10)).await.unwrap();
    assert!(cut.truncated);
    assert!(cut.files.len() < full.files.len(), "{:?}", cut.files);
}

#[tokio::test]
async fn an_unknown_session_and_a_session_without_a_base_are_refused() {
    let r = rig(false).await;
    assert_eq!(
        r.changes(false, None).await.unwrap_err().code,
        code::NO_BASE
    );
    let err = run(
        &r.store,
        &r.policy,
        &r.private,
        &GitOptions::isolated(),
        DisplayChangesQuery {
            session_id: "nope".into(),
            patch: false,
            max_bytes: None,
        },
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, code::UNKNOWN_SESSION);
}

fn summary(files: u32, insertions: u64, deletions: u64) -> DiffSummary {
    DiffSummary {
        files_changed: files,
        insertions,
        deletions,
    }
}

#[test]
fn the_parser_reads_quoted_names_spaces_renames_and_mode_changes() {
    let patch = "\
diff --git \"a/caf\\303\\251.txt\" \"b/caf\\303\\251.txt\"
--- \"a/caf\\303\\251.txt\"
+++ \"b/caf\\303\\251.txt\"
@@ -1 +1,2 @@
 x
+y
diff --git a/src/a b.rs b/src/a b.rs
--- a/src/a b.rs
+++ b/src/a b.rs
@@ -1,2 +1 @@
-gone
 keep
diff --git a/old.txt b/new.txt
similarity index 90%
rename from old.txt
rename to new.txt
--- a/old.txt
+++ b/new.txt
@@ -1 +1 @@
-a
+b
diff --git a/run.sh b/run.sh
old mode 100644
new mode 100755
diff --git a/pic.png b/pic.png
Binary files a/pic.png and b/pic.png differ
";
    let (files, s) = build(&MemberDeny::new(&[]), patch, summary(5, 3, 2), true);
    let seen: Vec<_> = files
        .iter()
        .map(|f| (f.path.clone().unwrap(), f.added, f.removed, f.binary))
        .collect();
    assert_eq!(
        seen,
        vec![
            ("caf\u{e9}.txt".to_owned(), Some(1), Some(0), false),
            ("src/a b.rs".to_owned(), Some(0), Some(1), false),
            ("new.txt".to_owned(), Some(1), Some(1), false),
            ("run.sh".to_owned(), Some(0), Some(0), false),
            ("pic.png".to_owned(), None, None, true),
        ]
    );
    assert!(files.iter().all(|f| {
        f.patch
            .as_deref()
            .is_some_and(|p| p.starts_with("diff --git "))
    }));
    assert_eq!(s, summary(5, 3, 2));
}

#[test]
fn an_unreadable_header_and_a_denied_file_are_hidden_and_leave_the_summary() {
    let patch = "\
diff --git a/ok.txt b/ok.txt
--- a/ok.txt
+++ b/ok.txt
@@ -1 +1,2 @@
 x
+y
diff --git a/.env b/.env
--- a/.env
+++ b/.env
@@ -1 +1,2 @@
 A=1
+B=2
diff --git a/one b/two
--- a/one
+++ b/two
@@ -1 +1,2 @@
 x
+y
";
    // `one` and `two` differ and there is no rename line: no readable path.
    let (files, s) = build(&MemberDeny::new(&[]), patch, summary(3, 3, 0), false);
    assert_eq!(files.len(), 3);
    assert_eq!(files[0].path.as_deref(), Some("ok.txt"));
    assert!(files[1].hidden && files[2].hidden);
    assert!(
        files
            .iter()
            .skip(1)
            .all(|f| f.path.is_none() && f.added.is_none())
    );
    assert_eq!(s, summary(1, 1, 0));
}

#[test]
fn the_section_list_and_the_cut_mark() {
    assert!(sections("").is_empty());
    let patch = "diff --git a/x b/x\n@@ -1 +1 @@\n-a\n+b\ndiff --git a/y b/y\n";
    let parts = sections(patch);
    assert_eq!(parts.len(), 2);
    assert_eq!(section_path(parts[1]).as_deref(), Some("y"));
    let (files, _) = build(
        &MemberDeny::new(&[]),
        &format!("{patch}\n[diff truncated]\n"),
        summary(2, 1, 1),
        true,
    );
    assert!(!files[1].patch.as_deref().unwrap().contains("truncated"));
}
