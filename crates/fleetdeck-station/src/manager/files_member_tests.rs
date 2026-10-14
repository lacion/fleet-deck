//! The member file deny list (R11.9, DL-166): a call that the deck marks as
//! made for a member never lists, reads, searches or diffs a sensitive file,
//! and a member browses the workspace of a session only. The owner's calls
//! are not changed.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use fleetdeck_proto::TurnId;
use fleetdeck_proto::event::InputState;
use fleetdeck_proto::files::{
    FileListQuery, FileReadQuery, FileSearchQuery, SearchMode, TurnDiffQuery, code,
};
use fleetdeck_proto::member_deny::MemberDeny;
use fleetdeck_testkit::Det;

use super::files_tests::{session, store_with_session, ws};
use super::{diff_as, list_as, read_as, search_as};
use crate::manager::policy::Policy;
use crate::manager::turns::turns_tests::{
    S1, completed, dispatch, git, input, repo, setup, started,
};
use crate::store::OfflineApprovalPolicy;
use crate::workspace::{GitOptions, PrivatePaths};

/// A policy whose deny list has one more pattern.
fn deny_policy(root: &Path, extra: &[&str]) -> Policy {
    let mut p = Policy::new(
        vec![root.to_path_buf()],
        4,
        OfflineApprovalPolicy::Wait,
        BTreeMap::new(),
    );
    let extra: Vec<String> = extra.iter().map(|s| (*s).to_owned()).collect();
    p.set_member_deny(MemberDeny::new(&extra));
    p
}

fn tree() -> (tempfile::TempDir, std::path::PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/main.rs"), "fn main() { token_here(); }\n").unwrap();
    std::fs::write(root.join(".env"), "token_here=abc\n").unwrap();
    std::fs::write(root.join("src/server.pem"), "token_here pem\n").unwrap();
    std::fs::create_dir_all(root.join("secrets")).unwrap();
    std::fs::write(root.join("secrets/db.txt"), "token_here db\n").unwrap();
    std::fs::create_dir_all(root.join("private")).unwrap();
    std::fs::write(root.join("private/plan.txt"), "token_here plan\n").unwrap();
    std::fs::write(root.join("README.md"), "token_here readme\n").unwrap();
    (tmp, root)
}

#[tokio::test]
async fn a_member_list_leaves_out_the_hidden_files_and_the_owner_sees_all() {
    let det = Det::new();
    let (_t, root) = tree();
    let s = store_with_session(&det, &root).await;
    let p = deny_policy(&root, &["private/"]);
    let names = |l: fleetdeck_proto::files::FileListing| -> Vec<String> {
        l.entries.into_iter().map(|e| e.name).collect()
    };
    let q = |path: &str| FileListQuery {
        root: session(),
        path: path.to_owned(),
    };
    let member = names(
        list_as(&s, &p, &PrivatePaths::default(), q(""), true)
            .await
            .unwrap(),
    );
    assert_eq!(member, ["src", "README.md"]);
    let owner = names(
        list_as(&s, &p, &PrivatePaths::default(), q(""), false)
            .await
            .unwrap(),
    );
    for hidden in [".env", "secrets", "private"] {
        assert!(owner.iter().any(|n| n == hidden), "{hidden}");
    }
    let src = names(
        list_as(&s, &p, &PrivatePaths::default(), q("src"), true)
            .await
            .unwrap(),
    );
    assert_eq!(src, ["main.rs"]);
    // A hidden directory is not found, not listed as empty.
    for path in ["secrets", "private"] {
        let err = list_as(&s, &p, &PrivatePaths::default(), q(path), true)
            .await
            .unwrap_err();
        assert_eq!(err.code, code::NOT_FOUND, "{path}");
    }
}

#[tokio::test]
async fn a_member_read_of_a_hidden_file_is_not_found() {
    let det = Det::new();
    let (_t, root) = tree();
    let s = store_with_session(&det, &root).await;
    let p = deny_policy(&root, &["private/"]);
    let q = |path: &str| FileReadQuery {
        root: session(),
        path: path.to_owned(),
        max_bytes: None,
    };
    for path in [
        ".env",
        "src/server.pem",
        "secrets/db.txt",
        "private/plan.txt",
    ] {
        let err = read_as(&s, &p, &PrivatePaths::default(), q(path), true)
            .await
            .unwrap_err();
        assert_eq!(err.code, code::NOT_FOUND, "{path}");
        assert!(
            read_as(&s, &p, &PrivatePaths::default(), q(path), false)
                .await
                .is_ok(),
            "{path}"
        );
    }
    let ok = read_as(&s, &p, &PrivatePaths::default(), q("README.md"), true)
        .await
        .unwrap();
    assert_eq!(ok.text.as_deref(), Some("token_here readme\n"));
}

#[tokio::test]
async fn a_member_search_never_names_or_reads_a_hidden_file() {
    let det = Det::new();
    let (_t, root) = tree();
    let s = store_with_session(&det, &root).await;
    let p = deny_policy(&root, &["private/"]);
    let git = GitOptions::isolated();
    for (mode, text) in [
        (SearchMode::Content, "token_here"),
        (SearchMode::Name, "en"),
    ] {
        let q = || FileSearchQuery {
            root: session(),
            query: text.to_owned(),
            mode,
            max_hits: None,
        };
        let member = search_as(&s, &p, &PrivatePaths::default(), &git, q(), true)
            .await
            .unwrap()
            .hits;
        for hit in &member {
            for hidden in [".env", "server.pem", "secrets", "private"] {
                assert!(!hit.path.contains(hidden), "{mode:?}: {}", hit.path);
            }
        }
        let owner = search_as(&s, &p, &PrivatePaths::default(), &git, q(), false)
            .await
            .unwrap()
            .hits;
        assert!(
            owner
                .iter()
                .any(|h| h.path.contains("secrets") || h.path.contains(".env"))
        );
    }
    // A content search finds the visible files only.
    let hits = search_as(
        &s,
        &p,
        &PrivatePaths::default(),
        &git,
        FileSearchQuery {
            root: session(),
            query: "token_here".to_owned(),
            mode: SearchMode::Content,
            max_hits: None,
        },
        true,
    )
    .await
    .unwrap()
    .hits;
    let mut found: Vec<&str> = hits.iter().map(|h| h.path.as_str()).collect();
    found.sort_unstable();
    assert_eq!(found, ["README.md", "src/main.rs"]);
}

#[tokio::test]
async fn a_member_browses_the_session_workspace_only() {
    let det = Det::new();
    let (_t, root) = tree();
    let s = store_with_session(&det, &root).await;
    let p = deny_policy(&root, &[]);
    let err = list_as(
        &s,
        &p,
        &PrivatePaths::default(),
        FileListQuery {
            root: ws(&root),
            path: String::new(),
        },
        true,
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, code::OUTSIDE_ROOTS);
    assert!(
        list_as(
            &s,
            &p,
            &PrivatePaths::default(),
            FileListQuery {
                root: ws(&root),
                path: String::new(),
            },
            false,
        )
        .await
        .is_ok()
    );
}

#[tokio::test]
async fn a_member_diff_loses_the_hunks_of_hidden_files() {
    let det = Det::new();
    let (_t, dir) = repo();
    // A checkpoint holds tracked files only: the secret is tracked.
    std::fs::write(dir.join(".env"), "TOKEN=old\n").unwrap();
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-q", "-m", "secret"]);
    let (s, turns) = setup(&det, &dir).await;
    let p = deny_policy(&dir, &[]);
    let git = GitOptions::isolated();
    dispatch(&turns, "in-1").await;
    turns.observe(&[started("turn-1")]).await;
    std::fs::write(dir.join("a.txt"), "one\nmore\n").unwrap();
    std::fs::write(dir.join(".env"), "TOKEN=abc\n").unwrap();
    turns
        .observe(&[
            input("in-1", Some("turn-1"), InputState::Started),
            completed("turn-1"),
        ])
        .await;
    Arc::clone(&turns).capture_due().await;
    let q = || TurnDiffQuery {
        session_id: S1.into(),
        turn_id: Some(TurnId::from("turn-1")),
        checkpoint: None,
        max_bytes: None,
    };
    let owner = diff_as(&s, &p, &PrivatePaths::default(), &git, q(), false)
        .await
        .unwrap();
    assert!(owner.patch.contains("TOKEN=abc"), "{}", owner.patch);
    assert_eq!(owner.hidden_files, 0);
    let member = diff_as(&s, &p, &PrivatePaths::default(), &git, q(), true)
        .await
        .unwrap();
    assert!(member.patch.contains("+more"), "{}", member.patch);
    assert!(!member.patch.contains("TOKEN"), "{}", member.patch);
    assert!(!member.patch.contains(".env"), "{}", member.patch);
    assert_eq!(member.hidden_files, 1);
    assert_eq!(
        member.summary.files_changed + 1,
        owner.summary.files_changed
    );
}

#[tokio::test]
async fn a_publish_with_a_new_deny_pattern_hides_the_files_from_the_next_member_call() {
    let det = Det::new();
    let (_t, root) = tree();
    let s = store_with_session(&det, &root).await;
    let hub = crate::manager::policy_hub::PolicyHub::new(deny_policy(&root, &[]));
    let names = |l: fleetdeck_proto::files::FileListing| -> Vec<String> {
        l.entries.into_iter().map(|e| e.name).collect()
    };
    let q = || FileListQuery {
        root: session(),
        path: String::new(),
    };
    let before = names(
        list_as(&s, &hub.current(), &PrivatePaths::default(), q(), true)
            .await
            .unwrap(),
    );
    assert!(before.iter().any(|n| n == "private"), "{before:?}");

    let config = crate::manager::policy_hub::test_config(&root, "member_deny = [\"private/\"]\n");
    hub.swap(hub.current().reconfigured(&config));
    let after = names(
        list_as(&s, &hub.current(), &PrivatePaths::default(), q(), true)
            .await
            .unwrap(),
    );
    assert!(!after.iter().any(|n| n == "private"), "{after:?}");
    // The owner still sees it.
    let owner = names(
        list_as(&s, &hub.current(), &PrivatePaths::default(), q(), false)
            .await
            .unwrap(),
    );
    assert!(owner.iter().any(|n| n == "private"), "{owner:?}");
}
