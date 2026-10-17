//! The cursor of `events.replay` acknowledges the deck's events (ADR-006
//! §6.5, test 25e): a deck that restarts caught up gets no event and sends no
//! `ack`, so the replay cursor itself lifts the lifecycle fence of the
//! coordinator outbox. A replay that is not a valid acknowledgment (another
//! incarnation, no incarnation, a cursor above the head) lifts nothing.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Duration;

use fleetdeck_core::identity::Identity;
use fleetdeck_link::control::{Control, Inbound, InboundRequest};
use fleetdeck_link::hello::LocalHello;
use fleetdeck_link::node::{self, NodeConfig};
use fleetdeck_link::tls::Role;
use fleetdeck_link::{Epochs, Grants, LinkListener, LiveLink};
use fleetdeck_proto::IncarnationId;
use fleetdeck_proto::command::GrantScope;
use fleetdeck_proto::event::{Event, ProviderKind, RunOrigin};
use fleetdeck_proto::link::{ControlFrame, Outcome, ReplayParams, Scope};
use fleetdeck_station::coordinator::{
    Binding, BindingState, Bindings, Caller, Forward, LinkForward, ToolClass,
};
use fleetdeck_station::link::{self, StationLink, StationPeers};
use fleetdeck_station::manager::{Adapters, Policy, SessionManager};
use fleetdeck_station::shell::ShellConfig;
use fleetdeck_station::store::peers::PairedKey;
use fleetdeck_station::store::{NewEvent, NewSession, OfflineApprovalPolicy, Store, StoreConfig};
use fleetdeck_testkit::Det;
use serde_json::{Value, json};

const T: Duration = Duration::from_secs(10);
const QUIET: Duration = Duration::from_millis(300);

struct Fixture {
    _det: Det,
    store: Store,
    ctx: Arc<StationLink>,
    listener: LinkListener,
    deck_node: NodeConfig,
    /// The highest sequence of the station log.
    head: u64,
}

fn caller() -> Caller {
    Caller {
        binding_id: "coord-1:g1".to_owned(),
        grant_id: "g1".to_owned(),
        session_id: "coord-1".into(),
        provider_instance: "claude".to_owned(),
        native_id: "native-1".to_owned(),
        generation: 1,
    }
}

async fn fixture() -> Fixture {
    let det = Det::new();
    let store = Store::open_in_memory(det.clock(), det.ids(), StoreConfig::default())
        .await
        .unwrap();
    let policy = Policy::new(Vec::new(), 4, OfflineApprovalPolicy::Wait, BTreeMap::new());
    let manager = SessionManager::start(
        store.clone(),
        policy,
        Adapters::new(),
        det.ids(),
        Arc::new(fleetdeck_station::workspace::PrivatePaths::default()),
    )
    .await
    .unwrap();

    // The coordinator session: a `coordinator.bound` event (the lifecycle
    // watermark), then one more event, so that the head is above it.
    store
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
        binding_id: "coord-1:g1".to_owned(),
        grant_id: "g1".to_owned(),
        generation: 1,
        provider_instance: "claude".to_owned(),
        native_id: "native-1".to_owned(),
    };
    let watermark = store
        .append("coord-1".into(), vec![NewEvent::from(bound)])
        .await
        .unwrap()[0]
        .station_seq;
    let run: NewEvent = Event::RunStarted {
        author: None,
        input_id: None,
        run_id: "run-1".into(),
        input_summary: "hi".to_owned(),
        origin: RunOrigin::Board,
        input_text: None,
    }
    .into();
    let head = store.append("coord-1".into(), vec![run]).await.unwrap()[0].station_seq;
    assert!(head > watermark);

    let bindings: Bindings = manager.coordinator_bindings().clone();
    bindings.insert(
        "digest-1".to_owned(),
        Binding {
            binding_id: "coord-1:g1".to_owned(),
            grant_id: "g1".to_owned(),
            session_id: "coord-1".into(),
            provider_instance: "claude".to_owned(),
            native_id: Some("native-1".to_owned()),
            generation: 1,
            classes: BTreeSet::from([ToolClass::Mail]),
            scope: GrantScope::Lineage,
            state: BindingState::Active,
            control_skill: "skill".to_owned(),
        },
    );

    let deck = Arc::new(Identity::generate().unwrap());
    store
        .pin_peer(PairedKey {
            key: deck.public_key(),
            role: Role::Deck,
            scopes: BTreeSet::from([Scope::Read]),
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
    // The deck lets the station make `coordinator.call` requests.
    let grants = Grants::new();
    grants.grant(station_id.fingerprint(), [Scope::Coordinate]);
    let deck_node = NodeConfig::new(
        deck,
        LocalHello::new(Role::Deck, IncarnationId::from("deck-inc")),
        Arc::new(Epochs::in_memory()),
    )
    .with_store(grants);
    deck_node.pins.pin(station_id.public_key(), Role::Station);
    let ctx = Arc::new(StationLink {
        manager: Arc::new(manager),
        node,
        peers,
        name: "box".to_owned(),
        shells: Arc::new(fleetdeck_station::shell::Shells::new(ShellConfig::default())),
        coordinator: LinkForward::with_outbox(store.clone(), bindings),
        paste_dir: None,
    });
    Fixture {
        _det: det,
        store,
        ctx,
        listener,
        deck_node,
        head,
    }
}

impl Fixture {
    async fn connect(&mut self) -> LiveLink {
        let peer = self.ctx.node.identity.public_key();
        let (d, a) = tokio::time::timeout(T, async {
            tokio::join!(
                node::dial(&self.deck_node, self.listener.local_addr, peer),
                self.listener.next_link()
            )
        })
        .await
        .expect("link setup");
        let at_station = fleetdeck_link::start(&self.ctx.node, a.expect("listener"));
        tokio::spawn(link::serve(Arc::clone(&self.ctx), at_station));
        fleetdeck_link::start(&self.deck_node, d.unwrap())
    }

    fn inc(&self) -> IncarnationId {
        self.store.incarnation().clone()
    }

    /// One `send_mail` call of the coordinator with a fixed key: the first
    /// call stores the row, later calls read it.
    async fn call(&self) -> Value {
        self.ctx
            .coordinator
            .call(caller(), "send_mail", self.mail())
            .await
            .unwrap()
    }

    fn mail(&self) -> Value {
        json!({
            "to": {"station": "st", "session_id": "s1"},
            "text": "hi",
            "idempotency_key": "k1",
        })
    }

    /// The sends of the pending outbox rows (`None` when no row is pending).
    async fn pending_sends(&self) -> Option<u32> {
        self.store
            .coordinator_pending()
            .await
            .unwrap()
            .first()
            .map(|r| r.sends)
    }

    /// The first call, once the station serves the link: the row is stored
    /// `pending` and nothing leaves (the lifecycle fence).
    async fn held_call(&self) {
        let deadline = tokio::time::Instant::now() + T;
        let first = loop {
            let attempt = self
                .ctx
                .coordinator
                .call(caller(), "send_mail", self.mail())
                .await;
            match attempt {
                Ok(v) => break v,
                Err(e) => {
                    assert!(e.starts_with("deck_unreachable"), "{e}");
                    assert!(tokio::time::Instant::now() < deadline, "no deck link");
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            }
        };
        assert_eq!(first["state"], "pending", "{first}");
        self.ctx.coordinator.flush().await.unwrap();
        assert_eq!(self.pending_sends().await, Some(0), "nothing leaves yet");
    }
}

async fn replay(deck: &Control, params: ReplayParams) {
    let o = tokio::time::timeout(
        T,
        deck.request("events.replay", None, serde_json::to_value(params).unwrap()),
    )
    .await
    .expect("response")
    .unwrap();
    assert!(matches!(o, Outcome::Ok(_)), "{o:?}");
}

/// The deck side takes the next `coordinator.call` request, if one comes in
/// `wait`; the other frames (snapshot batches) are skipped.
async fn next_call(deck: &mut LiveLink, wait: Duration) -> Option<InboundRequest> {
    let deadline = tokio::time::Instant::now() + wait;
    loop {
        match tokio::time::timeout_at(deadline, deck.inbound.recv()).await {
            Err(_) => return None,
            Ok(Some(Inbound::Request(req))) => return Some(req),
            Ok(Some(Inbound::Frame(f))) => {
                assert!(
                    matches!(
                        *f,
                        ControlFrame::SnapshotBegin { .. }
                            | ControlFrame::SnapshotSession { .. }
                            | ControlFrame::SnapshotEnd { .. }
                    ),
                    "unexpected frame {f:?}"
                );
            }
            Ok(None) => panic!("the link ended"),
        }
    }
}

#[tokio::test]
async fn a_replay_cursor_at_the_head_lifts_the_lifecycle_fence() {
    let mut f = fixture().await;
    let mut deck = f.connect().await;
    // The deck is caught up: its cursor is the head of the station log, so
    // the replay sends no event and the deck sends no `ack`.
    f.held_call().await;
    replay(
        &deck.control,
        ReplayParams {
            incarnation: Some(f.inc()),
            after: f.head,
        },
    )
    .await;

    // The cursor acknowledged the watermark: the next flush sends the row.
    let ctx = Arc::clone(&f.ctx);
    let flush = tokio::spawn(async move { ctx.coordinator.flush().await });
    let req = next_call(&mut deck, T).await.expect("a coordinator.call");
    assert_eq!(req.method, "coordinator.call");
    let id = req.command_id.clone().unwrap();
    deck.control
        .respond(
            req.id,
            Outcome::Ok(json!({
                "command_id": id, "state": "accepted", "station_seq": null,
                "result": null, "reject": null, "at": 1
            })),
        )
        .await
        .unwrap();
    flush.await.unwrap().unwrap();
    assert_eq!(f.pending_sends().await, None, "the row is settled");
    let done = f.call().await;
    assert_eq!(done["state"], "accepted", "{done}");
}

#[tokio::test]
async fn a_replay_of_another_incarnation_or_above_the_head_acknowledges_nothing() {
    let mut f = fixture().await;
    let mut deck = f.connect().await;
    f.held_call().await;
    let cases = [
        ReplayParams {
            incarnation: Some("an-old-database".into()),
            after: f.head,
        },
        ReplayParams {
            incarnation: None,
            after: f.head,
        },
        ReplayParams {
            incarnation: Some(f.inc()),
            after: f.head + 10,
        },
    ];
    for params in cases {
        // Each of these gets a snapshot batch; none is an acknowledgment.
        replay(&deck.control, params.clone()).await;
        f.ctx.coordinator.flush().await.unwrap();
        assert!(
            next_call(&mut deck, QUIET).await.is_none(),
            "a call left after {params:?}"
        );
        assert_eq!(f.pending_sends().await, Some(0), "{params:?}");
    }
}
