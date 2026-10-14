//! The mail of a member (NV-17) over loopback HTTPS: a queued mail is not
//! delivered after its share is revoked or expired. The rest of the shared
//! fleet is in `board_share.rs`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
mod share_support;

use common::board::BoardClient;
use common::*;
use fleetdeck_deck::store::{OutboxItem, OutboxState};
use fleetdeck_proto::command::DeckCommandState;
use serde_json::{Value, json};
use share_support::*;

/// A collaborator `Ann` of the session `s1` whose share lasts `days` days.
async fn mailer(c: &BoardClient, h: &Headers, st: &str, days: u32) -> (Headers, String) {
    let share = json!({ "kind": "session", "target": format!("{st}/s1"),
                        "role": "collaborator", "expires_in_days": days });
    let (m, share_id, _) = join(c, h, share, "Ann").await;
    (m, share_id)
}

fn mail_body(st: &str, id: &str) -> Value {
    json!({ "mail_id": id, "station": st, "session_id": "s1", "text": "hello" })
}

/// The only mail item of the deck.
async fn mail_item(f: &Fixture) -> OutboxItem {
    let mut items = f.store.outbox().await.unwrap();
    items.retain(|i| i.origin == "mail");
    assert_eq!(items.len(), 1, "{items:?}");
    items.remove(0)
}

async fn until_item(f: &Fixture, state: OutboxState) {
    let deadline = tokio::time::Instant::now() + T;
    while mail_item(f).await.state != state {
        assert!(tokio::time::Instant::now() < deadline, "timed out");
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn a_member_mail_is_delivered_while_its_share_holds() {
    let (f, mut station, c, h) = setup().await;
    let st = f.station.0.clone();
    let (m, _) = mailer(&c, &h, &st, 1).await;
    let r = post(&c, &m, "/api/mail", mail_body(&st, "ml1")).await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body["sender"], "member:Ann");
    let req = Fixture::answer(&mut station, accepted).await;
    assert_eq!(req.method, "session.follow_up");
}

/// NV-17: a mail that waits for its station is not delivered after the
/// share is revoked.
#[tokio::test]
async fn a_queued_member_mail_is_not_delivered_after_the_share_is_revoked() {
    let (mut f, station, c, h) = setup().await;
    let st = f.station.0.clone();
    let (m, share_id) = mailer(&c, &h, &st, 1).await;
    f.drop_link(station).await;
    let r = post(&c, &m, "/api/mail", mail_body(&st, "ml1")).await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(mail_item(&f).await.state, OutboxState::Queued);
    let r = post(&c, &h, &format!("/api/shares/{share_id}/revoke"), json!({})).await;
    assert_eq!(r.status, 200, "{}", r.body);

    let mut station = f.connect().await;
    until_item(&f, OutboxState::Dead).await;
    assert_eq!(
        mail_item(&f).await.reason.as_deref(),
        Some("authority_lost")
    );
    no_request(&mut station, std::time::Duration::from_millis(400)).await;
    assert!(f.store.commands().await.unwrap().is_empty());
}

/// NV-17: a command that is already pending is not resent after the
/// share is revoked.
#[tokio::test]
async fn a_pending_member_mail_command_is_not_resent_after_the_share_is_revoked() {
    let (mut f, mut station, c, h) = setup().await;
    let st = f.station.0.clone();
    let (m, share_id) = mailer(&c, &h, &st, 1).await;
    let r = post(&c, &m, "/api/mail", mail_body(&st, "ml1")).await;
    assert_eq!(r.status, 200, "{}", r.body);
    // The send reaches the station; the link drops before the answer.
    let first = next_request(&mut station).await;
    f.drop_link(station).await;
    let id = first.command_id.clone().unwrap();
    assert_eq!(f.command(&id).await.state, DeckCommandState::Pending);
    let r = post(&c, &h, &format!("/api/shares/{share_id}/revoke"), json!({})).await;
    assert_eq!(r.status, 200, "{}", r.body);

    let mut station = f.connect().await;
    let cmd = f.settled(&id).await;
    assert_eq!(cmd.state, DeckCommandState::Failed);
    assert_eq!(cmd.reason.as_deref(), Some("authority_lost"));
    let item = mail_item(&f).await;
    assert_eq!(item.state, OutboxState::Dead);
    assert_eq!(item.reason.as_deref(), Some("authority_lost"));
    no_request(&mut station, std::time::Duration::from_millis(400)).await;
}

/// NV-17: an expired share ends a queued mail like a revoke does.
#[tokio::test]
async fn a_queued_member_mail_is_not_delivered_after_the_share_expired() {
    let (mut f, station, c, h) = setup().await;
    let st = f.station.0.clone();
    let (m, _) = mailer(&c, &h, &st, 1).await;
    f.drop_link(station).await;
    // The mail starts one hour after the share, so it outlives the share by
    // one hour.
    f.det.advance(std::time::Duration::from_secs(3600));
    let r = post(&c, &m, "/api/mail", mail_body(&st, "ml1")).await;
    assert_eq!(r.status, 200, "{}", r.body);
    f.det
        .advance(std::time::Duration::from_secs(23 * 3600 + 60));
    let mut station = f.connect().await;
    until_item(&f, OutboxState::Dead).await;
    assert_eq!(
        mail_item(&f).await.reason.as_deref(),
        Some("authority_lost")
    );
    no_request(&mut station, std::time::Duration::from_millis(400)).await;
}
