#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;
use std::time::Duration;

use fleetdeck_core::identity::Identity;
use fleetdeck_proto::StationId;
use fleetdeck_proto::link::Scope;
use fleetdeck_testkit::Det;

use super::DeckStore;
use crate::store::stations::NewStation;

async fn store(det: &Det) -> DeckStore {
    DeckStore::open_in_memory(det.clock(), det.ids())
        .await
        .unwrap()
}

async fn paired(s: &DeckStore) -> StationId {
    s.pin_station(NewStation {
        key: Identity::generate().unwrap().public_key(),
        name: "s1".to_owned(),
        scopes: BTreeSet::from([Scope::Read]),
        dial_url: None,
    })
    .await
    .unwrap()
    .station_id
}

async fn last_seen(s: &DeckStore, id: &StationId) -> Option<i64> {
    s.station(id.clone()).await.unwrap().unwrap().last_seen
}

fn ms(det: &Det) -> i64 {
    i64::try_from(det.now_ms()).unwrap()
}

#[tokio::test]
async fn station_link_linked_since_keeps_the_first_up_and_clears_on_down() {
    let det = Det::new();
    let s = store(&det).await;
    let id = paired(&s).await;
    assert_eq!(s.linked_since(&id), None);
    let t0 = ms(&det);
    s.set_reachable(&id, true);
    assert_eq!(s.linked_since(&id), Some(t0));
    det.advance(Duration::from_secs(5));
    s.set_reachable(&id, true);
    assert_eq!(s.linked_since(&id), Some(t0));
    s.set_reachable(&id, false);
    assert_eq!(s.linked_since(&id), None);
    assert!(!s.is_reachable(&id));
    det.advance(Duration::from_secs(5));
    let t1 = ms(&det);
    s.set_reachable(&id, true);
    assert_eq!(s.linked_since(&id), Some(t1));
    assert!(s.is_reachable(&id));
}

#[tokio::test]
async fn station_link_board_version_grows_only_on_a_change() {
    let det = Det::new();
    let s = store(&det).await;
    let id = paired(&s).await;
    let rx = s.watch_board();
    let v0 = *rx.borrow();
    s.set_reachable(&id, true);
    s.set_reachable(&id, true);
    assert_eq!(*rx.borrow(), v0 + 1);
    s.set_reachable(&id, false);
    s.set_reachable(&id, false);
    assert_eq!(*rx.borrow(), v0 + 2);
}

#[tokio::test]
async fn station_link_heard_never_moves_last_seen_back() {
    let det = Det::new();
    let s = store(&det).await;
    let id = paired(&s).await;
    assert_eq!(last_seen(&s, &id).await, None);
    s.station_heard(id.clone(), 2000).await.unwrap();
    assert_eq!(last_seen(&s, &id).await, Some(2000));
    s.station_heard(id.clone(), 1000).await.unwrap();
    assert_eq!(last_seen(&s, &id).await, Some(2000));
    s.station_heard(id.clone(), 3000).await.unwrap();
    assert_eq!(last_seen(&s, &id).await, Some(3000));
}

#[tokio::test]
async fn station_link_heard_does_not_change_the_board_version() {
    let det = Det::new();
    let s = store(&det).await;
    let id = paired(&s).await;
    let rx = s.watch_board();
    let v0 = *rx.borrow();
    s.station_heard(id, 5000).await.unwrap();
    assert_eq!(*rx.borrow(), v0);
}

#[tokio::test]
async fn station_link_heard_of_an_unknown_station_is_ok() {
    let det = Det::new();
    let s = store(&det).await;
    s.station_heard(StationId::from("nobody"), 5000)
        .await
        .unwrap();
}

#[tokio::test]
async fn station_link_view_has_linked_at_only_while_reachable() {
    let det = Det::new();
    let s = store(&det).await;
    let id = paired(&s).await;
    let t0 = ms(&det);
    s.set_reachable(&id, true);
    let rec = s.station(id.clone()).await.unwrap().unwrap();
    let up = crate::local::station_view(&rec, &s);
    assert_eq!(up.linked_at, Some(t0));
    s.set_reachable(&id, false);
    let down = crate::local::station_view(&rec, &s);
    assert_eq!(down.linked_at, None);
    let json = serde_json::to_value(&down).unwrap();
    assert!(json.get("linked_at").is_none());
}
