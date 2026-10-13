//! The private paths of the station (DL-173 section 2.1) on the file
//! routes: list, read, search, the turn diff and the root check refuse the
//! station's own credential and config locations for the owner and for a
//! member, in the default split layout, below an allowed root that holds
//! them. A normal file next to them still works.

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;

use fleetdeck_proto::TurnId;
use fleetdeck_proto::event::InputState;
use fleetdeck_proto::files::{
    FileListQuery, FileReadQuery, FileRoot, FileSearchQuery, SearchBackend, SearchMode,
    TurnDiffQuery, code,
};
use fleetdeck_testkit::Det;

use super::files_tests::{policy, session, store_with_session, ws};
use super::{diff_as, list_as, read_as, search_as};
use crate::manager::turns::turns_tests::{
    S1, completed, dispatch, git, input, repo, setup, started,
};
use crate::workspace::GitOptions;
use crate::workspace::PrivatePaths;
use crate::workspace::private_paths::private_paths_tests::{
    NEEDLE, NORMAL, PRIVATE_FILES, layout, private_of,
};

fn world() -> (tempfile::TempDir, std::path::PathBuf, PrivatePaths) {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().canonicalize().unwrap();
    let private = private_of(&home);
    (tmp, home, private)
}

fn list_query(root: FileRoot, path: &str) -> FileListQuery {
    FileListQuery {
        root,
        path: path.to_owned(),
    }
}

fn names(listing: fleetdeck_proto::files::FileListing) -> Vec<String> {
    listing.entries.into_iter().map(|e| e.name).collect()
}

#[tokio::test]
async fn a_list_leaves_out_the_private_children_and_refuses_the_private_directories() {
    let det = Det::new();
    let (_t, home, private) = world();
    let s = store_with_session(&det, &home).await;
    let p = policy(&home);
    for member in [false, true] {
        let list = |path: &'static str| {
            let (s, p, private) = (&s, &p, &private);
            async move { list_as(s, p, private, list_query(session(), path), member).await }
        };
        let root = names(list("").await.unwrap());
        assert!(root.contains(&"notes.txt".to_owned()), "{root:?}");
        assert_eq!(
            names(list(".config/fleetdeck").await.unwrap()),
            ["readme.txt"],
            "member {member}: station.toml and its temp file are left out"
        );
        assert_eq!(
            names(list(".local/share/fleetdeck").await.unwrap()),
            ["other.txt"],
            "member {member}: the station directory is left out"
        );
        for path in [
            ".local/share/fleetdeck/station",
            ".local/share/fleetdeck/station/secrets",
            ".ssh",
            ".config/gh",
        ] {
            let err = list(path).await.unwrap_err();
            assert_eq!(err.code, code::NOT_FOUND, "member {member}: {path}");
        }
    }
    // The control: without the private set the same listing shows them.
    let open = names(
        list_as(
            &s,
            &p,
            &PrivatePaths::default(),
            list_query(session(), ".config/fleetdeck"),
            false,
        )
        .await
        .unwrap(),
    );
    assert!(open.contains(&"station.toml".to_owned()), "{open:?}");
    assert!(open.contains(&"station.toml.tmp-x".to_owned()), "{open:?}");
}

#[tokio::test]
async fn a_read_of_a_private_file_is_not_found_for_the_owner_and_a_member() {
    let det = Det::new();
    let (_t, home, private) = world();
    let s = store_with_session(&det, &home).await;
    let p = policy(&home);
    let read = |path: &str, member: bool, private: &PrivatePaths| {
        let q = FileReadQuery {
            root: session(),
            path: path.to_owned(),
            max_bytes: None,
        };
        let (s, p) = (s.clone(), p.clone());
        let private = private.clone();
        async move { read_as(&s, &p, &private, q, member).await }
    };
    for member in [false, true] {
        for path in PRIVATE_FILES {
            let err = read(path, member, &private).await.unwrap_err();
            assert_eq!(err.code, code::NOT_FOUND, "member {member}: {path}");
        }
        for path in [
            ".config/gh/hosts.yml",
            ".claude/.credentials.json",
            ".local/share/fleetdeck/station/secrets/jira_token",
        ] {
            let err = read(path, member, &private).await.unwrap_err();
            assert_eq!(err.code, code::NOT_FOUND, "member {member}: {path}");
        }
        for path in NORMAL {
            let ok = read(path, member, &private).await.unwrap();
            assert!(ok.text.unwrap().contains(NEEDLE), "member {member}: {path}");
        }
    }
    // The control: the owner reads the station.toml without the private set.
    let open = read(
        ".config/fleetdeck/station.toml",
        false,
        &PrivatePaths::default(),
    )
    .await
    .unwrap();
    assert!(open.text.unwrap().contains(NEEDLE));
}

fn hit_paths(found: &fleetdeck_proto::files::FileSearch) -> BTreeSet<String> {
    found.hits.iter().map(|h| h.path.clone()).collect()
}

async fn search_paths(
    home: &Path,
    private: &PrivatePaths,
    member: bool,
    mode: SearchMode,
    text: &str,
) -> (BTreeSet<String>, SearchBackend) {
    let det = Det::new();
    let s = store_with_session(&det, home).await;
    let found = search_as(
        &s,
        &policy(home),
        private,
        &GitOptions::isolated(),
        FileSearchQuery {
            root: session(),
            query: text.to_owned(),
            mode,
            max_hits: None,
        },
        member,
    )
    .await
    .unwrap();
    (hit_paths(&found), found.backend)
}

fn normal_set() -> BTreeSet<String> {
    NORMAL.iter().map(|p| (*p).to_owned()).collect()
}

#[tokio::test]
async fn a_walk_search_never_names_or_reads_a_private_path() {
    let (_t, home, private) = world();
    for member in [false, true] {
        let (content, backend) =
            search_paths(&home, &private, member, SearchMode::Content, NEEDLE).await;
        assert_eq!(backend, SearchBackend::Walk);
        assert_eq!(content, normal_set(), "member {member}");
        let (by_name, _) = search_paths(&home, &private, member, SearchMode::Name, "toml").await;
        assert!(by_name.is_empty(), "member {member}: {by_name:?}");
        let (by_name, _) = search_paths(&home, &private, member, SearchMode::Name, "jira").await;
        assert!(by_name.is_empty(), "member {member}: {by_name:?}");
    }
    // The control: without the private set the owner finds the private files.
    let (open, _) = search_paths(
        &home,
        &PrivatePaths::default(),
        false,
        SearchMode::Content,
        NEEDLE,
    )
    .await;
    assert!(open.contains(".config/fleetdeck/station.toml"), "{open:?}");
    assert!(
        open.contains(".config/fleetdeck/station.toml.tmp-x"),
        "{open:?}"
    );
}

#[tokio::test]
async fn a_git_search_never_names_or_reads_a_private_path() {
    let (_t, home, private) = world();
    git(&home, &["init", "-q", "-b", "main"]);
    git(&home, &["add", "-A", "-f"]);
    git(&home, &["commit", "-q", "-m", "everything"]);
    for member in [false, true] {
        let (content, backend) =
            search_paths(&home, &private, member, SearchMode::Content, NEEDLE).await;
        assert_eq!(backend, SearchBackend::Git);
        assert_eq!(content, normal_set(), "member {member}");
        let (by_name, _) = search_paths(&home, &private, member, SearchMode::Name, "toml").await;
        assert!(by_name.is_empty(), "member {member}: {by_name:?}");
    }
    let (open, backend) = search_paths(
        &home,
        &PrivatePaths::default(),
        false,
        SearchMode::Name,
        "toml",
    )
    .await;
    assert_eq!(backend, SearchBackend::Git);
    assert!(open.contains(".config/fleetdeck/station.toml"), "{open:?}");
}

#[tokio::test]
async fn a_root_that_is_private_or_inside_a_private_directory_is_outside_the_roots() {
    let det = Det::new();
    let (_t, home, private) = world();
    let p = policy(&home);
    let data = home.join(".local/share/fleetdeck/station");
    let s = store_with_session(&det, &home).await;
    for root in [data.clone(), data.join("secrets"), home.join(".config/gh")] {
        let err = list_as(&s, &p, &private, list_query(ws(&root), ""), false)
            .await
            .unwrap_err();
        assert_eq!(err.code, code::OUTSIDE_ROOTS, "{}", root.display());
        let err = read_as(
            &s,
            &p,
            &private,
            FileReadQuery {
                root: ws(&root),
                path: "station.db".into(),
                max_bytes: None,
            },
            false,
        )
        .await
        .unwrap_err();
        assert_eq!(err.code, code::OUTSIDE_ROOTS, "{}", root.display());
        let err = search_as(
            &s,
            &p,
            &private,
            &GitOptions::isolated(),
            FileSearchQuery {
                root: ws(&root),
                query: NEEDLE.into(),
                mode: SearchMode::Content,
                max_hits: None,
            },
            false,
        )
        .await
        .unwrap_err();
        assert_eq!(err.code, code::OUTSIDE_ROOTS, "{}", root.display());
    }
    // A session whose working directory is private, for a member too.
    let inside = store_with_session(&det, &data).await;
    for member in [false, true] {
        let err = list_as(&inside, &p, &private, list_query(session(), ""), member)
            .await
            .unwrap_err();
        assert_eq!(err.code, code::OUTSIDE_ROOTS, "member {member}");
    }
    // The workspace root that holds them is not private.
    assert!(
        list_as(&s, &p, &private, list_query(ws(&home), ""), false)
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn a_turn_diff_drops_the_private_paths_from_the_patch_and_the_summary() {
    let det = Det::new();
    let (_t, dir) = repo();
    layout(&dir);
    git(&dir, &["add", "-A", "-f"]);
    git(&dir, &["commit", "-q", "-m", "layout"]);
    let (s, turns) = setup(&det, &dir).await;
    let p = policy(&dir.canonicalize().unwrap());
    let private = private_of(&dir.canonicalize().unwrap());
    let git_options = GitOptions::isolated();
    dispatch(&turns, "in-1").await;
    turns.observe(&[started("turn-1")]).await;
    for file in PRIVATE_FILES.iter().chain(NORMAL) {
        std::fs::write(dir.join(file), format!("{NEEDLE} changed {file}\n")).unwrap();
    }
    turns
        .observe(&[
            input("in-1", Some("turn-1"), InputState::Started),
            completed("turn-1"),
        ])
        .await;
    Arc::clone(&turns).capture_due().await;
    let q = || TurnDiffQuery {
        session_id: S1.into(),
        turn_id: TurnId::from("turn-1"),
        max_bytes: None,
    };
    // The control: without the private set the patch holds the station files
    // (`.ssh` is hidden by the older rule in both cases).
    let open = diff_as(&s, &p, &PrivatePaths::default(), &git_options, q(), false)
        .await
        .unwrap();
    assert!(open.patch.contains("station.toml.tmp-x"), "{}", open.patch);
    assert!(open.patch.contains("jira_token"), "{}", open.patch);
    for member in [false, true] {
        let d = diff_as(&s, &p, &private, &git_options, q(), member)
            .await
            .unwrap();
        for file in [
            "station.toml",
            "station.db",
            "jira_token",
            ".tmp-x",
            "id_ed25519",
        ] {
            assert!(
                !d.patch.contains(file),
                "member {member}: {file}\n{}",
                d.patch
            );
        }
        for file in NORMAL {
            assert!(
                d.patch.contains(file),
                "member {member}: {file}\n{}",
                d.patch
            );
        }
        assert_eq!(
            d.summary.files_changed, 3,
            "member {member}: only the normal files are counted"
        );
        // `.ssh/id_ed25519` is dropped by the older hidden-name rule too.
        assert_eq!(
            open.summary.files_changed as usize,
            PRIVATE_FILES.len() - 1 + NORMAL.len()
        );
    }
}

#[tokio::test]
async fn a_manager_without_private_paths_refuses_every_file_call() {
    use crate::manager::{Adapters, SessionManager};
    let det = Det::new();
    let (_t, home, private) = world();
    let store = store_with_session(&det, &home).await;
    let manager = SessionManager::start(store.clone(), policy(&home), Adapters::new(), det.ids())
        .await
        .unwrap();
    let unset = manager.private_paths();
    assert!(unset.is_private(&home.join("notes.txt")));
    let err = list_as(
        &store,
        &manager.policy(),
        &unset,
        list_query(session(), ""),
        false,
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, code::OUTSIDE_ROOTS);
    // The setter works once; the first call wins.
    manager.set_private_paths(Arc::new(private));
    manager.set_private_paths(Arc::new(PrivatePaths::deny_all()));
    let set = manager.private_paths();
    assert!(!set.is_private(&home.join("notes.txt")));
    assert!(set.is_private(&home.join(".ssh/id_ed25519")));
    manager
        .shutdown(fleetdeck_proto::command::StopMode::Graceful)
        .await;
}

/// The hard links of PRE-PR r1 finding 1 inside `home`: one for each
/// credential kind that the identity list of the set does not hold. Returns
/// the names of the links.
fn credential_links(home: &Path) -> Vec<String> {
    let station = home.join(".local/share/fleetdeck/station");
    std::fs::create_dir_all(station.join("accounts")).unwrap();
    std::fs::write(
        station.join("accounts/acct-x.json"),
        format!("{NEEDLE} account\n"),
    )
    .unwrap();
    // A save replaces station.toml by rename: the new inode is not in the
    // identity list that was built at start.
    let config = home.join(".config/fleetdeck/station.toml");
    let temp = home.join(".config/fleetdeck/station.toml.tmp-save");
    std::fs::write(&temp, format!("{NEEDLE} saved\n")).unwrap();
    std::fs::rename(&temp, &config).unwrap();
    let mut names = Vec::new();
    for (source, name) in [
        (station.join("accounts/acct-x.json"), "link-account.txt"),
        (station.join("secrets/jira_token"), "link-secret.txt"),
        (config, "link-config.txt"),
    ] {
        std::fs::hard_link(source, home.join(name)).unwrap();
        names.push(name.to_owned());
    }
    names
}

#[tokio::test]
async fn a_hard_link_to_a_credential_file_is_not_read_for_the_owner_and_a_member() {
    let det = Det::new();
    let (_t, home, private) = world();
    let links = credential_links(&home);
    let s = store_with_session(&det, &home).await;
    let p = policy(&home);
    for member in [false, true] {
        for name in &links {
            let err = read_as(
                &s,
                &p,
                &private,
                FileReadQuery {
                    root: session(),
                    path: name.clone(),
                    max_bytes: None,
                },
                member,
            )
            .await
            .unwrap_err();
            assert_eq!(err.code, code::NOT_FOUND, "member {member}: {name}");
        }
        // A file with one link is unchanged.
        let ok = read_as(
            &s,
            &p,
            &private,
            FileReadQuery {
                root: session(),
                path: "notes.txt".into(),
                max_bytes: None,
            },
            member,
        )
        .await
        .unwrap();
        assert!(ok.text.unwrap().contains(NEEDLE));
    }
}

#[tokio::test]
async fn a_walk_search_skips_a_hard_link_to_a_credential_file() {
    let (_t, home, private) = world();
    let links = credential_links(&home);
    for member in [false, true] {
        let (content, backend) =
            search_paths(&home, &private, member, SearchMode::Content, NEEDLE).await;
        assert_eq!(backend, SearchBackend::Walk);
        assert_eq!(content, normal_set(), "member {member}");
        for name in &links {
            assert!(!content.contains(name), "member {member}: {name}");
        }
    }
}
