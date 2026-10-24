//! Workflow agents and the station launch (B12 §4.4): the launch facts that
//! commit with the process row, the binding after a restart, and the
//! interrupt between turns. The rig of `shim_resume_rig.rs`: its shim host
//! has no executable, so a real shim launch is not in reach here.

#![cfg(target_os = "linux")]
#![allow(clippy::unwrap_used)]

use fleetdeck_proto::command::SessionCommand;
use fleetdeck_proto::event::{WorkflowAgent, WorkflowUpdate};
use fleetdeck_proto::{Event, RejectCode, SessionId};

use crate::manager::shim_resume_rig::*;
use crate::store::NewEvent;

const RUN: &str = "wf_0918c8f1-e62";

fn agent(index: u32, id: &str) -> WorkflowAgent {
    WorkflowAgent {
        index,
        label: format!("agent {index}"),
        state: "running".to_owned(),
        agent_id: Some(id.to_owned()),
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

async fn append(rig: &Rig, session: &SessionId, events: Vec<NewEvent>) {
    rig.store().append(session.clone(), events).await.unwrap();
}

/// The facts that the launch of the session's live process stored.
async fn launch_facts(
    rig: &Rig,
    session: &SessionId,
) -> Option<crate::history::subagent::LaunchFacts> {
    let process = rig.store().live_process(session.clone()).await.unwrap()?;
    rig.store()
        .launch_facts_of(session.clone(), process)
        .await
        .unwrap()
}

#[tokio::test]
async fn a_direct_launch_commits_its_launch_facts_with_the_process() {
    let mut rig = Rig::direct(None).await;
    let session = rig.start("claude").await;
    let facts = launch_facts(&rig, &session)
        .await
        .expect("the launch facts");
    assert!(facts.provider_home.is_absolute());
    assert_eq!(facts.cwd, rig.workspace());
}

#[tokio::test]
async fn a_restart_right_after_the_launch_binds_the_later_agents() {
    let mut rig = Rig::direct(None).await;
    let session = rig.start("claude").await;
    let before = launch_facts(&rig, &session)
        .await
        .expect("the launch facts");
    let native = rig.native(&session).await;
    // The station restarts before any workflow event; the facts stay in the
    // database and the event transaction binds the agents of both phases.
    rig.as_shim_station().await;
    append(
        &rig,
        &session,
        vec![
            update("t1", None, "running", vec![agent(1, "aaa1")]),
            update("t1", Some(RUN), "running", Vec::new()),
            update("t1", None, "running", vec![agent(2, "aaa2")]),
        ],
    )
    .await;
    for id in ["aaa1", "aaa2"] {
        let row = rig
            .store()
            .workflow_agent_log(session.clone(), id.to_owned())
            .await
            .unwrap()
            .unwrap_or_else(|| panic!("no row for {id}"));
        assert_eq!(row.run_id, RUN);
        assert_eq!(row.native_id, native);
        assert_eq!(row.cwd, before.cwd);
        assert_eq!(row.provider_home, before.provider_home.to_string_lossy());
    }
}

#[tokio::test]
#[ignore = "B25: shim_resume_rig::as_shim_station restarts the manager on the same database while a task of the old one still writes (database is locked); see docs/v2/BACKLOG.md B25"]
async fn a_shim_hosted_session_binds_workflow_agents() {
    let mut rig = Rig::direct(None).await;
    let session = rig.start("claude").await;
    rig.as_shim_station().await;
    append(
        &rig,
        &session,
        vec![
            update("t1", None, "running", vec![agent(1, "aaa1")]),
            update("t1", Some(RUN), "running", vec![agent(2, "aaa2")]),
        ],
    )
    .await;
    // The row exists right after the append returns: no observer is waited
    // for.
    let row = rig
        .store()
        .workflow_agent_log(session.clone(), "aaa1".to_owned())
        .await
        .unwrap();
    assert_eq!(row.map(|r| r.run_id), Some(RUN.to_owned()));
}

#[tokio::test]
async fn an_interrupt_between_turns_is_refused_while_a_workflow_runs() {
    let mut rig = Rig::direct(None).await;
    let session = rig.start("claude").await;
    append(
        &rig,
        &session,
        vec![update("t1", Some(RUN), "running", vec![agent(1, "aaa1")])],
    )
    .await;
    let (code, message) = rig
        .refused(SessionCommand::Interrupt {
            session_id: session.clone(),
        })
        .await;
    assert_eq!(code, RejectCode::InvalidState);
    assert_eq!(message, "no turn runs");
}

/// A run with two agents: start, agents, run id and a final status.
fn complete_run(n: usize) -> Vec<NewEvent> {
    let task = format!("t{n:02}");
    let ids = |k: u32| format!("a{n:02}x{k}");
    vec![
        update(&task, None, "running", Vec::new()),
        update(
            &task,
            None,
            "running",
            vec![agent(1, &ids(1)), agent(2, &ids(2))],
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
#[ignore = "B25: shim_resume_rig::as_shim_station restarts the manager on the same database while a task of the old one still writes (database is locked); see docs/v2/BACKLOG.md B25"]
async fn an_adopted_session_binds_33_runs_before_its_observer_reads_them() {
    let mut rig = Rig::direct(None).await;
    let session = rig.start("claude").await;
    rig.as_shim_station().await;
    let events: Vec<NewEvent> = (0..34).flat_map(complete_run).collect();
    append(&rig, &session, events).await;
    // Right after the append returns, every agent of the 34 runs has a row.
    for n in 0..34_usize {
        for k in 1..=2 {
            let id = format!("a{n:02}x{k}");
            let row = rig
                .store()
                .workflow_agent_log(session.clone(), id.clone())
                .await
                .unwrap();
            assert!(row.is_some(), "no row for {id}");
        }
    }
}

#[tokio::test]
async fn an_adopted_session_gives_the_stored_facts_to_its_subagent_observer() {
    let mut rig = Rig::direct(None).await;
    let session = rig.start("claude").await;
    let process = rig
        .store()
        .live_process(session.clone())
        .await
        .unwrap()
        .unwrap();
    let stored = launch_facts(&rig, &session)
        .await
        .expect("the launch facts");
    rig.as_shim_station().await;
    // The adoption reads the facts of its own process and session only.
    let store = rig.store();
    let got = crate::workflows::launch::adopted(&store, &session, &process).await;
    assert_eq!(got, Some(stored));
    let other = crate::workflows::launch::adopted(&store, &"s-other".into(), &process).await;
    assert_eq!(other, None);
}
