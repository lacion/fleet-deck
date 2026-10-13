#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;

use fleetdeck_core::identity::Identity;
use fleetdeck_proto::command::{Receipt, ReceiptState};
use fleetdeck_proto::link::Scope;
use fleetdeck_proto::orchestration::{DeliveryState, MailDelivery, TaskState};
use fleetdeck_proto::{CommandId, SessionId, StationId};
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
    let Some(Completed::Ended(t)) = s
        .complete_task("t1".to_owned(), TaskState::Done, done.clone())
        .await
        .unwrap()
    else {
        panic!("not ended");
    };
    assert_eq!((t.state, t.result.clone()), (TaskState::Done, done.clone()));
    assert!(t.completed_at.is_some());
    // The same completion again: a duplicate.
    assert_eq!(
        s.complete_task("t1".to_owned(), TaskState::Done, done)
            .await
            .unwrap(),
        Some(Completed::Ended(t.clone()))
    );
    // Another completion: refused, the task keeps its end.
    assert_eq!(
        s.complete_task("t1".to_owned(), TaskState::Failed, None)
            .await
            .unwrap(),
        Some(Completed::Conflict(t))
    );
    assert_eq!(
        s.complete_task("nope".to_owned(), TaskState::Done, None)
            .await
            .unwrap(),
        None
    );
    assert!(
        s.complete_task("t1".to_owned(), TaskState::Open, None)
            .await
            .is_err()
    );
}
