//! ADR-006 §8.4: the durable station coordinator outbox (M3 6-5, DL-218).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use fleetdeck_proto::link::Outcome;
use fleetdeck_proto::{CommandId, IncarnationId, Receipt, Reject, RejectCode};
use fleetdeck_testkit::Det;
use serde_json::{Value, json};

use super::bindings::{Binding, BindingState, Bindings, ToolClass};
use super::forward::{LinkForward, Transport};
use super::ingress::Forward;
use crate::store::coordinator_outbox::{OutboxRow, OutboxState};
use crate::store::{Store, StoreConfig};

/// What the fake deck does with the next request.
#[derive(Clone, Copy)]
enum Mode {
    /// The request was written, the link dropped before the answer.
    Drop,
    /// The deck accepts (and deduplicates by `command_id`).
    Accept,
    /// A rejection that the deck does not store.
    Reject(RejectCode),
}

struct FakeDeck {
    mode: Mutex<Mode>,
    sent: Mutex<Vec<(Option<CommandId>, Value)>>,
}

impl FakeDeck {
    fn new(mode: Mode) -> Arc<Self> {
        Arc::new(Self {
            mode: Mutex::new(mode),
            sent: Mutex::default(),
        })
    }

    fn set(&self, mode: Mode) {
        *self.mode.lock().unwrap() = mode;
    }

    fn sent(&self) -> Vec<(Option<CommandId>, Value)> {
        self.sent.lock().unwrap().clone()
    }
}

#[async_trait]
impl Transport for FakeDeck {
    async fn request(
        &self,
        method: &str,
        command_id: Option<&CommandId>,
        params: Value,
    ) -> Result<Outcome, String> {
        assert_eq!(method, "coordinator.call");
        self.sent
            .lock()
            .unwrap()
            .push((command_id.cloned(), params));
        let id = command_id.cloned().unwrap();
        match *self.mode.lock().unwrap() {
            Mode::Drop => Err("link lost".to_owned()),
            Mode::Accept => Ok(Outcome::Ok(json!({
                "command_id": id, "state": "accepted", "station_seq": null,
                "result": null, "reject": null, "at": 1
            }))),
            Mode::Reject(code) => Ok(Outcome::Ok(
                serde_json::to_value(Receipt::rejected(id, Reject::new(code, "no"), 1)).unwrap(),
            )),
        }
    }
}

fn binding() -> Binding {
    Binding {
        binding_id: "coord-1:grant-1".to_owned(),
        grant_id: "grant-1".to_owned(),
        session_id: "coord-1".into(),
        provider_instance: "claude".to_owned(),
        native_id: Some("native-1".to_owned()),
        generation: 1,
        classes: [ToolClass::Mail].into_iter().collect(),
        state: BindingState::Active,
        control_skill: "SKILL".to_owned(),
        kind: fleetdeck_proto::command::GrantKind::Coordinator,
        scope: fleetdeck_proto::command::GrantScope::Lineage,
    }
}

struct Rig {
    det: Det,
    store: Store,
    bindings: Bindings,
    forward: LinkForward,
    deck: Arc<FakeDeck>,
}

async fn rig_on(det: Det, store: Store, mode: Mode) -> Rig {
    let bindings = Bindings::default();
    bindings.insert("digest-1".to_owned(), binding());
    let forward = LinkForward::with_outbox(store.clone(), bindings.clone());
    let deck = FakeDeck::new(mode);
    forward.connect_transport(deck.clone(), IncarnationId::from("deck-inc".to_owned()));
    Rig {
        det,
        store,
        bindings,
        forward,
        deck,
    }
}

async fn rig(mode: Mode) -> Rig {
    let det = Det::at(1_000_000);
    let store = Store::open_in_memory(det.clock(), det.ids(), StoreConfig::default())
        .await
        .unwrap();
    store.recover(Vec::new()).await.unwrap();
    rig_on(det, store, mode).await
}

fn mail(text: &str) -> Value {
    json!({"to": {"station": "st", "session_id": "s1"}, "text": text})
}

impl Rig {
    async fn call(&self, args: Value) -> Result<Value, String> {
        let caller = self.bindings.get("coord-1:grant-1").unwrap();
        let caller = super::bindings::Caller {
            binding_id: caller.binding_id,
            grant_id: caller.grant_id,
            session_id: caller.session_id,
            provider_instance: caller.provider_instance,
            native_id: caller.native_id.unwrap(),
            generation: caller.generation,
            input_any: false,
        };
        self.forward.call(caller, "send_mail", args).await
    }

    async fn row(&self, id: &str) -> OutboxRow {
        self.store
            .coordinator_row(CommandId::from(id.to_owned()))
            .await
            .unwrap()
            .unwrap()
    }
}

fn id_of(v: &Value) -> String {
    v["command_id"].as_str().unwrap().to_owned()
}

#[tokio::test]
async fn a_call_sent_before_a_link_loss_is_sent_again_with_the_same_envelope_and_deduplicated() {
    let r = rig(Mode::Drop).await;
    let first = r.call(mail("hi")).await.unwrap();
    assert_eq!(first["state"], "pending", "{first}");
    let id = id_of(&first);
    assert_eq!(r.row(&id).await.sends, 1);

    r.deck.set(Mode::Accept);
    r.forward.flush().await.unwrap();
    let sent = r.deck.sent();
    assert_eq!(sent.len(), 2);
    assert_eq!(sent[0], sent[1], "the same id and the same envelope bytes");
    assert_eq!(sent[0].0.as_ref().unwrap().as_str(), id);
    let row = r.row(&id).await;
    assert_eq!(row.state, OutboxState::Done);
    assert!(row.receipt.is_some());

    // A done row is never sent again.
    r.forward.flush().await.unwrap();
    assert_eq!(r.deck.sent().len(), 2);
}

#[tokio::test]
async fn a_call_without_a_receipt_after_retry_until_is_ambiguous_and_never_sent_again() {
    let r = rig(Mode::Drop).await;
    let id = id_of(&r.call(mail("hi")).await.unwrap());
    r.det.advance(Duration::from_secs(16 * 60));
    r.deck.set(Mode::Accept);
    r.forward.flush().await.unwrap();
    let row = r.row(&id).await;
    assert_eq!(row.state, OutboxState::Ambiguous);
    assert_eq!(row.reason.as_deref(), Some("expired"));
    r.forward.flush().await.unwrap();
    assert_eq!(r.deck.sent().len(), 1, "no send after the deadline");
}

#[tokio::test]
async fn a_never_sent_call_after_retry_until_is_failed_expired_and_never_sent() {
    let r = rig(Mode::Accept).await;
    // The link is down from intake to the deadline.
    let serial = r
        .forward
        .connect_transport(r.deck.clone(), IncarnationId::from("deck-inc".to_owned()));
    r.forward.disconnect(serial);
    let v = r.call(mail("hi")).await.unwrap();
    assert_eq!(v["state"], "pending");
    let id = id_of(&v);
    assert_eq!(r.row(&id).await.sends, 0);
    r.det.advance(Duration::from_secs(16 * 60));
    r.forward.flush().await.unwrap();
    let row = r.row(&id).await;
    assert_eq!(row.state, OutboxState::Failed);
    assert_eq!(row.reason.as_deref(), Some("expired"));
    r.forward
        .connect_transport(r.deck.clone(), IncarnationId::from("deck-inc".to_owned()));
    r.forward.flush().await.unwrap();
    assert!(r.deck.sent().is_empty());
}

#[tokio::test]
async fn a_crash_at_the_transmission_boundary_makes_the_row_ambiguous_never_failed() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("station.db");
    let det = Det::at(1_000_000);
    let open = || Store::open(&path, det.clock(), det.ids(), StoreConfig::default());
    let store = open().await.unwrap();
    store.recover(Vec::new()).await.unwrap();
    let r = rig_on(det.clone(), store.clone(), Mode::Drop).await;

    // Fault point 1: `sent_at` committed, the network write never happened.
    r.forward.disconnect(1);
    let before_write = id_of(&r.call(mail("one")).await.unwrap());
    store
        .coordinator_mark_sent(CommandId::from(before_write.clone()))
        .await
        .unwrap()
        .unwrap();
    // Fault point 2: the write happened, the answer never came.
    r.forward
        .connect_transport(r.deck.clone(), IncarnationId::from("deck-inc".to_owned()));
    let after_write = id_of(&r.call(mail("two")).await.unwrap());
    assert_eq!(r.deck.sent().len(), 1);
    // A row that never left, for contrast.
    r.forward.disconnect(2);
    let never = id_of(&r.call(mail("three")).await.unwrap());

    // The station restarts: start recovery classifies the rows.
    let again = open().await.unwrap();
    again.recover(Vec::new()).await.unwrap();
    for id in [&before_write, &after_write] {
        let row = again
            .coordinator_row(CommandId::from(id.clone()))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.state, OutboxState::Ambiguous, "{id}");
        assert_eq!(row.reason.as_deref(), Some("station_restart"));
    }
    let row = again
        .coordinator_row(CommandId::from(never))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.state, OutboxState::Failed);
}

#[tokio::test]
async fn incarnation_mismatch_or_expired_is_failed_before_any_send_and_ambiguous_after_one() {
    let r = rig(Mode::Reject(RejectCode::IncarnationMismatch)).await;
    let err = r.call(mail("one")).await.unwrap_err();
    assert!(err.starts_with("deck_reset"), "{err}");

    r.deck.set(Mode::Drop);
    let id = id_of(&r.call(mail("two")).await.unwrap());
    r.deck.set(Mode::Reject(RejectCode::Expired));
    r.forward.flush().await.unwrap();
    let row = r.row(&id).await;
    assert_eq!(row.state, OutboxState::Ambiguous);
    assert_eq!(row.reason.as_deref(), Some("expired"));

    r.deck.set(Mode::Drop);
    let id = id_of(&r.call(mail("three")).await.unwrap());
    r.deck.set(Mode::Reject(RejectCode::IncarnationMismatch));
    r.forward.flush().await.unwrap();
    assert_eq!(r.row(&id).await.state, OutboxState::Ambiguous);
    // A stored rejection is definitive: `done`.
    r.deck.set(Mode::Reject(RejectCode::InvalidCommand));
    let v = r.call(mail("four")).await.unwrap();
    assert_eq!(v["state"], "rejected", "{v}");
}

#[tokio::test]
async fn the_same_idempotency_key_returns_the_first_command_and_another_payload_is_refused() {
    let r = rig(Mode::Accept).await;
    let mut args = mail("hi");
    args["idempotency_key"] = json!("k-1");
    let first = r.call(args.clone()).await.unwrap();
    let again = r.call(args.clone()).await.unwrap();
    assert_eq!(id_of(&first), id_of(&again));
    assert_eq!(r.deck.sent().len(), 1);
    let sent: Value = r.deck.sent()[0].1.clone();
    assert_eq!(sent["call"]["idempotency_key"], "k-1");

    let mut other = mail("other text");
    other["idempotency_key"] = json!("k-1");
    let err = r.call(other).await.unwrap_err();
    assert!(err.starts_with("idempotency_key_reused"), "{err}");

    let mut long = mail("hi");
    long["idempotency_key"] = json!("k".repeat(65));
    assert!(
        r.call(long)
            .await
            .unwrap_err()
            .starts_with("invalid_params")
    );
    assert_eq!(r.deck.sent().len(), 1);
}

#[tokio::test]
async fn a_revocation_fails_queued_unsent_calls_without_a_send_and_makes_sent_ones_ambiguous_without_a_resend()
 {
    let r = rig(Mode::Drop).await;
    let sent = id_of(&r.call(mail("one")).await.unwrap());
    r.forward.disconnect(1);
    let queued = id_of(&r.call(mail("two")).await.unwrap());
    r.bindings.revoke_session(&"coord-1".into());

    r.deck.set(Mode::Accept);
    r.forward
        .connect_transport(r.deck.clone(), IncarnationId::from("deck-inc".to_owned()));
    r.forward.flush().await.unwrap();
    let row = r.row(&sent).await;
    assert_eq!(row.state, OutboxState::Ambiguous);
    assert_eq!(row.reason.as_deref(), Some("revoked"));
    let row = r.row(&queued).await;
    assert_eq!(row.state, OutboxState::Failed);
    assert_eq!(row.reason.as_deref(), Some("revoked"));
    assert_eq!(r.deck.sent().len(), 1, "no resend, no first send");
}

#[tokio::test]
async fn sent_rows_are_not_resent_after_a_reconnect_before_the_lifecycle_watermark_is_acknowledged()
{
    let r = rig(Mode::Drop).await;
    let sent = id_of(&r.call(mail("one")).await.unwrap());
    // Link loss, then a station-side invalidation (a native replacement
    // raises nothing; the binding is no longer active), then a reconnect.
    r.forward.disconnect(1);
    r.bindings.update("coord-1:grant-1", |b| {
        b.state = BindingState::Revoked;
    });
    r.deck.set(Mode::Accept);
    r.forward
        .connect_transport(r.deck.clone(), IncarnationId::from("deck-inc".to_owned()));
    r.forward.flush().await.unwrap();
    r.forward.flush().await.unwrap();
    assert_eq!(r.row(&sent).await.state, OutboxState::Ambiguous);
    // A call that the old caller identity still makes never leaves.
    let err = r.call(mail("late")).await.unwrap_err();
    assert!(err.starts_with("revoked"), "{err}");
    assert_eq!(r.deck.sent().len(), 1);
}

/// ADR-006 §6.5 and test 25e, station half (DL-220): after a (re)connect no
/// `coordinator.call` and no `coordinator.query` leaves before the deck
/// acknowledges the station event stream through the last coordinator
/// lifecycle event; an acknowledgment below the watermark or of an older
/// link does not open the fence, and each new link closes it again.
#[tokio::test]
async fn a_reconnect_holds_calls_and_queries_until_the_deck_acknowledges_the_lifecycle_watermark() {
    use crate::store::{NewEvent, NewSession, OfflineApprovalPolicy};
    use fleetdeck_proto::Event;
    use fleetdeck_proto::event::ProviderKind;

    let r = rig(Mode::Accept).await;
    r.store
        .create_session(NewSession {
            session_id: "coord-1".into(),
            provider: ProviderKind::Claude,
            provider_instance: "claude".into(),
            cwd: "/w".to_owned(),
            worktree: None,
            branch: None,
            mode: None,
            title: None,
            offline_approval_policy: OfflineApprovalPolicy::Wait,
        })
        .await
        .unwrap();
    let bound = Event::CoordinatorBound {
        binding_id: "coord-1:grant-1".to_owned(),
        grant_id: "grant-1".to_owned(),
        generation: 1,
        provider_instance: "claude".to_owned(),
        native_id: "native-1".to_owned(),
    };
    let appended = r
        .store
        .append("coord-1".into(), vec![NewEvent::from(bound)])
        .await
        .unwrap();
    let watermark = appended[0].station_seq;
    assert_eq!(
        r.store.coordinator_lifecycle_seq().await.unwrap(),
        watermark
    );

    let deck_inc = || IncarnationId::from("deck-inc".to_owned());
    let serial = r.forward.connect_transport(r.deck.clone(), deck_inc());
    let held = r.call(mail("one")).await.unwrap();
    assert_eq!(held["state"], "pending", "{held}");
    let id = id_of(&held);
    let caller = super::bindings::Caller {
        binding_id: "coord-1:grant-1".to_owned(),
        grant_id: "grant-1".to_owned(),
        session_id: "coord-1".into(),
        provider_instance: "claude".to_owned(),
        native_id: "native-1".to_owned(),
        generation: 1,
        input_any: false,
    };
    let query = r
        .forward
        .call(caller, "wait_task", json!({"task_id": "t1"}))
        .await
        .unwrap_err();
    assert!(query.starts_with("deck_syncing"), "{query}");

    // Below the watermark, or on an older link: still held.
    r.forward.acknowledged(serial, watermark - 1);
    r.forward.acknowledged(serial - 1, watermark);
    r.forward.flush().await.unwrap();
    assert!(r.deck.sent().is_empty(), "nothing leaves before the ack");
    assert_eq!(r.row(&id).await.sends, 0);

    // The acknowledgment of the watermark opens the fence.
    r.forward.acknowledged(serial, watermark);
    r.forward.flush().await.unwrap();
    assert_eq!(r.deck.sent().len(), 1);
    assert_eq!(r.row(&id).await.state, OutboxState::Done);

    // A new link closes it again.
    r.forward.connect_transport(r.deck.clone(), deck_inc());
    let again = r.call(mail("two")).await.unwrap();
    assert_eq!(again["state"], "pending", "{again}");
    assert_eq!(r.deck.sent().len(), 1);
}

/// R2.3: a worker's question is one direct call. It is not queued, and
/// with the link down it is `unavailable`, not `pending`.
#[tokio::test]
async fn a_worker_question_bypasses_the_outbox() {
    let r = rig(Mode::Accept).await;
    let worker = super::bindings::Caller {
        binding_id: "w-1".to_owned(),
        grant_id: String::new(),
        session_id: "worker-1".into(),
        provider_instance: "claude".to_owned(),
        native_id: "native-w".to_owned(),
        generation: 1,
        input_any: false,
    };
    let q = json!({"question": "which branch?"});
    r.forward
        .call(worker.clone(), "ask_coordinator", q.clone())
        .await
        .unwrap();
    assert_eq!(r.deck.sent().len(), 1);
    assert!(r.store.coordinator_pending().await.unwrap().is_empty());
    let id = r.deck.sent()[0].0.clone().unwrap();
    assert!(r.store.coordinator_row(id).await.unwrap().is_none());

    let down = rig(Mode::Accept).await;
    let serial = down.forward.connect_transport(
        down.deck.clone(),
        IncarnationId::from("deck-inc".to_owned()),
    );
    down.forward.disconnect(serial);
    let e = down
        .forward
        .call(worker, "ask_coordinator", q)
        .await
        .map(|_| ())
        .unwrap_err();
    assert_eq!(e, "unavailable: the deck is not connected");
    assert!(down.deck.sent().is_empty());
    assert!(down.store.coordinator_pending().await.unwrap().is_empty());
}

/// DL-251 item a: a transmission future that is dropped (the caller closed
/// the connection) releases its claim, so a later flush can send the row.
#[tokio::test]
async fn a_dropped_transmission_releases_its_claim() {
    let r = rig(Mode::Accept).await;
    let outbox = super::outbox::CallOutbox::new(r.store.clone(), r.bindings.clone());
    let id = CommandId::from("cmd-claim");
    let first = outbox.claim(&id).expect("a free row is claimed");
    assert!(outbox.claim(&id).is_none(), "one transmission at a time");
    drop(first);
    let held = async {
        let _claim = outbox.claim(&id).expect("released after the drop");
        std::future::pending::<()>().await;
    };
    assert!(
        tokio::time::timeout(Duration::from_millis(20), held)
            .await
            .is_err()
    );
    assert!(
        outbox.claim(&id).is_some(),
        "the dropped future released its claim"
    );
}
