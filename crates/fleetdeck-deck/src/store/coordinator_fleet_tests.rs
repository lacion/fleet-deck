//! R2.3 and R2.5: a worker's question to its coordinator, and group mail,
//! in the decision of `coordinator.call`.

use fleetdeck_proto::mutation::CoordinatorOp;
use fleetdeck_proto::orchestration::{MailGroup, TaskState};

use super::*;
use crate::store::{NewOutboxItem, NewTask};

fn group(group: MailGroup, text: &str) -> CoordinatorOp {
    CoordinatorOp::SendGroup {
        group,
        text: text.to_owned(),
        delivery: CoordinatorDelivery::FollowUp,
    }
}

/// The deck session of `world()` as an idle, reachable card: the read model
/// of the test world holds `Starting` sessions, so a group needs a state.
async fn running(w: &World, station: &StationId, ids: &[(&str, ProviderKind)]) {
    let sessions = ids
        .iter()
        .map(|(id, provider)| {
            let mut s = summary(id, "/w/app");
            s.provider = Some(*provider);
            s.state = fleetdeck_proto::event::SessionState::Running;
            s.activity = fleetdeck_proto::session::Activity::Idle;
            s
        })
        .collect();
    w.s.apply_snapshot(station.clone(), "inc".to_owned().into(), 0, sessions)
        .await
        .unwrap();
}

#[tokio::test]
async fn a_group_mail_reaches_each_live_session_of_the_grant_without_the_caller() {
    let w = world().await;
    let b = grant(&w, &[GrantClass::Mail], 0, &[]).await;
    running(
        &w,
        &w.alpha,
        &[
            ("coord-1", ProviderKind::Claude),
            ("worker-1", ProviderKind::Claude),
            ("worker-2", ProviderKind::Codex),
        ],
    )
    .await;
    running(&w, &w.beta, &[("other-1", ProviderKind::Claude)]).await;

    let r = decide(&w, env(&w, &b, "g1", group(MailGroup::Claude, "pause"))).await;
    assert_eq!(r.state, ReceiptState::Accepted, "{r:?}");
    let mails = w.s.mails(10).await.unwrap();
    // `coord-1` is the caller and `other-1` is outside the grant's stations.
    assert_eq!(mails.len(), 1, "{mails:?}");
    assert_eq!(mails[0].mail_id, "g1-1");
    assert_eq!(mails[0].session_id.0, "worker-1");
    assert_eq!(mails[0].sender, "coordinator coord-1");
    let r = decide(&w, env(&w, &b, "g2", group(MailGroup::Idle, "status?"))).await;
    assert_eq!(r.state, ReceiptState::Accepted, "{r:?}");
    assert_eq!(w.s.mails(10).await.unwrap().len(), 3);
    // A replay stores nothing new.
    decide(&w, env(&w, &b, "g2", group(MailGroup::Idle, "status?"))).await;
    assert_eq!(w.s.mails(10).await.unwrap().len(), 3);
    assert_eq!(w.s.outbox().await.unwrap().len(), 3);

    // `@codex` reaches the one Codex session; an empty text is refused.
    let r = decide(&w, env(&w, &b, "g3", group(MailGroup::Codex, "x"))).await;
    assert_eq!(r.state, ReceiptState::Accepted);
    assert_eq!(w.s.mails(10).await.unwrap().len(), 4);
    let r = decide(&w, env(&w, &b, "g4", group(MailGroup::Idle, " "))).await;
    assert_eq!(code(&r), Some(RejectCode::InvalidCommand));
}

#[tokio::test]
async fn a_group_mail_needs_the_mail_class_and_a_live_session() {
    let w = world().await;
    let read_only = grant(&w, &[GrantClass::Read], 0, &[]).await;
    let r = decide(&w, env(&w, &read_only, "g1", group(MailGroup::Idle, "x"))).await;
    assert_eq!(code(&r), Some(RejectCode::Forbidden));
    let w = world().await;
    let b = grant(&w, &[GrantClass::Mail], 0, &[]).await;
    // The sessions of `world()` are still `starting`: nobody is idle.
    let r = decide(&w, env(&w, &b, "g2", group(MailGroup::Idle, "x"))).await;
    assert_eq!(code(&r), Some(RejectCode::InvalidState));
    assert!(w.s.mails(10).await.unwrap().is_empty());
}

fn ask_op(question: &str) -> CoordinatorOp {
    CoordinatorOp::AskCoordinator {
        question: question.to_owned(),
    }
}

/// `worker-1` holds the grant of the test (as a sub-coordinator), and
/// `coord-1` is its coordinator that assigned the task.
async fn asking_world() -> (World, CoordinatorBinding, CoordinatorBinding) {
    let w = world().await;
    let boss = grant(&w, &[GrantClass::Mail], 0, &[]).await;
    let mut g = NewGrant::new(w.alpha.clone(), "worker-1".into());
    g.classes = [GrantClass::Mail].into_iter().collect();
    let stored = w.s.create_grant(g).await.unwrap();
    let mut b = binding_of(&stored.grant_id, 1);
    b.binding_id = "b-2".to_owned();
    b.native_id = "native-2".to_owned();
    assert!(w.s.bind_grant(b.clone()).await.unwrap());
    (w, boss, b)
}

fn from_worker(
    w: &World,
    b: &CoordinatorBinding,
    id: &str,
    op: CoordinatorOp,
) -> CoordinatorCallEnvelope {
    let mut e = env(w, b, id, op);
    e.call.caller = "worker-1".into();
    e
}

#[tokio::test]
async fn ask_coordinator_stores_a_question_and_mails_the_coordinator_once() {
    let (w, boss, worker) = asking_world().await;
    // No task yet: refused.
    let r = decide(&w, from_worker(&w, &worker, "q0", ask_op("which key?"))).await;
    assert_eq!(code(&r), Some(RejectCode::InvalidState));
    let assign = CoordinatorOp::Assign {
        task: "check it".to_owned(),
        to: target(&w.alpha, "worker-1"),
    };
    assert_eq!(
        decide(&w, env(&w, &boss, "t-1", assign)).await.state,
        ReceiptState::Accepted
    );

    let r = decide(&w, from_worker(&w, &worker, "q1", ask_op("  which key?  "))).await;
    assert_eq!(r.state, ReceiptState::Accepted, "{r:?}");
    let task = w.s.task("t-1".to_owned()).await.unwrap().unwrap();
    assert_eq!(task.state, TaskState::Question);
    assert_eq!(task.question.as_deref(), Some("which key?"));
    let mails = w.s.mails(10).await.unwrap();
    assert_eq!(mails.len(), 1);
    assert_eq!(mails[0].session_id.0, "coord-1");
    assert_eq!(mails[0].text, "which key?");
    assert_eq!(mails[0].sender, "worker worker-1");
    let outbox = w.s.outbox().await.unwrap();
    let text = outbox
        .iter()
        .filter_map(|o| o.payload["command"]["input"]["text"].as_str())
        .find(|t| t.contains("QUESTION"))
        .unwrap();
    assert_eq!(text, "[FLEETDECK QUESTION from worker-1] which key?");

    // A replay of the same call stores nothing new.
    decide(&w, from_worker(&w, &worker, "q1", ask_op("  which key?  "))).await;
    assert_eq!(w.s.mails(10).await.unwrap().len(), 1);
    // The task is still open: the worker completes it after the answer.
    let done = CoordinatorOp::CompleteTask {
        task_id: "t-1".to_owned(),
        result: Some("used the key".to_owned()),
        state: None,
        reason: None,
        question: None,
    };
    let r = decide(&w, from_worker(&w, &worker, "c1", done)).await;
    assert_eq!(r.state, ReceiptState::Accepted, "{r:?}");
    let task = w.s.task("t-1".to_owned()).await.unwrap().unwrap();
    assert_eq!((task.state, task.question), (TaskState::Done, None));
    // The task ended: there is nothing to ask about.
    let r = decide(&w, from_worker(&w, &worker, "q2", ask_op("again?"))).await;
    assert_eq!(code(&r), Some(RejectCode::InvalidState));
}

#[tokio::test]
async fn a_worker_reports_blocked_and_a_question_through_complete_task() {
    let (w, boss, worker) = asking_world().await;
    let assign = CoordinatorOp::Assign {
        task: "check it".to_owned(),
        to: target(&w.alpha, "worker-1"),
    };
    decide(&w, env(&w, &boss, "t-1", assign)).await;
    let complete =
        |state, reason: Option<&str>, question: Option<&str>| CoordinatorOp::CompleteTask {
            task_id: "t-1".to_owned(),
            result: None,
            state: Some(state),
            reason: reason.map(str::to_owned),
            question: question.map(str::to_owned),
        };
    // A blocked completion needs its reason.
    let r = decide(
        &w,
        from_worker(&w, &worker, "c1", complete(TaskState::Blocked, None, None)),
    )
    .await;
    assert_eq!(code(&r), Some(RejectCode::InvalidCommand));
    let q = complete(TaskState::Question, Some("stuck"), Some("what now?"));
    let r = decide(&w, from_worker(&w, &worker, "c2", q)).await;
    assert_eq!(r.state, ReceiptState::Accepted, "{r:?}");
    let blocked = complete(TaskState::Blocked, Some("no access"), None);
    let r = decide(&w, from_worker(&w, &worker, "c3", blocked)).await;
    assert_eq!(r.state, ReceiptState::Accepted, "{r:?}");
    let task = w.s.task("t-1".to_owned()).await.unwrap().unwrap();
    assert_eq!(task.state, TaskState::Blocked);
    assert_eq!(task.completion_reason.as_deref(), Some("no access"));
    // Blocked ends the task.
    let r = decide(
        &w,
        from_worker(&w, &worker, "c4", complete(TaskState::Done, None, None)),
    )
    .await;
    assert_eq!(code(&r), Some(RejectCode::InvalidState));
}

#[tokio::test]
async fn a_task_of_the_owner_keeps_the_question_for_the_board_and_sends_no_mail() {
    let (w, _boss, worker) = asking_world().await;
    let (_, _) =
        w.s.create_task(owner_task(&w.alpha, "t-owner", "worker-1"))
            .await
            .unwrap();
    let r = decide(&w, from_worker(&w, &worker, "q1", ask_op("who?"))).await;
    assert_eq!(r.state, ReceiptState::Accepted, "{r:?}");
    assert!(w.s.mails(10).await.unwrap().is_empty());
    let task = w.s.task("t-owner".to_owned()).await.unwrap().unwrap();
    assert_eq!(
        (task.state, task.question.as_deref()),
        (TaskState::Question, Some("who?"))
    );
}

fn owner_task(station: &StationId, id: &str, session: &str) -> NewTask {
    NewTask {
        task_id: id.to_owned(),
        title: "owner task".to_owned(),
        creator: "owner".to_owned(),
        session_id: session.into(),
        item: NewOutboxItem {
            station_id: station.clone(),
            origin: "mail".to_owned(),
            payload: serde_json::json!({ "kind": "session" }),
            expires_at: i64::MAX,
        },
    }
}

#[tokio::test]
async fn wait_tasks_returns_the_first_task_that_is_not_open() {
    let w = world().await;
    let b = grant(&w, &[GrantClass::Read, GrantClass::Mail], 0, &[]).await;
    for id in ["t-1", "t-2"] {
        let assign = CoordinatorOp::Assign {
            task: "check it".to_owned(),
            to: target(&w.alpha, "worker-1"),
        };
        decide(&w, env(&w, &b, id, assign)).await;
    }
    let ids = |ids: &[&str]| CoordinatorQuery::WaitTasks {
        task_ids: ids.iter().map(|i| (*i).to_owned()).collect(),
    };
    // All open: the wait ends at the deadline (the test clock moves) with
    // no task.
    let clock = w.det.clone();
    let tick = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(250)).await;
        clock.advance(Duration::from_secs(1));
    });
    let v = ask(&w, query(&w, &b, ids(&["t-1", "t-2"]), 300))
        .await
        .unwrap();
    tick.await.unwrap();
    assert!(v["task"].is_null(), "{v}");

    // A waiter sees the task that gets a question while it waits.
    let wait = query(&w, &b, ids(&["t-1", "t-2"]), 20_000);
    let s = w.s.clone();
    let mark = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(250)).await;
        let q = fleetdeck_proto::orchestration::TaskCompletion {
            state: TaskState::Question,
            result: None,
            reason: Some("r".to_owned()),
            question: Some("q?".to_owned()),
        };
        s.complete_task("t-2".to_owned(), q).await.unwrap();
    });
    let started = std::time::Instant::now();
    let v = ask(&w, wait).await.unwrap();
    mark.await.unwrap();
    assert_eq!(v["task"]["task_id"], "t-2");
    assert_eq!(v["task"]["state"], "question");
    assert!(started.elapsed() < Duration::from_secs(5));
    // A task that is not open already returns at once, in the order of ids.
    let done = fleetdeck_proto::orchestration::TaskCompletion {
        state: TaskState::Done,
        result: None,
        reason: None,
        question: None,
    };
    w.s.complete_task("t-1".to_owned(), done).await.unwrap();
    let v = ask(&w, query(&w, &b, ids(&["t-2", "t-1"]), 5_000))
        .await
        .unwrap();
    assert_eq!(v["task"]["task_id"], "t-2");

    // Bad lists and unknown ids are refused.
    assert_eq!(
        ask(&w, query(&w, &b, ids(&[]), 1_000)).await,
        Err(RejectCode::InvalidCommand)
    );
    assert_eq!(
        ask(&w, query(&w, &b, ids(&["t-1", "nope"]), 1_000)).await,
        Err(RejectCode::UnknownRequest)
    );
}
