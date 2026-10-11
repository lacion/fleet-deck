//! The station link service over a loopback link (M2 P3-5): the event
//! stream with incarnation, replay, snapshot batches and acks (G4); command
//! intake with `target_incarnation` / `retry_until` and the pin and grant
//! re-check in the decision transaction (G3, G5); `link.rotate_key`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Duration;

use fleetdeck_core::identity::Identity;
use fleetdeck_link::control::{Control, Inbound};
use fleetdeck_link::hello::LocalHello;
use fleetdeck_link::node::{self, NodeConfig};
use fleetdeck_link::revoke::{REVOKED, revoke, sign_rotation};
use fleetdeck_link::tls::Role;
use fleetdeck_link::{Epochs, Grants, LinkListener, LiveLink};
use fleetdeck_proto::command::{
    CommandOrigin, RETRY_WINDOW_MS, ReceiptState, SessionCommand, UserInput,
};
use fleetdeck_proto::event::{Event, ProviderKind, RunOrigin};
use fleetdeck_proto::link::{
    ControlFrame, Outcome, ReplayParams, Scope, StationInfo, StreamMode, StreamStart,
};
use fleetdeck_proto::mutation::{KeyRotationEnvelope, StationChange, StationConfigureEnvelope};
use fleetdeck_proto::{CommandEnvelope, CommandId, IncarnationId, Receipt, RejectCode};
use fleetdeck_station::link::{self, NOT_IMPLEMENTED, StationLink, StationPeers};
use fleetdeck_station::manager::{Adapters, Policy, SessionManager};
use fleetdeck_station::store::peers::PairedKey;
use fleetdeck_station::store::{NewEvent, NewSession, OfflineApprovalPolicy, Store, StoreConfig};
use fleetdeck_testkit::Det;
use serde_json::{Value, json};

const T: Duration = Duration::from_secs(10);

struct Fixture {
    _det: Det,
    store: Store,
    ctx: Arc<StationLink>,
    listener: LinkListener,
    deck: Arc<Identity>,
    deck_node: NodeConfig,
}

fn deck_node(identity: Arc<Identity>, station: &Identity) -> NodeConfig {
    let node = NodeConfig::new(
        identity,
        LocalHello::new(Role::Deck, IncarnationId::from("deck-inc")),
        Arc::new(Epochs::in_memory()),
    )
    .with_store(Grants::new());
    node.pins.pin(station.public_key(), Role::Station);
    node
}

async fn fixture() -> Fixture {
    let det = Det::new();
    let store = Store::open_in_memory(det.clock(), det.ids(), StoreConfig::default())
        .await
        .unwrap();
    let policy = Policy::new(Vec::new(), 4, OfflineApprovalPolicy::Wait, BTreeMap::new());
    let manager = SessionManager::start(store.clone(), policy, Adapters::new(), det.ids())
        .await
        .unwrap();
    let deck = Arc::new(Identity::generate().unwrap());
    store
        .pin_peer(PairedKey {
            key: deck.public_key(),
            role: Role::Deck,
            scopes: BTreeSet::from([Scope::Read, Scope::Drive, Scope::Approve, Scope::Admin]),
            dial_url: None,
        })
        .await
        .unwrap();
    let peers = StationPeers::load(store.clone()).await.unwrap();
    let station_id = Arc::new(Identity::generate().unwrap());
    let node = NodeConfig::new(
        Arc::clone(&station_id),
        LocalHello::new(Role::Station, store.incarnation().clone()),
        Arc::new(Epochs::in_memory()),
    )
    .with_store(Arc::clone(&peers));
    peers.pin_all(&node.pins).await.unwrap();
    let listener = node::listen(node.clone(), "127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    let deck_node = deck_node(Arc::clone(&deck), &station_id);
    let ctx = Arc::new(StationLink {
        manager: Arc::new(manager),
        node,
        peers,
        name: "box".to_owned(),
    });
    Fixture {
        _det: det,
        store,
        ctx,
        listener,
        deck,
        deck_node,
    }
}

impl Fixture {
    /// The deck dials; the station serves the link. Returns the deck side.
    async fn connect(&mut self) -> LiveLink {
        let deck = self.deck_node.clone();
        self.connect_as(&deck).await
    }

    async fn connect_as(&mut self, deck: &NodeConfig) -> LiveLink {
        let peer = self.ctx.node.identity.public_key();
        let (d, a) = tokio::time::timeout(T, async {
            tokio::join!(
                node::dial(deck, self.listener.local_addr, peer),
                self.listener.next_link()
            )
        })
        .await
        .expect("link setup");
        let at_station = fleetdeck_link::start(&self.ctx.node, a.expect("listener"));
        tokio::spawn(link::serve(Arc::clone(&self.ctx), at_station));
        fleetdeck_link::start(deck, d.unwrap())
    }

    async fn session(&self, id: &str) {
        self.store
            .create_session(NewSession {
                session_id: id.into(),
                provider: ProviderKind::Claude,
                provider_instance: "claude".into(),
                cwd: "/w".to_owned(),
                worktree: None,
                mode: None,
                title: None,
                offline_approval_policy: OfflineApprovalPolicy::Wait,
            })
            .await
            .unwrap();
    }

    async fn event(&self, id: &str, n: u32) -> u64 {
        let e: NewEvent = Event::RunStarted {
            run_id: format!("run-{n}").into(),
            input_summary: "hi".to_owned(),
            origin: RunOrigin::Board,
        }
        .into();
        self.store.append(id.into(), vec![e]).await.unwrap()[0].station_seq
    }

    fn inc(&self) -> IncarnationId {
        self.store.incarnation().clone()
    }

    fn prompt(&self, id: &str, target: Option<IncarnationId>, retry_until: i64) -> CommandEnvelope {
        CommandEnvelope {
            command_id: CommandId::from(id),
            target_incarnation: target,
            retry_until,
            origin: CommandOrigin::Board,
            command: SessionCommand::Prompt {
                session_id: "nope".into(),
                input: UserInput {
                    text: "hi".to_owned(),
                },
            },
        }
    }

    fn later(&self) -> i64 {
        self.store.now_ms() + RETRY_WINDOW_MS
    }
}

async fn frame(l: &mut LiveLink) -> ControlFrame {
    match tokio::time::timeout(T, l.inbound.recv())
        .await
        .expect("frame")
    {
        Some(Inbound::Frame(f)) => *f,
        other => panic!("expected a frame, got {other:?}"),
    }
}

fn ok(o: Outcome) -> Value {
    match o {
        Outcome::Ok(v) => v,
        Outcome::Err(e) => panic!("request failed: {e}"),
    }
}

async fn call(c: &Control, method: &str, params: Value) -> Outcome {
    tokio::time::timeout(T, c.request(method, None, params))
        .await
        .expect("response")
        .unwrap()
}

async fn mutate(c: &Control, method: &str, id: &str, params: impl serde::Serialize) -> Receipt {
    let cid = CommandId::from(id);
    let o = tokio::time::timeout(
        T,
        c.request(method, Some(&cid), serde_json::to_value(params).unwrap()),
    )
    .await
    .expect("response")
    .unwrap();
    serde_json::from_value(ok(o)).unwrap()
}

fn start(o: Outcome) -> StreamStart {
    serde_json::from_value(ok(o)).unwrap()
}

#[tokio::test]
async fn a_replay_sends_the_events_after_the_cursor_then_live_events_and_acks_keep_the_link() {
    let mut f = fixture().await;
    f.session("s1").await;
    let first = f.event("s1", 1).await;
    let second = f.event("s1", 2).await;
    let mut deck = f.connect().await;
    let replay = ReplayParams {
        incarnation: Some(f.inc()),
        after: first,
    };
    let s = start(call(&deck.control, "events.replay", json!(replay)).await);
    assert_eq!(
        s,
        StreamStart {
            mode: StreamMode::Replay,
            incarnation: f.inc(),
            from: first
        }
    );
    let ControlFrame::Event {
        incarnation,
        envelope,
    } = frame(&mut deck).await
    else {
        panic!("expected an event");
    };
    assert_eq!((incarnation, envelope.station_seq), (f.inc(), second));

    // A commit after the replay arrives live.
    let third = f.event("s1", 3).await;
    let ControlFrame::Event { envelope, .. } = frame(&mut deck).await else {
        panic!("expected an event");
    };
    assert_eq!(envelope.station_seq, third);

    deck.control
        .send(ControlFrame::Ack {
            incarnation: f.inc(),
            station_seq: third,
        })
        .await
        .unwrap();
    let info: StationInfo =
        serde_json::from_value(ok(call(&deck.control, "station.info", json!({})).await)).unwrap();
    assert_eq!((info.name.as_str(), info.head), ("box", third));
    assert_eq!(info.incarnation, f.inc());
    assert!(deck.control.ended().is_none(), "a valid ack keeps the link");
}

async fn expect_snapshot(
    deck: &mut LiveLink,
    inc: &IncarnationId,
    watermark: u64,
    sessions: &[&str],
) {
    assert_eq!(
        frame(deck).await,
        ControlFrame::SnapshotBegin {
            incarnation: inc.clone(),
            watermark,
            session_count: sessions.len() as u64
        }
    );
    for id in sessions {
        let ControlFrame::SnapshotSession {
            incarnation,
            snapshot,
        } = frame(deck).await
        else {
            panic!("expected a snapshot session");
        };
        assert_eq!(&incarnation, inc);
        assert_eq!(snapshot.summary.session_id.as_str(), *id);
    }
    assert_eq!(
        frame(deck).await,
        ControlFrame::SnapshotEnd {
            incarnation: inc.clone(),
            watermark
        }
    );
}

#[tokio::test]
async fn another_incarnation_a_cursor_ahead_or_session_snapshot_gets_a_snapshot_batch() {
    let mut f = fixture().await;
    f.session("s1").await;
    f.session("s2").await;
    let head = f.event("s2", 1).await;
    let mut deck = f.connect().await;
    let inc = f.inc();

    let cases = [
        (
            "events.replay",
            json!(ReplayParams {
                incarnation: Some("an-old-database".into()),
                after: head,
            }),
        ),
        (
            "events.replay",
            json!(ReplayParams {
                incarnation: None,
                after: 0
            }),
        ),
        (
            "events.replay",
            json!(ReplayParams {
                incarnation: Some(inc.clone()),
                after: head + 10,
            }),
        ),
        ("session.snapshot", json!({})),
    ];
    for (method, params) in cases {
        let s = start(call(&deck.control, method, params.clone()).await);
        assert_eq!(s.mode, StreamMode::Snapshot, "{method} {params}");
        assert_eq!((s.incarnation.clone(), s.from), (inc.clone(), head));
        expect_snapshot(&mut deck, &inc, head, &["s1", "s2"]).await;
    }
    // After the batch, the stream continues from the watermark.
    let next = f.event("s1", 2).await;
    let ControlFrame::Event { envelope, .. } = frame(&mut deck).await else {
        panic!("expected an event");
    };
    assert_eq!(envelope.station_seq, next);
    deck.control
        .send(ControlFrame::Ack {
            incarnation: inc,
            station_seq: next,
        })
        .await
        .unwrap();
    call(&deck.control, "session.list", json!({})).await;
    assert!(deck.control.ended().is_none());
}

#[tokio::test]
async fn an_ack_above_the_delivered_watermark_or_of_another_incarnation_closes_the_link() {
    let mut f = fixture().await;
    f.session("s1").await;
    let head = f.event("s1", 1).await;
    let inc = f.inc();
    for ack in [
        // Nothing was delivered on this link yet.
        ControlFrame::Ack {
            incarnation: inc.clone(),
            station_seq: head,
        },
        ControlFrame::Ack {
            incarnation: "other".into(),
            station_seq: 0,
        },
    ] {
        let deck = f.connect().await;
        deck.control.send(ack.clone()).await.unwrap();
        let closing = tokio::time::timeout(T, deck.control.closed())
            .await
            .expect("the link closes");
        assert_eq!(
            closing.reason, "protocol_error",
            "{ack:?}: {}",
            closing.detail
        );
    }
}

#[tokio::test]
async fn commands_follow_the_receipt_rules_and_need_target_incarnation_over_a_link() {
    let mut f = fixture().await;
    let deck = f.connect().await;
    let c = &deck.control;
    let inc = f.inc();

    // Stored: an unknown session is a stored rejection; a resend returns it.
    let first = f.prompt("c1", Some(inc.clone()), f.later());
    let r = mutate(c, "session.prompt", "c1", &first).await;
    assert_eq!(r.reject.as_ref().unwrap().code, RejectCode::UnknownSession);
    let again = mutate(c, "session.prompt", "c1", &first).await;
    assert_eq!(again, r);
    assert_eq!(f.store.receipt("c1".into()).await.unwrap(), Some(r.clone()));
    let polled: Option<Receipt> = serde_json::from_value(ok(call(
        c,
        "command.receipt",
        json!({ "command_id": "c1" }),
    )
    .await))
    .unwrap();
    assert_eq!(polled, Some(r));

    // Not stored: no target, another incarnation, past `retry_until`.
    let past = f.store.now_ms() - 3 * 60 * 1000;
    for (id, target, until, code) in [
        ("c2", None, f.later(), RejectCode::InvalidCommand),
        (
            "c3",
            Some("old".into()),
            f.later(),
            RejectCode::IncarnationMismatch,
        ),
        ("c4", Some(inc.clone()), past, RejectCode::Expired),
    ] {
        let r = mutate(c, "session.prompt", id, f.prompt(id, target, until)).await;
        assert_eq!(r.state, ReceiptState::Rejected, "{id}");
        assert_eq!(r.reject.unwrap().code, code, "{id}");
        assert_eq!(f.store.receipt(id.into()).await.unwrap(), None, "{id}");
    }
    // A frame command_id that differs from the envelope's.
    let other = f.prompt("c6", Some(inc.clone()), f.later());
    let r = mutate(c, "session.prompt", "c5", other).await;
    assert_eq!(r.reject.unwrap().code, RejectCode::InvalidCommand);

    // DL-047: `station.configure` is decided and refused as unsupported.
    let configure = StationConfigureEnvelope {
        command_id: "c7".into(),
        target_incarnation: inc.clone(),
        retry_until: f.later(),
        change: StationChange::SetMaxSessions { max_sessions: 1 },
    };
    let r = mutate(c, "station.configure", "c7", configure).await;
    assert_eq!(r.reject.unwrap().code, RejectCode::Unsupported);

    // Phase 6 serves artifacts.
    let o = call(c, "artifact.open", json!({})).await;
    assert!(matches!(o, Outcome::Err(e) if e.code == NOT_IMPLEMENTED));
}

#[tokio::test]
async fn a_command_queued_after_forget_deck_is_forbidden_and_the_link_closes_within_one_second() {
    let mut f = fixture().await;
    let deck = f.connect().await;
    let inc = f.inc();

    // The store revocation commits before the link layer closes the link:
    // a command that arrives in this window passes the cached request
    // check, and the decision transaction refuses it.
    assert!(f.store.revoke_peer(f.deck.public_key()).await.unwrap());
    let queued = f.prompt("q1", Some(inc), f.later());
    let r = mutate(&deck.control, "session.prompt", "q1", queued).await;
    assert_eq!(r.reject.unwrap().code, RejectCode::Forbidden);
    assert_eq!(f.store.receipt("q1".into()).await.unwrap(), None);

    // `forget-deck` through the link layer closes the link within 1 s.
    let t0 = tokio::time::Instant::now();
    revoke(&f.ctx.node, &f.deck.public_key()).await;
    let closing = tokio::time::timeout(Duration::from_secs(1), deck.control.closed())
        .await
        .expect("closed within 1 s");
    assert_eq!(closing.reason, REVOKED);
    assert!(t0.elapsed() < Duration::from_secs(1));
    assert!(f.store.paired_keys().await.unwrap().is_empty());
}

#[tokio::test]
async fn a_key_rotation_moves_role_and_grants_in_one_transaction_and_the_new_key_links() {
    let mut f = fixture().await;
    let deck = f.connect().await;
    let new = Arc::new(Identity::generate().unwrap());
    let station = f.ctx.node.identity.public_key();
    let cid = CommandId::from("rot1");
    let envelope = KeyRotationEnvelope {
        command_id: cid.clone(),
        target_incarnation: f.inc(),
        retry_until: f.later(),
        rotation: sign_rotation(&f.deck, &new.public_key(), &station, &cid),
    };
    let r = mutate(&deck.control, "link.rotate_key", "rot1", envelope).await;
    assert_eq!(r.state, ReceiptState::Accepted, "{r:?}");
    let closing = tokio::time::timeout(Duration::from_secs(1), deck.control.closed())
        .await
        .expect("the old key's link closes");
    assert_eq!(closing.reason, REVOKED);

    let keys = f.store.paired_keys().await.unwrap();
    assert_eq!(keys.len(), 1);
    assert_eq!(keys[0].key, new.public_key());
    assert_eq!(keys[0].role, Role::Deck);
    assert_eq!(keys[0].scopes.len(), 4);
    assert_eq!(f.store.revocation_generation().await.unwrap(), 1);
    assert_eq!(f.ctx.node.pins.role_of(&f.deck.public_key()), None);

    // The new key links and keeps the old key's grants.
    let identity = Arc::clone(&f.ctx.node.identity);
    let new_node = deck_node(new, &identity);
    let deck2 = f.connect_as(&new_node).await;
    let info = call(&deck2.control, "station.info", json!({})).await;
    assert!(matches!(info, Outcome::Ok(_)));
}
