//! B26 the return gate of a station link (design `grants-update.md` §3.3a,
//! build plan T5): a link of a station with a stored `station.info` answer
//! authorizes no grant work before its first return check committed.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::time::Duration;

use common::fleet::{bound_on, claude_info, coordinate, query};
use common::*;
use fleetdeck_deck::knobs::DeckKnobs;
use fleetdeck_deck::store::GrantClass;
use fleetdeck_link::LiveLink;
use fleetdeck_link::control::{Inbound, InboundRequest};
use fleetdeck_link::node;
use fleetdeck_proto::command::CommandOrigin;
use fleetdeck_proto::link::{ControlFrame, Outcome, StreamMode, StreamStart};
use fleetdeck_proto::mutation::CoordinatorQuery;
use serde_json::{Value, json};

/// A linked fake station and the `station.info` request that arrived before
/// the replay request was answered, when it did.
struct Up {
    station: LiveLink,
    info: Option<InboundRequest>,
}

impl Up {
    /// The `station.info` request of the deck, unanswered.
    async fn info(&mut self) -> InboundRequest {
        if let Some(req) = self.info.take() {
            return req;
        }
        loop {
            match tokio::time::timeout(T, self.station.inbound.recv())
                .await
                .expect("a station.info request")
            {
                Some(Inbound::Request(req)) if req.method == "station.info" => return req,
                Some(_) => {}
                None => panic!("the link ended"),
            }
        }
    }

    /// Answers the `station.info` request with `version`.
    async fn answer_info(&mut self, version: &str) {
        let req = self.info().await;
        let mut info = claude_info();
        info.version = Some(version.to_owned());
        self.station
            .control
            .respond(req.id, Outcome::Ok(json!(info)))
            .await
            .unwrap();
    }
}

/// [`Fixture::connect`] that keeps the `station.info` request for the test
/// (the helper drops it and leaves it unanswered).
async fn up(f: &mut Fixture) -> Up {
    let (d, a) = tokio::time::timeout(T, async {
        tokio::join!(
            node::dial(&f.station_node, f.listener.local_addr, f.deck_key),
            f.listener.next_link()
        )
    })
    .await
    .expect("link setup");
    let at_deck = fleetdeck_link::start(&f.deck_node, a.expect("listener"));
    tokio::spawn(fleetdeck_deck::link::serve(
        std::sync::Arc::clone(&f.ctx),
        at_deck,
    ));
    let mut station = fleetdeck_link::start(&f.station_node, d.unwrap());
    let mut info = None;
    let replay = loop {
        match tokio::time::timeout(T, station.inbound.recv())
            .await
            .expect("a request")
        {
            Some(Inbound::Request(req)) if req.method == "station.info" => info = Some(req),
            Some(Inbound::Request(req)) => break req,
            Some(Inbound::Frame(_)) => {}
            None => panic!("the link ended"),
        }
    };
    assert_eq!(replay.method, "events.replay");
    let mode = if f.synced {
        StreamMode::Replay
    } else {
        StreamMode::Snapshot
    };
    let start = StreamStart {
        mode,
        incarnation: f.inc.clone(),
        from: 0,
    };
    station
        .control
        .respond(replay.id, Outcome::Ok(json!(start)))
        .await
        .unwrap();
    if !f.synced {
        for frame in [
            ControlFrame::SnapshotBegin {
                incarnation: f.inc.clone(),
                watermark: 0,
                session_count: 0,
            },
            ControlFrame::SnapshotEnd {
                incarnation: f.inc.clone(),
                watermark: 0,
                coordinator: None,
            },
        ] {
            station.control.send(frame).await.unwrap();
        }
        f.wait("the snapshot", |f| {
            let (store, id) = (f.store.clone(), f.station.clone());
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
        f.synced = true;
    }
    Up { station, info }
}

/// A fixture whose first link answered `station.info` with `v1` and ended,
/// with a bound grant of `coord-1`.
async fn returned(keep: bool) -> (Fixture, fleetdeck_proto::mutation::CoordinatorBinding) {
    let mut f = fixture_with(coordinate()).await;
    f.store.set_knobs(DeckKnobs {
        keep_grants_on_update: keep,
        ..DeckKnobs::default()
    });
    let mut first = up(&mut f).await;
    first.answer_info("v1").await;
    // The first answer is stored: the next link of the station returns.
    f.wait("the first answer stored", |f| {
        let (store, id) = (f.store.clone(), f.station.clone());
        async move { store.hold_returning(id).await.unwrap() }
    })
    .await;
    let b = bound_on(&f, &f.station, "coord-1", &[GrantClass::Read], false).await;
    f.drop_link(first.station).await;
    (f, b)
}

/// Sends `coordinator.query` (`ListSessions`) from the station on its own
/// task.
fn send_query(
    f: &Fixture,
    link: &Up,
    b: &fleetdeck_proto::mutation::CoordinatorBinding,
) -> tokio::task::JoinHandle<Outcome> {
    let params = json!(query(f, b, CoordinatorQuery::ListSessions));
    let control = link.station.control.clone();
    tokio::spawn(async move {
        control
            .request("coordinator.query", None, params)
            .await
            .expect("a response")
    })
}

#[tokio::test]
async fn a_request_on_a_returning_link_waits_for_a_delayed_station_info_answer() {
    let (mut f, b) = returned(false).await;
    let mut second = up(&mut f).await;
    let mut pending = send_query(&f, &second, &b);
    let early = tokio::time::timeout(Duration::from_millis(500), &mut pending).await;
    assert!(early.is_err(), "the query waits for the return check");
    second.answer_info("v2").await;
    let out = tokio::time::timeout(T, pending).await.unwrap().unwrap();
    let Outcome::Ok(v) = out else {
        panic!("{out:?}")
    };
    let text = v["reject"]["message"].as_str().expect("a reject");
    assert!(text.contains("station was updated"), "{text}");
}

#[tokio::test]
async fn a_returning_link_whose_station_info_never_comes_refuses_after_eight_seconds() {
    let (mut f, b) = returned(false).await;
    let second = up(&mut f).await;
    let pending = send_query(&f, &second, &b);
    let out = tokio::time::timeout(Duration::from_secs(15), pending)
        .await
        .unwrap()
        .unwrap();
    let Outcome::Err(e) = out else {
        panic!("{out:?}")
    };
    assert_eq!(e.code, "timeout");
    assert!(
        e.message.contains("has not finished the check"),
        "{}",
        e.message
    );
}

#[tokio::test]
async fn the_router_holds_grant_work_until_the_return_check() {
    let (mut f, _b) = returned(true).await;
    let held = f
        .ctx
        .submit(f.station.clone(), CommandOrigin::Coordinator, interrupt())
        .await
        .unwrap();
    let mut second = up(&mut f).await;
    no_request(&mut second.station, Duration::from_millis(500)).await;
    second.answer_info("v1").await;
    // The first `station.info` answer also sends the Fleet Git snapshot
    // (`fleet_git.projects_set`); it is not grant work.
    let req = loop {
        let req = next_request(&mut second.station).await;
        if req.method != "fleet_git.projects_set" {
            break req;
        }
    };
    assert_eq!(req.method, "session.interrupt");
    assert_eq!(req.command_id, Some(held.command_id));
}

#[tokio::test]
async fn a_board_command_is_not_held_on_a_returning_link() {
    let (mut f, _b) = returned(true).await;
    let mut second = up(&mut f).await;
    let cmd = f.submit().await;
    let req = next_request(&mut second.station).await;
    assert_eq!(req.method, "session.interrupt");
    assert_eq!(req.command_id, Some(cmd.command_id));
}

#[tokio::test]
async fn a_first_link_never_waits() {
    let mut f = fixture_with(coordinate()).await;
    let first = up(&mut f).await;
    let b = bound_on(&f, &f.station, "coord-1", &[GrantClass::Read], false).await;
    // No stored answer, and the station never answers `station.info`.
    let out = tokio::time::timeout(T, send_query(&f, &first, &b))
        .await
        .unwrap()
        .unwrap();
    let Outcome::Ok(v) = out else {
        panic!("{out:?}")
    };
    assert!(
        !matches!(&v, Value::Object(o) if o.contains_key("reject")),
        "{v}"
    );
}
