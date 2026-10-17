//! The deck outbox against a fake station (M2 P3-7b; A5, PLAN §8.4): an
//! item waits for its station, is delivered once through a routed command,
//! is attempted again (with a new command) only when the station provably
//! never ran the last attempt, and ends `dead` when the outcome is unknown,
//! the station rejected it, it expired or the station was removed. The
//! attempt's command and the item's move to `sending` share one
//! transaction.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::time::Duration;

use common::*;
use fleetdeck_core::identity::Identity;
use fleetdeck_deck::DeckStore;
use fleetdeck_deck::store::outbox::{ATTEMPT_BACKOFF_MS, MAX_BACKOFF_MS};
use fleetdeck_deck::store::stations::NewStation;
use fleetdeck_deck::store::{NewCommand, NewOutboxItem, OutboxItem, OutboxState};
use fleetdeck_proto::command::{CommandOrigin, DeckCommandState, RETRY_WINDOW_MS};
use fleetdeck_proto::{CommandId, RejectCode};
use fleetdeck_testkit::Det;
use serde_json::json;

const DAY: Duration = Duration::from_secs(24 * 3600);

async fn enqueue(f: &Fixture) -> OutboxItem {
    f.ctx
        .enqueue(f.station.clone(), CommandOrigin::Mail, interrupt(), DAY)
        .await
        .unwrap()
}

async fn item(f: &Fixture, id: &str) -> OutboxItem {
    f.store.outbox_item(id.to_owned()).await.unwrap().unwrap()
}

async fn until_state(f: &Fixture, id: &str, state: OutboxState) -> OutboxItem {
    let deadline = tokio::time::Instant::now() + T;
    loop {
        let it = item(f, id).await;
        if it.state == state {
            return it;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {state:?}: {it:?}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn ms(v: i64) -> Duration {
    Duration::from_millis(u64::try_from(v).unwrap())
}

#[tokio::test]
async fn an_item_waits_for_its_station_and_is_delivered_once() {
    let mut f = fixture().await;
    // Sync once, then the station goes away; the item waits.
    let station = f.connect().await;
    f.drop_link(station).await;
    let queued = enqueue(&f).await;
    assert_eq!(queued.state, OutboxState::Queued);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(item(&f, &queued.outbox_id).await.attempts, 0);
    assert!(f.store.commands().await.unwrap().is_empty());

    let mut station = f.connect().await;
    let req = Fixture::answer(&mut station, accepted).await;
    assert_eq!(req.method, "session.interrupt");
    assert_eq!(req.params["origin"], json!("mail"));
    let done = until_state(&f, &queued.outbox_id, OutboxState::Delivered).await;
    assert_eq!(done.attempts, 1);
    assert_eq!(done.command_id, req.command_id);
    no_request(&mut station, Duration::from_millis(400)).await;
    assert_eq!(f.store.commands().await.unwrap().len(), 1);
}

#[tokio::test]
async fn an_attempt_the_station_never_ran_is_retried_with_a_new_command_after_a_backoff() {
    let mut f = fixture().await;
    let mut station = f.connect().await;
    let queued = enqueue(&f).await;
    // The station's database was replaced: it rejects the first send
    // without running it (`station_reset`).
    let first = Fixture::answer(&mut station, |id| {
        rejected(id, RejectCode::IncarnationMismatch)
    })
    .await;
    let waiting = until_state(&f, &queued.outbox_id, OutboxState::Queued).await;
    assert_eq!(waiting.reason.as_deref(), Some("station_reset"));
    assert_eq!(waiting.attempts, 1);
    // No new attempt during the backoff.
    no_request(&mut station, Duration::from_millis(400)).await;

    f.det.advance(ms(ATTEMPT_BACKOFF_MS));
    let second = Fixture::answer(&mut station, accepted).await;
    assert_ne!(second.command_id, first.command_id);
    let done = until_state(&f, &queued.outbox_id, OutboxState::Delivered).await;
    assert_eq!(done.attempts, 2);
    assert_eq!(done.command_id, second.command_id);
    let first = f.command(first.command_id.as_ref().unwrap()).await;
    assert_eq!(first.state, DeckCommandState::Failed);
}

#[tokio::test]
async fn an_ambiguous_attempt_is_dead_and_never_sent_again() {
    let mut f = fixture().await;
    let mut station = f.connect().await;
    let queued = enqueue(&f).await;
    // The send reaches the station; the response is lost; the station
    // stays away until `retry_until`.
    next_request(&mut station).await;
    f.drop_link(station).await;
    f.det.advance(ms(RETRY_WINDOW_MS + 1000));
    let dead = until_state(&f, &queued.outbox_id, OutboxState::Dead).await;
    assert_eq!(dead.reason.as_deref(), Some("ambiguous: no_receipt"));

    let mut station = f.connect().await;
    f.det.advance(ms(MAX_BACKOFF_MS));
    no_request(&mut station, Duration::from_millis(500)).await;
    assert_eq!(f.store.commands().await.unwrap().len(), 1);
}

#[tokio::test]
async fn a_stored_rejection_an_expiry_and_a_station_removal_end_items_dead() {
    let mut f = fixture().await;
    let mut station = f.connect().await;
    let rejected_item = enqueue(&f).await;
    Fixture::answer(&mut station, |id| rejected(id, RejectCode::UnknownSession)).await;
    let dead = until_state(&f, &rejected_item.outbox_id, OutboxState::Dead).await;
    assert_eq!(dead.reason.as_deref(), Some("rejected: unknown_session"));

    f.drop_link(station).await;
    let short = f
        .ctx
        .enqueue(
            f.station.clone(),
            CommandOrigin::Mail,
            interrupt(),
            Duration::from_secs(60),
        )
        .await
        .unwrap();
    let waiting = enqueue(&f).await;
    f.det.advance(Duration::from_secs(61));
    let dead = until_state(&f, &short.outbox_id, OutboxState::Dead).await;
    assert_eq!(dead.reason.as_deref(), Some("expired"));
    assert_eq!(dead.attempts, 0);

    let key = f
        .store
        .station(f.station.clone())
        .await
        .unwrap()
        .unwrap()
        .key;
    f.store.revoke_station(key).await.unwrap();
    let dead = item(&f, &waiting.outbox_id).await;
    assert_eq!(dead.state, OutboxState::Dead);
    assert_eq!(dead.reason.as_deref(), Some("station_removed"));
    let err = f
        .ctx
        .enqueue(f.station.clone(), CommandOrigin::Mail, interrupt(), DAY)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("unknown station"), "{err}");
}

/// The attempt's command and the item's move are one transaction: after a
/// restart the item is `sending` with a stored `pending` command (the
/// router resends that command), and a second start of the same attempt
/// does nothing.
#[tokio::test]
async fn an_attempt_and_its_command_are_stored_in_one_transaction_and_survive_a_restart() {
    let det = Det::new();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("deck.db");
    let store = DeckStore::open(&path, det.clock(), det.ids())
        .await
        .unwrap();
    let st = store
        .pin_station(NewStation {
            key: Identity::generate().unwrap().public_key(),
            name: "s".to_owned(),
            scopes: std::collections::BTreeSet::new(),
            dial_url: None,
        })
        .await
        .unwrap();
    let it = store
        .enqueue(NewOutboxItem {
            station_id: st.station_id.clone(),
            origin: "mail".to_owned(),
            payload: serde_json::to_value(interrupt()).unwrap(),
            expires_at: store.now_ms() + 60_000,
        })
        .await
        .unwrap();
    let cmd = |id: &str| NewCommand {
        author: None,
        command_id: CommandId::from(id),
        station_id: st.station_id.clone(),
        method: "session.interrupt".to_owned(),
        origin: "mail".to_owned(),
        envelope: json!({ "command_id": id }),
        retry_until: it.expires_at,
    };
    let started = store
        .start_attempt(it.outbox_id.clone(), cmd("a1"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(started.state, OutboxState::Sending);
    assert!(
        store
            .start_attempt(it.outbox_id.clone(), cmd("a2"))
            .await
            .unwrap()
            .is_none()
    );
    drop(store);

    let store = DeckStore::open(&path, det.clock(), det.ids())
        .await
        .unwrap();
    let after = store
        .outbox_item(it.outbox_id.clone())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(after.state, OutboxState::Sending);
    assert_eq!(after.command_id, Some(CommandId::from("a1")));
    assert_eq!(after.attempts, 1);
    let pending = store.pending_commands(st.station_id.clone()).await.unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].command_id, CommandId::from("a1"));
    assert_eq!(store.commands().await.unwrap().len(), 1);
}

/// An attempt's `retry_until` is capped at the item's `expires_at`: when
/// the item expires while its attempt is `sending`, the router stops
/// resending, and the item ends `dead` (`ambiguous`, the station may have
/// run it), never sent again.
#[tokio::test]
async fn an_item_that_expires_while_sending_is_never_resent() {
    let mut f = fixture().await;
    let mut station = f.connect().await;
    let ttl = Duration::from_secs(30);
    assert!(ttl < ms(RETRY_WINDOW_MS));
    let queued = f
        .ctx
        .enqueue(f.station.clone(), CommandOrigin::Mail, interrupt(), ttl)
        .await
        .unwrap();
    // The first send arrives; its envelope ends with the item.
    let first = next_request(&mut station).await;
    assert_eq!(first.params["retry_until"], json!(queued.expires_at));
    let cmd = f.command(first.command_id.as_ref().unwrap()).await;
    assert_eq!(cmd.retry_until, queued.expires_at);
    // The response is lost; before `expires_at` the router resends.
    f.drop_link(station).await;
    let mut station = f.connect().await;
    let resend = next_request(&mut station).await;
    assert_eq!(resend.command_id, first.command_id);
    f.drop_link(station).await;

    // The item expires while `sending`: no resend after it.
    f.det.advance(ttl + Duration::from_secs(1));
    let dead = until_state(&f, &queued.outbox_id, OutboxState::Dead).await;
    assert_eq!(dead.reason.as_deref(), Some("ambiguous: no_receipt"));
    let mut station = f.connect().await;
    no_request(&mut station, Duration::from_millis(500)).await;
    assert_eq!(f.store.commands().await.unwrap().len(), 1);
}

#[tokio::test]
async fn the_store_refuses_an_attempt_that_outlives_its_item_or_starts_after_it() {
    let f = fixture().await;
    let it = f
        .store
        .enqueue(NewOutboxItem {
            station_id: f.station.clone(),
            origin: "mail".to_owned(),
            payload: serde_json::to_value(interrupt()).unwrap(),
            expires_at: f.store.now_ms() + 10_000,
        })
        .await
        .unwrap();
    let cmd = |id: &str, retry_until: i64| NewCommand {
        author: None,
        command_id: CommandId::from(id),
        station_id: f.station.clone(),
        method: "session.interrupt".to_owned(),
        origin: "mail".to_owned(),
        envelope: json!({ "command_id": id }),
        retry_until,
    };
    let late = f
        .store
        .start_attempt(it.outbox_id.clone(), cmd("x1", it.expires_at + 1))
        .await
        .unwrap_err();
    assert!(late.to_string().contains("outlives"), "{late}");
    f.det.advance(Duration::from_secs(11));
    let after = f
        .store
        .start_attempt(it.outbox_id.clone(), cmd("x2", it.expires_at))
        .await
        .unwrap();
    assert!(after.is_none());
    assert!(f.store.commands().await.unwrap().is_empty());
}
