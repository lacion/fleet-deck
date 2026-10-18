//! The station event stream for a link without `artifacts_video.v1` (R13 P5b
//! task K2, design `omarchy-p5b.md` §14, decision O12-15): the
//! `artifact.published` event of a video goes out with kind `other`; a link
//! with the feature gets it unchanged; the stored event never changes.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Duration;

use fleetdeck_core::identity::Identity;
use fleetdeck_link::control::Inbound;
use fleetdeck_link::hello::LocalHello;
use fleetdeck_link::node::{self, NodeConfig};
use fleetdeck_link::tls::Role;
use fleetdeck_link::{Epochs, Grants, LinkListener, LiveLink};
use fleetdeck_proto::IncarnationId;
use fleetdeck_proto::artifacts::ArtifactKind;
use fleetdeck_proto::event::{Event, EventBody, ProviderKind};
use fleetdeck_proto::link::{ControlFrame, Outcome, ReplayParams, Scope};
use fleetdeck_proto::version::feature;
use fleetdeck_station::link::{self, StationLink, StationPeers};
use fleetdeck_station::manager::{Adapters, Policy, SessionManager};
use fleetdeck_station::shell::ShellConfig;
use fleetdeck_station::store::peers::PairedKey;
use fleetdeck_station::store::{NewEvent, NewSession, OfflineApprovalPolicy, Store, StoreConfig};
use fleetdeck_testkit::Det;
use serde_json::json;

const T: Duration = Duration::from_secs(10);

struct Fixture {
    _det: Det,
    store: Store,
    ctx: Arc<StationLink>,
    listener: LinkListener,
    deck_node: NodeConfig,
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
    let deck_node = NodeConfig::new(
        deck,
        LocalHello::new(Role::Deck, IncarnationId::from("deck-inc")),
        Arc::new(Epochs::in_memory()),
    )
    .with_store(Grants::new());
    deck_node.pins.pin(station_id.public_key(), Role::Station);
    let ctx = Arc::new(StationLink {
        manager: Arc::new(manager),
        node,
        peers,
        name: "box".to_owned(),
        shells: Arc::new(fleetdeck_station::shell::Shells::new(ShellConfig::default())),
        coordinator: fleetdeck_station::coordinator::LinkForward::default(),
        paste_dir: None,
        summary: std::sync::Arc::default(),
        ready: std::sync::Arc::default(),
        fleet_git: Arc::new(fleetdeck_station::fleet_git_runtime::FleetGitRuntime::new(
            Arc::new(fleetdeck_station::workspace::GitOptions::isolated),
        )),
    });
    store
        .create_session(NewSession {
            session_id: "s1".into(),
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
    Fixture {
        _det: det,
        store,
        ctx,
        listener,
        deck_node,
    }
}

impl Fixture {
    /// The deck dials with `features` in its offer; the station serves the
    /// link. Returns the deck side.
    async fn connect(&mut self, skip_video: bool) -> LiveLink {
        let mut deck = self.deck_node.clone();
        if skip_video {
            deck.local
                .features
                .retain(|f| f.as_str() != feature::ARTIFACTS_VIDEO);
        }
        let peer = self.ctx.node.identity.public_key();
        let (d, a) = tokio::time::timeout(T, async {
            tokio::join!(
                node::dial(&deck, self.listener.local_addr, peer),
                self.listener.next_link()
            )
        })
        .await
        .expect("link setup");
        let at_station = fleetdeck_link::start(&self.ctx.node, a.expect("listener"));
        tokio::spawn(link::serve(Arc::clone(&self.ctx), at_station));
        fleetdeck_link::start(&deck, d.unwrap())
    }

    async fn publish(&self, id: &str, kind: ArtifactKind) -> u64 {
        let e: NewEvent = Event::ArtifactPublished {
            artifact_id: id.to_owned(),
            version: 1,
            title: "t".to_owned(),
            artifact_kind: kind,
        }
        .into();
        self.store.append("s1".into(), vec![e]).await.unwrap()[0].station_seq
    }
}

/// The `artifact_kind` of the next `artifact.published` event of the link.
async fn next_kind(deck: &mut LiveLink) -> (String, ArtifactKind) {
    loop {
        let inbound = tokio::time::timeout(T, deck.inbound.recv())
            .await
            .expect("an event")
            .expect("the link is open");
        if let Inbound::Frame(f) = inbound
            && let ControlFrame::Event { envelope, .. } = *f
            && let EventBody::Known(Event::ArtifactPublished {
                artifact_id,
                artifact_kind,
                ..
            }) = envelope.event
        {
            return (artifact_id, artifact_kind);
        }
    }
}

async fn replay(deck: &LiveLink, inc: IncarnationId) {
    let params = ReplayParams {
        incarnation: Some(inc),
        after: 0,
    };
    let o = tokio::time::timeout(
        T,
        deck.control.request("events.replay", None, json!(params)),
    )
    .await
    .expect("response")
    .unwrap();
    assert!(matches!(o, Outcome::Ok(_)), "{o:?}");
}

#[tokio::test]
async fn the_stream_shows_a_video_artifact_as_other_on_a_link_without_the_video_feature() {
    let mut f = fixture().await;
    let before = f.publish("old", ArtifactKind::Video).await;
    let mut deck = f.connect(true).await;
    assert!(
        !deck
            .control
            .params()
            .negotiated
            .has(feature::ARTIFACTS_VIDEO)
    );
    // The replay of a stored video and a live one both go out as `other`.
    replay(&deck, f.store.incarnation().clone()).await;
    let live = f.publish("live", ArtifactKind::Video).await;
    let html = f.publish("page", ArtifactKind::Html).await;
    assert!(before < live && live < html);
    assert_eq!(
        next_kind(&mut deck).await,
        ("old".to_owned(), ArtifactKind::Other)
    );
    assert_eq!(
        next_kind(&mut deck).await,
        ("live".to_owned(), ArtifactKind::Other)
    );
    assert_eq!(
        next_kind(&mut deck).await,
        ("page".to_owned(), ArtifactKind::Html)
    );
    // The stored events never changed.
    let stored = f.store.read_after(0, 10).await.unwrap();
    let fleetdeck_station::store::ReadResult::Events { events, .. } = stored else {
        panic!("expected events");
    };
    let kinds: Vec<ArtifactKind> = events
        .iter()
        .filter_map(|e| match &e.event {
            EventBody::Known(Event::ArtifactPublished { artifact_kind, .. }) => {
                Some(*artifact_kind)
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        kinds,
        [ArtifactKind::Video, ArtifactKind::Video, ArtifactKind::Html]
    );
}

#[tokio::test]
async fn the_stream_keeps_a_video_artifact_on_a_link_with_the_video_feature() {
    let mut f = fixture().await;
    f.publish("old", ArtifactKind::Video).await;
    let mut deck = f.connect(false).await;
    assert!(
        deck.control
            .params()
            .negotiated
            .has(feature::ARTIFACTS_VIDEO)
    );
    replay(&deck, f.store.incarnation().clone()).await;
    f.publish("live", ArtifactKind::Video).await;
    assert_eq!(
        next_kind(&mut deck).await,
        ("old".to_owned(), ArtifactKind::Video)
    );
    assert_eq!(
        next_kind(&mut deck).await,
        ("live".to_owned(), ArtifactKind::Video)
    );
}
