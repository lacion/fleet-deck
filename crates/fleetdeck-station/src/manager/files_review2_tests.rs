//! Regression tests for the files.v1 re-review sa-fbed2a6d (DL-103): a
//! configured allowed root that is a private directory never serves
//! (finding 1), and a content search reads a bounded prefix of a large file
//! and reports `truncated` instead of skipping the file silently (finding
//! 2), for the Git and the walk backends.

use std::path::Path;

use fleetdeck_proto::files::{
    FileListQuery, FileReadQuery, FileSearch, FileSearchQuery, SearchBackend, SearchMode, code,
};
use fleetdeck_testkit::Det;

use super::files_tests::{policy, session, store_with_session, ws};
use super::{list, read, search};
use crate::manager::turns::turns_tests::repo;
use crate::workspace::GitOptions;
use crate::workspace::files::WALK_FILE_BYTES;

#[tokio::test]
async fn a_configured_allowed_root_that_is_a_private_directory_never_serves() {
    let det = Det::new();
    let tmp = tempfile::tempdir().unwrap();
    let top = tmp.path().canonicalize().unwrap();
    for private in [".SSH", ".ssh/work", "p/.Git"] {
        let dir = top.join(private);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("id_test"), "PRIVATE_ROOT_SECRET\n").unwrap();
        // The owner configured the private directory itself as the root.
        let p = policy(&dir);
        let query = |root| FileReadQuery {
            root,
            path: "id_test".into(),
            max_bytes: None,
        };
        let s = store_with_session(&det, &dir).await;
        for root in [ws(&dir), session()] {
            let err = read(&s, &p, query(root)).await.unwrap_err();
            assert_eq!(err.code, code::OUTSIDE_ROOTS, "{private}: {err}");
        }
        let err = list(
            &s,
            &p,
            FileListQuery {
                root: session(),
                path: String::new(),
            },
        )
        .await
        .unwrap_err();
        assert_eq!(err.code, code::OUTSIDE_ROOTS, "{private}: {err}");
    }
}

/// `head` bytes of filler lines, then `tail`.
fn large(head: u64, tail: &str) -> String {
    let line = "filler line of a large text file\n";
    let mut text = String::new();
    while (text.len() as u64) < head {
        text.push_str(line);
    }
    text.push_str(tail);
    text
}

async fn content_search(dir: &Path, needle: &str) -> FileSearch {
    let det = Det::new();
    let s = store_with_session(&det, dir).await;
    search(
        &s,
        &policy(dir.parent().unwrap()),
        &GitOptions::isolated(),
        FileSearchQuery {
            root: session(),
            query: needle.into(),
            mode: SearchMode::Content,
            max_hits: None,
        },
    )
    .await
    .unwrap()
}

async fn large_file_search(dir: &Path, backend: SearchBackend) {
    // A match on the first line of a file above the limit is found.
    let first = format!(
        "needle legitimate large text\n{}",
        large(WALK_FILE_BYTES + 200_000, "")
    );
    std::fs::write(dir.join("large.txt"), first).unwrap();
    // A match only past the limit cannot be found: the search says so.
    std::fs::write(
        dir.join("far.txt"),
        large(WALK_FILE_BYTES + 100_000, "farneedle here\n"),
    )
    .unwrap();
    let found = content_search(dir, "needle legitimate").await;
    assert_eq!(found.backend, backend);
    assert_eq!(found.hits.len(), 1, "{found:?}");
    assert_eq!(found.hits[0].path, "large.txt");
    assert_eq!(found.hits[0].line, Some(1));
    assert!(found.truncated, "a large file was not searched to its end");
    let found = content_search(dir, "farneedle").await;
    assert!(found.hits.is_empty(), "{found:?}");
    assert!(found.truncated, "the miss past the limit is not silent");
    // Without large files the same search is complete.
    std::fs::remove_file(dir.join("large.txt")).unwrap();
    std::fs::remove_file(dir.join("far.txt")).unwrap();
    std::fs::write(dir.join("small.txt"), "farneedle small\n").unwrap();
    let found = content_search(dir, "farneedle").await;
    assert_eq!(found.hits.len(), 1, "{found:?}");
    assert!(!found.truncated);
}

#[tokio::test]
async fn a_git_content_search_reads_the_prefix_of_a_large_file_and_marks_the_search_truncated() {
    let (_tmp, dir) = repo();
    let dir = dir.canonicalize().unwrap();
    large_file_search(&dir, SearchBackend::Git).await;
}

#[tokio::test]
async fn a_walk_content_search_reads_the_prefix_of_a_large_file_and_marks_the_search_truncated() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().canonicalize().unwrap().join("plain");
    std::fs::create_dir(&dir).unwrap();
    large_file_search(&dir, SearchBackend::Walk).await;
}
