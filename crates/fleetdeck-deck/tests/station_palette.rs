//! `station.palette` from a fake station (R13 P4 task F1, design
//! `omarchy-p4.md` §3.7): the deck stores the palette under the calling
//! station only, replaces it, refuses every wrong shape with
//! `invalid_params`, and deletes it with the station.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::BTreeSet;

use common::*;
use fleetdeck_core::identity::Identity;
use fleetdeck_deck::store::stations::NewStation;
use fleetdeck_proto::StationId;
use fleetdeck_proto::link::{Outcome, Scope};
use fleetdeck_proto::palette::{MAX_PALETTE_BYTES, PaletteMode};
use serde_json::{Value, json};

fn palette(base: &str, mode: &str) -> Value {
    let c = |n: &str| format!("#{base}{base}{n}");
    json!({"v": 1, "mode": mode, "colors": {
        "background": c("00"), "foreground": c("01"), "accent": c("02"),
        "muted": c("03"), "dark_background": c("04"), "dark_foreground": c("05"),
        "bright_foreground": c("06"), "selection": c("07"), "red": c("08"),
        "yellow": c("09")}})
}

async fn call(station: &fleetdeck_link::LiveLink, params: Value) -> Outcome {
    tokio::time::timeout(T, station.control.request("station.palette", None, params))
        .await
        .expect("answer")
        .unwrap()
}

fn refused(o: Outcome) -> (String, String) {
    match o {
        Outcome::Err(e) => (e.code, e.message),
        Outcome::Ok(v) => panic!("expected an error, got {v}"),
    }
}

async fn other_station(f: &Fixture) -> StationId {
    f.store
        .pin_station(NewStation {
            key: Identity::generate().unwrap().public_key(),
            name: "other".to_owned(),
            scopes: BTreeSet::from([Scope::Admin]),
            dial_url: None,
        })
        .await
        .unwrap()
        .station_id
}

#[tokio::test]
async fn the_deck_stores_the_palette_and_a_second_one_replaces_it() {
    let mut f = fixture().await;
    let station = f.connect().await;
    assert_eq!(
        f.store.station_palette(f.station.clone()).await.unwrap(),
        None
    );

    let Outcome::Ok(answer) = call(&station, palette("aB", "dark")).await else {
        panic!("the deck refused");
    };
    assert_eq!(answer, json!({"ok": true}));
    let first = f
        .store
        .station_palette(f.station.clone())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first.mode, PaletteMode::Dark);
    // Stored as given: the upper case stays.
    assert_eq!(first.colors.background, "#aBaB00");
    assert_eq!(first.colors.yellow, "#aBaB09");

    f.det.advance(std::time::Duration::from_secs(5));
    assert!(matches!(
        call(&station, palette("Cd", "light")).await,
        Outcome::Ok(_)
    ));
    let second = f
        .store
        .station_palette(f.station.clone())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(second.mode, PaletteMode::Light);
    assert_eq!(second.colors.red, "#CdCd08");
    assert!(second.updated_at > first.updated_at);
}

#[tokio::test]
async fn the_deck_never_stores_a_palette_under_another_station() {
    let mut f = fixture().await;
    let station = f.connect().await;
    let other = other_station(&f).await;
    // The params cannot name a station: a key of this kind is refused.
    for key in ["station_id", "station"] {
        let mut named = palette("11", "dark");
        named[key] = json!(other.as_str());
        let (code, text) = refused(call(&station, named).await);
        assert_eq!(code, "invalid_params", "{key}");
        assert_eq!(text, "The palette is not valid.");
    }
    assert_eq!(f.store.station_palette(other.clone()).await.unwrap(), None);
    assert_eq!(
        f.store.station_palette(f.station.clone()).await.unwrap(),
        None
    );
    // A good palette lands under the calling station, and under no other.
    assert!(matches!(
        call(&station, palette("22", "dark")).await,
        Outcome::Ok(_)
    ));
    assert!(
        f.store
            .station_palette(f.station.clone())
            .await
            .unwrap()
            .is_some()
    );
    assert_eq!(f.store.station_palette(other).await.unwrap(), None);
}

#[tokio::test]
async fn every_wrong_palette_is_invalid_params_and_changes_nothing() {
    let mut f = fixture().await;
    let station = f.connect().await;
    assert!(matches!(
        call(&station, palette("33", "dark")).await,
        Outcome::Ok(_)
    ));
    let kept = f.store.station_palette(f.station.clone()).await.unwrap();

    let mut cases: Vec<Value> = Vec::new();
    let mut v2 = palette("44", "dark");
    v2["v"] = json!(2);
    cases.push(v2);
    cases.push(palette("44", "blue"));
    let mut short = palette("44", "dark");
    short["colors"]["red"] = json!("#ff00");
    cases.push(short);
    let mut word = palette("44", "dark");
    word["colors"]["accent"] = json!("red");
    cases.push(word);
    let mut missing = palette("44", "dark");
    missing["colors"].as_object_mut().unwrap().remove("muted");
    cases.push(missing);
    let mut extra = palette("44", "dark");
    extra["colors"]["color3"] = json!("#000000");
    cases.push(extra);
    let mut top = palette("44", "dark");
    top["x"] = json!(1);
    cases.push(top);
    let mut big = palette("44", "dark");
    big["pad"] = json!("x".repeat(MAX_PALETTE_BYTES));
    cases.push(big);
    cases.push(json!({}));
    cases.push(json!([]));
    for case in cases {
        let (code, text) = refused(call(&station, case.clone()).await);
        assert_eq!(code, "invalid_params", "{case}");
        assert_eq!(text, "The palette is not valid.");
    }
    assert_eq!(
        f.store.station_palette(f.station.clone()).await.unwrap(),
        kept
    );
}

#[tokio::test]
async fn a_palette_writes_no_audit_row() {
    let mut f = fixture().await;
    let station = f.connect().await;
    assert!(matches!(
        call(&station, palette("55", "dark")).await,
        Outcome::Ok(_)
    ));
    let rows = f.store.audit_log(50).await.unwrap();
    assert!(
        rows.iter().all(|r| !r.action.contains("palette")),
        "{rows:?}"
    );
}

#[tokio::test]
async fn the_palette_goes_when_the_station_is_removed() {
    let mut f = fixture().await;
    let station = f.connect().await;
    assert!(matches!(
        call(&station, palette("66", "dark")).await,
        Outcome::Ok(_)
    ));
    assert!(
        f.store
            .station_palette(f.station.clone())
            .await
            .unwrap()
            .is_some()
    );
    let key = f.station_node.identity.public_key();
    f.store.revoke_station(key).await.unwrap().unwrap();
    assert_eq!(
        f.store.station_palette(f.station.clone()).await.unwrap(),
        None
    );
}
