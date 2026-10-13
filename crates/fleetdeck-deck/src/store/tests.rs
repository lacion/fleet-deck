#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;
use std::sync::Arc;

use fleetdeck_core::identity::{Identity, PeerKey};
use fleetdeck_link::grants::GrantSource;
use fleetdeck_link::pairing::{PairedPeer, PairingSink};
use fleetdeck_link::tls::Role;
use fleetdeck_proto::event::{Event, EventBody, RunOrigin, SessionState};
use fleetdeck_proto::link::{ReplayParams, Scope};
use fleetdeck_proto::{CardStatus, EventEnvelope, IncarnationId, SessionSummary, StationId};
use fleetdeck_testkit::Det;

use super::stations::NewStation;
use super::{DeckError, DeckStore, Direction, TIMELINE_CAP};
use crate::peers::{DeckPeers, Expected};

fn key() -> PeerKey {
    Identity::generate().unwrap().public_key()
}

async fn store(det: &Det) -> DeckStore {
    DeckStore::open_in_memory(det.clock(), det.ids())
        .await
        .unwrap()
}

async fn paired(s: &DeckStore, name: &str) -> (PeerKey, StationId) {
    let k = key();
    let rec = s
        .pin_station(NewStation {
            key: k,
            name: name.to_owned(),
            scopes: BTreeSet::from([Scope::Read]),
            dial_url: Some("127.0.0.1:7000".to_owned()),
        })
        .await
        .unwrap();
    (k, rec.station_id)
}

fn env(seq: u64, session: &str, event: Event) -> EventEnvelope {
    EventEnvelope {
        station_seq: seq,
        session_id: session.into(),
        session_seq: seq,
        at: 1_000,
        proto_version: 1,
        raw_ref: None,
        event: EventBody::Known(event),
    }
}

fn run(seq: u64, session: &str) -> EventEnvelope {
    env(
        seq,
        session,
        Event::RunStarted {
            author: None,
            run_id: format!("r{seq}").into(),
            input_summary: "hi".to_owned(),
            origin: RunOrigin::Board,
        },
    )
}

fn delta(seq: u64, session: &str) -> EventEnvelope {
    env(
        seq,
        session,
        Event::MessageDelta {
            turn_id: "t".into(),
            parent_node_id: None,
            text: "x".to_owned(),
        },
    )
}

fn inc(s: &str) -> IncarnationId {
    IncarnationId::from(s)
}

#[tokio::test]
async fn pairing_stores_one_row_and_revocation_removes_it_with_its_read_model() {
    let det = Det::new();
    let s = store(&det).await;
    let (k, id) = paired(&s, "box").await;
    assert_eq!(id.as_str(), k.fingerprint().to_string());
    let rec = s.station(id.clone()).await.unwrap().unwrap();
    assert_eq!(rec.name, "box");
    assert_eq!(rec.direction, Direction::DeckDials);
    assert_eq!(rec.scopes, BTreeSet::from([Scope::Read]));
    assert_eq!((rec.incarnation, rec.cursor), (None, 0));
    assert_eq!(
        s.find_station("box".to_owned()).await.unwrap().unwrap().key,
        k
    );

    s.apply_snapshot(
        id.clone(),
        inc("i1"),
        1,
        vec![SessionSummary::new("s1".into())],
    )
    .await
    .unwrap();
    s.apply_events(id.clone(), inc("i1"), vec![run(2, "s1")])
        .await
        .unwrap();
    assert_eq!(s.sessions().await.unwrap().len(), 1);

    let removed = s.revoke_station(k).await.unwrap().unwrap();
    assert_eq!(removed.station_id, id);
    assert_eq!(s.revocation_generation().await.unwrap(), 1);
    assert!(s.stations().await.unwrap().is_empty());
    assert!(s.sessions().await.unwrap().is_empty());
    assert!(s.timeline(id, "s1".into()).await.unwrap().is_empty());
    // A second revocation changes nothing.
    assert!(s.revoke_station(k).await.unwrap().is_none());
    assert_eq!(s.revocation_generation().await.unwrap(), 1);
}

#[tokio::test]
async fn a_station_that_dials_has_no_address_and_names_are_unique() {
    let det = Det::new();
    let s = store(&det).await;
    paired(&s, "box").await;
    let err = s
        .pin_station(NewStation {
            key: key(),
            name: "box".to_owned(),
            scopes: BTreeSet::new(),
            dial_url: None,
        })
        .await
        .unwrap_err();
    assert!(matches!(err, DeckError::NameTaken(n) if n == "box"));
    let rec = s
        .pin_station(NewStation {
            key: key(),
            name: "laptop".to_owned(),
            scopes: BTreeSet::new(),
            dial_url: None,
        })
        .await
        .unwrap();
    assert_eq!(rec.direction, Direction::StationDials);
}

#[tokio::test]
async fn a_rotation_keeps_the_station_id_name_and_cursor_and_bumps_the_generation() {
    let det = Det::new();
    let s = store(&det).await;
    let (old, id) = paired(&s, "box").await;
    s.apply_snapshot(id.clone(), inc("i1"), 7, Vec::new())
        .await
        .unwrap();
    let new = key();
    s.rotate_station(old, new).await.unwrap();
    let rec = s.station(id.clone()).await.unwrap().unwrap();
    assert_eq!((rec.key, rec.name.as_str(), rec.cursor), (new, "box", 7));
    assert_eq!(s.revocation_generation().await.unwrap(), 1);
    assert!(matches!(
        s.rotate_station(old, key()).await,
        Err(DeckError::UnknownStation(_))
    ));
    assert!(s.revoke_station(old).await.unwrap().is_none());
}

#[tokio::test]
async fn events_apply_once_only_for_the_held_incarnation_and_the_timeline_drops_deltas() {
    let det = Det::new();
    let s = store(&det).await;
    let (_, id) = paired(&s, "box").await;

    // The first stream starts with a snapshot.
    let err = s
        .apply_events(id.clone(), inc("i1"), vec![run(1, "s1")])
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        DeckError::IncarnationMismatch { expected: None, .. }
    ));
    s.apply_snapshot(id.clone(), inc("i1"), 0, Vec::new())
        .await
        .unwrap();

    let batch = vec![run(1, "s1"), delta(2, "s1"), run(3, "s2")];
    let a = s
        .apply_events(id.clone(), inc("i1"), batch.clone())
        .await
        .unwrap();
    assert_eq!((a.applied, a.cursor), (3, 3));
    // A replay after a link drop resends the same events: nothing applies twice.
    let again = s.apply_events(id.clone(), inc("i1"), batch).await.unwrap();
    assert_eq!((again.applied, again.cursor), (0, 3));
    assert_eq!(
        s.replay_params(id.clone()).await.unwrap(),
        ReplayParams {
            incarnation: Some(inc("i1")),
            after: 3
        }
    );

    let t = s.timeline(id.clone(), "s1".into()).await.unwrap();
    assert_eq!(t.iter().map(|e| e.station_seq).collect::<Vec<_>>(), [1]);
    let cards = s.sessions().await.unwrap();
    assert_eq!(cards.len(), 2);
    assert_eq!(
        cards[0].summary.current_run.as_ref().map(|r| r.as_str()),
        Some("r1")
    );

    // Events of another incarnation need a snapshot.
    let err = s
        .apply_events(id, inc("i2"), vec![run(1, "s1")])
        .await
        .unwrap_err();
    assert!(matches!(err, DeckError::IncarnationMismatch { .. }));
}

#[tokio::test]
async fn a_snapshot_replaces_the_sessions_and_a_new_incarnation_clears_the_timeline() {
    let det = Det::new();
    let s = store(&det).await;
    let (_, id) = paired(&s, "box").await;
    s.apply_snapshot(id.clone(), inc("i1"), 0, Vec::new())
        .await
        .unwrap();
    s.apply_events(id.clone(), inc("i1"), vec![run(1, "s1"), run(2, "s2")])
        .await
        .unwrap();

    // Same incarnation, a batch without s2: s2 and its timeline go.
    s.apply_snapshot(
        id.clone(),
        inc("i1"),
        5,
        vec![SessionSummary::new("s1".into())],
    )
    .await
    .unwrap();
    let cards = s.sessions().await.unwrap();
    assert_eq!(cards.len(), 1);
    assert_eq!(s.timeline(id.clone(), "s1".into()).await.unwrap().len(), 1);
    assert!(
        s.timeline(id.clone(), "s2".into())
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(s.replay_params(id.clone()).await.unwrap().after, 5);

    // A reset station database: the old station_seq values mean nothing.
    s.apply_snapshot(
        id.clone(),
        inc("i2"),
        2,
        vec![SessionSummary::new("s1".into())],
    )
    .await
    .unwrap();
    assert!(
        s.timeline(id.clone(), "s1".into())
            .await
            .unwrap()
            .is_empty()
    );
    let p = s.replay_params(id.clone()).await.unwrap();
    assert_eq!((p.incarnation, p.after), (Some(inc("i2")), 2));
    let a = s
        .apply_events(id, inc("i2"), vec![run(2, "s1"), run(3, "s1")])
        .await
        .unwrap();
    assert_eq!((a.applied, a.cursor), (1, 3));
}

#[tokio::test]
async fn the_timeline_keeps_the_newest_rows_up_to_the_cap() {
    let det = Det::new();
    let s = store(&det).await;
    let (_, id) = paired(&s, "box").await;
    s.apply_snapshot(id.clone(), inc("i1"), 0, Vec::new())
        .await
        .unwrap();
    let n = u64::from(TIMELINE_CAP) + 20;
    let events: Vec<_> = (1..=n).map(|i| run(i, "s1")).collect();
    s.apply_events(id.clone(), inc("i1"), events).await.unwrap();
    let t = s.timeline(id, "s1".into()).await.unwrap();
    assert_eq!(t.len(), TIMELINE_CAP as usize);
    assert_eq!(t.first().unwrap().station_seq, 21);
    assert_eq!(t.last().unwrap().station_seq, n);
}

#[tokio::test]
async fn the_status_projection_marks_unreachable_stations_and_lost_sessions() {
    let det = Det::new();
    let s = store(&det).await;
    let (_, id) = paired(&s, "box").await;
    let mut lost = SessionSummary::new("s2".into());
    lost.state = SessionState::Lost;
    s.apply_snapshot(
        id.clone(),
        inc("i1"),
        0,
        vec![SessionSummary::new("s1".into()), lost],
    )
    .await
    .unwrap();
    s.apply_events(id.clone(), inc("i1"), vec![run(1, "s1")])
        .await
        .unwrap();
    let status =
        |cards: Vec<crate::DeckSession>| cards.into_iter().map(|c| c.status).collect::<Vec<_>>();
    assert_eq!(
        status(s.sessions().await.unwrap()),
        [CardStatus::Unreachable, CardStatus::Lost]
    );
    s.set_reachable(&id, true);
    assert!(s.is_reachable(&id));
    assert_eq!(
        status(s.sessions().await.unwrap()),
        [CardStatus::Working, CardStatus::Lost]
    );
    s.set_reachable(&id, false);
    assert_eq!(
        status(s.sessions().await.unwrap())[0],
        CardStatus::Unreachable
    );
}

#[tokio::test]
async fn the_registry_cursor_and_incarnations_survive_a_deck_restart() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("deck.db");
    let det = Det::new();
    let (deck_inc, id) = {
        let s = DeckStore::open(&path, det.clock(), det.ids())
            .await
            .unwrap();
        let (_, id) = paired(&s, "box").await;
        s.apply_snapshot(id.clone(), inc("i1"), 0, Vec::new())
            .await
            .unwrap();
        s.apply_events(id.clone(), inc("i1"), vec![run(1, "s1"), run(2, "s1")])
            .await
            .unwrap();
        (s.incarnation().clone(), id)
    };
    let s = DeckStore::open(&path, det.clock(), det.ids())
        .await
        .unwrap();
    assert_eq!(s.incarnation(), &deck_inc);
    let p = s.replay_params(id.clone()).await.unwrap();
    assert_eq!((p.incarnation, p.after), (Some(inc("i1")), 2));
    assert_eq!(s.timeline(id.clone(), "s1".into()).await.unwrap().len(), 2);
    // Reachability is link state: after a restart no station is reachable.
    assert!(!s.is_reachable(&id));
}

fn peer(k: PeerKey, role: Role) -> PairedPeer {
    PairedPeer {
        key: k,
        role,
        scopes: BTreeSet::from([Scope::Read]),
    }
}

#[tokio::test]
async fn deck_peers_store_the_expected_name_refuse_decks_and_revoke_fail_closed() {
    let det = Det::new();
    let s = store(&det).await;
    let peers: Arc<DeckPeers> = DeckPeers::load(s.clone()).await.unwrap();

    let deck = key();
    assert!(peers.paired(&peer(deck, Role::Deck)).await.is_err());
    assert!(peers.scopes(&deck.fingerprint()).is_empty());

    let k = key();
    peers.expect(
        k.fingerprint(),
        Expected {
            name: "box".to_owned(),
            dial_url: Some("127.0.0.1:7100".to_owned()),
        },
    );
    peers.paired(&peer(k, Role::Station)).await.unwrap();
    let rec = s.find_station("box".to_owned()).await.unwrap().unwrap();
    assert_eq!(rec.dial_url.as_deref(), Some("127.0.0.1:7100"));
    assert_eq!(
        peers.scopes(&k.fingerprint()),
        BTreeSet::from([Scope::Read])
    );

    // Without a registration the name is the short fingerprint.
    let other = key();
    peers.paired(&peer(other, Role::Station)).await.unwrap();
    let rec = s
        .find_station(other.fingerprint().short())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(rec.direction, Direction::StationDials);

    // A reload sees the stored grants.
    let reloaded = DeckPeers::load(s.clone()).await.unwrap();
    assert_eq!(
        reloaded.scopes(&k.fingerprint()),
        BTreeSet::from([Scope::Read])
    );

    let new = key();
    peers.rotated(&k, &new).await.unwrap();
    assert!(peers.scopes(&k.fingerprint()).is_empty());
    assert_eq!(
        peers.scopes(&new.fingerprint()),
        BTreeSet::from([Scope::Read])
    );

    s.set_reachable(&rec.station_id, true);
    peers.revoked(&other).await.unwrap();
    assert!(peers.scopes(&other.fingerprint()).is_empty());
    assert!(!s.is_reachable(&rec.station_id));
    assert_eq!(s.stations().await.unwrap().len(), 1);
    assert_eq!(s.revocation_generation().await.unwrap(), 2);
}
