//! The known deck origins of the board barrier (R13 P7 §7.2): the hosts come
//! from the dial address of the paired deck, and a preview that is launched on
//! one of them is marked and never receives input.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;

use fleetdeck_core::ChildEnv;
use fleetdeck_core::identity::{Identity, PeerKey};
use fleetdeck_link::tls::Role;
use fleetdeck_proto::link::Scope;
use fleetdeck_testkit::Det;

use super::{Desktop, Store, deck_hosts, mark_deck_origin};
use crate::store::StoreConfig;
use crate::store::peers::PairedKey;

async fn store() -> Store {
    let det = Det::new();
    let store = Store::open_in_memory(det.clock(), det.ids(), StoreConfig::default())
        .await
        .unwrap();
    store.recover(Vec::new()).await.unwrap();
    store
}

fn key() -> PeerKey {
    Identity::generate().unwrap().public_key()
}

fn deck(dial_url: Option<&str>) -> PairedKey {
    PairedKey {
        key: key(),
        role: Role::Deck,
        scopes: BTreeSet::from([Scope::Read]),
        dial_url: dial_url.map(str::to_owned),
    }
}

#[tokio::test]
async fn the_deck_hosts_are_the_dial_host_of_the_paired_deck() {
    let s = store().await;
    assert!(deck_hosts(&s).await.unwrap().is_empty());
    s.pin_peer(deck(Some("wss://Deck.Example:7000/path")))
        .await
        .unwrap();
    assert_eq!(deck_hosts(&s).await.unwrap(), ["deck.example"]);
}

#[tokio::test]
async fn a_station_that_the_deck_dials_knows_no_deck_host() {
    let s = store().await;
    s.pin_peer(deck(None)).await.unwrap();
    assert!(deck_hosts(&s).await.unwrap().is_empty());
}

#[tokio::test]
async fn a_preview_on_a_deck_origin_is_marked_and_another_is_not() {
    let s = store().await;
    s.pin_peer(deck(Some("wss://deck.example:7000")))
        .await
        .unwrap();
    let desktop = Desktop::with_env(ChildEnv::empty());
    mark_deck_origin(&s, &desktop, "https://deck.example/board", "marked").await;
    mark_deck_origin(&s, &desktop, "http://localhost:3000/", "plain").await;
    mark_deck_origin(&s, &desktop, "https://deck.example.evil/", "evil").await;
    assert!(desktop.is_board_origin("marked"));
    assert!(!desktop.is_board_origin("plain"));
    assert!(!desktop.is_board_origin("evil"));
}
