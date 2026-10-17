#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;

use fleetdeck_core::identity::Identity;
use fleetdeck_proto::board::DeckSession;
use fleetdeck_proto::command::{Receipt, ReceiptState};
use fleetdeck_proto::event::{Event, EventBody, ProviderKind, SessionState};
use fleetdeck_proto::link::Scope;
use fleetdeck_proto::orchestration::{
    DeliveryState, MailDelivery, MailGroup, TaskCompletion, TaskState,
};
use fleetdeck_proto::session::Activity;
use fleetdeck_proto::{
    CommandId, EventEnvelope, IncarnationId, SessionId, SessionSummary, StationId, card_status,
};
use fleetdeck_testkit::Det;
use serde_json::json;

use super::*;
use crate::store::stations::NewStation;
use crate::store::{NewCommand, Settle};

async fn setup(det: &Det) -> (DeckStore, StationId) {
    let s = DeckStore::open_in_memory(det.clock(), det.ids())
        .await
        .unwrap();
    let rec = s
        .pin_station(NewStation {
            key: Identity::generate().unwrap().public_key(),
            name: "alpha".to_owned(),
            scopes: BTreeSet::from([Scope::Read]),
            dial_url: None,
        })
        .await
        .unwrap();
    (s, rec.station_id)
}

fn item(station: &StationId) -> NewOutboxItem {
    NewOutboxItem {
        station_id: station.clone(),
        origin: "mail".to_owned(),
        payload: json!({ "kind": "session" }),
        expires_at: i64::MAX,
    }
}

fn mail(station: &StationId, id: &str, session: &str) -> NewMail {
    NewMail {
        mail_id: id.to_owned(),
        sender: "owner".to_owned(),
        session_id: SessionId::from(session.to_owned()),
        delivery: MailDelivery::FollowUp,
        text: "hello".to_owned(),
        item: item(station),
    }
}

/// Runs one delivery attempt of every due item and accepts it.
async fn deliver(s: &DeckStore, station: &StationId) {
    for due in s.due_outbox(station.clone()).await.unwrap() {
        let id = CommandId::from(format!("c-{}", due.outbox_id));
        let cmd = NewCommand {
            author: None,
            command_id: id.clone(),
            station_id: station.clone(),
            method: "session.follow_up".to_owned(),
            origin: "mail".to_owned(),
            envelope: json!({}),
            retry_until: i64::MAX,
        };
        s.start_attempt(due.outbox_id, cmd).await.unwrap();
        let receipt = Receipt {
            command_id: id.clone(),
            state: ReceiptState::Accepted,
            station_seq: Some(1),
            result: None,
            reject: None,
            at: 0,
        };
        s.settle_command(id, Settle::Receipt(receipt))
            .await
            .unwrap();
    }
    s.reconcile_outbox().await.unwrap();
}

#[tokio::test]
async fn a_mail_and_its_outbox_item_are_stored_once_and_delivered() {
    let det = Det::new();
    let (s, st) = setup(&det).await;
    let (m, new) = s.send_mail(mail(&st, "m1", "s1")).await.unwrap();
    assert!(new);
    assert_eq!(m.state, DeliveryState::Queued);
    assert_eq!(s.outbox().await.unwrap().len(), 1);
    // A resend with the same id stores and queues nothing.
    let (again, new) = s.send_mail(mail(&st, "m1", "s1")).await.unwrap();
    assert!(!new);
    assert_eq!(again, m);
    assert_eq!(s.outbox().await.unwrap().len(), 1);
    s.send_mail(mail(&st, "m2", "s1")).await.unwrap();
    s.send_mail(mail(&st, "m3", "s2")).await.unwrap();
    let buried = s.undelivered_mail().await.unwrap();
    assert_eq!(buried.get(&SessionId::from("s1".to_owned())), Some(&2));

    deliver(&s, &st).await;
    let all = s.mails(10).await.unwrap();
    assert_eq!(all.len(), 3);
    assert!(
        all.iter().all(|m| m.state == DeliveryState::Delivered),
        "{all:?}"
    );
    assert!(s.undelivered_mail().await.unwrap().is_empty());

    // A mail for an unknown station stores nothing.
    let mut lost = mail(&st, "m4", "s1");
    lost.item.station_id = StationId::from("nope".to_owned());
    assert!(matches!(
        s.send_mail(lost).await,
        Err(DeckError::UnknownStation(_))
    ));
    assert_eq!(s.mails(10).await.unwrap().len(), 3);
}

#[tokio::test]
async fn a_task_completes_once_and_a_different_completion_is_a_conflict() {
    let det = Det::new();
    let (s, st) = setup(&det).await;
    let task = NewTask {
        task_id: "t1".to_owned(),
        title: "write the docs".to_owned(),
        creator: "owner".to_owned(),
        session_id: SessionId::from("s1".to_owned()),
        item: item(&st),
    };
    let (t, new) = s.create_task(task.clone()).await.unwrap();
    assert!(new);
    assert_eq!(
        (t.state, t.delivery),
        (TaskState::Open, DeliveryState::Queued)
    );
    assert!(!s.create_task(task).await.unwrap().1);
    deliver(&s, &st).await;
    assert_eq!(
        s.task("t1".to_owned()).await.unwrap().unwrap().delivery,
        DeliveryState::Delivered
    );

    let done = Some("merged".to_owned());
    let completion = |state, result: Option<&str>| TaskCompletion {
        state,
        result: result.map(str::to_owned),
        reason: None,
        question: None,
    };
    let Some(Completed::Ended(t)) = s
        .complete_task("t1".to_owned(), completion(TaskState::Done, Some("merged")))
        .await
        .unwrap()
    else {
        panic!("not ended");
    };
    assert_eq!((t.state, t.result.clone()), (TaskState::Done, done.clone()));
    assert!(t.completed_at.is_some());
    // The same completion again: a duplicate.
    assert_eq!(
        s.complete_task("t1".to_owned(), completion(TaskState::Done, Some("merged")))
            .await
            .unwrap(),
        Some(Completed::Ended(t.clone()))
    );
    // Another completion: refused, the task keeps its end.
    assert_eq!(
        s.complete_task("t1".to_owned(), completion(TaskState::Failed, None))
            .await
            .unwrap(),
        Some(Completed::Conflict(t))
    );
    assert_eq!(
        s.complete_task("nope".to_owned(), completion(TaskState::Done, None))
            .await
            .unwrap(),
        None
    );
    assert!(
        s.complete_task("t1".to_owned(), completion(TaskState::Open, None))
            .await
            .is_err()
    );
}

fn task(st: &StationId, id: &str, session: &str) -> NewTask {
    NewTask {
        task_id: id.to_owned(),
        title: "write the docs".to_owned(),
        creator: "owner".to_owned(),
        session_id: SessionId::from(session.to_owned()),
        item: item(st),
    }
}

fn with(state: TaskState, reason: Option<&str>, question: Option<&str>) -> TaskCompletion {
    TaskCompletion {
        state,
        result: None,
        reason: reason.map(str::to_owned),
        question: question.map(str::to_owned),
    }
}

#[tokio::test]
async fn a_question_leaves_the_task_open_and_blocked_ends_it() {
    let det = Det::new();
    let (s, st) = setup(&det).await;
    s.create_task(task(&st, "t1", "s1")).await.unwrap();
    // A blocked or question completion without its text is refused.
    for bad in [
        with(TaskState::Blocked, None, None),
        with(TaskState::Blocked, Some(" "), None),
        with(TaskState::Question, Some("why"), None),
        with(TaskState::Done, Some("why"), None),
        with(TaskState::Failed, None, Some("q?")),
    ] {
        assert!(s.complete_task("t1".to_owned(), bad).await.is_err());
    }
    let ask = with(TaskState::Question, Some("needs a key"), Some("which key?"));
    let Some(Completed::Ended(t)) = s.complete_task("t1".to_owned(), ask.clone()).await.unwrap()
    else {
        panic!("not stored");
    };
    assert_eq!(
        (
            t.state,
            t.completion_reason.as_deref(),
            t.question.as_deref()
        ),
        (TaskState::Question, Some("needs a key"), Some("which key?"))
    );
    assert!(t.state.is_open());
    assert!(t.completed_at.is_none());
    // The same question again is a duplicate.
    assert_eq!(
        s.complete_task("t1".to_owned(), ask).await.unwrap(),
        Some(Completed::Ended(t))
    );
    // After the answer the worker is blocked, and that ends the task.
    let blocked = with(TaskState::Blocked, Some("no key"), None);
    let Some(Completed::Ended(t)) = s
        .complete_task("t1".to_owned(), blocked.clone())
        .await
        .unwrap()
    else {
        panic!("not blocked");
    };
    assert_eq!(t.state, TaskState::Blocked);
    assert_eq!(t.question, None);
    assert!(t.completed_at.is_some());
    let done = with(TaskState::Done, None, None);
    assert!(matches!(
        s.complete_task("t1".to_owned(), done).await.unwrap(),
        Some(Completed::Conflict(_))
    ));
    // A question can end as done.
    s.create_task(task(&st, "t2", "s1")).await.unwrap();
    let q = with(TaskState::Question, Some("r"), Some("q?"));
    s.complete_task("t2".to_owned(), q).await.unwrap();
    let done = with(TaskState::Done, None, None);
    let Some(Completed::Ended(t)) = s.complete_task("t2".to_owned(), done).await.unwrap() else {
        panic!("not done");
    };
    assert_eq!((t.state, t.question), (TaskState::Done, None));
}

fn named(id: &str, state: SessionState) -> SessionSummary {
    let mut summary = SessionSummary::new(id.into());
    summary.state = state;
    summary
}

fn state_event(seq: u64, session: &str, state: SessionState) -> EventEnvelope {
    EventEnvelope {
        station_seq: seq,
        session_id: session.into(),
        session_seq: seq,
        at: 1_000,
        proto_version: 1,
        raw_ref: None,
        event: EventBody::Known(Event::SessionStateChanged {
            state,
            detail: None,
        }),
    }
}

async fn synced(det: &Det) -> (DeckStore, StationId) {
    let (s, st) = setup(det).await;
    let inc: IncarnationId = "inc".into();
    let running = vec![
        named("s1", SessionState::Running),
        named("s2", SessionState::Running),
        named("s3", SessionState::Running),
    ];
    s.apply_snapshot(st.clone(), inc, 0, running).await.unwrap();
    for id in ["t1", "t2", "t3"] {
        s.create_task(task(&st, id, &format!("s{}", &id[1..])))
            .await
            .unwrap();
    }
    (s, st)
}

#[tokio::test]
async fn a_worker_that_exits_or_is_lost_ends_its_open_tasks_as_blocked_once() {
    let det = Det::new();
    let (s, st) = synced(&det).await;
    let inc: IncarnationId = "inc".into();
    // `s1` has asked a question: that is still an open task.
    let ask = with(TaskState::Question, Some("r"), Some("q?"));
    s.complete_task("t1".to_owned(), ask).await.unwrap();
    // `s3` completed before it ended: its end stays.
    s.complete_task("t3".to_owned(), with(TaskState::Done, None, None))
        .await
        .unwrap();
    let events = vec![
        state_event(1, "s1", SessionState::Exited),
        state_event(2, "s2", SessionState::Lost),
        state_event(3, "s3", SessionState::Exited),
    ];
    s.apply_events(st.clone(), inc.clone(), events.clone())
        .await
        .unwrap();
    let t1 = s.task("t1".to_owned()).await.unwrap().unwrap();
    assert_eq!(t1.state, TaskState::Blocked);
    assert_eq!(
        t1.completion_reason.as_deref(),
        Some("the worker session ended without a completion (exited)")
    );
    let t2 = s.task("t2".to_owned()).await.unwrap().unwrap();
    assert_eq!(t2.state, TaskState::Blocked);
    assert_eq!(
        t2.completion_reason.as_deref(),
        Some("the worker session ended without a completion (lost)")
    );
    assert_eq!(
        s.task("t3".to_owned()).await.unwrap().unwrap().state,
        TaskState::Done
    );
    // A replay and a later event change nothing: one synthesis per task.
    let blocked_at = t1.completed_at;
    s.apply_events(st.clone(), inc.clone(), events)
        .await
        .unwrap();
    s.apply_events(st, inc, vec![state_event(4, "s1", SessionState::Lost)])
        .await
        .unwrap();
    let again = s.task("t1".to_owned()).await.unwrap().unwrap();
    assert_eq!(
        (again.completed_at, again.completion_reason.as_deref()),
        (
            blocked_at,
            Some("the worker session ended without a completion (exited)")
        )
    );
}

#[tokio::test]
async fn a_snapshot_that_drops_or_ends_a_session_blocks_its_open_tasks() {
    let det = Det::new();
    let (s, st) = synced(&det).await;
    let inc: IncarnationId = "inc".into();
    // `s1` is gone, `s2` is lost, `s3` runs on.
    let batch = vec![
        named("s2", SessionState::Lost),
        named("s3", SessionState::Running),
    ];
    s.apply_snapshot(st, inc, 5, batch).await.unwrap();
    let reason = |id: &str| {
        let s = s.clone();
        let id = id.to_owned();
        async move {
            let t = s.task(id).await.unwrap().unwrap();
            (t.state, t.completion_reason)
        }
    };
    assert_eq!(
        reason("t1").await,
        (
            TaskState::Blocked,
            Some("the worker session ended without a completion (removed)".to_owned())
        )
    );
    assert_eq!(
        reason("t2").await,
        (
            TaskState::Blocked,
            Some("the worker session ended without a completion (lost)".to_owned())
        )
    );
    assert_eq!(reason("t3").await, (TaskState::Open, None));
}

#[tokio::test]
async fn removing_a_station_blocks_the_open_tasks_of_its_sessions() {
    let det = Det::new();
    let k = Identity::generate().unwrap().public_key();
    let s = DeckStore::open_in_memory(det.clock(), det.ids())
        .await
        .unwrap();
    let rec = s
        .pin_station(NewStation {
            key: k,
            name: "alpha".to_owned(),
            scopes: BTreeSet::from([Scope::Read]),
            dial_url: None,
        })
        .await
        .unwrap();
    s.create_task(task(&rec.station_id, "t1", "s1"))
        .await
        .unwrap();
    s.revoke_station(k).await.unwrap();
    let t = s.task("t1".to_owned()).await.unwrap().unwrap();
    assert_eq!(t.state, TaskState::Blocked);
}

fn card(id: &str, provider: ProviderKind, state: SessionState, activity: Activity) -> DeckSession {
    let mut summary = SessionSummary::new(id.into());
    summary.provider = Some(provider);
    summary.state = state;
    summary.activity = activity;
    DeckSession {
        station_id: StationId::from("st".to_owned()),
        status: card_status(&summary, true),
        summary,
        unread: None,
        project_id: None,
        review_rev: None,
    }
}

#[test]
fn a_group_names_the_live_sessions_of_its_kind() {
    use ProviderKind::{Claude, Codex};
    let running = SessionState::Running;
    let sessions = [
        card("idle-claude", Claude, running, Activity::Idle),
        card("busy-claude", Claude, running, Activity::Working),
        card("idle-codex", Codex, running, Activity::Idle),
        card(
            "ended-claude",
            Claude,
            SessionState::Exited,
            Activity::Ended,
        ),
        card("lost-codex", Codex, SessionState::Lost, Activity::Idle),
    ];
    let ids = |group| {
        group_members(&sessions, group)
            .iter()
            .map(|s| s.summary.session_id.0.clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(ids(MailGroup::Idle), ["idle-claude", "idle-codex"]);
    assert_eq!(ids(MailGroup::Claude), ["idle-claude", "busy-claude"]);
    assert_eq!(ids(MailGroup::Codex), ["idle-codex"]);
}

#[tokio::test]
async fn a_group_send_stores_one_mail_per_session_once() {
    let det = Det::new();
    let (s, st) = setup(&det).await;
    let batch = |ids: &[&str]| -> Vec<NewMail> {
        ids.iter()
            .enumerate()
            .map(|(i, id)| mail(&st, &group_mail_id("g1", i + 1), id))
            .collect()
    };
    let (views, new) = s
        .send_group_mail("g1".to_owned(), batch(&["s1", "s2"]))
        .await
        .unwrap();
    assert!(new);
    assert_eq!(
        views.iter().map(|m| m.mail_id.as_str()).collect::<Vec<_>>(),
        ["g1-1", "g1-2"]
    );
    assert_eq!(s.outbox().await.unwrap().len(), 2);
    // A resend returns the stored mails, even for a changed group.
    let (again, new) = s
        .send_group_mail("g1".to_owned(), batch(&["s3"]))
        .await
        .unwrap();
    assert!(!new);
    assert_eq!(again, views);
    assert_eq!(s.outbox().await.unwrap().len(), 2);
}

#[tokio::test]
async fn tasks_by_ids_keep_the_order_and_name_an_unknown_id() {
    let det = Det::new();
    let (s, st) = setup(&det).await;
    s.create_task(task(&st, "a", "s1")).await.unwrap();
    s.create_task(task(&st, "b", "s1")).await.unwrap();
    let ids = |ids: &[&str]| ids.iter().map(|i| (*i).to_owned()).collect::<Vec<_>>();
    let tasks = s.tasks_by_ids(ids(&["b", "a"])).await.unwrap().unwrap();
    assert_eq!(
        tasks.iter().map(|t| t.task_id.as_str()).collect::<Vec<_>>(),
        ["b", "a"]
    );
    assert_eq!(
        s.tasks_by_ids(ids(&["a", "nope"])).await.unwrap(),
        Err("nope".to_owned())
    );
}
