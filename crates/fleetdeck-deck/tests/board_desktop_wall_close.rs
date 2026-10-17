//! A wall close clears only the binding that it saw at its start (R13 P3b,
//! PRE-PR round 1, F3): a binding that the owner changes while the close is
//! in flight stays, and `display_clear_binding_at` is one conditional step.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
mod desktop_support;
mod share_support;

use std::collections::BTreeSet;

use common::board::BoardClient;
use common::*;
use desktop_support::*;
use fleetdeck_core::identity::Identity;
use fleetdeck_deck::store::displays::DisplayRecord;
use fleetdeck_deck::store::stations::NewStation;
use fleetdeck_link::LiveLink;
use fleetdeck_proto::StationId;
use fleetdeck_proto::display::DisplayBinding;
use fleetdeck_proto::link::Scope;
use serde_json::json;
use share_support::*;

async fn record(c: &BoardClient, id: &str) -> DisplayRecord {
    c.store.display_get(id.to_owned()).await.unwrap().unwrap()
}

fn binding(station: &StationId, monitor: &str) -> DisplayBinding {
    DisplayBinding {
        station_id: station.clone(),
        monitor: monitor.to_owned(),
    }
}

async fn pin_other(f: &Fixture) -> StationId {
    f.store
        .pin_station(NewStation {
            key: Identity::generate().unwrap().public_key(),
            name: "other".to_owned(),
            scopes: BTreeSet::from([Scope::Read]),
            dial_url: None,
        })
        .await
        .unwrap()
        .station_id
}

/// A close whose station answer arrives after `during` ran on the store.
async fn close_during<F: std::future::Future<Output = ()>>(
    c: &BoardClient,
    h: &Headers,
    station: &mut LiveLink,
    st: &str,
    id: &str,
    during: F,
) {
    let body = json!({ "action": "wall_close", "display_id": id });
    let answer = async {
        let req = next_request(station).await;
        assert_eq!(req.params["action"], "wall_close");
        during.await;
        station.control.respond(req.id, ok(None)).await.unwrap();
    };
    let (r, ()) = tokio::join!(call(c, h, st, body), answer);
    assert_eq!(r.status, 200, "{}", r.body);
}

#[tokio::test]
async fn a_binding_that_moves_to_another_station_during_a_close_is_kept() {
    let (f, mut station, c, h) = setup().await;
    enable(&c, true);
    let st = f.station.0.clone();
    let id = display(&f, &c, "Wall").await;
    let other = pin_other(&f).await;
    c.store
        .display_set_binding(id.clone(), Some(binding(&f.station, "DP-1")))
        .await
        .unwrap();
    let elsewhere = binding(&other, "DP-2");
    close_during(&c, &h, &mut station, &st, &id, async {
        c.store
            .display_set_binding(id.clone(), Some(elsewhere.clone()))
            .await
            .unwrap();
    })
    .await;
    assert_eq!(record(&c, &id).await.binding, Some(elsewhere));
}

#[tokio::test]
async fn a_new_revision_of_the_same_binding_during_a_close_is_kept() {
    let (f, mut station, c, h) = setup().await;
    enable(&c, true);
    let st = f.station.0.clone();
    let id = display(&f, &c, "Wall").await;
    c.store
        .display_set_binding(id.clone(), Some(binding(&f.station, "DP-1")))
        .await
        .unwrap();
    // The owner saves the settings again: same station, new monitor.
    let again = binding(&f.station, "DP-2");
    close_during(&c, &h, &mut station, &st, &id, async {
        c.store
            .display_set_binding(id.clone(), Some(again.clone()))
            .await
            .unwrap();
    })
    .await;
    assert_eq!(record(&c, &id).await.binding, Some(again));
}

#[tokio::test]
async fn a_close_that_nothing_overtakes_clears_the_binding_and_moves_the_revision() {
    let (f, mut station, c, h) = setup().await;
    enable(&c, true);
    let st = f.station.0.clone();
    let id = display(&f, &c, "Wall").await;
    c.store
        .display_set_binding(id.clone(), Some(binding(&f.station, "DP-1")))
        .await
        .unwrap();
    let before = record(&c, &id).await.binding_revision;
    close_during(&c, &h, &mut station, &st, &id, async {}).await;
    let after = record(&c, &id).await;
    assert_eq!(after.binding, None);
    assert!(after.binding_revision > before);
}

#[tokio::test]
async fn the_conditional_clear_needs_the_station_and_the_revision() {
    let (f, _station, c, _h) = setup().await;
    let id = display(&f, &c, "Wall").await;
    let other = pin_other(&f).await;
    let set = c
        .store
        .display_set_binding(id.clone(), Some(binding(&f.station, "DP-1")))
        .await
        .unwrap();
    let rev = set.binding_revision;
    // A wrong revision, a wrong station and an unknown display change nothing.
    for (display_id, station, revision) in [
        (id.clone(), f.station.clone(), rev - 1),
        (id.clone(), other.clone(), rev),
        ("nope".to_owned(), f.station.clone(), rev),
    ] {
        assert!(
            !c.store
                .display_clear_binding_at(display_id, station, revision)
                .await
                .unwrap()
        );
    }
    let same = record(&c, &id).await;
    assert_eq!(same.binding, Some(binding(&f.station, "DP-1")));
    assert_eq!(same.binding_revision, rev);
    // The matching call clears it once and moves the revision.
    assert!(
        c.store
            .display_clear_binding_at(id.clone(), f.station.clone(), rev)
            .await
            .unwrap()
    );
    let cleared = record(&c, &id).await;
    assert_eq!(cleared.binding, None);
    assert_eq!(cleared.binding_revision, rev + 1);
    assert!(
        !c.store
            .display_clear_binding_at(id.clone(), f.station.clone(), rev + 1)
            .await
            .unwrap()
    );
}
