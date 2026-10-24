//! The launch facts and the binding of workflow agents (B12 §4.4).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;

use fleetdeck_proto::event::{Event, ProviderKind, WorkflowAgent, WorkflowUpdate};
use fleetdeck_testkit::Det;

use super::super::tests::{new_session, store};
use super::super::{NewEvent, ProcessIncarnation};
use super::*;
use crate::history::subagent::LaunchFacts;

const S1: &str = "s1";
const NATIVE: &str = "native-1";
const RUN: &str = "wf_0918c8f1-e62";

fn facts(home: &str) -> LaunchFacts {
    LaunchFacts {
        provider_home: PathBuf::from(home),
        cwd: "/w".to_owned(),
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

fn agent(index: u32, id: Option<&str>) -> WorkflowAgent {
    WorkflowAgent {
        index,
        label: format!("agent {index}"),
        state: "running".to_owned(),
        agent_id: id.map(str::to_owned),
        ..WorkflowAgent::default()
    }
}

fn update(task: &str, run_id: Option<&str>, status: &str, agents: Vec<WorkflowAgent>) -> NewEvent {
    Event::WorkflowUpdated(Box::new(WorkflowUpdate {
        task_id: task.to_owned(),
        node_id: None,
        name: None,
        run_id: run_id.map(str::to_owned),
        status: status.to_owned(),
        untracked: false,
        phases: Vec::new(),
        running_agents: 0,
        agents_total: u32::try_from(agents.len()).unwrap(),
        agents,
    }))
    .into()
}

fn process(name: &str) -> ProcessIncarnation {
    name.to_owned().into()
}

/// A session with a process that has launch facts and a native id.
async fn rig() -> Store {
    let det = Det::new();
    let s = store(&det).await;
    s.create_session(new_session(S1)).await.unwrap();
    s.process_launched(S1.into(), process("p1"), None, Some(facts("/h/.claude")))
        .await
        .unwrap();
    s.append(S1.into(), vec![started(Some(NATIVE))])
        .await
        .unwrap();
    s
}

async fn rows(s: &Store, table: &'static str) -> i64 {
    call(&s.inner.db, move |conn| {
        Ok(conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))?)
    })
    .await
    .unwrap()
}

async fn log(s: &Store, agent: &str) -> Option<WorkflowAgentLogRow> {
    s.workflow_agent_log(S1.into(), agent.to_owned())
        .await
        .unwrap()
}

// ------------------------------------------------------------ launch facts

#[tokio::test]
async fn process_launched_writes_the_process_and_its_facts_in_one_transaction() {
    let det = Det::new();
    let s = store(&det).await;
    s.create_session(new_session(S1)).await.unwrap();
    s.process_launched(S1.into(), process("p1"), Some(7), Some(facts("/h/.claude")))
        .await
        .unwrap();
    assert_eq!(rows(&s, "processes").await, 1);
    let got = s
        .launch_facts_of(S1.into(), process("p1"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(got, facts("/h/.claude"));

    // A facts row of the incarnation exists already: the call fails and the
    // process row rolls back with it.
    call(&s.inner.db, |conn| {
        conn.execute(
            "INSERT INTO process_launch_facts
               (process_incarnation, session_id, provider_home, cwd, created_at)
             VALUES ('p2', 's1', '/h/.claude', '/w', 1)",
            [],
        )?;
        Ok(())
    })
    .await
    .unwrap();
    let failed = s
        .process_launched(S1.into(), process("p2"), None, Some(facts("/h/.claude")))
        .await;
    assert!(failed.is_err());
    assert_eq!(rows(&s, "processes").await, 1, "no processes row for p2");
}

#[tokio::test]
async fn process_launched_without_facts_writes_only_the_process() {
    let det = Det::new();
    let s = store(&det).await;
    s.create_session(new_session(S1)).await.unwrap();
    s.process_launched(S1.into(), process("p1"), None, None)
        .await
        .unwrap();
    // A home that is not absolute gives no facts row either.
    s.process_launched(S1.into(), process("p2"), None, Some(facts("relative/home")))
        .await
        .unwrap();
    assert_eq!(rows(&s, "processes").await, 2);
    assert_eq!(rows(&s, "process_launch_facts").await, 0);
}

#[tokio::test]
async fn launch_facts_of_reads_no_facts_of_another_session_or_process() {
    let det = Det::new();
    let s = store(&det).await;
    s.create_session(new_session(S1)).await.unwrap();
    s.create_session(new_session("s2")).await.unwrap();
    s.process_launched(S1.into(), process("p1"), None, Some(facts("/h/.claude")))
        .await
        .unwrap();
    assert!(
        s.launch_facts_of("s2".into(), process("p1"))
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        s.launch_facts_of(S1.into(), process("p9"))
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        s.launch_facts_of(S1.into(), process("p1"))
            .await
            .unwrap()
            .is_some()
    );
}

// ----------------------------------------------------------------- binding

#[tokio::test]
async fn the_run_id_binds_every_agent_that_came_before_it() {
    let s = rig().await;
    s.append(
        S1.into(),
        vec![
            update(
                "t1",
                None,
                "running",
                vec![agent(1, Some("aaa1")), agent(2, None)],
            ),
            update("t1", None, "running", vec![agent(3, Some("aaa3"))]),
        ],
    )
    .await
    .unwrap();
    assert_eq!(rows(&s, "workflow_agent_logs").await, 0);
    // The event with the run id carries no agent: the stored agents bind.
    s.append(
        S1.into(),
        vec![update("t1", Some(RUN), "running", Vec::new())],
    )
    .await
    .unwrap();
    let a = log(&s, "aaa1").await.unwrap();
    assert_eq!(a.run_id, RUN);
    assert_eq!(a.native_id, NATIVE);
    assert_eq!(a.cwd, "/w");
    assert_eq!(a.provider_home, "/h/.claude");
    assert!(log(&s, "aaa3").await.is_some());
    assert_eq!(rows(&s, "workflow_agent_logs").await, 2);
}

#[tokio::test]
async fn a_later_agent_is_bound_by_its_own_event() {
    let s = rig().await;
    s.append(
        S1.into(),
        vec![
            update("t1", Some(RUN), "running", vec![agent(1, Some("aaa1"))]),
            update("t1", Some(RUN), "running", vec![agent(2, Some("aaa2"))]),
        ],
    )
    .await
    .unwrap();
    assert!(log(&s, "aaa1").await.is_some());
    assert!(log(&s, "aaa2").await.is_some());
    // An agent of the run that an earlier event stored without an id is not
    // scanned again: only the agents of the event bind.
    s.append(
        S1.into(),
        vec![
            update("t1", Some(RUN), "running", vec![agent(3, None)]),
            update("t1", None, "running", vec![agent(4, Some("aaa4"))]),
        ],
    )
    .await
    .unwrap();
    assert!(log(&s, "aaa4").await.is_some());
    assert_eq!(rows(&s, "workflow_agent_logs").await, 3);
}

#[tokio::test]
async fn no_row_without_run_id_agent_id_native_id_process_or_launch_facts() {
    let agents = || vec![agent(1, Some("aaa1"))];
    // No run id; a run id that is not valid.
    let s = rig().await;
    s.append(S1.into(), vec![update("t1", None, "running", agents())])
        .await
        .unwrap();
    s.append(
        S1.into(),
        vec![update("t2", Some("wf_../x"), "running", agents())],
    )
    .await
    .unwrap();
    s.append(
        S1.into(),
        vec![update("t3", Some("wf_UPPER"), "running", agents())],
    )
    .await
    .unwrap();
    // A bad agent id.
    s.append(
        S1.into(),
        vec![update(
            "t4",
            Some(RUN),
            "running",
            vec![
                agent(1, Some("a.b")),
                agent(2, Some("A1")),
                agent(3, Some("")),
            ],
        )],
    )
    .await
    .unwrap();
    assert_eq!(rows(&s, "workflow_agent_logs").await, 0);

    // No native id.
    let det = Det::new();
    let s = store(&det).await;
    s.create_session(new_session(S1)).await.unwrap();
    s.process_launched(S1.into(), process("p1"), None, Some(facts("/h/.claude")))
        .await
        .unwrap();
    s.append(
        S1.into(),
        vec![update("t1", Some(RUN), "running", agents())],
    )
    .await
    .unwrap();
    assert_eq!(rows(&s, "workflow_agent_logs").await, 0);

    // No process row; a process without facts.
    let det = Det::new();
    let s = store(&det).await;
    s.create_session(new_session(S1)).await.unwrap();
    s.append(
        S1.into(),
        vec![
            started(Some(NATIVE)),
            update("t1", Some(RUN), "running", agents()),
        ],
    )
    .await
    .unwrap();
    assert_eq!(rows(&s, "workflow_agent_logs").await, 0);
    s.process_launched(S1.into(), process("p1"), None, None)
        .await
        .unwrap();
    s.append(
        S1.into(),
        vec![update("t5", Some(RUN), "running", agents())],
    )
    .await
    .unwrap();
    assert_eq!(rows(&s, "workflow_agent_logs").await, 0);
}

#[tokio::test]
async fn the_first_binding_of_an_agent_stays() {
    let s = rig().await;
    s.append(
        S1.into(),
        vec![update(
            "t1",
            Some(RUN),
            "running",
            vec![agent(1, Some("aaa1"))],
        )],
    )
    .await
    .unwrap();
    s.append(
        S1.into(),
        vec![update(
            "t2",
            Some("wf_other"),
            "running",
            vec![agent(1, Some("aaa1"))],
        )],
    )
    .await
    .unwrap();
    assert_eq!(log(&s, "aaa1").await.unwrap().run_id, RUN);
    assert_eq!(rows(&s, "workflow_agent_logs").await, 1);
}

#[tokio::test]
async fn the_binding_uses_the_newest_process_of_the_session() {
    let s = rig().await;
    s.append(
        S1.into(),
        vec![update(
            "t1",
            Some(RUN),
            "running",
            vec![agent(1, Some("aaa1"))],
        )],
    )
    .await
    .unwrap();
    assert_eq!(log(&s, "aaa1").await.unwrap().provider_home, "/h/.claude");
    // Time must pass for `started_at`; the rowid breaks a tie anyway.
    s.process_launched(
        S1.into(),
        process("p2"),
        None,
        Some(facts("/other/.claude")),
    )
    .await
    .unwrap();
    s.append(
        S1.into(),
        vec![update(
            "t1",
            Some(RUN),
            "running",
            vec![agent(2, Some("aaa2"))],
        )],
    )
    .await
    .unwrap();
    assert_eq!(
        log(&s, "aaa2").await.unwrap().provider_home,
        "/other/.claude"
    );
    assert_eq!(log(&s, "aaa1").await.unwrap().provider_home, "/h/.claude");
}

/// A run with three agents: start, agents, run id and a final status.
fn complete_run(n: usize) -> Vec<NewEvent> {
    let task = format!("t{n:02}");
    let ids = |k: usize| format!("a{n:02}x{k}");
    vec![
        update(&task, None, "running", Vec::new()),
        update(
            &task,
            None,
            "running",
            (1..=3_u32)
                .map(|k| agent(k, Some(&ids(k as usize))))
                .collect(),
        ),
        update(
            &task,
            Some(&format!("wf_run-{n:02}")),
            "running",
            Vec::new(),
        ),
        update(&task, None, "completed", Vec::new()),
    ]
}

#[tokio::test]
async fn bindings_survive_33_ended_runs_without_an_observer() {
    let s = rig().await;
    let events: Vec<NewEvent> = (0..34).flat_map(complete_run).collect();
    s.append(S1.into(), events).await.unwrap();
    assert_eq!(rows(&s, "workflow_agent_logs").await, 34 * 3);
    for n in 0..34_usize {
        for k in 1..=3 {
            let row = log(&s, &format!("a{n:02}x{k}")).await;
            assert_eq!(row.unwrap().run_id, format!("wf_run-{n:02}"));
        }
    }
    // R6: 32 ended runs stay in the projection; the first run is gone.
    assert_eq!(rows(&s, "workflow_runs").await, 32);
    let first: i64 = call(&s.inner.db, |conn| {
        Ok(conn.query_row(
            "SELECT (SELECT COUNT(*) FROM workflow_runs WHERE task_id = 't00')
                  + (SELECT COUNT(*) FROM workflow_agents WHERE task_id = 't00')",
            [],
            |r| r.get(0),
        )?)
    })
    .await
    .unwrap();
    assert_eq!(first, 0);
}

#[tokio::test]
async fn a_run_removed_by_r6_in_the_same_event_is_bound_first() {
    let s = rig().await;
    // 32 ended runs with later `run_order`; the first run that ends is the
    // 33rd ended one and has the smallest `run_order`.
    let mut early = vec![update(
        "early",
        None,
        "running",
        vec![agent(1, Some("early1"))],
    )];
    early.extend((0..32).flat_map(|n| {
        let task = format!("late{n:02}");
        vec![
            update(
                &task,
                Some(&format!("wf_late-{n:02}")),
                "running",
                Vec::new(),
            ),
            update(&task, None, "completed", Vec::new()),
        ]
    }));
    s.append(S1.into(), early).await.unwrap();
    assert_eq!(rows(&s, "workflow_runs").await, 33, "one running, 32 ended");
    // One event sets the run id and the final status of `early`.
    s.append(
        S1.into(),
        vec![update("early", Some("wf_early"), "completed", Vec::new())],
    )
    .await
    .unwrap();
    // R6 removed `early` (smallest `run_order`) after the binding.
    let left: i64 = call(&s.inner.db, |conn| {
        Ok(conn.query_row(
            "SELECT COUNT(*) FROM workflow_runs WHERE task_id = 'early'",
            [],
            |r| r.get(0),
        )?)
    })
    .await
    .unwrap();
    assert_eq!(left, 0);
    assert_eq!(log(&s, "early1").await.unwrap().run_id, "wf_early");
}
