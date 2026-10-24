#![allow(clippy::unwrap_used, clippy::expect_used)]

//! The hold of an item whose target session ended (module rules of
//! [`super`]).

use std::collections::BTreeSet;
use std::time::Duration;

use fleetdeck_core::identity::Identity;
use fleetdeck_proto::command::{Receipt, Reject, RejectCode, SessionCommand, StopMode};
use fleetdeck_proto::event::{Event, EventBody, SessionState};
use fleetdeck_proto::input::UserInput;
use fleetdeck_proto::link::Scope;
use fleetdeck_proto::{
    CommandId, EventEnvelope, IncarnationId, SessionId, SessionSummary, StationId,
};
use fleetdeck_testkit::Det;
use serde_json::{Value, json};

use super::*;
use crate::store::stations::NewStation;
use crate::store::{NewCommand, Settle};

const TTL_MS: i64 = 60_000;

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

fn input() -> UserInput {
    UserInput {
        text: "hello".to_owned(),
        images: Vec::new(),
        author: None,
        shell_results: Vec::new(),
    }
}

fn payload(command: SessionCommand) -> Value {
    serde_json::to_value(CommandKind::Session { command }).unwrap()
}

fn follow_up(session: &str) -> Value {
    payload(SessionCommand::FollowUp {
        session_id: session.into(),
        input: input(),
    })
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

/// The deck shows each `(session, state)` of `sessions` (incarnation `inc`,
/// cursor 0).
async fn show(s: &DeckStore, st: &StationId, sessions: &[(&str, SessionState)]) {
    let list = sessions
        .iter()
        .map(|(id, state)| named(id, *state))
        .collect();
    s.apply_snapshot(st.clone(), "inc".into(), 0, list)
        .await
        .unwrap();
}

async fn queue(s: &DeckStore, st: &StationId, payload: Value, ttl_ms: i64) -> OutboxItem {
    s.enqueue(NewOutboxItem {
        station_id: st.clone(),
        origin: "mail".to_owned(),
        payload,
        expires_at: s.now_ms() + ttl_ms,
    })
    .await
    .unwrap()
}

/// Runs one attempt of `item`, the station rejects it with `code`, and the
/// outbox reconciles.
async fn rejected(
    s: &DeckStore,
    st: &StationId,
    item: &OutboxItem,
    code: RejectCode,
) -> OutboxItem {
    let id = CommandId::from(format!("c-{}", item.outbox_id));
    let cmd = NewCommand {
        author: None,
        command_id: id.clone(),
        station_id: st.clone(),
        method: "session.follow_up".to_owned(),
        origin: "mail".to_owned(),
        envelope: json!({}),
        retry_until: item.expires_at,
    };
    s.start_attempt(item.outbox_id.clone(), cmd).await.unwrap();
    let receipt = Receipt::rejected(id.clone(), Reject::new(code, "refused"), 0);
    s.settle_command(id, Settle::Receipt(receipt))
        .await
        .unwrap();
    s.reconcile_outbox().await.unwrap();
    s.outbox_item(item.outbox_id.clone())
        .await
        .unwrap()
        .unwrap()
}

async fn due(s: &DeckStore, st: &StationId) -> Vec<String> {
    s.due_outbox(st.clone())
        .await
        .unwrap()
        .into_iter()
        .map(|i| i.outbox_id)
        .collect()
}

fn assert_held(item: &OutboxItem) {
    assert_eq!(item.state, OutboxState::Queued, "{item:?}");
    assert_eq!(item.reason.as_deref(), Some("session_ended"));
    assert_eq!(item.next_at, item.expires_at);
}

#[tokio::test]
async fn a_mail_rejected_while_its_session_is_exited_is_held() {
    let det = Det::new();
    let (s, st) = setup(&det).await;
    show(&s, &st, &[("s1", SessionState::Exited)]).await;
    let item = queue(&s, &st, follow_up("s1"), TTL_MS).await;
    let held = rejected(&s, &st, &item, RejectCode::InvalidState).await;
    assert_held(&held);
    assert_eq!(held.attempts, 1);
    assert!(due(&s, &st).await.is_empty());
    det.advance(Duration::from_secs(30));
    assert!(due(&s, &st).await.is_empty());
}

#[tokio::test]
async fn a_mail_rejected_while_its_session_is_lost_is_held() {
    let det = Det::new();
    let (s, st) = setup(&det).await;
    show(&s, &st, &[("s1", SessionState::Lost)]).await;
    let item = queue(&s, &st, follow_up("s1"), TTL_MS).await;
    let held = rejected(&s, &st, &item, RejectCode::InvalidState).await;
    assert_held(&held);
    assert!(due(&s, &st).await.is_empty());
}

#[tokio::test]
async fn invalid_state_while_the_session_runs_is_dead() {
    let det = Det::new();
    let (s, st) = setup(&det).await;
    show(&s, &st, &[("s1", SessionState::Running)]).await;
    let item = queue(&s, &st, follow_up("s1"), TTL_MS).await;
    let dead = rejected(&s, &st, &item, RejectCode::InvalidState).await;
    assert_eq!(dead.state, OutboxState::Dead);
    assert_eq!(
        dead.reason.as_deref(),
        Some("rejected: invalid_state: refused")
    );
}

#[tokio::test]
async fn invalid_state_for_a_session_that_the_deck_does_not_know_is_dead() {
    let det = Det::new();
    let (s, st) = setup(&det).await;
    show(&s, &st, &[("s2", SessionState::Exited)]).await;
    let item = queue(&s, &st, follow_up("s1"), TTL_MS).await;
    let dead = rejected(&s, &st, &item, RejectCode::InvalidState).await;
    assert_eq!(dead.state, OutboxState::Dead);
    assert_eq!(
        dead.reason.as_deref(),
        Some("rejected: invalid_state: refused")
    );
}

#[tokio::test]
async fn another_rejection_while_the_session_is_exited_is_dead() {
    let det = Det::new();
    let (s, st) = setup(&det).await;
    show(&s, &st, &[("s1", SessionState::Exited)]).await;
    let item = queue(&s, &st, follow_up("s1"), TTL_MS).await;
    let dead = rejected(&s, &st, &item, RejectCode::Forbidden).await;
    assert_eq!(dead.state, OutboxState::Dead);
    assert_eq!(dead.reason.as_deref(), Some("rejected: forbidden: refused"));
}

#[tokio::test]
async fn a_stop_rejected_while_its_session_is_exited_is_dead() {
    let det = Det::new();
    let (s, st) = setup(&det).await;
    show(&s, &st, &[("s1", SessionState::Exited)]).await;
    let stop = payload(SessionCommand::Stop {
        session_id: "s1".into(),
        how: StopMode::Graceful,
    });
    let item = queue(&s, &st, stop, TTL_MS).await;
    let dead = rejected(&s, &st, &item, RejectCode::InvalidState).await;
    assert_eq!(dead.state, OutboxState::Dead);
    assert_eq!(
        dead.reason.as_deref(),
        Some("rejected: invalid_state: refused")
    );
}

#[tokio::test]
async fn a_held_item_is_released_when_its_session_runs_again() {
    let det = Det::new();
    let (s, st) = setup(&det).await;
    show(
        &s,
        &st,
        &[("s1", SessionState::Exited), ("s2", SessionState::Exited)],
    )
    .await;
    let one = queue(&s, &st, follow_up("s1"), TTL_MS).await;
    let two = queue(&s, &st, follow_up("s2"), TTL_MS).await;
    assert_held(&rejected(&s, &st, &one, RejectCode::InvalidState).await);
    assert_held(&rejected(&s, &st, &two, RejectCode::InvalidState).await);

    // A summary that is still ended releases nothing.
    let inc: IncarnationId = "inc".into();
    s.apply_events(
        st.clone(),
        inc.clone(),
        vec![state_event(1, "s1", SessionState::Lost)],
    )
    .await
    .unwrap();
    assert!(due(&s, &st).await.is_empty());

    det.advance(Duration::from_secs(5));
    s.apply_events(
        st.clone(),
        inc,
        vec![state_event(2, "s1", SessionState::Running)],
    )
    .await
    .unwrap();
    let released = s.outbox_item(one.outbox_id.clone()).await.unwrap().unwrap();
    assert_eq!(released.state, OutboxState::Queued);
    assert_eq!(released.reason, None);
    assert!(released.next_at <= s.now_ms());
    assert_eq!(due(&s, &st).await, vec![one.outbox_id.clone()]);
    assert_held(&s.outbox_item(two.outbox_id).await.unwrap().unwrap());
}

#[tokio::test]
async fn release_held_releases_only_the_named_session() {
    let det = Det::new();
    let (s, st) = setup(&det).await;
    show(
        &s,
        &st,
        &[("s1", SessionState::Exited), ("s2", SessionState::Exited)],
    )
    .await;
    let one = queue(&s, &st, follow_up("s1"), TTL_MS).await;
    let two = queue(&s, &st, follow_up("s2"), TTL_MS).await;
    rejected(&s, &st, &one, RejectCode::InvalidState).await;
    rejected(&s, &st, &two, RejectCode::InvalidState).await;
    assert_eq!(s.release_held(st.clone(), "s2".into()).await.unwrap(), 1);
    assert_eq!(due(&s, &st).await, vec![two.outbox_id]);
    assert_eq!(s.release_held(st, "s2".into()).await.unwrap(), 0);
    assert_held(&s.outbox_item(one.outbox_id).await.unwrap().unwrap());
}

#[tokio::test]
async fn a_held_item_past_its_expiry_is_dead() {
    let det = Det::new();
    let (s, st) = setup(&det).await;
    show(&s, &st, &[("s1", SessionState::Exited)]).await;
    let item = queue(&s, &st, follow_up("s1"), 10_000).await;
    assert_held(&rejected(&s, &st, &item, RejectCode::InvalidState).await);
    det.advance(Duration::from_secs(11));
    assert_eq!(s.reconcile_outbox().await.unwrap(), 1);
    let dead = s.outbox_item(item.outbox_id).await.unwrap().unwrap();
    assert_eq!(dead.state, OutboxState::Dead);
    assert_eq!(dead.reason.as_deref(), Some("expired"));
}

#[test]
fn a_follow_up_and_a_steer_target_their_session() {
    assert_eq!(
        target_session(&follow_up("s1")),
        Some(SessionId::from("s1"))
    );
    let steer = payload(SessionCommand::Steer {
        session_id: "s2".into(),
        input: input(),
    });
    assert_eq!(target_session(&steer), Some(SessionId::from("s2")));
    // The wire shape of the payload.
    let wire = json!({
        "kind": "session",
        "command": { "op": "follow_up", "session_id": "s3", "input": { "text": "hi" } },
    });
    assert_eq!(target_session(&wire), Some(SessionId::from("s3")));
}

#[test]
fn every_other_payload_targets_no_session() {
    let stop = payload(SessionCommand::Stop {
        session_id: "s1".into(),
        how: StopMode::Kill,
    });
    assert_eq!(target_session(&stop), None);
    let interrupt = payload(SessionCommand::Interrupt {
        session_id: "s1".into(),
    });
    assert_eq!(target_session(&interrupt), None);
    let renew = payload(SessionCommand::CoordinatorRenew {
        session_id: "s1".into(),
        grant_id: "g1".to_owned(),
        generation: 1,
    });
    assert_eq!(target_session(&renew), None);
    let revoke = payload(SessionCommand::CoordinatorRevoke {
        session_id: "s1".into(),
        grant_id: "g1".to_owned(),
    });
    assert_eq!(target_session(&revoke), None);
    // A payload that is not a router command.
    assert_eq!(target_session(&json!({ "kind": "configure" })), None);
    assert_eq!(target_session(&json!("text")), None);
}

#[test]
fn a_running_or_starting_session_can_take_input() {
    for (state, want) in [
        (SessionState::Starting, true),
        (SessionState::Running, true),
        (SessionState::Stopping, false),
        (SessionState::Exited, false),
        (SessionState::Lost, false),
    ] {
        assert_eq!(can_take_input(state), want, "{state:?}");
    }
}

#[tokio::test]
async fn a_mail_rejected_while_its_session_is_starting_is_held() {
    let det = Det::new();
    let (s, st) = setup(&det).await;
    show(&s, &st, &[("s1", SessionState::Starting)]).await;
    let item = queue(&s, &st, follow_up("s1"), TTL_MS).await;
    let held = rejected(&s, &st, &item, RejectCode::InvalidState).await;
    assert_held(&held);
    assert!(due(&s, &st).await.is_empty());
}

#[tokio::test]
async fn a_stopping_session_does_not_hold_an_invalid_state_rejection() {
    let det = Det::new();
    let (s, st) = setup(&det).await;
    show(&s, &st, &[("s1", SessionState::Stopping)]).await;
    let item = queue(&s, &st, follow_up("s1"), TTL_MS).await;
    let dead = rejected(&s, &st, &item, RejectCode::InvalidState).await;
    assert_eq!(dead.state, OutboxState::Dead);
    assert_eq!(
        dead.reason.as_deref(),
        Some("rejected: invalid_state: refused")
    );
}

#[tokio::test]
async fn a_starting_summary_releases_a_held_item_and_a_stopping_one_does_not() {
    let det = Det::new();
    let (s, st) = setup(&det).await;
    show(&s, &st, &[("s1", SessionState::Exited)]).await;
    let item = queue(&s, &st, follow_up("s1"), TTL_MS).await;
    assert_held(&rejected(&s, &st, &item, RejectCode::InvalidState).await);

    // A stopping summary releases nothing.
    let inc: IncarnationId = "inc".into();
    s.apply_events(
        st.clone(),
        inc.clone(),
        vec![state_event(2, "s1", SessionState::Stopping)],
    )
    .await
    .unwrap();
    assert_held(
        &s.outbox_item(item.outbox_id.clone())
            .await
            .unwrap()
            .unwrap(),
    );

    det.advance(Duration::from_secs(5));
    s.apply_events(
        st.clone(),
        inc,
        vec![state_event(3, "s1", SessionState::Starting)],
    )
    .await
    .unwrap();
    let released = s
        .outbox_item(item.outbox_id.clone())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(released.state, OutboxState::Queued);
    assert_eq!(released.reason, None);
    assert_eq!(due(&s, &st).await, vec![item.outbox_id]);
}
