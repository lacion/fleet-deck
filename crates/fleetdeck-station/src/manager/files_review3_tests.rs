//! Regression tests for the files.v1 re-review sa-713c2ae4 (DL-109): a
//! content search that stops at the per-file hit budget reports
//! `truncated` (MAJOR), and a file of exactly the size budget that was read
//! completely keeps its final line without a newline (MINOR). Both
//! findings are tested for the Git and the walk backends.

use std::path::{Path, PathBuf};

use fleetdeck_proto::files::{FileSearch, FileSearchQuery, SearchBackend, SearchMode};
use fleetdeck_testkit::Det;

use super::files_tests::{policy, session, store_with_session};
use super::search;
use crate::manager::turns::turns_tests::repo;
use crate::workspace::GitOptions;
use crate::workspace::files::WALK_FILE_BYTES;

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

fn plain() -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().canonicalize().unwrap().join("plain");
    std::fs::create_dir(&dir).unwrap();
    (tmp, dir)
}

fn git() -> (tempfile::TempDir, PathBuf) {
    let (tmp, dir) = repo();
    let dir = dir.canonicalize().unwrap();
    (tmp, dir)
}

async fn per_file_budget(dir: &Path, backend: SearchBackend) {
    std::fs::write(dir.join("many.txt"), "needle\n".repeat(6)).unwrap();
    let found = content_search(dir, "needle").await;
    assert_eq!(found.backend, backend);
    assert_eq!(found.hits.len(), 5, "{found:?}");
    assert!(found.truncated, "the sixth match was dropped silently");
    // Exactly the budget is a complete result.
    std::fs::write(dir.join("many.txt"), "needle\n".repeat(5)).unwrap();
    let found = content_search(dir, "needle").await;
    assert_eq!(found.hits.len(), 5, "{found:?}");
    assert!(!found.truncated, "{found:?}");
}

async fn exact_budget_file(dir: &Path, backend: SearchBackend) {
    let size = usize::try_from(WALK_FILE_BYTES).unwrap();
    // A newline at byte 0, filler, then `needle` in the final 6 bytes and
    // no trailing newline: exactly 1,048,576 bytes.
    let mut text = String::from("\n");
    text.push_str(&"x".repeat(size - 1 - "needle".len()));
    text.push_str("needle");
    assert_eq!(text.len(), 1_048_576);
    std::fs::write(dir.join("exact.txt"), &text).unwrap();
    let found = content_search(dir, "needle").await;
    assert_eq!(found.backend, backend);
    assert_eq!(found.hits.len(), 1, "{found:?}");
    assert_eq!(found.hits[0].path, "exact.txt");
    assert_eq!(found.hits[0].line, Some(2));
    assert!(!found.truncated, "a completely read file is not cut");
    // One byte more is a file above the budget: its last line is cut.
    text.push('\n');
    text.insert(1, 'x');
    std::fs::write(dir.join("exact.txt"), &text).unwrap();
    let found = content_search(dir, "needle").await;
    assert!(found.hits.is_empty(), "{found:?}");
    assert!(found.truncated, "{found:?}");
}

#[tokio::test]
async fn a_git_search_that_stops_at_the_per_file_hit_budget_is_truncated() {
    let (_tmp, dir) = git();
    per_file_budget(&dir, SearchBackend::Git).await;
}

#[tokio::test]
async fn a_walk_search_that_stops_at_the_per_file_hit_budget_is_truncated() {
    let (_tmp, dir) = plain();
    per_file_budget(&dir, SearchBackend::Walk).await;
}

#[tokio::test]
async fn a_git_search_reads_a_file_of_exactly_the_size_budget_to_its_final_line() {
    let (_tmp, dir) = git();
    exact_budget_file(&dir, SearchBackend::Git).await;
}

#[tokio::test]
async fn a_walk_search_reads_a_file_of_exactly_the_size_budget_to_its_final_line() {
    let (_tmp, dir) = plain();
    exact_budget_file(&dir, SearchBackend::Walk).await;
}
