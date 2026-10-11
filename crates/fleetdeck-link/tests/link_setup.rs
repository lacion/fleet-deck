//! Link setup over loopback with the Revision 2 rules (ADR-004 §9.1): both
//! dial directions, persistent epochs and `attempt_id`, stale epochs, the
//! node-wide link table with its resolution rules (G1), and cleanup after a
//! failed setup (G10).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use fleetdeck_core::identity::Identity;
use fleetdeck_link::hello::{LocalHello, Side, feature};
use fleetdeck_link::node::{self, DialFault, NO_CONTROL_CHANNEL, NO_DATA_CHANNEL, NodeConfig};
use fleetdeck_link::table::{Dialer, SUPERSEDED};
use fleetdeck_link::tls::Role;
use fleetdeck_link::{Epochs, LinkError, LinkListener, OpenLink};
use fleetdeck_proto::IncarnationId;

const T: Duration = Duration::from_secs(10);

fn lo() -> SocketAddr {
    "127.0.0.1:0".parse().unwrap()
}

fn node(role: Role, timeout: Duration) -> NodeConfig {
    let identity = Arc::new(Identity::generate().unwrap());
    let inc = IncarnationId::from(format!("inc-{}", identity.fingerprint().short()));
    let mut n = NodeConfig::new(
        identity,
        LocalHello::new(role, inc),
        Arc::new(Epochs::in_memory()),
    );
    n.handshake_timeout = timeout;
    n
}

fn paired(timeout: Duration) -> (NodeConfig, NodeConfig) {
    let deck = node(Role::Deck, timeout);
    let station = node(Role::Station, timeout);
    deck.pins.pin(station.identity.public_key(), Role::Station);
    station.pins.pin(deck.identity.public_key(), Role::Deck);
    (deck, station)
}

async fn listen(n: &NodeConfig) -> LinkListener {
    node::listen(n.clone(), lo()).await.unwrap()
}

async fn next(l: &mut LinkListener) -> Result<OpenLink, LinkError> {
    tokio::time::timeout(T, l.next())
        .await
        .expect("listener timeout")
        .expect("listener ended")
}

async fn dial(from: &NodeConfig, to: &NodeConfig, addr: SocketAddr) -> Result<OpenLink, LinkError> {
    tokio::time::timeout(T, node::dial(from, addr, to.identity.public_key()))
        .await
        .expect("dial timeout")
}

async fn dial_with(
    from: &NodeConfig,
    to: &NodeConfig,
    addr: SocketAddr,
    fault: DialFault,
) -> Result<OpenLink, LinkError> {
    let peer = to.identity.public_key();
    tokio::time::timeout(T, node::dial_with(from, addr, peer, fault))
        .await
        .expect("dial timeout")
}

/// Dials and accepts concurrently; returns (dialed, accepted).
async fn link(from: &NodeConfig, to: &NodeConfig, l: &mut LinkListener) -> (OpenLink, OpenLink) {
    let addr = l.local_addr;
    let (d, a) = tokio::join!(dial(from, to, addr), next(l));
    (d.unwrap(), a.unwrap())
}

async fn eventually(what: &str, mut ok: impl FnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + T;
    while !ok() {
        assert!(tokio::time::Instant::now() < deadline, "timed out: {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn either_role_dials_and_both_sides_agree_on_the_link() {
    for station_dials in [true, false] {
        let (deck, station) = paired(T);
        let (dialer, listener) = if station_dials {
            (&station, &deck)
        } else {
            (&deck, &station)
        };
        let mut l = listen(listener).await;
        let (dialed, accepted) = link(dialer, listener, &mut l).await;
        assert_eq!(dialed.side, Side::Dialer);
        assert_eq!(accepted.side, Side::Listener);
        assert_eq!(dialed.id.epoch, 1);
        assert_eq!(dialed.id.attempt_id, accepted.id.attempt_id);
        assert_eq!(dialed.id.attempt_id.len(), 32);
        assert_eq!(
            (dialed.id.dialer, accepted.id.dialer),
            (Dialer::Local, Dialer::Remote)
        );
        assert_eq!(dialed.negotiated, accepted.negotiated);
        assert!(dialed.negotiated.has(feature::DATA_STREAMS));
        assert!(dialed.data.is_some() && accepted.data.is_some());
        assert_eq!(dialed.peer, listener.identity.public_key());
        assert_eq!(accepted.peer, dialer.identity.public_key());
        assert_eq!(dialer.table.len(), 1);
        assert_eq!(listener.table.len(), 1);
        drop((dialed, accepted));
        assert!(dialer.table.is_empty() && listener.table.is_empty());
    }
}

#[tokio::test]
async fn each_dial_commits_a_higher_epoch_and_the_newer_link_supersedes_the_older() {
    let (deck, station) = paired(T);
    let mut l = listen(&deck).await;
    let (d1, a1) = link(&station, &deck, &mut l).await;
    let (d2, a2) = link(&station, &deck, &mut l).await;
    assert_eq!((d1.id.epoch, d2.id.epoch), (1, 2));
    assert_ne!(d1.id.attempt_id, d2.id.attempt_id);
    for old in [&d1, &a1] {
        assert_eq!(old.closing.borrow().as_ref().unwrap().reason, SUPERSEDED);
        assert!(!old.guard.is_current());
    }
    assert!(d2.closing.borrow().is_none() && a2.closing.borrow().is_none());
    drop((d1, a1));
    assert_eq!(
        deck.table
            .current(&station.identity.fingerprint())
            .unwrap()
            .epoch,
        2
    );
    assert_eq!(
        station
            .table
            .current(&deck.identity.fingerprint())
            .unwrap()
            .epoch,
        2
    );
}

#[tokio::test]
async fn a_stale_epoch_is_refused_with_min_epoch_and_a_reset_dialer_recovers_after_one_attempt() {
    let (deck, station) = paired(T);
    let mut l = listen(&deck).await;
    for _ in 0..3 {
        drop(link(&station, &deck, &mut l).await);
    }
    // A replayed old dialer: epoch 2 after epoch 3 was accepted.
    let (d, a) = tokio::join!(
        dial_with(&station, &deck, l.local_addr, DialFault::FixedEpoch(2)),
        next(&mut l)
    );
    let LinkError::Handshake(fleetdeck_link::hello::HandshakeError::StaleEpoch {
        min_epoch, ..
    }) = d.unwrap_err()
    else {
        panic!("expected a stale epoch")
    };
    assert_eq!(min_epoch, 4);
    assert_eq!(a.unwrap_err().reason(), Some("stale_link_epoch"));
    // The station loses its database: its counter starts at 1 again.
    let mut reset = station.clone();
    reset.epochs = Arc::new(Epochs::in_memory());
    let (d, failed, accepted) = async {
        let dialing = dial(&reset, &deck, l.local_addr);
        let accepting = async { (next(&mut l).await, next(&mut l).await) };
        let (d, (x, y)) = tokio::join!(dialing, accepting);
        (d, x, y)
    }
    .await;
    let d = d.unwrap();
    assert_eq!(d.id.epoch, 4, "one refused attempt, then min_epoch");
    assert_eq!(failed.unwrap_err().reason(), Some("stale_link_epoch"));
    assert_eq!(accepted.unwrap().id.epoch, 4);
}

#[tokio::test]
async fn a_reciprocal_dial_keeps_the_link_dialed_by_the_lower_fingerprint_on_both_nodes() {
    for _ in 0..3 {
        let (deck, station) = paired(T);
        let mut dl = listen(&deck).await;
        let mut sl = listen(&station).await;
        let (to_station, to_deck) = (
            dial(&deck, &station, sl.local_addr),
            dial(&station, &deck, dl.local_addr),
        );
        let accept_deck = async { tokio::time::timeout(Duration::from_secs(3), dl.next()).await };
        let accept_station =
            async { tokio::time::timeout(Duration::from_secs(3), sl.next()).await };
        let (r1, r2, r3, r4) = tokio::join!(to_station, to_deck, accept_deck, accept_station);
        let links: Vec<OpenLink> = [r1, r2]
            .into_iter()
            .chain([r3, r4].into_iter().filter_map(|r| r.ok().flatten()))
            .filter_map(Result::ok)
            .collect();
        let (dfp, sfp) = (deck.identity.fingerprint(), station.identity.fingerprint());
        let deck_wins = dfp < sfp;
        // Both tables name the same winner: the link the lower node dialed.
        let at_deck = deck.table.current(&sfp).unwrap();
        let at_station = station.table.current(&dfp).unwrap();
        assert_eq!(at_deck.attempt_id, at_station.attempt_id);
        assert_eq!(at_deck.dialer == Dialer::Local, deck_wins);
        assert_eq!(at_station.dialer == Dialer::Local, !deck_wins);
        // Every live loser was told to close.
        for l in &links {
            if l.id.attempt_id != at_deck.attempt_id {
                assert_eq!(l.closing.borrow().as_ref().unwrap().reason, SUPERSEDED);
            }
        }
        drop(links);
        assert!(deck.table.is_empty() && station.table.is_empty());
    }
}

#[tokio::test]
async fn a_setup_that_fails_at_ready_or_at_the_data_channel_leaves_both_tables_empty() {
    let short = Duration::from_millis(400);
    for fault in [
        DialFault::CloseBeforeReady,
        DialFault::NoData,
        DialFault::WrongDataAttempt,
    ] {
        let (deck, station) = paired(short);
        let mut l = listen(&deck).await;
        let (d, a) = tokio::join!(
            dial_with(&station, &deck, l.local_addr, fault),
            next(&mut l)
        );
        let (deck_fp, station_fp) = (deck.identity.fingerprint(), station.identity.fingerprint());
        match fault {
            DialFault::NoData => {
                // The dialer holds a link without data; the listener gave up.
                let d = d.unwrap();
                assert!(d.data.is_none());
                assert_eq!(a.unwrap_err().reason(), Some(NO_DATA_CHANNEL));
                drop(d);
            }
            DialFault::WrongDataAttempt => {
                assert_eq!(d.unwrap_err().reason(), Some(NO_CONTROL_CHANNEL));
                // The listener refused the foreign data channel; its control
                // setup waits for its own data channel and then gives up.
                assert_eq!(a.unwrap_err().reason(), Some(NO_CONTROL_CHANNEL));
                let control = next(&mut l).await;
                assert_eq!(control.unwrap_err().reason(), Some(NO_DATA_CHANNEL));
            }
            _ => {
                assert!(d.is_err());
                assert!(a.is_err(), "{fault:?}");
            }
        }
        eventually("empty tables", || {
            deck.table.is_empty() && station.table.is_empty()
        })
        .await;
        assert!(deck.table.current(&station_fp).is_none());
        assert!(station.table.current(&deck_fp).is_none());
    }
}

#[tokio::test]
async fn unpaired_keys_and_wrong_roles_are_refused_with_a_reason() {
    let (deck, station) = paired(T);
    let stranger = node(Role::Station, T);
    let mut l = listen(&deck).await;
    // The dialer refuses to dial a key it has not pinned.
    let e = dial(&station, &stranger, l.local_addr).await.unwrap_err();
    assert_eq!(e.reason(), Some("not_paired"));
    // The listener refuses a key it has not pinned: TLS fails.
    stranger.pins.pin(deck.identity.public_key(), Role::Deck);
    let (d, a) = tokio::join!(dial(&stranger, &deck, l.local_addr), next(&mut l));
    assert!(d.is_err() && a.is_err());
    // A deck pinned as a station (wrong role) is refused too.
    let other_deck = node(Role::Deck, T);
    other_deck
        .pins
        .pin(deck.identity.public_key(), Role::Station);
    deck.pins
        .pin(other_deck.identity.public_key(), Role::Station);
    let (d, a) = tokio::join!(dial(&other_deck, &deck, l.local_addr), next(&mut l));
    assert!(d.is_err() && a.is_err());
    assert!(deck.table.is_empty());
    let _ = station;
}

#[tokio::test]
async fn epochs_in_sqlite_survive_a_node_restart() {
    use fleetdeck_core::db::Db;
    use rusqlite_migration::{M, Migrations};
    let dir = tempfile::tempdir().unwrap();
    let open = |name: &str| {
        let m = Migrations::new(vec![M::up(fleetdeck_link::LINK_EPOCHS_SQL)]);
        Arc::new(Epochs::sqlite(Db::open(&dir.path().join(name), m).unwrap()))
    };
    let (mut deck, mut station) = paired(T);
    deck.epochs = open("deck.db");
    station.epochs = open("station.db");
    {
        let mut l = listen(&deck).await;
        drop(link(&station, &deck, &mut l).await);
        drop(link(&station, &deck, &mut l).await);
    }
    // Both nodes restart with their databases.
    deck.epochs = open("deck.db");
    station.epochs = open("station.db");
    let mut l = listen(&deck).await;
    let (d, a) = link(&station, &deck, &mut l).await;
    assert_eq!((d.id.epoch, a.id.epoch), (3, 3));
}
