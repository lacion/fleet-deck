//! Tests of the station palette table (R13 P4 task F1, design
//! `omarchy-p4.md` §3.7): a store, a replace, and the delete with the station.

#![allow(clippy::unwrap_used)]

use std::collections::BTreeSet;
use std::time::Duration;

use fleetdeck_core::identity::Identity;
use fleetdeck_proto::StationId;
use fleetdeck_proto::palette::{PaletteColors, PaletteMode, StationPalette};
use fleetdeck_testkit::Det;

use super::{color_ok, palette_ok};
use crate::store::DeckStore;
use crate::store::stations::NewStation;

fn colors(base: &str) -> PaletteColors {
    let c = |n: &str| format!("#{base}{base}{n}");
    PaletteColors {
        background: c("00"),
        foreground: c("01"),
        accent: c("02"),
        muted: c("03"),
        dark_background: c("04"),
        dark_foreground: c("05"),
        bright_foreground: c("06"),
        selection: c("07"),
        red: c("08"),
        yellow: c("09"),
    }
}

fn palette(base: &str, mode: PaletteMode) -> StationPalette {
    StationPalette {
        v: 1,
        mode,
        colors: colors(base),
    }
}

async fn station(s: &DeckStore, name: &str) -> (StationId, Identity) {
    let key = Identity::generate().unwrap();
    let id = s
        .pin_station(NewStation {
            key: key.public_key(),
            name: name.to_owned(),
            scopes: BTreeSet::from([fleetdeck_proto::link::Scope::Admin]),
            dial_url: None,
        })
        .await
        .unwrap()
        .station_id;
    (id, key)
}

#[test]
fn a_color_is_a_hash_and_six_hex_digits_of_either_case() {
    for ok in ["#000000", "#ffffff", "#FFFFFF", "#aB09fE"] {
        assert!(color_ok(ok), "{ok}");
    }
    for bad in [
        "",
        "#",
        "000000",
        "#00000",
        "#0000000",
        "#00000g",
        "#00000G",
        " #000000",
        "#000000\n",
        "#00 000",
        "##00000",
        "#+00000",
        "red",
    ] {
        assert!(!color_ok(bad), "{bad:?}");
    }
}

#[test]
fn a_palette_needs_version_1_and_ten_good_colors() {
    let good = palette("00", PaletteMode::Dark);
    assert!(palette_ok(&good));
    let mut v2 = good.clone();
    v2.v = 2;
    assert!(!palette_ok(&v2));
    // Each of the ten colors is checked.
    for i in 0..10 {
        let mut bad = good.clone();
        let c = &mut bad.colors;
        let slot = [
            &mut c.background,
            &mut c.foreground,
            &mut c.accent,
            &mut c.muted,
            &mut c.dark_background,
            &mut c.dark_foreground,
            &mut c.bright_foreground,
            &mut c.selection,
            &mut c.red,
            &mut c.yellow,
        ]
        .into_iter()
        .nth(i)
        .unwrap();
        *slot = "#12345".to_owned();
        assert!(!palette_ok(&bad), "color {i}");
    }
}

#[tokio::test]
async fn a_palette_is_stored_and_a_second_one_replaces_it() {
    let det = Det::new();
    let s = DeckStore::open_in_memory(det.clock(), det.ids())
        .await
        .unwrap();
    let (a, _) = station(&s, "a").await;
    assert_eq!(s.station_palette(a.clone()).await.unwrap(), None);

    s.put_station_palette(a.clone(), palette("aB", PaletteMode::Dark))
        .await
        .unwrap();
    let first = s.station_palette(a.clone()).await.unwrap().unwrap();
    assert_eq!(first.mode, PaletteMode::Dark);
    // The colors are stored as given: the upper case stays.
    assert_eq!(first.colors, colors("aB"));

    det.advance(Duration::from_secs(5));
    s.put_station_palette(a.clone(), palette("Cd", PaletteMode::Light))
        .await
        .unwrap();
    let second = s.station_palette(a.clone()).await.unwrap().unwrap();
    assert_eq!(second.mode, PaletteMode::Light);
    assert_eq!(second.colors, colors("Cd"));
    assert!(second.updated_at > first.updated_at);
    // One row for the station.
    let rows: i64 = crate::store::call(&s.inner.db, |conn| {
        Ok(conn.query_row("SELECT count(*) FROM station_palettes", [], |r| r.get(0))?)
    })
    .await
    .unwrap();
    assert_eq!(rows, 1);
}

#[tokio::test]
async fn each_station_keeps_its_own_palette() {
    let det = Det::new();
    let s = DeckStore::open_in_memory(det.clock(), det.ids())
        .await
        .unwrap();
    let (a, _) = station(&s, "a").await;
    let (b, _) = station(&s, "b").await;
    s.put_station_palette(a.clone(), palette("11", PaletteMode::Dark))
        .await
        .unwrap();
    assert_eq!(s.station_palette(b.clone()).await.unwrap(), None);
    s.put_station_palette(b.clone(), palette("22", PaletteMode::Light))
        .await
        .unwrap();
    let a_row = s.station_palette(a).await.unwrap().unwrap();
    let b_row = s.station_palette(b).await.unwrap().unwrap();
    assert_eq!(a_row.colors, colors("11"));
    assert_eq!(b_row.colors, colors("22"));
}

#[tokio::test]
async fn the_palette_goes_with_its_station() {
    let det = Det::new();
    let s = DeckStore::open_in_memory(det.clock(), det.ids())
        .await
        .unwrap();
    let (a, key_a) = station(&s, "a").await;
    let (b, _) = station(&s, "b").await;
    for id in [&a, &b] {
        s.put_station_palette(id.clone(), palette("33", PaletteMode::Dark))
            .await
            .unwrap();
    }
    s.revoke_station(key_a.public_key()).await.unwrap().unwrap();
    assert_eq!(s.station_palette(a).await.unwrap(), None);
    assert!(s.station_palette(b).await.unwrap().is_some());
}

#[tokio::test]
async fn a_palette_of_a_station_that_is_not_paired_is_not_stored() {
    let det = Det::new();
    let s = DeckStore::open_in_memory(det.clock(), det.ids())
        .await
        .unwrap();
    // The call of a station that `revoke_station` just removed.
    let gone = StationId::from("st-gone".to_owned());
    s.put_station_palette(gone.clone(), palette("aB", PaletteMode::Dark))
        .await
        .unwrap();
    assert_eq!(s.station_palette(gone).await.unwrap(), None);
}
