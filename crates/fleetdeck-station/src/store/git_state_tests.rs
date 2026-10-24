//! The Fleet Git rows of the station store (design §3.5, §5.5, §5.10, V28).

use fleetdeck_proto::fleet_git::{
    GitProjectView, GitSettings, GitSnapshot, HomeCheck, HomeCheckState, PushMode, PushState,
    RemoteIdentity, StationHome,
};
use fleetdeck_proto::{Event, EventBody, SessionId};
use fleetdeck_testkit::Det;

use super::{NewSessionGit, SnapshotResult, insert_push};
use crate::store::tests::{new_session as plain_session, store as plain_store};
use crate::store::{ReadResult, Store, StoreConfig, call};

/// A recovered in-memory store.
pub(crate) async fn store(det: &Det) -> Store {
    plain_store(det).await
}

/// A new session row.
pub(crate) fn new_session(id: &str) -> crate::store::NewSession {
    plain_session(id)
}

pub(crate) const DEST: &str = "https://h:443/acme/app";

impl Store {
    /// Writes the Fleet Git rows of a session outside a command.
    pub(crate) async fn git_record_session(
        &self,
        new: NewSessionGit,
        push: Option<(String, String)>,
    ) -> crate::store::Result<()> {
        let now = self.now();
        call(&self.inner.db, move |conn| {
            super::insert_session_git(conn, &new, now)?;
            if let (Some((target, dest)), Some(publish_ref)) = (&push, &new.publish_ref) {
                insert_push(conn, &new.session_id, (target, dest), publish_ref, now)?;
            }
            Ok(())
        })
        .await
    }
}

/// The view of a synced project `id` with push on every turn.
pub(crate) fn view(id: &str) -> GitProjectView {
    GitProjectView {
        project_id: id.to_owned(),
        project_name: "app".to_owned(),
        settings_revision: 1,
        settings: GitSettings {
            sync: true,
            push: PushMode::OnTurn,
            ..GitSettings::default()
        },
        remote: RemoteIdentity {
            repo: "h/acme/app".to_owned(),
            dest: DEST.to_owned(),
        },
        forge: None,
        clone_url: "https://h/acme/app".to_owned(),
        home_generation: 1,
        home_path: "/r".to_owned(),
        radar_home: false,
    }
}

pub(crate) fn verified(id: &str) -> HomeCheck {
    HomeCheck {
        project_id: id.to_owned(),
        generation: 1,
        state: HomeCheckState::Verified,
        canonical_path: Some("/r".to_owned()),
        seen: None,
        code: None,
    }
}

fn snapshot(revision: u64, projects: &[&str]) -> GitSnapshot {
    GitSnapshot {
        revision,
        projects: projects.iter().map(|p| view(p)).collect(),
        homes: projects
            .iter()
            .map(|p| StationHome {
                project_id: (*p).to_owned(),
                generation: 1,
                path: "/r".to_owned(),
                sync: true,
            })
            .collect(),
    }
}

/// The first snapshot with `projects` at `revision`.
pub(crate) async fn apply(
    store: &Store,
    revision: u64,
    digest: &str,
    projects: &[&str],
) -> SnapshotResult {
    let checks = projects.iter().map(|p| verified(p)).collect();
    store
        .git_apply_snapshot(snapshot(revision, projects), digest.to_owned(), checks)
        .await
        .unwrap()
}

pub(crate) fn new_git(id: &str, project: &str) -> NewSessionGit {
    let branch = format!("fd/alpha/{id}");
    NewSessionGit {
        session_id: id.into(),
        project_id: project.to_owned(),
        kind: "worktree",
        settings_revision: 1,
        home_generation: 1,
        slug: Some("alpha".to_owned()),
        branch: Some(branch.clone()),
        publish_ref: Some(branch),
        created_branch: true,
        repo_root: "/r".to_owned(),
        remote_dest: DEST.to_owned(),
    }
}

/// A store with the session `id` and its push row (state `idle`).
pub(crate) async fn with_push(det: &Det, id: &str) -> Store {
    let store = store(det).await;
    store.create_session(new_session(id)).await.unwrap();
    let push = Some(("origin".to_owned(), DEST.to_owned()));
    store
        .git_record_session(new_git(id, "p1"), push)
        .await
        .unwrap();
    store
}

const A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

async fn push_states(store: &Store, session: &str) -> Vec<PushState> {
    let ReadResult::Events { events, .. } = store.read_after(0, 1000).await.unwrap() else {
        panic!("snapshot required");
    };
    events
        .into_iter()
        .filter(|e| e.session_id.as_str() == session)
        .filter_map(|e| match e.event {
            EventBody::Known(Event::SessionPush { state, .. }) => Some(state),
            _ => None,
        })
        .collect()
}

async fn row(store: &Store, session: &str) -> super::PushRow {
    store
        .git_push(SessionId::from(session))
        .await
        .unwrap()
        .unwrap()
}

#[tokio::test]
async fn a_stale_or_conflicting_snapshot_changes_nothing() {
    let det = Det::new();
    let store = store(&det).await;
    assert_eq!(
        apply(&store, 10, "a", &["p1"]).await,
        SnapshotResult::Stored
    );
    // A lower revision, and the same revision with another digest, are stale.
    assert_eq!(
        apply(&store, 9, "z", &["p2"]).await,
        SnapshotResult::Stale { revision: 10 }
    );
    assert_eq!(
        apply(&store, 10, "b", &["p2"]).await,
        SnapshotResult::Stale { revision: 10 }
    );
    assert!(store.git_project("p2".into()).await.unwrap().is_none());
    // The same revision and digest is a retry.
    assert_eq!(apply(&store, 10, "a", &["p1"]).await, SnapshotResult::Same);
    assert_eq!(
        apply(&store, 11, "c", &["p2"]).await,
        SnapshotResult::Stored
    );
    assert!(store.git_project("p1".into()).await.unwrap().is_none());
}

#[tokio::test]
async fn a_newer_snapshot_replaces_the_projects_and_the_homes() {
    let det = Det::new();
    let store = store(&det).await;
    apply(&store, 1, "a", &["p1", "p2"]).await;
    let row = store.git_project("p1".into()).await.unwrap().unwrap();
    assert_eq!(row.check_state, HomeCheckState::Verified);
    assert_eq!(row.canonical_path.as_deref(), Some("/r"));
    assert_eq!(row.view, view("p1"));
    let checks = store.git_project_checks().await.unwrap();
    assert_eq!(checks.len(), 2);
    apply(&store, 2, "b", &["p2"]).await;
    assert!(store.git_project("p1".into()).await.unwrap().is_none());
    assert_eq!(store.git_project_checks().await.unwrap().len(), 1);
}

#[tokio::test]
async fn a_project_that_leaves_the_snapshot_stops_its_push_rows() {
    let det = Det::new();
    let store = with_push(&det, "s1").await;
    apply(&store, 1, "a", &["p1"]).await;
    store
        .git_push_trigger("s1".into(), A.to_owned())
        .await
        .unwrap();
    assert_eq!(row(&store, "s1").await.state, PushState::Due);
    apply(&store, 2, "b", &[]).await;
    let stopped = row(&store, "s1").await;
    assert_eq!(stopped.state, PushState::Stopped);
    assert_eq!(stopped.stop_code.as_deref(), Some("project_gone"));
    // The trigger still updates `desired_oid`; a stopped row never pushes.
    store
        .git_push_trigger("s1".into(), B.to_owned())
        .await
        .unwrap();
    let stopped = row(&store, "s1").await;
    assert_eq!(
        (stopped.state, stopped.desired_oid.as_deref()),
        (PushState::Stopped, Some(B))
    );
    assert!(store.git_push_due(i64::MAX).await.unwrap().is_empty());
    assert_eq!(
        push_states(&store, "s1").await.last(),
        Some(&PushState::Stopped)
    );
}

#[tokio::test]
async fn the_destination_and_the_ref_are_unique() {
    let det = Det::new();
    let store = with_push(&det, "s1").await;
    store.create_session(new_session("s2")).await.unwrap();
    let mut other = new_git("s2", "p1");
    other.branch = new_git("s1", "p1").branch;
    other.publish_ref = new_git("s1", "p1").publish_ref;
    let push = Some(("origin".to_owned(), DEST.to_owned()));
    assert!(store.git_record_session(other, push).await.is_err());
}

#[tokio::test]
async fn the_publication_ref_must_be_below_the_stored_slug() {
    let det = Det::new();
    let store = store(&det).await;
    store.create_session(new_session("s1")).await.unwrap();
    let mut bad = new_git("s1", "p1");
    bad.publish_ref = Some("fd/other/s1".to_owned());
    assert!(store.git_record_session(bad, None).await.is_err());
}

#[tokio::test]
async fn a_trigger_makes_an_idle_row_due_and_only_moves_the_tip_of_a_busy_one() {
    let det = Det::new();
    let store = with_push(&det, "s1").await;
    let due = store
        .git_push_trigger("s1".into(), A.to_owned())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (due.state, due.desired_oid.as_deref()),
        (PushState::Due, Some(A))
    );
    // `due` keeps its state; the tip moves.
    let due = store
        .git_push_trigger("s1".into(), B.to_owned())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (due.state, due.desired_oid.as_deref()),
        (PushState::Due, Some(B))
    );
    assert_eq!(
        push_states(&store, "s1").await,
        [PushState::Due, PushState::Due]
    );
    // A session without a push row has nothing to trigger.
    assert!(
        store
            .git_push_trigger("none".into(), A.to_owned())
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn an_open_attempt_keeps_its_values_while_a_trigger_changes_the_tip_only() {
    let det = Det::new();
    let store = with_push(&det, "s1").await;
    store
        .git_push_trigger("s1".into(), A.to_owned())
        .await
        .unwrap();
    let open = store
        .git_push_open_attempt("s1".into(), "file:///v".to_owned())
        .await
        .unwrap();
    assert_eq!(open.state, PushState::Pushing);
    assert_eq!(open.attempt_oid.as_deref(), Some(A));
    assert_eq!(open.attempt_lease.as_deref(), Some(""));
    assert_eq!(open.attempt_url.as_deref(), Some("file:///v"));
    // A second attempt over an open one is refused.
    assert!(
        store
            .git_push_open_attempt("s1".into(), "x".to_owned())
            .await
            .is_err()
    );
    store
        .git_push_trigger("s1".into(), B.to_owned())
        .await
        .unwrap();
    let busy = row(&store, "s1").await;
    assert_eq!(busy.state, PushState::Pushing);
    assert_eq!(busy.desired_oid.as_deref(), Some(B));
    assert_eq!(busy.attempt_oid.as_deref(), Some(A));
    // The attempt applied: the lease moves and a newer tip is due again.
    let done = store.git_push_applied("s1".into()).await.unwrap().unwrap();
    assert_eq!(done.last_pushed_oid.as_deref(), Some(A));
    assert_eq!((done.state, done.attempt_id), (PushState::Due, None));
    let again = store
        .git_push_open_attempt("s1".into(), "file:///v".to_owned())
        .await
        .unwrap();
    assert_eq!(
        (again.attempt_oid.as_deref(), again.attempt_lease.as_deref()),
        (Some(B), Some(A))
    );
    store.git_push_applied("s1".into()).await.unwrap();
    let pushed = row(&store, "s1").await;
    assert_eq!((pushed.state, pushed.attempts), (PushState::Pushed, 0));
    // The same tip again gives no push.
    store
        .git_push_trigger("s1".into(), B.to_owned())
        .await
        .unwrap();
    assert_eq!(row(&store, "s1").await.state, PushState::Pushed);
    assert_eq!(
        push_states(&store, "s1").await,
        [
            PushState::Due,
            PushState::Pushing,
            PushState::Pushing,
            PushState::Due,
            PushState::Pushing,
            PushState::Pushed
        ]
    );
}

#[tokio::test]
async fn a_failure_backs_off_one_two_four_minutes_and_keeps_an_unknown_attempt() {
    let det = Det::at(1_000_000);
    let store = with_push(&det, "s1").await;
    store
        .git_push_trigger("s1".into(), A.to_owned())
        .await
        .unwrap();
    let delay = crate::workspace::fleet_git::backoff_delay;
    store
        .git_push_open_attempt("s1".into(), "u".to_owned())
        .await
        .unwrap();
    let text = ("push_failed".to_owned(), "Push failed".to_owned());
    // The outcome is unknown: the attempt stays open.
    let first = store
        .git_push_failed("s1".into(), text.clone(), true, delay)
        .await
        .unwrap()
        .unwrap();
    assert_eq!((first.state, first.attempts), (PushState::Failing, 1));
    assert_eq!(first.next_attempt_at, Some(1_000_000 + 60_000));
    assert!(first.attempt_id.is_some());
    // A trigger during `failing` keeps the attempts and the time.
    store
        .git_push_trigger("s1".into(), B.to_owned())
        .await
        .unwrap();
    let kept = row(&store, "s1").await;
    assert_eq!(
        (kept.attempts, kept.next_attempt_at),
        (1, first.next_attempt_at)
    );
    assert!(store.git_push_due(1_000_000).await.unwrap().is_empty());
    assert_eq!(store.git_push_due(1_060_000).await.unwrap().len(), 1);
    // Confirmed not applied: the attempt closes, the delay doubles.
    let second = store
        .git_push_failed("s1".into(), text, false, delay)
        .await
        .unwrap()
        .unwrap();
    assert_eq!((second.attempts, second.attempt_id), (2, None));
    assert_eq!(second.next_attempt_at, Some(1_000_000 + 120_000));
    // "Push now" moves the time only.
    let now = store.git_push_now("s1".into()).await.unwrap().unwrap();
    assert_eq!((now.attempts, now.next_attempt_at), (2, Some(1_000_000)));
    assert_eq!(store.git_push_due(1_000_000).await.unwrap().len(), 1);
}

#[tokio::test]
async fn the_push_state_survives_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("station.db");
    let det = Det::new();
    let open = || Store::open(&path, det.clock(), det.ids(), StoreConfig::default());
    {
        let store = open().await.unwrap();
        store.recover(Vec::new()).await.unwrap();
        store.create_session(new_session("s1")).await.unwrap();
        let push = Some(("origin".to_owned(), DEST.to_owned()));
        store
            .git_record_session(new_git("s1", "p1"), push)
            .await
            .unwrap();
        apply(&store, 5, "d", &["p1"]).await;
        store
            .git_push_trigger("s1".into(), A.to_owned())
            .await
            .unwrap();
        store
            .git_push_open_attempt("s1".into(), "file:///v".to_owned())
            .await
            .unwrap();
        store.git_set_push_cursor(7).await.unwrap();
    }
    let store = open().await.unwrap();
    store.recover(Vec::new()).await.unwrap();
    let kept = row(&store, "s1").await;
    // A row in `pushing` has its attempt values: the recovery read uses them.
    assert_eq!(kept.state, PushState::Pushing);
    assert_eq!(kept.attempt_oid.as_deref(), Some(A));
    assert_eq!(kept.attempt_lease.as_deref(), Some(""));
    assert_eq!(kept.attempt_url.as_deref(), Some("file:///v"));
    assert_eq!(store.git_push_due(0).await.unwrap().len(), 1);
    assert_eq!(store.git_push_cursor().await.unwrap(), Some(7));
    assert_eq!(apply(&store, 5, "d", &["p1"]).await, SnapshotResult::Same);
}

#[tokio::test]
async fn the_fetch_flag_and_the_session_row_are_stored() {
    let det = Det::new();
    let store = with_push(&det, "s1").await;
    let session = store.git_session("s1".into()).await.unwrap().unwrap();
    assert_eq!(session.slug.as_deref(), Some("alpha"));
    assert_eq!(session.publish_ref.as_deref(), Some("fd/alpha/s1"));
    assert!(session.created_branch);
    store.git_set_base_stale("s1".into()).await.unwrap();
    assert!(
        store
            .git_session("s1".into())
            .await
            .unwrap()
            .unwrap()
            .base_stale
    );
}
