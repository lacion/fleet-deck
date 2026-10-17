//! The deck command router against a fake station (M2 P3-7a; G3, PLAN
//! §8.4): the envelope is stored `pending` before the send; a lost response
//! or a link drop leads to a resend with the same `command_id` and the same
//! bytes; `retry_until` ends a command as `ambiguous` (sent) or `failed`
//! (never sent); a rejection without a stored receipt after an earlier send
//! is `ambiguous`; a link refusal is `failed`; nothing is redispatched.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::time::Duration;

use common::*;
use fleetdeck_deck::store::DeckError;
use fleetdeck_proto::RejectCode;
use fleetdeck_proto::command::{CommandOrigin, DeckCommandState, RETRY_WINDOW_MS, ReceiptState};
use fleetdeck_proto::link::{Outcome, WireError};
use serde_json::json;

#[tokio::test]
async fn a_lost_response_leads_to_a_resend_with_the_same_id_and_bytes_and_one_outcome() {
    let mut f = fixture().await;
    let mut station = f.connect().await;
    let cmd = f.submit().await;
    assert_eq!(cmd.state, DeckCommandState::Pending);
    assert_eq!(cmd.envelope["target_incarnation"], json!(f.inc));
    let first = next_request(&mut station).await;
    assert_eq!(first.method, "session.interrupt");
    assert_eq!(first.command_id.as_ref(), Some(&cmd.command_id));
    assert_eq!(first.params, cmd.envelope, "the stored bytes are sent");
    assert_eq!(f.command(&cmd.command_id).await.attempts, 1);

    // The link drops before the response: the command stays pending.
    f.drop_link(station).await;
    assert_eq!(
        f.command(&cmd.command_id).await.state,
        DeckCommandState::Pending
    );

    let mut station = f.connect().await;
    let second = Fixture::answer(&mut station, accepted).await;
    assert_eq!(second.command_id, first.command_id);
    assert_eq!(second.params, first.params);
    let done = f.settled(&cmd.command_id).await;
    assert_eq!(done.state, DeckCommandState::Done);
    assert_eq!(done.attempts, 2);
    assert_eq!(done.receipt.unwrap().state, ReceiptState::Accepted);
    // Done is final: no more sends.
    no_request(&mut station, Duration::from_millis(400)).await;
    assert_eq!(f.store.commands().await.unwrap().len(), 1);
}

#[tokio::test]
async fn a_command_for_an_unreachable_station_waits_and_is_sent_when_it_links() {
    let mut f = fixture().await;
    let station = f.connect().await;
    f.drop_link(station).await;
    let cmd = f.submit().await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(f.command(&cmd.command_id).await.attempts, 0);

    let mut station = f.connect().await;
    Fixture::answer(&mut station, |id| rejected(id, RejectCode::UnknownSession)).await;
    let done = f.settled(&cmd.command_id).await;
    assert_eq!(done.state, DeckCommandState::Done, "a stored rejection");
    assert_eq!(done.attempts, 1);
}

#[tokio::test]
async fn a_rejection_without_a_receipt_is_failed_on_the_first_send_and_ambiguous_after_one() {
    let mut f = fixture().await;
    let mut station = f.connect().await;

    let c1 = f.submit().await;
    Fixture::answer(&mut station, |id| {
        rejected(id, RejectCode::IncarnationMismatch)
    })
    .await;
    let c1 = f.settled(&c1.command_id).await;
    assert_eq!(c1.state, DeckCommandState::Failed);
    assert_eq!(c1.reason.as_deref(), Some("station_reset"));

    let c2 = f.submit().await;
    next_request(&mut station).await;
    f.drop_link(station).await;
    let mut station = f.connect().await;
    Fixture::answer(&mut station, |id| rejected(id, RejectCode::Expired)).await;
    let c2 = f.settled(&c2.command_id).await;
    assert_eq!(c2.state, DeckCommandState::Ambiguous);
    assert_eq!(c2.reason.as_deref(), Some("expired"));
    no_request(&mut station, Duration::from_millis(400)).await;
}

#[tokio::test]
async fn a_link_refusal_fails_the_command_and_too_many_requests_is_resent() {
    let mut f = fixture().await;
    let mut station = f.connect().await;

    let c1 = f.submit().await;
    Fixture::answer(&mut station, |_| {
        Outcome::Err(WireError::new("forbidden", "the scope is missing"))
    })
    .await;
    let c1 = f.settled(&c1.command_id).await;
    assert_eq!(c1.state, DeckCommandState::Failed);
    assert_eq!(c1.reason.as_deref(), Some("forbidden"));

    let c2 = f.submit().await;
    Fixture::answer(&mut station, |_| {
        Outcome::Err(WireError::new("too_many_requests", "busy"))
    })
    .await;
    // The tick resends the same command.
    let again = Fixture::answer(&mut station, accepted).await;
    assert_eq!(again.command_id.as_ref(), Some(&c2.command_id));
    let c2 = f.settled(&c2.command_id).await;
    assert_eq!((c2.state, c2.attempts), (DeckCommandState::Done, 2));
}

#[tokio::test]
async fn retry_until_ends_a_sent_command_ambiguous_and_an_unsent_one_failed() {
    let mut f = fixture().await;
    let mut station = f.connect().await;
    let sent = f.submit().await;
    next_request(&mut station).await;
    f.drop_link(station).await;
    let unsent = f.submit().await;

    f.det.advance(Duration::from_millis(
        u64::try_from(RETRY_WINDOW_MS).unwrap() + 1000,
    ));
    let sent = f.settled(&sent.command_id).await;
    assert_eq!(sent.state, DeckCommandState::Ambiguous);
    assert_eq!(sent.reason.as_deref(), Some("no_receipt"));
    let unsent = f.settled(&unsent.command_id).await;
    assert_eq!(unsent.state, DeckCommandState::Failed);
    assert_eq!(unsent.reason.as_deref(), Some("not_sent"));

    // Nothing is sent after `retry_until`, and nothing is redispatched.
    let mut station = f.connect().await;
    no_request(&mut station, Duration::from_millis(400)).await;
    assert_eq!(f.store.commands().await.unwrap().len(), 2);
}

#[tokio::test]
async fn removing_a_station_fails_its_pending_commands_and_unsynced_or_unknown_stations_refuse() {
    let mut f = fixture().await;
    // Before the first sync the deck has no target incarnation.
    let err = f
        .ctx
        .submit(f.station.clone(), CommandOrigin::Board, interrupt())
        .await
        .unwrap_err();
    assert!(matches!(err, DeckError::NotSynced(_)), "{err}");

    let station = f.connect().await;
    f.drop_link(station).await;
    let cmd = f.submit().await;
    let key = f
        .store
        .station(f.station.clone())
        .await
        .unwrap()
        .unwrap()
        .key;
    f.store.revoke_station(key).await.unwrap();
    let cmd = f.command(&cmd.command_id).await;
    assert_eq!(cmd.state, DeckCommandState::Failed);
    assert_eq!(cmd.reason.as_deref(), Some("station_removed"));

    let err = f
        .ctx
        .submit(f.station.clone(), CommandOrigin::Board, interrupt())
        .await
        .unwrap_err();
    assert!(matches!(err, DeckError::UnknownStation(_)), "{err}");
}

#[tokio::test]
async fn a_refusal_after_a_lost_response_is_ambiguous_because_the_first_send_may_have_run() {
    let mut f = fixture().await;
    let mut station = f.connect().await;
    let cmd = f.submit().await;
    // The first send reaches the station (it may run); its response is lost.
    next_request(&mut station).await;
    f.drop_link(station).await;
    // The resend is refused by the link layer (for example, the grant
    // changed meanwhile): the station decided nothing for this send.
    let mut station = f.connect().await;
    Fixture::answer(&mut station, |_| {
        Outcome::Err(WireError::new("forbidden", "the scope is missing"))
    })
    .await;
    let cmd = f.settled(&cmd.command_id).await;
    assert_eq!(cmd.state, DeckCommandState::Ambiguous, "{cmd:?}");
    assert_eq!(cmd.reason.as_deref(), Some("forbidden"));
    assert_eq!(cmd.attempts, 2);
    no_request(&mut station, Duration::from_millis(400)).await;
}
