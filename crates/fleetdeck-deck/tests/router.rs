//! The deck command router against a fake station (M2 P3-7a; G3, PLAN
//! §8.4): the envelope is stored `pending` before the send; a lost response
//! or a link drop leads to a resend with the same `command_id` and the same
//! bytes; `retry_until` ends a command as `ambiguous` (sent) or `failed`
//! (never sent); a rejection without a stored receipt after an earlier send
//! is `ambiguous`; a link refusal is `failed`; nothing is redispatched.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use fleetdeck_core::identity::{Identity, PeerKey};
use fleetdeck_deck::link::runtime::STATION_SCOPES;
use fleetdeck_deck::link::{self, CommandKind, DeckLink};
use fleetdeck_deck::store::stations::NewStation;
use fleetdeck_deck::store::{DeckCommand, DeckError};
use fleetdeck_deck::{DeckPeers, DeckStore};
use fleetdeck_link::control::{Inbound, InboundRequest};
use fleetdeck_link::hello::LocalHello;
use fleetdeck_link::node::{self, LinkListener, NodeConfig};
use fleetdeck_link::tls::Role;
use fleetdeck_link::{Epochs, Grants, LiveLink};
use fleetdeck_proto::command::{
    CommandOrigin, DeckCommandState, RETRY_WINDOW_MS, ReceiptState, Reject, SessionCommand,
};
use fleetdeck_proto::link::{ControlFrame, Outcome, Scope, StreamMode, StreamStart, WireError};
use fleetdeck_proto::{CommandId, IncarnationId, Receipt, RejectCode, StationId};
use fleetdeck_testkit::Det;
use serde_json::json;

const T: Duration = Duration::from_secs(10);

struct Fixture {
    det: Det,
    store: DeckStore,
    ctx: Arc<DeckLink>,
    deck_node: NodeConfig,
    deck_key: PeerKey,
    listener: LinkListener,
    station_node: NodeConfig,
    station: StationId,
    inc: IncarnationId,
    synced: bool,
}

async fn fixture() -> Fixture {
    let det = Det::new();
    let store = DeckStore::open_in_memory(det.clock(), det.ids())
        .await
        .unwrap();
    let deck_id = Arc::new(Identity::generate().unwrap());
    let station_id = Arc::new(Identity::generate().unwrap());
    let rec = store
        .pin_station(NewStation {
            key: station_id.public_key(),
            name: "fake".to_owned(),
            scopes: STATION_SCOPES.into_iter().collect(),
            dial_url: None,
        })
        .await
        .unwrap();
    let peers = DeckPeers::load(store.clone()).await.unwrap();
    let deck_node = NodeConfig::new(
        Arc::clone(&deck_id),
        LocalHello::new(Role::Deck, store.incarnation().clone()),
        Arc::new(Epochs::in_memory()),
    )
    .with_store(Arc::clone(&peers));
    peers.pin_all(&deck_node.pins).await.unwrap();
    let listener = node::listen(deck_node.clone(), "127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    let inc = IncarnationId::from("fake-station-db");
    let grants = Grants::new();
    grants.grant(
        deck_id.fingerprint(),
        BTreeSet::from([Scope::Read, Scope::Drive, Scope::Approve, Scope::Admin]),
    );
    let station_node = NodeConfig::new(
        station_id,
        LocalHello::new(Role::Station, inc.clone()),
        Arc::new(Epochs::in_memory()),
    )
    .with_store(grants);
    station_node.pins.pin(deck_id.public_key(), Role::Deck);
    let ctx = Arc::new(DeckLink::new(store.clone(), peers, deck_node.clone()));
    // The router tick of `DeckNode`, faster.
    let ticker = Arc::clone(&ctx);
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_millis(100)).await;
            ticker.tick().await;
        }
    });
    Fixture {
        det,
        store,
        ctx,
        deck_node,
        deck_key: deck_id.public_key(),
        listener,
        station_node,
        station: rec.station_id,
        inc,
        synced: false,
    }
}

async fn next_request(station: &mut LiveLink) -> InboundRequest {
    loop {
        match tokio::time::timeout(T, station.inbound.recv())
            .await
            .expect("a request")
        {
            Some(Inbound::Request(req)) => return req,
            Some(Inbound::Frame(_)) => {}
            None => panic!("the link ended"),
        }
    }
}

/// No request arrives within `wait`.
async fn no_request(station: &mut LiveLink, wait: Duration) {
    let got = tokio::time::timeout(wait, async {
        loop {
            match station.inbound.recv().await {
                Some(Inbound::Request(req)) => return Some(req),
                Some(Inbound::Frame(_)) => {}
                None => return None,
            }
        }
    })
    .await;
    if let Ok(Some(req)) = got {
        panic!("unexpected request {} {}", req.method, req.params);
    }
}

fn accepted(id: &CommandId) -> Outcome {
    Outcome::Ok(json!(Receipt {
        command_id: id.clone(),
        state: ReceiptState::Accepted,
        station_seq: Some(1),
        result: None,
        reject: None,
        at: 0,
    }))
}

fn rejected(id: &CommandId, code: RejectCode) -> Outcome {
    Outcome::Ok(json!(Receipt {
        command_id: id.clone(),
        state: ReceiptState::Rejected,
        station_seq: None,
        result: None,
        reject: Some(Reject {
            code,
            message: "no".to_owned(),
            unsupported: None,
        }),
        at: 0,
    }))
}

fn interrupt() -> CommandKind {
    CommandKind::Session {
        command: SessionCommand::Interrupt {
            session_id: "s1".into(),
        },
    }
}

impl Fixture {
    /// Links the fake station to the deck. The first link answers the
    /// deck's replay with an empty snapshot (the deck learns the
    /// incarnation); later links answer with a replay.
    async fn connect(&mut self) -> LiveLink {
        let (d, a) = tokio::time::timeout(T, async {
            tokio::join!(
                node::dial(&self.station_node, self.listener.local_addr, self.deck_key),
                self.listener.next_link()
            )
        })
        .await
        .expect("link setup");
        let at_deck = fleetdeck_link::start(&self.deck_node, a.expect("listener"));
        tokio::spawn(link::serve(Arc::clone(&self.ctx), at_deck));
        let mut station = fleetdeck_link::start(&self.station_node, d.unwrap());
        let req = next_request(&mut station).await;
        assert_eq!(req.method, "events.replay");
        let mode = if self.synced {
            StreamMode::Replay
        } else {
            StreamMode::Snapshot
        };
        let start = StreamStart {
            mode,
            incarnation: self.inc.clone(),
            from: 0,
        };
        station
            .control
            .respond(req.id, Outcome::Ok(json!(start)))
            .await
            .unwrap();
        if !self.synced {
            for frame in [
                ControlFrame::SnapshotBegin {
                    incarnation: self.inc.clone(),
                    watermark: 0,
                    session_count: 0,
                },
                ControlFrame::SnapshotEnd {
                    incarnation: self.inc.clone(),
                    watermark: 0,
                },
            ] {
                station.control.send(frame).await.unwrap();
            }
            self.wait("the snapshot", |f| {
                let store = f.store.clone();
                let id = f.station.clone();
                async move {
                    store
                        .station(id)
                        .await
                        .unwrap()
                        .unwrap()
                        .incarnation
                        .is_some()
                }
            })
            .await;
            self.synced = true;
        }
        station
    }

    /// Closes the fake station's link and waits until the deck sees it.
    async fn drop_link(&self, station: LiveLink) {
        station.control.close("test", "the link drops");
        let _ = station.control.closed().await;
        let store = self.store.clone();
        let id = self.station.clone();
        self.wait("the station unreachable", move |_| {
            let (store, id) = (store.clone(), id.clone());
            async move { !store.is_reachable(&id) }
        })
        .await;
    }

    async fn wait<F, Fut>(&self, what: &str, f: F)
    where
        F: Fn(&Self) -> Fut,
        Fut: std::future::Future<Output = bool>,
    {
        let deadline = tokio::time::Instant::now() + T;
        while !f(self).await {
            assert!(tokio::time::Instant::now() < deadline, "timed out: {what}");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    async fn submit(&self) -> DeckCommand {
        self.ctx
            .submit(self.station.clone(), CommandOrigin::Board, interrupt())
            .await
            .unwrap()
    }

    async fn command(&self, id: &CommandId) -> DeckCommand {
        self.store.command(id.clone()).await.unwrap().unwrap()
    }

    async fn settled(&self, id: &CommandId) -> DeckCommand {
        let cmd = self.ctx.wait_command(id.clone(), T).await.unwrap();
        assert_ne!(cmd.state, DeckCommandState::Pending, "{cmd:?}");
        cmd
    }

    /// Answers one request and returns it.
    async fn answer(
        station: &mut LiveLink,
        outcome: impl FnOnce(&CommandId) -> Outcome,
    ) -> InboundRequest {
        let req = next_request(station).await;
        let id = req.command_id.clone().expect("a command id");
        station.control.respond(req.id, outcome(&id)).await.unwrap();
        req
    }
}

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
