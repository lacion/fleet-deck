//! The home attempts of the station store (design §3.5 V29, §8.4).

use std::path::Path;

use fleetdeck_proto::fleet_git::{HomeAttemptState, code};
use fleetdeck_testkit::Det;

use super::{BeginHome, HomeOutcome, NewHomeAttempt};
use crate::store::Store;
use crate::store::git_state::tests::{apply, store};

fn attempt(id: &str, project: &str, generation: u64, dest: &Path) -> NewHomeAttempt {
    NewHomeAttempt {
        attempt_id: id.to_owned(),
        project_id: project.to_owned(),
        generation,
        request_digest: format!("digest-{id}"),
        retry_of: None,
        dest_path: dest.to_string_lossy().into_owned(),
        url: "https://h/acme/app.git".to_owned(),
        remote_name: "origin".to_owned(),
    }
}

fn retry(id: &str, of: &str, project: &str, generation: u64, dest: &Path) -> NewHomeAttempt {
    NewHomeAttempt {
        retry_of: Some(of.to_owned()),
        ..attempt(id, project, generation, dest)
    }
}

fn refused(result: BeginHome) -> (&'static str, String) {
    match result {
        BeginHome::Refused(r) => (r.code, r.text),
        other => panic!("not refused: {other:?}"),
    }
}

fn created(result: BeginHome) -> super::HomeAttemptRow {
    match result {
        BeginHome::Created(row) => row,
        other => panic!("not created: {other:?}"),
    }
}

fn outcome(state: HomeAttemptState, path: Option<&str>) -> HomeOutcome {
    HomeOutcome {
        state,
        path: path.map(str::to_owned),
        dev: None,
        ino: None,
        code: None,
        message: None,
    }
}

async fn failed(store: &Store, id: &str) {
    let mut o = outcome(HomeAttemptState::Failed, None);
    o.code = Some(code::CLONE_FAILED.to_owned());
    store.home_finish(id.to_owned(), o).await.unwrap();
}

fn dir() -> (tempfile::TempDir, std::path::PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    (tmp, root)
}

#[tokio::test]
async fn a_new_attempt_is_cloning_and_the_same_attempt_returns_its_state() {
    let (_tmp, root) = dir();
    let store = store(&Det::new()).await;
    let dest = root.join("app");
    let row = created(
        store
            .home_begin(attempt("a1", "p1", 1, &dest))
            .await
            .unwrap(),
    );
    assert_eq!(row.state, HomeAttemptState::Cloning);
    assert!(!row.superseded && row.path.is_none());
    // The same id and digest: the stored row, nothing starts again.
    let again = store
        .home_begin(attempt("a1", "p1", 1, &dest))
        .await
        .unwrap();
    assert_eq!(again, BeginHome::Existing(row.clone()));
    // Also after the attempt failed.
    failed(&store, "a1").await;
    let again = store
        .home_begin(attempt("a1", "p1", 1, &dest))
        .await
        .unwrap();
    let BeginHome::Existing(stored) = again else {
        panic!("not existing")
    };
    assert_eq!(stored.state, HomeAttemptState::Failed);
    // Another digest under the same id.
    let mut other = attempt("a1", "p1", 1, &dest);
    other.request_digest = "other".to_owned();
    let (c, text) = refused(store.home_begin(other).await.unwrap());
    assert_eq!(c, code::ATTEMPT_REUSED);
    assert_eq!(text, "This attempt id was used for another request.");
}

#[tokio::test]
async fn a_higher_generation_fences_a_lower_one() {
    let (_tmp, root) = dir();
    let store = store(&Det::new()).await;
    created(
        store
            .home_begin(attempt("g2", "p1", 2, &root.join("a")))
            .await
            .unwrap(),
    );
    let (c, text) = refused(
        store
            .home_begin(attempt("g1", "p1", 1, &root.join("b")))
            .await
            .unwrap(),
    );
    assert_eq!(c, code::STALE_GENERATION);
    assert_eq!(text, "The home of this project changed. Start again.");
    // Another project is not fenced.
    created(
        store
            .home_begin(attempt("o1", "p2", 1, &root.join("c")))
            .await
            .unwrap(),
    );
}

#[tokio::test]
async fn one_attempt_at_a_time_for_a_generation() {
    let (_tmp, root) = dir();
    let store = store(&Det::new()).await;
    let first = created(
        store
            .home_begin(attempt("a1", "p1", 1, &root.join("a")))
            .await
            .unwrap(),
    );
    let second = store
        .home_begin(attempt("a2", "p1", 1, &root.join("b")))
        .await
        .unwrap();
    assert_eq!(second, BeginHome::Existing(first));
    assert!(
        store
            .home_attempt("p1".into(), "a2".into())
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn a_retry_needs_the_newest_failed_attempt() {
    let (_tmp, root) = dir();
    let store = store(&Det::new()).await;
    let dest = root.join("app");
    created(
        store
            .home_begin(attempt("a1", "p1", 1, &dest))
            .await
            .unwrap(),
    );
    // A cloning attempt cannot be retried; an unknown one neither.
    for of in ["a1", "nope"] {
        let (c, text) = refused(
            store
                .home_begin(retry("r1", of, "p1", 1, &dest))
                .await
                .unwrap(),
        );
        assert_eq!(c, code::RETRY_REFUSED, "{of}");
        assert_eq!(text, "Only the newest failed attempt can be retried.");
    }
    failed(&store, "a1").await;
    // The folder is still there: never adopted, the retry is refused and
    // the old attempt stays failed.
    std::fs::create_dir(&dest).unwrap();
    let (c, text) = refused(
        store
            .home_begin(retry("r1", "a1", "p1", 1, &dest))
            .await
            .unwrap(),
    );
    assert_eq!(c, code::INCOMPLETE_FOLDER);
    assert_eq!(
        text,
        format!(
            "The folder {} holds an incomplete clone. Remove it, then retry.",
            dest.display()
        )
    );
    let old = store
        .home_attempt("p1".into(), "a1".into())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(old.state, HomeAttemptState::Failed);
    // The folder is gone: the old attempt is cleared and the retry clones.
    std::fs::remove_dir(&dest).unwrap();
    let row = created(
        store
            .home_begin(retry("r1", "a1", "p1", 1, &dest))
            .await
            .unwrap(),
    );
    assert_eq!(row.retry_of.as_deref(), Some("a1"));
    let old = store
        .home_attempt("p1".into(), "a1".into())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(old.state, HomeAttemptState::Cleared);
    assert_eq!(old.code.as_deref(), Some(code::DESTINATION_CLEARED));
    // The old attempt is not the newest any more.
    let (c, _) = refused(
        store
            .home_begin(retry("r2", "a1", "p1", 1, &dest))
            .await
            .unwrap(),
    );
    assert_eq!(c, code::RETRY_REFUSED);
}

#[tokio::test]
async fn a_refused_retry_changes_nothing() {
    // x is cleared only when the whole add goes on: another project holds the
    // folder of the retry, so the refusal keeps x failed.
    let (_tmp, root) = dir();
    let store = store(&Det::new()).await;
    let a = root.join("a");
    let b = root.join("b");
    created(store.home_begin(attempt("a1", "p1", 1, &a)).await.unwrap());
    failed(&store, "a1").await;
    created(store.home_begin(attempt("o1", "p2", 1, &b)).await.unwrap());
    let mut again = retry("r1", "a1", "p1", 1, &b);
    again.dest_path = b.to_string_lossy().into_owned();
    let (c, _) = refused(store.home_begin(again).await.unwrap());
    assert_eq!(c, code::DESTINATION_RESERVED);
    let old = store
        .home_attempt("p1".into(), "a1".into())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(old.state, HomeAttemptState::Failed);
}

#[tokio::test]
async fn another_project_cannot_take_a_reserved_folder() {
    let (_tmp, root) = dir();
    let det = Det::new();
    let store = store(&det).await;
    let dest = root.join("app");
    created(
        store
            .home_begin(attempt("a1", "p1", 1, &dest))
            .await
            .unwrap(),
    );
    // No snapshot names the holder: the text has its id.
    let (c, text) = refused(
        store
            .home_begin(attempt("b1", "p2", 1, &dest))
            .await
            .unwrap(),
    );
    assert_eq!(c, code::DESTINATION_RESERVED);
    assert_eq!(
        text,
        format!(
            "The folder {} is reserved by project p1 on this station.",
            dest.display()
        )
    );
    // With a snapshot the text has the project name (the view of the tests is "app").
    apply(&store, 1, "d1", &["p1"]).await;
    let (_, text) = refused(
        store
            .home_begin(attempt("b2", "p2", 1, &dest))
            .await
            .unwrap(),
    );
    assert!(text.contains("reserved by project app on"), "{text}");
}

#[tokio::test]
async fn a_failed_attempt_of_the_same_project_holds_its_folder() {
    let (_tmp, root) = dir();
    let store = store(&Det::new()).await;
    let dest = root.join("app");
    created(
        store
            .home_begin(attempt("a1", "p1", 1, &dest))
            .await
            .unwrap(),
    );
    failed(&store, "a1").await;
    std::fs::create_dir(&dest).unwrap();
    // A new generation after a removal and a re-add, the folder still there.
    let (c, text) = refused(
        store
            .home_begin(attempt("a2", "p1", 2, &dest))
            .await
            .unwrap(),
    );
    assert_eq!(c, code::INCOMPLETE_FOLDER);
    assert_eq!(
        text,
        format!(
            "The folder {} holds an incomplete clone from an earlier attempt. Remove it, then add the home again.",
            dest.display()
        )
    );
    // The folder is untouched and the old row too.
    assert!(dest.is_dir());
    let old = store
        .home_attempt("p1".into(), "a1".into())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(old.state, HomeAttemptState::Failed);
}

#[tokio::test]
async fn an_absent_folder_clears_the_failed_attempts_of_any_project() {
    let (_tmp, root) = dir();
    let store = store(&Det::new()).await;
    let dest = root.join("app");
    created(
        store
            .home_begin(attempt("a1", "p1", 1, &dest))
            .await
            .unwrap(),
    );
    failed(&store, "a1").await;
    // Generation 2 of the same project, and then another project.
    created(
        store
            .home_begin(attempt("a2", "p1", 2, &dest))
            .await
            .unwrap(),
    );
    let old = store
        .home_attempt("p1".into(), "a1".into())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(old.state, HomeAttemptState::Cleared);
    assert_eq!(old.code.as_deref(), Some(code::DESTINATION_CLEARED));
    failed(&store, "a2").await;
    created(
        store
            .home_begin(attempt("b1", "p2", 1, &dest))
            .await
            .unwrap(),
    );
    let old = store
        .home_attempt("p1".into(), "a2".into())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(old.state, HomeAttemptState::Cleared);
}

#[tokio::test]
async fn a_dangling_link_is_not_an_absent_folder() {
    let (_tmp, root) = dir();
    let store = store(&Det::new()).await;
    let dest = root.join("app");
    created(
        store
            .home_begin(attempt("a1", "p1", 1, &dest))
            .await
            .unwrap(),
    );
    failed(&store, "a1").await;
    std::os::unix::fs::symlink(root.join("nowhere"), &dest).unwrap();
    let (c, _) = refused(
        store
            .home_begin(attempt("a2", "p1", 2, &dest))
            .await
            .unwrap(),
    );
    assert_eq!(c, code::INCOMPLETE_FOLDER);
    let old = store
        .home_attempt("p1".into(), "a1".into())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(old.state, HomeAttemptState::Failed);
}

#[tokio::test]
async fn a_superseded_ready_home_frees_its_folder_but_a_live_one_does_not() {
    let (_tmp, root) = dir();
    let store = store(&Det::new()).await;
    let dest = root.join("app");
    created(
        store
            .home_begin(attempt("a1", "p1", 1, &dest))
            .await
            .unwrap(),
    );
    let done = outcome(HomeAttemptState::Ready, Some(&dest.to_string_lossy()));
    store.home_finish("a1".into(), done).await.unwrap();
    let (c, _) = refused(
        store
            .home_begin(attempt("b1", "p2", 1, &dest))
            .await
            .unwrap(),
    );
    assert_eq!(c, code::DESTINATION_RESERVED);
    // The snapshot has no home of p1: its ready attempt is superseded.
    assert_eq!(store.home_supersede(Vec::new()).await.unwrap(), 1);
    created(
        store
            .home_begin(attempt("b2", "p2", 1, &dest))
            .await
            .unwrap(),
    );
}

#[tokio::test]
async fn the_snapshot_supersedes_only_older_or_absent_attempts_that_are_not_cloning() {
    let (_tmp, root) = dir();
    let store = store(&Det::new()).await;
    for (id, project, generation) in [("a1", "p1", 1), ("c1", "p3", 1), ("d1", "p4", 1)] {
        let dest = root.join(id);
        created(
            store
                .home_begin(attempt(id, project, generation, &dest))
                .await
                .unwrap(),
        );
    }
    failed(&store, "a1").await;
    failed(&store, "d1").await;
    // p1 has a home of generation 2 (older attempt), p3 is cloning and absent,
    // p4 has a home of the same generation.
    let homes = vec![("p1".to_owned(), 2), ("p4".to_owned(), 1)];
    assert_eq!(store.home_supersede(homes.clone()).await.unwrap(), 1);
    let row = |id: &str, p: &str| {
        let store = store.clone();
        let (id, p) = (id.to_owned(), p.to_owned());
        async move { store.home_attempt(p, id).await.unwrap().unwrap() }
    };
    assert!(row("a1", "p1").await.superseded);
    assert!(!row("c1", "p3").await.superseded);
    assert!(!row("d1", "p4").await.superseded);
    // A second run changes nothing.
    assert_eq!(store.home_supersede(homes).await.unwrap(), 0);
}

#[tokio::test]
async fn a_restart_marks_every_cloning_attempt_interrupted_and_keeps_the_rest() {
    let (_tmp, root) = dir();
    let store = store(&Det::new()).await;
    let dest = root.join("app");
    created(
        store
            .home_begin(attempt("a1", "p1", 1, &dest))
            .await
            .unwrap(),
    );
    created(
        store
            .home_begin(attempt("a2", "p2", 1, &root.join("b")))
            .await
            .unwrap(),
    );
    failed(&store, "a2").await;
    assert_eq!(store.home_interrupt_cloning().await.unwrap(), 1);
    let row = store
        .home_attempt("p1".into(), "a1".into())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.state, HomeAttemptState::Interrupted);
    assert_eq!(row.code.as_deref(), Some(code::CLONE_INTERRUPTED));
    assert_eq!(
        row.message.unwrap(),
        format!(
            "The clone stopped when the station restarted. Remove {}, then retry.",
            dest.display()
        )
    );
    let kept = store
        .home_attempt("p2".into(), "a2".into())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(kept.state, HomeAttemptState::Failed);
    assert_eq!(store.home_interrupt_cloning().await.unwrap(), 0);
}

#[tokio::test]
async fn a_ready_attempt_needs_a_path() {
    let (_tmp, root) = dir();
    let store = store(&Det::new()).await;
    created(
        store
            .home_begin(attempt("a1", "p1", 1, &root.join("a")))
            .await
            .unwrap(),
    );
    let err = store
        .home_finish("a1".into(), outcome(HomeAttemptState::Ready, None))
        .await;
    assert!(
        err.is_err(),
        "the CHECK of the table refuses a ready row without a path"
    );
}

#[tokio::test]
async fn an_attempt_of_another_project_is_unknown() {
    let (_tmp, root) = dir();
    let store = store(&Det::new()).await;
    created(
        store
            .home_begin(attempt("a1", "p1", 1, &root.join("a")))
            .await
            .unwrap(),
    );
    assert!(
        store
            .home_attempt("p2".into(), "a1".into())
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .home_attempt("p1".into(), "a1".into())
            .await
            .unwrap()
            .is_some()
    );
}
