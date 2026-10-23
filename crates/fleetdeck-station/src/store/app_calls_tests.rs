//! The app call rows: the start advance, the settlement, the barrier, the
//! release and the lists of the open calls (B15, design §4.4 and §4.6.1).

use fleetdeck_proto::apps::AppCsp;
use fleetdeck_proto::event::ProviderKind;
use fleetdeck_testkit::Det;

use super::*;
use crate::store::apps::{AppLookup, MAX_APPS_PER_SESSION, NewApp, ToolAppEvent};
use crate::store::{NewSession, NewTurnCheckpoint, OfflineApprovalPolicy, RowSide, StoreConfig};

const S1: &str = "s1";
const S2: &str = "s2";
const ROOT: &str = "/w";

async fn session(s: &Store, id: &str) {
    s.create_session(NewSession {
        session_id: id.into(),
        provider: ProviderKind::Codex,
        provider_instance: "codex".into(),
        cwd: ROOT.to_owned(),
        worktree: None,
        branch: None,
        mode: None,
        title: None,
        offline_approval_policy: OfflineApprovalPolicy::Wait,
    })
    .await
    .unwrap();
}

async fn store(det: &Det) -> Store {
    let s = Store::open_in_memory(det.clock(), det.ids(), StoreConfig::default())
        .await
        .unwrap();
    s.recover(Vec::new()).await.unwrap();
    session(&s, S1).await;
    session(&s, S2).await;
    s
}

fn new_call(s: &Store, call_id: &str, session: &str, root: &str, read_only: bool) -> NewAppCall {
    NewAppCall {
        call_id: call_id.to_owned(),
        session_id: session.into(),
        app_id: format!("app-{call_id}"),
        worktree_root: root.to_owned(),
        server: "demo".to_owned(),
        tool: "bump".to_owned(),
        read_only,
        process: "proc-1".to_owned(),
        incarnation: s.incarnation().as_str().to_owned(),
    }
}

async fn begin(
    s: &Store,
    call_id: &str,
    session: &str,
    root: &str,
    read_only: bool,
) -> Option<i64> {
    s.app_call_begin(new_call(s, call_id, session, root, read_only))
        .await
        .unwrap()
}

async fn state(s: &Store, call_id: &str) -> CallState {
    s.app_call(call_id.to_owned()).await.unwrap().unwrap().state
}

async fn generation(s: &Store, root: &str) -> i64 {
    let root = root.to_owned();
    call(&s.inner.db, move |conn| shell_runs::generation(conn, &root))
        .await
        .unwrap()
}

#[tokio::test]
async fn app_call_begin_advances_generation_only_when_not_read_only() {
    let det = Det::new();
    let s = store(&det).await;
    assert_eq!(generation(&s, ROOT).await, 0);
    assert_eq!(begin(&s, "ro", S1, ROOT, true).await, None);
    assert_eq!(
        generation(&s, ROOT).await,
        0,
        "a read-only call advances nothing"
    );
    assert_eq!(begin(&s, "w1", S1, ROOT, false).await, Some(1));
    assert_eq!(begin(&s, "w2", S1, ROOT, false).await, Some(2));
    assert_eq!(generation(&s, ROOT).await, 2);
    let row = s.app_call("w2".to_owned()).await.unwrap().unwrap();
    assert_eq!(row.generation, Some(2));
    assert_eq!(row.state, CallState::Running);
    assert!(!row.advance_pending);
    assert!(!row.read_only);
    let ro = s.app_call("ro".to_owned()).await.unwrap().unwrap();
    assert_eq!(ro.generation, None);
    assert!(ro.read_only);
}

#[tokio::test]
async fn app_call_settle_is_guarded_and_sets_advance_pending() {
    let det = Det::new();
    let s = store(&det).await;
    begin(&s, "w", S1, ROOT, false).await;
    begin(&s, "ro", S1, ROOT, true).await;
    assert!(
        s.app_call_settle("w".to_owned(), AppCallEnd::Done)
            .await
            .unwrap()
    );
    assert!(
        s.app_call_settle("ro".to_owned(), AppCallEnd::Failed)
            .await
            .unwrap()
    );
    // A terminal row does not move again.
    assert!(
        !s.app_call_settle("w".to_owned(), AppCallEnd::Ended)
            .await
            .unwrap()
    );
    assert!(
        !s.app_call_settle("nope".to_owned(), AppCallEnd::Ended)
            .await
            .unwrap()
    );
    let w = s.app_call("w".to_owned()).await.unwrap().unwrap();
    assert_eq!((w.state, w.advance_pending), (CallState::Done, true));
    assert!(w.ended_at.is_some());
    let ro = s.app_call("ro".to_owned()).await.unwrap().unwrap();
    assert_eq!((ro.state, ro.advance_pending), (CallState::Failed, false));
    assert_eq!(
        s.app_call_pending_advances().await.unwrap(),
        [("w".to_owned(), ROOT.to_owned())]
    );
    // The advance moves the generation once and clears the marker.
    assert_eq!(generation(&s, ROOT).await, 1);
    assert_eq!(
        s.app_call_advance("w".to_owned(), ROOT.to_owned())
            .await
            .unwrap(),
        2
    );
    assert_eq!(
        s.app_call_advance("w".to_owned(), ROOT.to_owned())
            .await
            .unwrap(),
        2
    );
    assert!(s.app_call_pending_advances().await.unwrap().is_empty());
    // An uncertain row settles too.
    begin(&s, "u", S1, ROOT, false).await;
    assert!(
        s.app_call_mark_uncertain("u".to_owned(), UncertainCause::ProviderError)
            .await
            .unwrap()
    );
    assert!(
        s.app_call_settle("u".to_owned(), AppCallEnd::Ended)
            .await
            .unwrap()
    );
    assert_eq!(state(&s, "u").await, CallState::Ended);
}

/// A turn row of `s1` whose start checkpoint was taken at `generation`.
async fn turn(s: &Store, app_turn: &str, generation: i64) {
    s.plan_turn_checkpoint(NewTurnCheckpoint {
        app_turn: app_turn.into(),
        session_id: S1.into(),
        process: None,
        input_id: None,
        turn_id: None,
        worktree_id: None,
        worktree_root: Some(ROOT.to_owned()),
        scope: "tracked".to_owned(),
        start: RowSide::Capturing,
        generation: Some(generation),
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn app_call_barrier_running_then_ran_after_turn() {
    let det = Det::new();
    let s = store(&det).await;
    turn(&s, "t0", 0).await;
    let barrier = || s.app_call_barrier(ROOT.to_owned(), "t0".to_owned());
    assert_eq!(barrier().await.unwrap(), None);
    begin(&s, "w", S1, ROOT, false).await;
    assert_eq!(barrier().await.unwrap(), Some(AppCallBarrier::Running));
    // A settled row whose advance is still pending is unsettled.
    s.app_call_settle("w".to_owned(), AppCallEnd::Done)
        .await
        .unwrap();
    assert_eq!(barrier().await.unwrap(), Some(AppCallBarrier::Running));
    s.app_call_advance("w".to_owned(), ROOT.to_owned())
        .await
        .unwrap();
    // The call started at generation 1; the turn started at 0.
    assert_eq!(barrier().await.unwrap(), Some(AppCallBarrier::RanAfterTurn));
    // A turn that started at the generation of the call is not behind it.
    turn(&s, "t1", 1).await;
    assert_eq!(
        s.app_call_barrier(ROOT.to_owned(), "t1".to_owned())
            .await
            .unwrap(),
        None
    );
    // Another root is not touched; a read-only row after its settlement is None.
    assert_eq!(
        s.app_call_barrier("/other".to_owned(), "t0".to_owned())
            .await
            .unwrap(),
        None
    );
    begin(&s, "ro", S1, "/ro", true).await;
    s.app_call_settle("ro".to_owned(), AppCallEnd::Done)
        .await
        .unwrap();
    assert_eq!(
        s.app_call_barrier("/ro".to_owned(), "t0".to_owned())
            .await
            .unwrap(),
        None
    );
}

#[tokio::test]
async fn app_call_release_states() {
    let det = Det::new();
    let s = store(&det).await;
    begin(&s, "run", S1, ROOT, false).await;
    begin(&s, "unc", S1, ROOT, false).await;
    begin(&s, "ro", S1, ROOT, true).await;
    for id in ["unc", "ro"] {
        s.app_call_mark_uncertain(id.to_owned(), UncertainCause::StationRestart)
            .await
            .unwrap();
    }
    let rel = |session: &str, id: &str| s.app_call_release(session.into(), id.to_owned());
    assert_eq!(rel(S1, "run").await.unwrap(), Release::Running);
    // A row of another session is unknown, and stays open.
    assert_eq!(rel(S2, "unc").await.unwrap(), Release::Unknown);
    assert_eq!(rel(S1, "nope").await.unwrap(), Release::Unknown);
    assert_eq!(state(&s, "unc").await, CallState::Uncertain);
    assert_eq!(rel(S1, "unc").await.unwrap(), Release::Released);
    assert_eq!(rel(S1, "unc").await.unwrap(), Release::NotOpen);
    let unc = s.app_call("unc".to_owned()).await.unwrap().unwrap();
    assert_eq!(
        (unc.state, unc.advance_pending),
        (CallState::Released, true)
    );
    assert_eq!(rel(S1, "ro").await.unwrap(), Release::Released);
    let ro = s.app_call("ro".to_owned()).await.unwrap().unwrap();
    assert!(!ro.advance_pending, "a read-only release needs no advance");
    // A terminal row is not open.
    s.app_call_settle("run".to_owned(), AppCallEnd::Done)
        .await
        .unwrap();
    assert_eq!(rel(S1, "run").await.unwrap(), Release::NotOpen);
}

/// Stores an app of `s1` (the row and its event).
async fn put_app(s: &Store) -> String {
    s.insert_app(
        NewApp {
            session_id: S1.into(),
            html: b"<html>x</html>".to_vec(),
            csp: AppCsp::default(),
            tool_input: serde_json::Value::Null,
            tool_result: None,
        },
        ToolAppEvent {
            node_id: "n".into(),
            server: "demo".to_owned(),
            tool: "bump".to_owned(),
            resource_uri: "ui://demo/x".to_owned(),
            prefers_border: None,
        },
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn app_call_release_needs_no_app_row() {
    let det = Det::new();
    let s = store(&det).await;
    let app_id = put_app(&s).await;
    let mut row = new_call(&s, "c", S1, ROOT, false);
    row.app_id.clone_from(&app_id);
    s.app_call_begin(row).await.unwrap();
    s.app_call_mark_uncertain("c".to_owned(), UncertainCause::ProviderError)
        .await
        .unwrap();
    // The caps delete the app row.
    for _ in 0..MAX_APPS_PER_SESSION {
        put_app(&s).await;
    }
    assert_eq!(
        s.app_lookup(S1.into(), app_id).await.unwrap(),
        AppLookup::Gone
    );
    assert_eq!(
        s.app_call_release(S1.into(), "c".to_owned()).await.unwrap(),
        Release::Released
    );
}

#[tokio::test]
async fn app_call_mark_uncertain_sets_cause() {
    let det = Det::new();
    let s = store(&det).await;
    begin(&s, "a", S1, ROOT, false).await;
    begin(&s, "b", S1, ROOT, false).await;
    assert!(
        s.app_call_mark_uncertain("a".to_owned(), UncertainCause::StationRestart)
            .await
            .unwrap()
    );
    assert!(
        s.app_call_mark_uncertain("b".to_owned(), UncertainCause::ProviderError)
            .await
            .unwrap()
    );
    // Guarded: only a row in `running` moves.
    assert!(
        !s.app_call_mark_uncertain("a".to_owned(), UncertainCause::ProviderError)
            .await
            .unwrap()
    );
    let a = s.app_call("a".to_owned()).await.unwrap().unwrap();
    let b = s.app_call("b".to_owned()).await.unwrap().unwrap();
    assert_eq!(
        (a.state, a.cause),
        (CallState::Uncertain, Some(UncertainCause::StationRestart))
    );
    assert_eq!(b.cause, Some(UncertainCause::ProviderError));
    assert_eq!(s.app_calls_uncertain().await.unwrap().len(), 2);
    begin(&s, "c", S1, ROOT, false).await;
    assert_eq!(
        s.app_call("c".to_owned()).await.unwrap().unwrap().cause,
        None
    );
}

#[tokio::test]
async fn app_calls_uncertain_of_process_filters_by_process() {
    let det = Det::new();
    let s = store(&det).await;
    let mut other = new_call(&s, "p2", S1, ROOT, false);
    other.process = "proc-2".to_owned();
    s.app_call_begin(other).await.unwrap();
    begin(&s, "p1", S1, ROOT, false).await;
    begin(&s, "x", S2, ROOT, false).await;
    begin(&s, "run", S1, ROOT, false).await;
    for id in ["p1", "p2", "x"] {
        s.app_call_mark_uncertain(id.to_owned(), UncertainCause::ProviderError)
            .await
            .unwrap();
    }
    let ids = |rows: Vec<AppCallRow>| rows.into_iter().map(|r| r.call_id).collect::<Vec<_>>();
    assert_eq!(
        ids(s
            .app_calls_uncertain_of_process(S1.into(), "proc-1".to_owned())
            .await
            .unwrap()),
        ["p1"]
    );
    assert_eq!(
        ids(s
            .app_calls_uncertain_of_process(S1.into(), "proc-2".to_owned())
            .await
            .unwrap()),
        ["p2"]
    );
}

#[tokio::test]
async fn app_calls_of_older_incarnations_lists_only_running_rows() {
    let det = Det::new();
    let s = store(&det).await;
    begin(&s, "now", S1, ROOT, false).await;
    let mut old = new_call(&s, "old", S1, ROOT, false);
    old.incarnation = "an-older-incarnation".to_owned();
    s.app_call_begin(old).await.unwrap();
    let mut old_unc = new_call(&s, "old-unc", S1, ROOT, false);
    old_unc.incarnation = "an-older-incarnation".to_owned();
    s.app_call_begin(old_unc).await.unwrap();
    s.app_call_mark_uncertain("old-unc".to_owned(), UncertainCause::StationRestart)
        .await
        .unwrap();
    let rows = s.app_calls_of_older_incarnations().await.unwrap();
    assert_eq!(
        rows.into_iter().map(|r| r.call_id).collect::<Vec<_>>(),
        ["old"]
    );
}

/// Stores a revert row of `session` at `root` (raw SQL: the revert state
/// machine is not under test).
async fn revert(s: &Store, revert_id: &str, session: &str, root: &str, created_at: i64) {
    let (revert_id, session, root, app_turn) = (
        revert_id.to_owned(),
        session.to_owned(),
        root.to_owned(),
        "t0".to_owned(),
    );
    call(&s.inner.db, move |conn| {
        conn.execute(
            "INSERT INTO reverts (revert_id, session_id, process, turn_id, app_turn,
                 current_app_turn, turns, target_commit, current_commit, worktree_root, step,
                 created_at, updated_at)
             VALUES (?1, ?2, 'p', 't', ?4, ?4, 1, 'a', 'b', ?3, 'succeeded', ?5, ?5)",
            rusqlite::params![revert_id, session, root, app_turn, created_at],
        )?;
        Ok(())
    })
    .await
    .unwrap();
}

/// The call ids of `app_calls_open` for `s1`.
async fn open(s: &Store, extra: Option<&str>) -> Vec<String> {
    s.app_calls_open(S1.into(), extra.map(str::to_owned))
        .await
        .unwrap()
        .into_iter()
        .map(|r| r.call_id)
        .collect()
}

#[tokio::test]
async fn app_calls_open_lists_by_session_and_folder() {
    let det = Det::new();
    let s = store(&det).await;
    // s1 has an open row on /a; s2 has rows on /a, /b and /c; s3 is unrelated.
    session(&s, "s3").await;
    for (id, owner, root) in [
        ("own", S1, "/a"),
        ("other-a", S2, "/a"),
        ("other-b", S2, "/b"),
        ("other-c", S2, "/c"),
        ("unrelated", "s3", "/z"),
        ("done-a", S2, "/a"),
    ] {
        begin(&s, id, owner, root, true).await;
        det.advance(std::time::Duration::from_millis(1));
    }
    s.app_call_settle("done-a".to_owned(), AppCallEnd::Done)
        .await
        .unwrap();
    // Own rows, and the rows of other sessions on the root of an own open row.
    assert_eq!(open(&s, None).await, ["own", "other-a"]);
    // The root of the session's newest revert row.
    turn(&s, "t0", 0).await;
    revert(&s, "r-old", S1, "/c", 1).await;
    revert(&s, "r-new", S1, "/b", 2).await;
    assert_eq!(open(&s, None).await, ["own", "other-a", "other-b"]);
    // `extra_root`.
    assert_eq!(
        open(&s, Some("/c")).await,
        ["own", "other-a", "other-b", "other-c"]
    );
    // An unrelated root stays out; terminal rows never show.
    assert!(!open(&s, Some("/c")).await.contains(&"unrelated".to_owned()));
    assert!(!open(&s, Some("/a")).await.contains(&"done-a".to_owned()));
}

#[tokio::test]
async fn app_calls_open_is_oldest_first_and_limited_to_32() {
    let det = Det::new();
    let s = store(&det).await;
    // The ids fall while the rows get older: the order is by `created_at`.
    for i in 0..40 {
        begin(&s, &format!("c{:02}", 39 - i), S1, ROOT, true).await;
        det.advance(std::time::Duration::from_millis(1));
    }
    let rows = s.app_calls_open(S1.into(), None).await.unwrap();
    assert_eq!(rows.len(), OPEN_CALLS_LIMIT);
    assert_eq!(rows[0].call_id, "c39");
    assert_eq!(rows[31].call_id, "c08");
}

#[tokio::test]
async fn app_call_open_count_counts_running_and_uncertain() {
    let det = Det::new();
    let s = store(&det).await;
    begin(&s, "a", S1, ROOT, false).await;
    begin(&s, "b", S1, ROOT, false).await;
    begin(&s, "c", S1, ROOT, false).await;
    begin(&s, "d", S2, ROOT, false).await;
    s.app_call_mark_uncertain("b".to_owned(), UncertainCause::ProviderError)
        .await
        .unwrap();
    s.app_call_settle("c".to_owned(), AppCallEnd::Done)
        .await
        .unwrap();
    assert_eq!(s.app_call_open_count(S1.into()).await.unwrap(), (1, 1));
    assert_eq!(s.app_call_open_count(S2.into()).await.unwrap(), (1, 0));
}
