//! `display.directive` and `station.palette` from a fake station (R13 P4
//! task A, feature `desktop_shell.v1`, decision P4-6): the rows use `admin`,
//! which every paired station holds. No scope is added to a station. Until
//! tasks E and F1 build the handlers, the deck answers `unsupported`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::BTreeSet;

use common::*;
use fleetdeck_deck::link::runtime::STATION_SCOPES;
use fleetdeck_link::GrantSource;
use fleetdeck_link::methods::code;
use fleetdeck_proto::link::{Outcome, Scope};
use serde_json::json;

fn refused(o: Outcome) -> (String, String) {
    match o {
        Outcome::Err(e) => (e.code, e.message),
        Outcome::Ok(v) => panic!("expected an error, got {v}"),
    }
}

#[tokio::test]
async fn a_paired_station_gets_no_new_scope() {
    let f = fixture().await;
    let admin: BTreeSet<Scope> = BTreeSet::from([Scope::Admin]);
    assert_eq!(STATION_SCOPES.into_iter().collect::<BTreeSet<_>>(), admin);
    // The stored scopes and the cached scopes are unchanged.
    let stored = f.store.station(f.station.clone()).await.unwrap().unwrap();
    assert_eq!(stored.scopes, admin);
    let cached = f.ctx.peers.scopes(&stored.key.fingerprint());
    assert_eq!(cached, admin);
}

#[tokio::test]
async fn a_station_call_to_the_two_rows_passes_the_scope_check_and_gets_the_stub_answer() {
    let mut f = fixture().await;
    let station = f.connect().await;
    for (method, text) in [
        (
            "display.directive",
            "The deck does not serve display.directive in this build.",
        ),
        (
            "station.palette",
            "The deck does not serve station.palette in this build.",
        ),
    ] {
        let o = tokio::time::timeout(T, station.control.request(method, None, json!({})))
            .await
            .expect("answer")
            .unwrap();
        let (c, message) = refused(o);
        assert_eq!(c, "unsupported", "{method}");
        assert_eq!(message, text, "{method}");
    }
}

#[tokio::test]
async fn a_station_without_the_admin_scope_is_forbidden() {
    let mut f = fixture_with(BTreeSet::from([Scope::Read])).await;
    let station = f.connect().await;
    for method in ["display.directive", "station.palette"] {
        let o = tokio::time::timeout(T, station.control.request(method, None, json!({})))
            .await
            .expect("answer")
            .unwrap();
        let (c, _) = refused(o);
        assert_eq!(c, code::FORBIDDEN, "{method}");
    }
}
