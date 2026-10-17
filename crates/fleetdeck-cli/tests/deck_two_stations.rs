//! A deck with two stations as separate processes on loopback (M2 P3-6b;
//! PLAN Phase 3 exit, part 1): station `alpha` listens and the deck dials
//! it (`station invite` + `deck add-station`); station `bravo` has no
//! listener and dials the deck (`deck invite` + `station join`, the NAT
//! case). Deck and station restarts lose no events and repeat none: the
//! deck's cards equal the stations' own summaries.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use common::deck::Deck;
use common::{Station, WAIT};
use fleetdeck_deck::DeckSession;
use fleetdeck_proto::SessionSummary;

/// The station name of every card, by session id.
fn by_session(deck: &Deck, cards: &[DeckSession]) -> BTreeMap<String, (String, SessionSummary)> {
    let names: BTreeMap<_, _> = deck
        .stations()
        .into_iter()
        .map(|st| (st.station_id, st.name))
        .collect();
    cards
        .iter()
        .map(|c| {
            (
                c.summary.session_id.as_str().to_owned(),
                (names[&c.station_id].clone(), c.summary.clone()),
            )
        })
        .collect()
}

/// Waits until the deck's cards are exactly the sessions of the given
/// stations, each equal to the station's own summary.
fn converged(deck: &Deck, stations: &[&Station]) {
    let deadline = Instant::now() + WAIT;
    loop {
        let mut want = BTreeMap::new();
        for st in stations {
            for s in st.sessions() {
                want.insert(
                    s.session_id.as_str().to_owned(),
                    (st.node.name().to_owned(), s),
                );
            }
        }
        let cards = deck.sessions();
        let got = by_session(deck, &cards);
        if got == want && cards.len() == want.len() {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "the deck did not converge:\nwant {want:#?}\ngot {got:#?}"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[test]
fn a_dialed_and_a_dialing_station_sync_through_deck_and_station_restarts() {
    let mut deck = Deck::start();

    // Alpha listens; the deck dials it.
    let mut alpha = Station::named("alpha");
    alpha.spawn(None);
    let inv = alpha.ok(&["station", "invite", "--listen", "127.0.0.1:0"]);
    let out = deck.ok_stdin(&["deck", "add-station", "--stdin"], inv.trim());
    assert!(out.contains("paired with station alpha"), "{out}");

    // Bravo has no listener (listen = []); it dials the deck.
    let mut bravo = Station::named("bravo");
    bravo.spawn(None);
    let inv = deck.ok(&["deck", "invite"]);
    bravo.ok_stdin(&["station", "join", "--stdin"], inv.trim());

    let a1 = alpha.run("hello alpha");
    let b1 = bravo.run("hello bravo");
    converged(&deck, &[&alpha, &bravo]);
    let stations = deck.stations();
    let dirs: BTreeMap<_, _> = stations
        .iter()
        .map(|st| (st.name.as_str(), (st.direction.as_str(), st.reachable)))
        .collect();
    assert_eq!(dirs["alpha"], ("deck_dials", true));
    assert_eq!(dirs["bravo"], ("station_dials", true));

    // The deck restarts; both stations work on while it is away.
    deck.stop();
    let a2 = alpha.run("while the deck is away");
    let b2 = bravo.run("while the deck is away");
    deck.spawn();
    converged(&deck, &[&alpha, &bravo]);

    // Both stations restart; the links come back from the stored pins.
    alpha.stop();
    bravo.stop();
    deck.wait_for("the cards of stopped stations", |cards| {
        cards.len() == 4
            && cards
                .iter()
                .all(|c| serde_json::to_value(c.status).unwrap() != "working")
    });
    alpha.spawn(None);
    bravo.spawn(None);
    let a3 = alpha.run("after the station restart");
    let b3 = bravo.run("after the station restart");
    converged(&deck, &[&alpha, &bravo]);
    let ids: Vec<String> = deck
        .sessions()
        .iter()
        .map(|c| c.summary.session_id.as_str().to_owned())
        .collect();
    for id in [&a1, &a2, &a3, &b1, &b2, &b3] {
        assert_eq!(ids.iter().filter(|i| *i == id).count(), 1, "{id}");
    }

    // `remove-station` drops bravo's cards; alpha stays.
    let out = deck.ok(&["deck", "remove-station", "bravo"]);
    assert!(out.contains("removed station bravo"), "{out}");
    assert_eq!(deck.stations().len(), 1);
    converged(&deck, &[&alpha]);
}
