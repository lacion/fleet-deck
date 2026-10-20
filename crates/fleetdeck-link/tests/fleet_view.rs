//! `fleet.view` on real links over loopback (MCP P3 design §2.1, §6 test 6):
//! only a station may call it, it needs the scope `admin` and the feature
//! `fleet_view.v1`. The table agreement with the ADR is in `method_table.rs`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::{Node, code, link, listen, paired, serve_one};
use fleetdeck_link::LiveLink;
use fleetdeck_proto::link::{Outcome, Scope};
use fleetdeck_proto::version::feature;
use serde_json::{Value, json};

fn params() -> Value {
    json!({"caller": "s1", "view": "stations"})
}

/// A deck and a station linked in one dial direction: (at the deck, at the
/// station).
async fn linked(deck: &Node, station: &Node, station_dials: bool) -> (LiveLink, LiveLink) {
    if station_dials {
        let mut l = listen(deck).await;
        let (s, d) = link(station, deck, &mut l).await;
        (d, s)
    } else {
        let mut l = listen(station).await;
        link(deck, station, &mut l).await
    }
}

#[tokio::test]
async fn fleet_view_is_refused_for_a_wrong_role_a_missing_admin_scope_or_a_missing_feature() {
    for station_dials in [true, false] {
        let (deck, station) = paired();
        let (mut at_deck, at_station) = linked(&deck, &station, station_dials).await;
        let fp = station.config.identity.fingerprint();

        // The deck grants the station only `coordinate`: no `admin`.
        let o = at_station
            .control
            .request("fleet.view", None, params())
            .await
            .unwrap();
        assert_eq!(code(&o), "forbidden", "{o:?}");

        // A station with the scope `admin` reaches the deck, with no
        // `coordinate` and no `command_id` (not mutating).
        deck.grants.grant(fp.clone(), [Scope::Admin]);
        let c = at_station.control.clone();
        let call = tokio::spawn(async move { c.request("fleet.view", None, params()).await });
        let answer = json!({"items": [], "next_cursor": null});
        assert_eq!(serve_one(&mut at_deck, answer.clone()).await, "fleet.view");
        assert_eq!(call.await.unwrap().unwrap(), Outcome::Ok(answer));

        // The wrong role: a deck never calls it on a station.
        let o = at_deck
            .control
            .request("fleet.view", None, params())
            .await
            .unwrap();
        assert_eq!(code(&o), "wrong_role", "{o:?}");

        // Revoking `admin` applies to the next request.
        deck.grants.grant(fp, [Scope::Coordinate]);
        let o = at_station
            .control
            .request("fleet.view", None, params())
            .await
            .unwrap();
        assert_eq!(code(&o), "forbidden", "{o:?}");
    }
}

#[tokio::test]
async fn fleet_view_needs_the_feature_on_both_sides() {
    for deck_has in [true, false] {
        for station_has in [true, false] {
            let (mut deck, mut station) = paired();
            if !deck_has {
                deck.config
                    .local
                    .features
                    .retain(|f| f != feature::FLEET_VIEW);
            }
            if !station_has {
                station
                    .config
                    .local
                    .features
                    .retain(|f| f != feature::FLEET_VIEW);
            }
            deck.grants
                .grant(station.config.identity.fingerprint(), [Scope::Admin]);
            let (mut at_deck, at_station) = linked(&deck, &station, true).await;
            if deck_has && station_has {
                let c = at_station.control.clone();
                let call =
                    tokio::spawn(async move { c.request("fleet.view", None, params()).await });
                serve_one(&mut at_deck, json!({"items": [], "next_cursor": null})).await;
                assert_eq!(code(&call.await.unwrap().unwrap()), "ok");
            } else {
                // The station refuses at its own pre-check: nothing is sent.
                let o = at_station
                    .control
                    .request("fleet.view", None, params())
                    .await
                    .unwrap();
                assert_eq!(code(&o), "feature_not_negotiated", "{o:?}");
            }
        }
    }
}
