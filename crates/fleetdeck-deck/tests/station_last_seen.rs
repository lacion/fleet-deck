//! B22: the end of a link is the last-seen time of the station, written
//! before the station goes offline; `linked_at` is the link start.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::time::Duration;

use common::{T, fixture};
use fleetdeck_deck::local::station_view;

#[tokio::test]
async fn the_end_of_a_link_is_the_last_seen_time_and_is_written_before_the_station_goes_offline() {
    let mut f = fixture().await;
    let t0 = i64::try_from(f.det.now_ms()).unwrap();
    let station = f.connect().await;
    assert_eq!(f.store.linked_since(&f.station), Some(t0));
    f.det.advance(Duration::from_secs(600));
    let mut board = f.store.watch_board();
    let _ = board.borrow_and_update();
    station.control.close("test", "the station stops");
    let deadline = tokio::time::Instant::now() + T;
    loop {
        if !f.store.is_reachable(&f.station) {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the station never went offline"
        );
        let _ = tokio::time::timeout(Duration::from_millis(50), board.changed()).await;
    }
    let rec = f.store.station(f.station.clone()).await.unwrap().unwrap();
    let seen = rec.last_seen.expect("last_seen is set");
    assert!(seen >= t0 + 600_000 - 5_000, "last_seen {seen} is too old");
    assert!(seen <= i64::try_from(f.det.now_ms()).unwrap());
    assert_eq!(f.store.linked_since(&f.station), None);
    assert_eq!(station_view(&rec, &f.store).linked_at, None);
}
