//! The card extras (DL-141): `GET /api/fleet/extras` returns the pending
//! mail count and the newest touched files of the sessions that have any;
//! a session without both is left out, and the route needs a login.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::board::board;
use common::*;
use fleetdeck_deck::store::{NewCommand, NewMail, NewOutboxItem, Settle};
use fleetdeck_proto::command::{Receipt, ReceiptState};
use fleetdeck_proto::event::{Event, EventBody, FileChange};
use fleetdeck_proto::orchestration::MailDelivery;
use fleetdeck_proto::{CommandId, EventEnvelope, SessionId, SessionSummary};
use serde_json::json;

fn changed(seq: u64, session: &str, at: i64, path: &str) -> EventEnvelope {
    EventEnvelope {
        station_seq: seq,
        session_id: session.into(),
        session_seq: seq,
        at,
        proto_version: 1,
        raw_ref: None,
        event: EventBody::Known(Event::FileChanged {
            path: path.to_owned(),
            change: FileChange::Modified,
            node_id: None,
        }),
    }
}

fn new_mail(f: &Fixture, id: &str, session: &str) -> NewMail {
    NewMail {
        mail_id: id.to_owned(),
        sender: "owner".to_owned(),
        session_id: SessionId::from(session.to_owned()),
        delivery: MailDelivery::FollowUp,
        text: "hello".to_owned(),
        item: NewOutboxItem {
            station_id: f.station.clone(),
            origin: "mail".to_owned(),
            payload: json!({ "kind": "session" }),
            expires_at: i64::MAX,
        },
    }
}

/// Runs one delivery attempt of every due item and accepts it.
async fn deliver(f: &Fixture) {
    let s = &f.store;
    for due in s.due_outbox(f.station.clone()).await.unwrap() {
        let id = CommandId::from(format!("c-{}", due.outbox_id));
        let cmd = NewCommand {
            author: None,
            command_id: id.clone(),
            station_id: f.station.clone(),
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
async fn the_extras_list_pending_mail_and_the_four_newest_files() {
    let f = fixture().await;
    let c = board(std::sync::Arc::clone(&f.ctx)).await;
    let s = &f.store;
    s.apply_snapshot(
        f.station.clone(),
        f.inc.clone(),
        0,
        vec![
            SessionSummary::new("s1".into()),
            SessionSummary::new("s2".into()),
        ],
    )
    .await
    .unwrap();
    // s2 has one mail that is delivered, and no touches.
    s.send_mail(new_mail(&f, "m0", "s2")).await.unwrap();
    deliver(&f).await;
    // s1 has two mails that wait, and five touched files.
    s.send_mail(new_mail(&f, "m1", "s1")).await.unwrap();
    s.send_mail(new_mail(&f, "m2", "s1")).await.unwrap();
    let now = i64::try_from(f.det.now_ms()).unwrap();
    let events = ["a.rs", "b.rs", "c.rs", "d.rs", "e.rs"]
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let n = i64::try_from(i).unwrap();
            changed(i as u64 + 1, "s1", now - 5_000 + n * 1_000, p)
        })
        .collect();
    s.apply_events(f.station.clone(), f.inc.clone(), events)
        .await
        .unwrap();

    let (cookie, _) = c.login().await;
    let r = c
        .get("/api/fleet/extras", &[("Cookie", cookie.as_str())])
        .await;
    assert_eq!(r.status, 200, "{}", r.body);
    let list = r.body["sessions"].as_array().unwrap();
    assert_eq!(list.len(), 1, "{}", r.body);
    assert_eq!(list[0]["station_id"], f.station.as_str());
    assert_eq!(list[0]["session_id"], "s1");
    assert_eq!(list[0]["mail_pending"], 2);
    assert_eq!(
        list[0]["files"],
        json!(["e.rs", "d.rs", "c.rs", "b.rs"]),
        "the 4 newest, newest first"
    );

    // Without a login the route is closed.
    let r = c.get("/api/fleet/extras", &[]).await;
    assert_eq!(r.status, 401);
}
