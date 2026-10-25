#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;
use std::time::Duration;

use fleetdeck_core::identity::Identity;
use fleetdeck_proto::StationId;
use fleetdeck_proto::link::Scope;
use fleetdeck_testkit::Det;
use tokio::sync::oneshot;

use super::{SEEN_REFRESH, heard_at_ms, seen_loop};
use crate::store::DeckStore;
use crate::store::stations::NewStation;

async fn store_with_station() -> (DeckStore, StationId) {
    let det = Det::new();
    let s = DeckStore::open_in_memory(det.clock(), det.ids())
        .await
        .unwrap();
    let id = s
        .pin_station(NewStation {
            key: Identity::generate().unwrap().public_key(),
            name: "s1".to_owned(),
            scopes: BTreeSet::from([Scope::Read]),
            dial_url: None,
        })
        .await
        .unwrap()
        .station_id;
    (s, id)
}

async fn last_seen(s: &DeckStore, id: &StationId) -> Option<i64> {
    s.station(id.clone()).await.unwrap().unwrap().last_seen
}

#[tokio::test]
async fn seen_loop_writes_every_interval_until_the_link_closes() {
    let (s, id) = store_with_station().await;
    let (close, closed) = oneshot::channel::<()>();
    let task = {
        let (s, id) = (s.clone(), id.clone());
        tokio::spawn(async move {
            let mut n = 0;
            let at = move || {
                n += 1000;
                n
            };
            seen_loop(&s, &id, Duration::from_millis(20), at, closed).await;
        })
    };
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while last_seen(&s, &id).await.is_none_or(|v| v < 3000) {
        assert!(tokio::time::Instant::now() < deadline, "no writes");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    drop(close);
    tokio::time::timeout(Duration::from_secs(10), task)
        .await
        .expect("the loop ends when the link closes")
        .unwrap();
    let after = last_seen(&s, &id).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(last_seen(&s, &id).await, after);
}

#[tokio::test]
async fn seen_loop_writes_nothing_before_the_first_interval() {
    let (s, id) = store_with_station().await;
    let (close, closed) = oneshot::channel::<()>();
    drop(close);
    tokio::time::timeout(
        Duration::from_secs(10),
        seen_loop(&s, &id, Duration::from_secs(60), || 1000, closed),
    )
    .await
    .expect("the loop ends at once");
    assert_eq!(last_seen(&s, &id).await, None);
}

#[test]
fn heard_at_ms_subtracts_the_idle_time() {
    assert_eq!(heard_at_ms(10_000, Duration::from_secs(3)), 7_000);
    assert_eq!(heard_at_ms(1_000, Duration::from_secs(5)), 0);
    assert_eq!(heard_at_ms(5, Duration::MAX), 0);
}

#[test]
fn seen_refresh_is_30_seconds() {
    assert_eq!(SEEN_REFRESH, Duration::from_secs(30));
}
