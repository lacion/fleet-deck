//! The shim binding rows, their pure transitions and the output
//! transaction that writes them (DL-194 T3).

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use fleetdeck_proto::command::{
    CommandOrigin, CommandResult, RETRY_WINDOW_MS, SessionCommand, UserInput,
};
use fleetdeck_proto::event::ProviderKind;
use fleetdeck_proto::{CommandId, Event, EventBody};
use fleetdeck_providers::host::shim::wire::ProviderIdentity;
use fleetdeck_testkit::Det;

use super::*;
use crate::coordinator::{Binding, BindingState};
use crate::store::{
    Caller, NewSession, NewShim, OfflineApprovalPolicy, OutputCommit, OutputMeta, ReadResult,
    StoreConfig,
};

fn row(digest: &str, state: RowState, native: Option<&str>) -> BindingRow {
    BindingRow {
        digest: digest.to_owned(),
        shim_id: "a".to_owned(),
        session_id: "s1".into(),
        binding_id: format!("b-{digest}"),
        grant_id: format!("g-{digest}"),
        provider_instance: "claude".to_owned(),
        native_id: native.map(str::to_owned),
        generation: u32::from(native.is_some()),
        classes: "[\"read\"]".to_owned(),
        scope: "{\"scope\":\"own\"}".to_owned(),
        state,
        control_skill: "{\"skill\":1}".to_owned(),
        kind: fleetdeck_proto::command::GrantKind::Coordinator,
        expires_at: Some(99),
        created_at: 5,
    }
}

fn fleet_only(digest: &str, state: RowState) -> BindingRow {
    BindingRow {
        grant_id: String::new(),
        expires_at: None,
        ..row(digest, state, None)
    }
}

fn started(native: Option<&str>) -> NewEvent {
    let mut refs = fleetdeck_proto::event::ProviderRefs::new();
    if let Some(n) = native {
        refs.insert("session_id".to_owned(), n.into());
    }
    Event::SessionStarted {
        provider: ProviderKind::Claude,
        provider_instance: "claude".into(),
        provider_version: None,
        model: None,
        cwd: "/w".to_owned(),
        mode: None,
        capabilities: Box::new(fleetdeck_proto::capability::Capabilities::minimal()),
        resumed: None,
        provider_refs: refs,
        title: None,
        branch: None,
        repo: None,
    }
    .into()
}

fn mode(m: &str) -> NewEvent {
    Event::ModeChanged { mode: m.to_owned() }.into()
}

fn signal(native: &str, after: usize) -> IdentitySignal {
    IdentitySignal {
        native_id: native.to_owned(),
        after,
    }
}

fn bound(digest: &str, native: &str) -> Transition {
    Transition::Bound {
        digest: digest.to_owned(),
        native_id: native.to_owned(),
        generation: 1,
    }
}

fn ended(digest: &str, reason: &str) -> Transition {
    Transition::Ended {
        digest: digest.to_owned(),
        reason: reason.to_owned(),
    }
}

// ---- the pure rules ----

#[test]
fn a_pending_row_binds_on_session_started() {
    let rows = [row("d1", RowState::Pending, None)];
    let ts = transitions(&rows, &[started(Some("n1"))], &[], false);
    assert_eq!(ts, [bound("d1", "n1")]);
}

#[test]
fn session_started_reads_the_thread_id_when_there_is_no_session_id() {
    let rows = [row("d1", RowState::Pending, None)];
    let mut refs = fleetdeck_proto::event::ProviderRefs::new();
    refs.insert("thread_id".to_owned(), "t1".into());
    let mut event = started(None);
    let EventBody::Known(Event::SessionStarted { provider_refs, .. }) = &mut event.event else {
        panic!("no session.started");
    };
    *provider_refs = refs;
    assert_eq!(
        transitions(&rows, &[event], &[], false),
        [bound("d1", "t1")]
    );
}

#[test]
fn session_started_without_a_native_id_changes_nothing() {
    let rows = [row("d1", RowState::Pending, None)];
    assert!(transitions(&rows, &[started(None)], &[], false).is_empty());
}

#[test]
fn an_active_row_with_the_same_identity_does_not_change() {
    let rows = [row("d1", RowState::Active, Some("n1"))];
    assert!(transitions(&rows, &[mode("x")], &[signal("n1", 0)], false).is_empty());
    assert!(transitions(&rows, &[started(Some("n1"))], &[], false).is_empty());
}

#[test]
fn an_active_row_with_another_identity_ends_as_native_replaced() {
    let rows = [row("d1", RowState::Active, Some("n1"))];
    let ts = transitions(&rows, &[mode("x")], &[signal("n2", 1)], false);
    assert_eq!(ts, [ended("d1", REASON_NATIVE_REPLACED)]);
}

#[test]
fn an_identity_signal_binds_a_pending_row() {
    let rows = [row("d1", RowState::Pending, None)];
    assert_eq!(
        transitions(&rows, &[], &[signal("n1", 0)], false),
        [bound("d1", "n1")]
    );
}

#[test]
fn a_signal_before_session_started_binds_and_the_later_start_adds_nothing() {
    let rows = [row("d1", RowState::Pending, None)];
    let ts = transitions(&rows, &[started(Some("n1"))], &[signal("n1", 0)], false);
    assert_eq!(ts, [bound("d1", "n1")]);
}

#[test]
fn a_signal_after_the_events_is_applied_after_them() {
    let rows = [row("d1", RowState::Pending, None)];
    let ts = transitions(&rows, &[started(Some("n1"))], &[signal("n2", 1)], false);
    assert_eq!(ts, [bound("d1", "n1"), ended("d1", REASON_NATIVE_REPLACED)]);
}

#[test]
fn exit_ends_the_pending_and_active_rows() {
    let rows = [
        row("d1", RowState::Pending, None),
        row("d2", RowState::Active, Some("n1")),
    ];
    let ts = transitions(&rows, &[], &[], true);
    assert_eq!(
        ts,
        [
            ended("d1", REASON_PROCESS_EXIT),
            ended("d2", REASON_PROCESS_EXIT)
        ]
    );
}

#[test]
fn ended_and_suspended_rows_never_change() {
    let rows = [
        row("d1", RowState::Ended, Some("n1")),
        row("d2", RowState::Suspended, Some("n1")),
    ];
    let ts = transitions(&rows, &[started(Some("n9"))], &[signal("n8", 0)], true);
    assert!(ts.is_empty());
}

#[test]
fn lifecycle_events_skip_fleet_only_rows() {
    let rows = [
        row("d1", RowState::Pending, None),
        fleet_only("d2", RowState::Pending),
    ];
    let ts = transitions(&rows, &[started(Some("n1"))], &[], true);
    assert_eq!(ts.len(), 4, "{ts:?}");
    let events = lifecycle_events(&rows, &ts);
    let kinds: Vec<_> = events
        .iter()
        .map(|e| match &e.event {
            EventBody::Known(Event::CoordinatorBound {
                binding_id,
                grant_id,
                generation,
                provider_instance,
                native_id,
            }) => format!(
                "bound {binding_id} {grant_id} {generation} {provider_instance} {native_id}"
            ),
            EventBody::Known(Event::CoordinatorEnded {
                binding_id,
                generation,
                reason,
            }) => format!("ended {binding_id} {generation} {reason}"),
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(
        kinds,
        ["bound b-d1 g-d1 1 claude n1", "ended b-d1 1 process_exit"]
    );
}

// ---- the rows ----

async fn store(det: &Det) -> Store {
    let s = Store::open_in_memory(det.clock(), det.ids(), StoreConfig::default())
        .await
        .unwrap();
    s.recover(Vec::new()).await.unwrap();
    s.create_session(NewSession {
        session_id: "s1".into(),
        provider: ProviderKind::Claude,
        provider_instance: "claude".into(),
        cwd: "/w".to_owned(),
        worktree: None,
        branch: None,
        mode: None,
        title: None,
        offline_approval_policy: OfflineApprovalPolicy::Wait,
    })
    .await
    .unwrap();
    s
}

/// A store with the running shim `a` of session `s1` at generation 1.
async fn running(det: &Det) -> Store {
    let s = store(det).await;
    s.shim_starting(NewShim {
        shim_id: "a".to_owned(),
        session_id: "s1".into(),
        socket_path: PathBuf::from("/run/x/a.sock"),
        launch_boot_id: None,
        native_target: None,
    })
    .await
    .unwrap();
    let identity = ProviderIdentity {
        pid: 1,
        start_time: 2,
        boot_id: "boot".to_owned(),
    };
    assert!(s.shim_running("a".to_owned(), identity).await.unwrap());
    assert_eq!(
        s.shim_next_generation("a".to_owned()).await.unwrap(),
        Some(1)
    );
    s
}

#[tokio::test]
async fn a_row_round_trips_with_every_column() {
    let det = Det::new();
    let s = store(&det).await;
    let full = row("d1", RowState::Active, Some("n1"));
    let fleet = fleet_only("d2", RowState::Pending);
    s.shim_binding_insert(full.clone()).await.unwrap();
    s.shim_binding_insert(fleet.clone()).await.unwrap();
    assert_eq!(s.shim_bindings_load().await.unwrap(), [full.clone(), fleet]);
    assert!(
        s.shim_binding_insert(full).await.is_err(),
        "a digest is unique"
    );
}

#[tokio::test]
async fn retain_deletes_the_rows_of_other_shims() {
    let det = Det::new();
    let s = store(&det).await;
    for (digest, shim) in [("d1", "a"), ("d2", "b"), ("d3", "c"), ("d4", "a")] {
        let mut r = row(digest, RowState::Pending, None);
        r.shim_id = shim.to_owned();
        s.shim_binding_insert(r).await.unwrap();
    }
    assert_eq!(
        s.shim_bindings_retain(vec!["a".to_owned()]).await.unwrap(),
        2
    );
    let left: Vec<_> = s
        .shim_bindings_load()
        .await
        .unwrap()
        .into_iter()
        .map(|r| r.digest)
        .collect();
    assert_eq!(left, ["d1", "d4"]);
    assert_eq!(s.shim_bindings_retain(Vec::new()).await.unwrap(), 2);
    assert!(s.shim_bindings_load().await.unwrap().is_empty());
}

#[tokio::test]
async fn set_state_changes_one_row() {
    let det = Det::new();
    let s = store(&det).await;
    s.shim_binding_insert(row("d1", RowState::Active, Some("n1")))
        .await
        .unwrap();
    assert!(
        s.shim_binding_set_state("d1".to_owned(), RowState::Suspended)
            .await
            .unwrap()
    );
    assert!(
        !s.shim_binding_set_state("zz".to_owned(), RowState::Ended)
            .await
            .unwrap()
    );
    let rows = s.shim_bindings_load().await.unwrap();
    assert_eq!(rows[0].state, RowState::Suspended);
}

fn envelope(det: &Det, id: &str) -> fleetdeck_proto::CommandEnvelope {
    fleetdeck_proto::CommandEnvelope {
        authority: None,
        command_id: CommandId::from(id),
        target_incarnation: None,
        retry_until: i64::try_from(det.now_ms()).unwrap() + RETRY_WINDOW_MS,
        origin: CommandOrigin::Board,
        command: SessionCommand::Prompt {
            session_id: "s1".into(),
            input: UserInput::new(id.to_owned()),
        },
    }
}

#[tokio::test]
async fn a_decision_ends_a_row_once_and_returns_the_old_row() {
    let det = Det::new();
    let s = store(&det).await;
    let active = row("d1", RowState::Active, Some("n1"));
    s.shim_binding_insert(active.clone()).await.unwrap();
    s.shim_binding_insert(row("d2", RowState::Suspended, Some("n1")))
        .await
        .unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&seen);
    s.decide(envelope(&det, "c1"), Caller::local(), move |d| {
        let mut out = sink.lock().unwrap();
        out.push(d.shim_binding_end("d1", "revoked")?);
        out.push(d.shim_binding_end("d1", "revoked")?);
        out.push(d.shim_binding_end("zz", "revoked")?);
        out.push(d.shim_binding_end("d2", "revoked")?);
        Ok(CommandResult {
            session_id: Some("s1".into()),
            input_id: None,
            seq: None,
        })
    })
    .await
    .unwrap();
    assert_eq!(*seen.lock().unwrap(), [Some(active), None, None, None]);
    let rows = s.shim_bindings_load().await.unwrap();
    assert_eq!(rows[0].state, RowState::Ended);
    assert_eq!(rows[1].state, RowState::Suspended);
}

// ---- the output transaction ----

/// Records the calls of the store.
#[derive(Default)]
struct Recorder(Mutex<Vec<String>>);

impl Recorder {
    fn calls(&self) -> Vec<String> {
        self.0.lock().unwrap().clone()
    }
}

fn names(ts: &[Transition]) -> String {
    ts.iter()
        .map(|t| match t {
            Transition::Bound { digest, .. } => format!("bound:{digest}"),
            Transition::Ended { digest, .. } => format!("ended:{digest}"),
        })
        .collect::<Vec<_>>()
        .join(",")
}

impl BindingSink for Recorder {
    fn invalidate(&self, ts: &[Transition]) {
        self.0
            .lock()
            .unwrap()
            .push(format!("invalidate {}", names(ts)));
    }

    fn activate(&self, ts: &[Transition]) {
        self.0
            .lock()
            .unwrap()
            .push(format!("activate {}", names(ts)));
    }
}

async fn commit(
    s: &Store,
    cursor: u64,
    events: Vec<NewEvent>,
    identities: Vec<IdentitySignal>,
    exited: bool,
) -> OutputCommit {
    s.shim_commit_output_with(
        "a".to_owned(),
        1,
        cursor,
        events,
        OutputMeta {
            identities,
            exited,
            ..OutputMeta::default()
        },
    )
    .await
    .unwrap()
}

async fn kinds(s: &Store) -> Vec<&'static str> {
    let ReadResult::Events { events, .. } = s.read_after(0, 1000).await.unwrap() else {
        panic!("no events");
    };
    events
        .iter()
        .map(|e| match &e.event {
            EventBody::Known(Event::SessionStarted { .. }) => "started",
            EventBody::Known(Event::ModeChanged { .. }) => "mode",
            EventBody::Known(Event::CoordinatorBound { .. }) => "bound",
            EventBody::Known(Event::CoordinatorEnded { .. }) => "ended",
            _ => "other",
        })
        .collect()
}

async fn states(s: &Store) -> Vec<(RowState, Option<String>, u32)> {
    s.shim_bindings_load()
        .await
        .unwrap()
        .into_iter()
        .map(|r| (r.state, r.native_id, r.generation))
        .collect()
}

async fn with_rows(det: &Det) -> (Store, Arc<Recorder>) {
    let s = running(det).await;
    let sink = Arc::new(Recorder::default());
    s.set_binding_sink(Arc::clone(&sink) as Arc<dyn BindingSink>);
    (s, sink)
}

#[tokio::test]
async fn session_started_binds_the_pending_row_in_the_same_commit() {
    let det = Det::new();
    let (s, sink) = with_rows(&det).await;
    s.shim_binding_insert(row("d1", RowState::Pending, None))
        .await
        .unwrap();
    s.shim_binding_insert(fleet_only("d2", RowState::Pending))
        .await
        .unwrap();
    let out = commit(
        &s,
        1,
        vec![started(Some("n1")), mode("m")],
        Vec::new(),
        false,
    )
    .await;
    let OutputCommit::Committed(events) = out else {
        panic!("{out:?}");
    };
    // The lifecycle event follows the batch events; the fleet-only row has none.
    assert_eq!(events.len(), 3);
    assert_eq!(kinds(&s).await, ["started", "mode", "bound"]);
    assert_eq!(
        states(&s).await,
        [
            (RowState::Active, Some("n1".to_owned()), 1),
            (RowState::Active, Some("n1".to_owned()), 1)
        ]
    );
    assert_eq!(sink.calls(), ["activate bound:d1,bound:d2"]);
}

#[tokio::test]
async fn activate_runs_only_after_a_successful_commit() {
    let det = Det::new();
    let (s, sink) = with_rows(&det).await;
    s.shim_binding_insert(row("d1", RowState::Pending, None))
        .await
        .unwrap();
    // A batch without a transition calls nothing.
    commit(&s, 1, vec![mode("m")], Vec::new(), false).await;
    assert!(sink.calls().is_empty());
    // A stale generation commits nothing and activates nothing.
    let stale = s
        .shim_commit_output_with(
            "a".to_owned(),
            9,
            2,
            vec![started(Some("n1"))],
            OutputMeta::default(),
        )
        .await
        .unwrap();
    assert_eq!(stale, OutputCommit::StaleGeneration);
    assert!(sink.calls().is_empty());
    commit(&s, 2, Vec::new(), vec![signal("n1", 0)], false).await;
    assert_eq!(sink.calls(), ["activate bound:d1"]);
}

#[tokio::test]
async fn without_a_sink_the_rows_and_events_are_still_written() {
    let det = Det::new();
    let s = running(&det).await;
    s.shim_binding_insert(row("d1", RowState::Pending, None))
        .await
        .unwrap();
    commit(&s, 1, vec![started(Some("n1"))], Vec::new(), false).await;
    assert_eq!(kinds(&s).await, ["started", "bound"]);
    assert_eq!(states(&s).await[0].0, RowState::Active);
}

#[tokio::test]
async fn an_exit_commit_ends_the_open_rows_with_process_exit() {
    let det = Det::new();
    let (s, sink) = with_rows(&det).await;
    s.shim_binding_insert(row("d1", RowState::Active, Some("n1")))
        .await
        .unwrap();
    s.shim_binding_insert(row("d2", RowState::Pending, None))
        .await
        .unwrap();
    s.shim_binding_insert(row("d3", RowState::Suspended, Some("n1")))
        .await
        .unwrap();
    commit(&s, 1, vec![mode("m")], Vec::new(), true).await;
    assert_eq!(kinds(&s).await, ["mode", "ended", "ended"]);
    let rows = states(&s).await;
    assert_eq!(rows[0].0, RowState::Ended);
    assert_eq!(rows[1].0, RowState::Ended);
    assert_eq!(rows[2].0, RowState::Suspended);
    assert_eq!(sink.calls(), ["invalidate ended:d1,ended:d2"]);
}

#[tokio::test]
async fn a_failed_transaction_leaves_memory_invalidated_and_writes_nothing() {
    let det = Det::new();
    let (s, sink) = with_rows(&det).await;
    s.shim_binding_insert(row("d1", RowState::Active, Some("n1")))
        .await
        .unwrap();
    call(&s.inner.db, |conn| {
        conn.execute_batch(
            "CREATE TRIGGER fail_bindings BEFORE UPDATE ON shim_bindings
             BEGIN SELECT RAISE(ABORT, 'disk full'); END;",
        )?;
        Ok(())
    })
    .await
    .unwrap();
    let failed = s
        .shim_commit_output_with(
            "a".to_owned(),
            1,
            1,
            vec![mode("m")],
            OutputMeta {
                identities: vec![signal("n2", 1)],
                ..OutputMeta::default()
            },
        )
        .await;
    assert!(failed.is_err());
    assert_eq!(sink.calls(), ["invalidate ended:d1"]);
    assert!(kinds(&s).await.is_empty(), "no event committed");
    assert_eq!(states(&s).await[0].0, RowState::Active);
    assert_eq!(s.shim("a".to_owned()).await.unwrap().unwrap().cursor, 0);
}

#[tokio::test]
async fn the_memory_sink_binds_a_pending_binding_and_never_revives_a_terminal_one() {
    let bindings = crate::coordinator::Bindings::default();
    let binding = |id: &str, state: BindingState| Binding {
        binding_id: id.to_owned(),
        grant_id: "g".to_owned(),
        session_id: "s1".into(),
        provider_instance: "claude".to_owned(),
        native_id: None,
        generation: 0,
        classes: std::collections::BTreeSet::new(),
        scope: fleetdeck_proto::command::GrantScope::default(),
        state,
        control_skill: String::new(),
        kind: fleetdeck_proto::command::GrantKind::Coordinator,
    };
    bindings.insert("d1".to_owned(), binding("b1", BindingState::Pending));
    bindings.insert("d2".to_owned(), binding("b2", BindingState::Revoked));
    bindings.insert("d3".to_owned(), binding("b3", BindingState::Active));
    let sink = MemorySink(bindings.clone());
    sink.activate(&[bound("d1", "n1"), bound("d2", "n1")]);
    let d1 = bindings.get_digest("d1").unwrap();
    assert_eq!(
        (d1.state, d1.native_id.as_deref(), d1.generation),
        (BindingState::Active, Some("n1"), 1)
    );
    assert_eq!(
        bindings.get_digest("d2").unwrap().state,
        BindingState::Revoked
    );
    sink.invalidate(&[ended("d3", REASON_PROCESS_EXIT), bound("d1", "n1")]);
    assert_eq!(
        bindings.get_digest("d3").unwrap().state,
        BindingState::Revoked
    );
    assert_eq!(
        bindings.get_digest("d1").unwrap().state,
        BindingState::Active
    );
}

#[test]
fn a_v25_database_migrates_to_v26_with_the_bindings_table() {
    let mut conn = rusqlite::Connection::open_in_memory().unwrap();
    let migrations = crate::store::schema::migrations();
    migrations.to_version(&mut conn, 25).unwrap();
    let exists = |conn: &rusqlite::Connection| -> bool {
        conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE name = 'shim_bindings')",
            [],
            |r| r.get(0),
        )
        .unwrap()
    };
    assert!(!exists(&conn));
    migrations.to_version(&mut conn, 26).unwrap();
    assert!(exists(&conn));
    conn.execute(
        "INSERT INTO shim_bindings (digest, shim_id, session_id, binding_id, grant_id,
             provider_instance, generation, classes, scope, state, control_skill, created_at)
         VALUES ('d', 'a', 's', 'b', '', 'claude', 0, '[]', '{}', 'pending', '{}', 1)",
        [],
    )
    .unwrap();
    assert!(
        conn.execute(
            "UPDATE shim_bindings SET state = 'bogus' WHERE digest = 'd'",
            []
        )
        .is_err(),
        "the state check holds"
    );
}
