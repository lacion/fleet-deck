//! The workflow projection in the append transaction (B12 §4.3a, R1 to R6).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use fleetdeck_proto::event::{
    BackgroundTask, Event, SessionState, WorkflowAgent, WorkflowPhase, WorkflowUpdate,
};
use fleetdeck_testkit::Det;

use super::super::tests::{new_session, store};
use super::super::{NewEvent, Store, call};

const S1: &str = "s1";

async fn rig() -> Store {
    let det = Det::new();
    let s = store(&det).await;
    s.create_session(new_session(S1)).await.unwrap();
    s
}

fn agent(index: u32, state: &str) -> WorkflowAgent {
    WorkflowAgent {
        index,
        label: format!("agent {index}"),
        state: state.to_owned(),
        ..WorkflowAgent::default()
    }
}

fn update(task: &str, status: &str, agents: Vec<WorkflowAgent>) -> WorkflowUpdate {
    let running = agents.iter().filter(|a| a.state == "running").count();
    WorkflowUpdate {
        task_id: task.to_owned(),
        node_id: None,
        name: None,
        run_id: None,
        status: status.to_owned(),
        untracked: false,
        phases: Vec::new(),
        running_agents: u32::try_from(running).unwrap(),
        agents_total: u32::try_from(agents.len()).unwrap(),
        agents,
    }
}

fn event(update: WorkflowUpdate) -> NewEvent {
    Event::WorkflowUpdated(Box::new(update)).into()
}

fn listed(tasks: &[&str]) -> NewEvent {
    Event::BackgroundUpdated {
        tasks: tasks
            .iter()
            .map(|t| BackgroundTask {
                task_id: (*t).to_owned(),
                task_kind: "local_workflow".to_owned(),
            })
            .collect(),
    }
    .into()
}

/// (task id, status, running agents) in `run_order`.
async fn runs(s: &Store) -> Vec<(String, String, i64)> {
    call(&s.inner.db, |conn| {
        let mut stmt = conn.prepare(
            "SELECT task_id, status, running_agents FROM workflow_runs
             WHERE session_id = 's1' ORDER BY run_order",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    })
    .await
    .unwrap()
}

/// (index, state column, state in the entry JSON) of a run.
async fn agents(s: &Store, task: &'static str) -> Vec<(i64, String, String)> {
    call(&s.inner.db, move |conn| {
        let mut stmt = conn.prepare(
            "SELECT idx, state, json_extract(entry, '$.state') FROM workflow_agents
             WHERE session_id = 's1' AND task_id = ?1 ORDER BY idx",
        )?;
        let rows = stmt.query_map([task], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    })
    .await
    .unwrap()
}

async fn count(s: &Store, table: &'static str, task: &'static str) -> i64 {
    call(&s.inner.db, move |conn| {
        Ok(conn.query_row(
            &format!("SELECT COUNT(*) FROM {table} WHERE session_id = 's1' AND task_id = ?1"),
            [task],
            |r| r.get(0),
        )?)
    })
    .await
    .unwrap()
}

fn state_of(rows: &[(i64, String, String)], index: i64) -> &str {
    &rows.iter().find(|r| r.0 == index).unwrap().1
}

#[tokio::test]
async fn the_projection_merges_changes_by_index() {
    let s = rig().await;
    let mut first = update(
        "t1",
        "running",
        vec![agent(1, "queued"), agent(2, "running")],
    );
    first.name = Some("review".to_owned());
    first.node_id = Some("n1".to_owned());
    first.phases = vec![WorkflowPhase {
        index: 1,
        title: "Alpha".to_owned(),
    }];
    let mut second = update("t1", "running", vec![agent(1, "running")]);
    second.phases = vec![
        WorkflowPhase {
            index: 1,
            title: "Alpha 2".to_owned(),
        },
        WorkflowPhase {
            index: 2,
            title: "Beta".to_owned(),
        },
    ];
    // Absolute fields are replaced; `name` and `node_id` stay when absent.
    second.running_agents = 2;
    second.agents_total = 2;
    s.append(S1.into(), vec![event(first), event(second)])
        .await
        .unwrap();
    assert_eq!(runs(&s).await, [("t1".into(), "running".into(), 2)]);
    let rows = agents(&s, "t1").await;
    assert_eq!(rows.len(), 2);
    assert_eq!(state_of(&rows, 1), "running");
    assert_eq!(state_of(&rows, 2), "running");
    assert_eq!(count(&s, "workflow_phases", "t1").await, 2);
    let (name, node, total, title): (String, String, i64, String) = call(&s.inner.db, |conn| {
        Ok(conn.query_row(
            "SELECT r.name, r.node_id, r.agents_total, p.title
                 FROM workflow_runs r JOIN workflow_phases p
                   ON p.session_id = r.session_id AND p.task_id = r.task_id
                 WHERE p.idx = 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )?)
    })
    .await
    .unwrap();
    assert_eq!(
        (name.as_str(), node.as_str(), total, title.as_str()),
        ("review", "n1", 2, "Alpha 2")
    );
}

#[tokio::test]
async fn a_final_status_stops_queued_and_running_agents() {
    let s = rig().await;
    s.append(
        S1.into(),
        vec![
            event(update(
                "t1",
                "running",
                vec![agent(1, "queued"), agent(2, "running"), agent(3, "done")],
            )),
            event(update("t1", "completed", Vec::new())),
        ],
    )
    .await
    .unwrap();
    assert_eq!(runs(&s).await, [("t1".into(), "completed".into(), 0)]);
    let rows = agents(&s, "t1").await;
    assert_eq!(state_of(&rows, 1), "stopped");
    assert_eq!(state_of(&rows, 2), "stopped");
    assert_eq!(state_of(&rows, 3), "done");
    // The entry follows the column.
    assert_eq!(rows[0].2, "stopped");
    assert_eq!(rows[1].2, "stopped");
    assert_eq!(rows[2].2, "done");
}

#[tokio::test]
async fn background_updated_stops_a_missing_run_while_another_runs() {
    let s = rig().await;
    s.append(
        S1.into(),
        vec![
            event(update("a", "running", vec![agent(1, "running")])),
            event(update("b", "running", vec![agent(1, "running")])),
            listed(&["b"]),
        ],
    )
    .await
    .unwrap();
    assert_eq!(
        runs(&s).await,
        [
            ("a".into(), "stopped".into(), 0),
            ("b".into(), "running".into(), 1)
        ]
    );
    assert_eq!(state_of(&agents(&s, "a").await, 1), "stopped");
    assert_eq!(state_of(&agents(&s, "b").await, 1), "running");
}

#[tokio::test]
async fn the_end_of_the_session_stops_running_runs() {
    let s = rig().await;
    s.append(
        S1.into(),
        vec![
            event(update("a", "running", vec![agent(1, "running")])),
            Event::SessionEnded {
                reason: "stopped".to_owned(),
                exit_code: None,
            }
            .into(),
        ],
    )
    .await
    .unwrap();
    assert_eq!(runs(&s).await, [("a".into(), "stopped".into(), 0)]);
    assert_eq!(state_of(&agents(&s, "a").await, 1), "stopped");

    // `session.state` with `lost` stops the runs too; `running` does not.
    let s = rig().await;
    s.append(
        S1.into(),
        vec![
            event(update("a", "running", vec![agent(1, "running")])),
            Event::SessionStateChanged {
                state: SessionState::Running,
                detail: None,
            }
            .into(),
        ],
    )
    .await
    .unwrap();
    assert_eq!(runs(&s).await, [("a".into(), "running".into(), 1)]);
    s.append(
        S1.into(),
        vec![
            Event::SessionStateChanged {
                state: SessionState::Lost,
                detail: None,
            }
            .into(),
        ],
    )
    .await
    .unwrap();
    assert_eq!(runs(&s).await, [("a".into(), "stopped".into(), 0)]);
}

#[tokio::test]
async fn a_running_event_revives_a_run_stopped_by_an_early_list() {
    let s = rig().await;
    s.append(
        S1.into(),
        vec![
            event(update("a", "running", vec![agent(1, "running")])),
            listed(&[]),
        ],
    )
    .await
    .unwrap();
    assert_eq!(runs(&s).await, [("a".into(), "stopped".into(), 0)]);
    s.append(
        S1.into(),
        vec![event(update("a", "running", vec![agent(1, "done")]))],
    )
    .await
    .unwrap();
    // R5: the run runs again; its agents change with their own events.
    assert_eq!(runs(&s).await, [("a".into(), "running".into(), 0)]);
    assert_eq!(state_of(&agents(&s, "a").await, 1), "done");
}

#[tokio::test]
async fn at_most_32_ended_runs_and_no_running_run_is_removed() {
    let s = rig().await;
    // The first run runs for the whole test; it has the smallest `run_order`.
    let mut events = vec![event(update("keep", "running", vec![agent(1, "running")]))];
    for n in 0..34 {
        let task = format!("e{n:02}");
        let mut first = update(&task, "running", vec![agent(1, "running")]);
        first.phases = vec![WorkflowPhase {
            index: 1,
            title: "P".to_owned(),
        }];
        events.push(event(first));
        events.push(event(update(&task, "completed", Vec::new())));
    }
    s.append(S1.into(), events).await.unwrap();
    let rows = runs(&s).await;
    let ended: Vec<_> = rows.iter().filter(|r| r.1 != "running").collect();
    assert_eq!(ended.len(), 32, "{rows:?}");
    assert!(rows.iter().any(|r| r.0 == "keep" && r.1 == "running"));
    // The two oldest ended runs went, with their phases and agents.
    for gone in ["e00", "e01"] {
        assert!(!rows.iter().any(|r| r.0 == gone), "{gone}");
    }
    assert!(rows.iter().any(|r| r.0 == "e02"));
    assert_eq!(count(&s, "workflow_phases", "e00").await, 0);
    assert_eq!(count(&s, "workflow_agents", "e01").await, 0);
    assert_eq!(count(&s, "workflow_agents", "e02").await, 1);
    assert_eq!(count(&s, "workflow_agents", "keep").await, 1);
}

#[tokio::test]
async fn three_thousand_events_keep_every_agent() {
    let s = rig().await;
    for state in ["queued", "running", "done"] {
        let events: Vec<NewEvent> = (1..=1000)
            .map(|i| event(update("big", "running", vec![agent(i, state)])))
            .collect();
        s.append(S1.into(), events).await.unwrap();
    }
    let rows = agents(&s, "big").await;
    assert_eq!(rows.len(), 1000);
    assert!(rows.iter().all(|r| r.1 == "done" && r.2 == "done"));
    // A new index beyond the 1,000 agents of a run is ignored.
    s.append(
        S1.into(),
        vec![event(update(
            "big",
            "running",
            vec![agent(1001, "running")],
        ))],
    )
    .await
    .unwrap();
    assert_eq!(count(&s, "workflow_agents", "big").await, 1000);
}
