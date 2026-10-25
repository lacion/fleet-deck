//! The land run rows (design §11.5, V30).

use std::collections::BTreeMap;

use fleetdeck_proto::fleet_git::{
    LandMode, LandPhase, LandPlan, LandState, PushTarget, RemoteIdentity,
};
use fleetdeck_proto::mutation::CommandAuthority;
use fleetdeck_testkit::Det;

use super::{LandRunRow, NewLandRun, by_command, get, guard_running, insert, running};
use crate::store::git_state::tests::{new_session, store};
use crate::store::{Store, call, land_fences};

pub(crate) fn plan() -> LandPlan {
    LandPlan {
        project_id: "p1".to_owned(),
        settings_revision: 3,
        home_generation: 1,
        mode: LandMode::Merge,
        fetch_remote: "origin".to_owned(),
        remote: RemoteIdentity {
            repo: "h/acme/app".to_owned(),
            dest: "https://h:443/acme/app".to_owned(),
        },
        default_branch: Some("main".to_owned()),
        branch_ref: "fd/alpha/s1".to_owned(),
        publish_ref: "fd/alpha/s1".to_owned(),
        branch_target: PushTarget {
            target: "origin".to_owned(),
            dest: "https://h:443/acme/app".to_owned(),
        },
        forge: None,
        check_cmd: None,
        check_env: BTreeMap::new(),
        pr_title: "t".to_owned(),
        pr_body: "b".to_owned(),
    }
}

pub(crate) fn new_run(land_id: &str, command_id: &str, session: &str) -> NewLandRun {
    NewLandRun {
        land_id: land_id.to_owned(),
        command_id: command_id.to_owned(),
        session_id: session.into(),
        plan: plan(),
        authority: None,
    }
}

pub(crate) async fn with_session(det: &Det, id: &str) -> Store {
    let store = store(det).await;
    store.create_session(new_session(id)).await.unwrap();
    store
}

async fn put(store: &Store, new: NewLandRun) -> crate::store::Result<()> {
    call(&store.inner.db, move |conn| insert(conn, &new, 1_000)).await
}

#[tokio::test]
async fn a_new_run_starts_running_in_phase_admitted_and_reads_back() {
    let det = Det::new();
    let store = with_session(&det, "s1").await;
    put(&store, new_run("land-1", "landcmd-1", "s1"))
        .await
        .unwrap();
    let row: LandRunRow = store.land_run("land-1".to_owned()).await.unwrap().unwrap();
    assert_eq!(row.state, LandState::Running);
    assert_eq!(row.phase, LandPhase::Admitted);
    assert_eq!(row.attempt, 1);
    assert_eq!(row.plan, plan());
    assert_eq!(row.authority, None);
    assert_eq!(row.pending_action, None);
    assert!(!row.reconciling);
    assert_eq!(row.files, Vec::<String>::new());
    assert_eq!(
        (row.created_at, row.updated_at, row.ended_at),
        (1_000, 1_000, None)
    );
    let by = store
        .land_run_by_command("landcmd-1".to_owned())
        .await
        .unwrap();
    assert_eq!(by, Some(row));
    assert_eq!(store.land_run("land-x".to_owned()).await.unwrap(), None);
}

#[tokio::test]
async fn the_authority_of_a_coordinator_land_is_stored_and_read() {
    let det = Det::new();
    let store = with_session(&det, "s1").await;
    let mut new = new_run("land-1", "landcmd-1", "s1");
    let authority = CommandAuthority {
        grant_id: "g1".into(),
        caller_station: "st".into(),
        caller_session: "cs".into(),
        generation: 4,
    };
    new.authority = Some(authority.clone());
    put(&store, new).await.unwrap();
    let row = store.land_run("land-1".to_owned()).await.unwrap().unwrap();
    assert_eq!(row.authority, Some(authority));
}

#[tokio::test]
async fn a_land_id_and_a_command_id_are_stored_once() {
    let det = Det::new();
    let store = with_session(&det, "s1").await;
    put(&store, new_run("land-1", "landcmd-1", "s1"))
        .await
        .unwrap();
    assert!(
        put(&store, new_run("land-1", "landcmd-2", "s1"))
            .await
            .is_err()
    );
    assert!(
        put(&store, new_run("land-2", "landcmd-1", "s1"))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn a_run_needs_a_session() {
    let det = Det::new();
    let store = with_session(&det, "s1").await;
    assert!(
        put(&store, new_run("land-1", "landcmd-1", "nobody"))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn only_running_rows_are_listed_oldest_first() {
    let det = Det::new();
    let store = with_session(&det, "s1").await;
    for (n, at) in [("b", 20), ("a", 10), ("c", 30)] {
        let new = new_run(&format!("land-{n}"), &format!("cmd-{n}"), "s1");
        call(&store.inner.db, move |conn| insert(conn, &new, at))
            .await
            .unwrap();
    }
    call(&store.inner.db, |conn| {
        conn.execute(
            "UPDATE land_runs SET state = 'landed' WHERE land_id = 'land-b'",
            [],
        )?;
        Ok(())
    })
    .await
    .unwrap();
    let ids: Vec<String> = call(&store.inner.db, |conn| running(conn))
        .await
        .unwrap()
        .into_iter()
        .map(|r| r.land_id)
        .collect();
    assert_eq!(ids, ["land-a", "land-c"]);
    let ids: Vec<String> = store
        .land_runs_running()
        .await
        .unwrap()
        .into_iter()
        .map(|r| r.land_id)
        .collect();
    assert_eq!(ids, ["land-a", "land-c"]);
}

#[tokio::test]
async fn the_attempt_is_one_or_two() {
    let det = Det::new();
    let store = with_session(&det, "s1").await;
    put(&store, new_run("land-1", "landcmd-1", "s1"))
        .await
        .unwrap();
    let bad = call(&store.inner.db, |conn| {
        Ok(conn
            .execute("UPDATE land_runs SET attempt = 3", [])
            .is_err())
    })
    .await
    .unwrap();
    assert!(bad);
}

#[tokio::test]
async fn a_pending_write_needs_its_operands_and_reconciling_needs_a_write() {
    let det = Det::new();
    let store = with_session(&det, "s1").await;
    put(&store, new_run("land-1", "landcmd-1", "s1"))
        .await
        .unwrap();
    let (partial, reconciling, whole) = call(&store.inner.db, |conn| {
        let partial = conn
            .execute("UPDATE land_runs SET pending_action = 'branch_push'", [])
            .is_err();
        let reconciling = conn
            .execute("UPDATE land_runs SET reconciling = 1", [])
            .is_err();
        let whole = conn
            .execute(
                "UPDATE land_runs SET pending_action = 'default_push', pending_attempt = 1,
                        pending_url = 'u', pending_ref = 'refs/heads/main', pending_oid = 'o',
                        reconciling = 1",
                [],
            )
            .is_ok();
        Ok((partial, reconciling, whole))
    })
    .await
    .unwrap();
    assert!(partial, "an action without its operands is refused");
    assert!(reconciling, "reconciling without an action is refused");
    assert!(whole);
    let row = store.land_run("land-1".to_owned()).await.unwrap().unwrap();
    assert_eq!(row.pending_action.as_deref(), Some("default_push"));
    assert_eq!(row.pending_attempt, Some(1));
    assert!(row.reconciling);
}

#[tokio::test]
async fn the_guard_needs_a_running_run_without_a_fence() {
    let det = Det::new();
    let store = with_session(&det, "s1").await;
    put(&store, new_run("land-1", "landcmd-1", "s1"))
        .await
        .unwrap();
    let guard = |id: &'static str| {
        let store = store.clone();
        async move {
            call(&store.inner.db, move |conn| guard_running(conn, id))
                .await
                .unwrap()
        }
    };
    assert!(guard("land-1").await);
    assert!(!guard("land-unknown").await);
    call(&store.inner.db, |conn| {
        land_fences::insert(conn, "land-1", 5)
    })
    .await
    .unwrap();
    assert!(!guard("land-1").await, "a fenced run does not pass");
    let by = call(&store.inner.db, |conn| by_command(conn, "landcmd-1"))
        .await
        .unwrap();
    assert!(by.is_some());
    let none = call(&store.inner.db, |conn| get(conn, "land-9"))
        .await
        .unwrap();
    assert!(none.is_none());
}

#[tokio::test]
async fn a_terminal_run_does_not_pass_the_guard() {
    let det = Det::new();
    let store = with_session(&det, "s1").await;
    put(&store, new_run("land-1", "landcmd-1", "s1"))
        .await
        .unwrap();
    call(&store.inner.db, |conn| {
        conn.execute("UPDATE land_runs SET state = 'failed'", [])?;
        Ok(())
    })
    .await
    .unwrap();
    let passed = call(&store.inner.db, |conn| guard_running(conn, "land-1"))
        .await
        .unwrap();
    assert!(!passed);
}
