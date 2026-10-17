//! The artifact columns of `session_git` and the exposure inputs (design
//! §15.4, §15.5).

use std::path::Path;

use fleetdeck_proto::Event;
use fleetdeck_proto::event::SessionState;
use fleetdeck_proto::fleet_git::{ArtifactAuthorization, HomeAttemptState, ShareVerdict};
use fleetdeck_testkit::Det;

use super::{ArtifactsRecord, ExposureInputs};
use crate::store::NewEvent;
use crate::store::git_state::tests::{apply, new_git, new_session, store};
use crate::store::home_attempts::{BeginHome, HomeOutcome, NewHomeAttempt};

fn auth(share: &str) -> ArtifactAuthorization {
    ArtifactAuthorization {
        share: share.to_owned(),
        exporting_station: None,
        inventory_revision: Some(7),
        verdict: ShareVerdict::Ok,
        exposed: Vec::new(),
        checked_at: Some(5),
        issued_at: 10,
        expires_at: 610,
    }
}

fn record(path: &str) -> ArtifactsRecord {
    ArtifactsRecord {
        path: Some(path.to_owned()),
        dev: Some(3),
        ino: Some(4),
        url: None,
        auth: auth("box/out"),
    }
}

#[tokio::test]
async fn the_artifact_columns_and_the_authorization_are_stored_and_read() {
    let store = store(&Det::new()).await;
    store.create_session(new_session("S1")).await.unwrap();
    store
        .git_record_session(new_git("S1", "p1"), None)
        .await
        .unwrap();
    let blank = store.artifacts_row("S1".into()).await.unwrap().unwrap();
    assert!(blank.path.is_none() && blank.url.is_none() && blank.auth.is_none());
    assert!(blank.live);
    assert!(store.artifacts_live().await.unwrap().is_empty());
    store
        .artifacts_record("S1".into(), record("/m/app/ab/"))
        .await
        .unwrap();
    let row = store.artifacts_row("S1".into()).await.unwrap().unwrap();
    assert_eq!(row.project_id, "p1");
    assert_eq!(row.path.as_deref(), Some("/m/app/ab/"));
    assert_eq!((row.dev, row.ino), (Some(3), Some(4)));
    assert_eq!(row.auth, Some(auth("box/out")));
    assert_eq!(store.artifacts_live().await.unwrap().len(), 1);
    // A new authorization replaces the stored one only.
    store
        .artifacts_set_auth("S1".into(), auth("box/other"))
        .await
        .unwrap();
    let row = store.artifacts_row("S1".into()).await.unwrap().unwrap();
    assert_eq!(row.auth.unwrap().share, "box/other");
    assert_eq!(row.path.as_deref(), Some("/m/app/ab/"));
    // A session without a `session_git` row has none.
    store.create_session(new_session("S2")).await.unwrap();
    assert!(store.artifacts_row("S2".into()).await.unwrap().is_none());
}

#[tokio::test]
async fn an_ended_session_is_not_live() {
    let store = store(&Det::new()).await;
    store.create_session(new_session("S1")).await.unwrap();
    store
        .git_record_session(new_git("S1", "p1"), None)
        .await
        .unwrap();
    store
        .artifacts_record("S1".into(), record("/m/app/ab/"))
        .await
        .unwrap();
    assert_eq!(store.artifacts_live().await.unwrap().len(), 1);
    store
        .append(
            "S1".into(),
            vec![NewEvent::from(Event::SessionStateChanged {
                state: SessionState::Exited,
                detail: None,
            })],
        )
        .await
        .unwrap();
    assert!(store.artifacts_live().await.unwrap().is_empty());
    let row = store.artifacts_row("S1".into()).await.unwrap().unwrap();
    assert!(!row.live);
}

fn attempt(id: &str, dest: &Path) -> NewHomeAttempt {
    NewHomeAttempt {
        attempt_id: id.to_owned(),
        // One attempt for each project and generation.
        project_id: format!("q-{id}"),
        generation: 1,
        request_digest: format!("d-{id}"),
        retry_of: None,
        dest_path: dest.to_string_lossy().into_owned(),
        url: "https://h/a.git".to_owned(),
        remote_name: "origin".to_owned(),
    }
}

#[tokio::test]
async fn the_exposure_inputs_list_every_home_every_worktree_and_the_snapshot() {
    let store = store(&Det::new()).await;
    let empty = store.exposure_inputs().await.unwrap();
    assert_eq!(empty, ExposureInputs::default());
    assert!(!empty.snapshot_seen);
    apply(&store, 1, "d", &["p1"]).await;
    // A running clone, a failed one and a ready one; a superseded one is left out.
    for (id, dest) in [("c", "/r/cloning"), ("f", "/r/failed"), ("ok", "/r/ready")] {
        let begun = store
            .home_begin(attempt(id, Path::new(dest)))
            .await
            .unwrap();
        assert!(matches!(begun, BeginHome::Created(_)), "{begun:?}");
    }
    let outcome = |state, path: Option<&str>| HomeOutcome {
        state,
        path: path.map(str::to_owned),
        dev: None,
        ino: None,
        code: None,
        message: None,
    };
    store
        .home_finish("f".into(), outcome(HomeAttemptState::Failed, None))
        .await
        .unwrap();
    store
        .home_finish(
            "ok".into(),
            outcome(HomeAttemptState::Ready, Some("/r/ready-real")),
        )
        .await
        .unwrap();
    store.create_session(new_session("S1")).await.unwrap();
    store
        .git_record_session(new_git("S1", "p1"), None)
        .await
        .unwrap();
    let inputs = store.exposure_inputs().await.unwrap();
    assert!(inputs.snapshot_seen);
    // The snapshot home and its canonical path (`/r`), the reserved folders
    // of the clone and the failed attempt, and the path of the ready one.
    for want in ["/r", "/r/cloning", "/r/failed", "/r/ready-real"] {
        assert!(
            inputs.homes.iter().any(|h| h == want),
            "{want}: {:?}",
            inputs.homes
        );
    }
    assert!(!inputs.homes.iter().any(|h| h == "/r/ready"));
    // The `repo_root` of the `session_git` row.
    assert!(
        inputs.worktrees.iter().any(|w| w == "/r"),
        "{:?}",
        inputs.worktrees
    );
}
