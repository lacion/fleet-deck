//! The palette routes over loopback HTTPS (R13 P4 task F1, design
//! `omarchy-p4.md` §3.7): the owner route by station id, and the display
//! route with its five steps and its one `404 no_palette`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
mod displays_support;
mod share_support;

use std::collections::BTreeSet;

use common::Fixture;
use common::board::{BoardClient, Http};
use displays_support::*;
use fleetdeck_core::Secret;
use fleetdeck_core::identity::Identity;
use fleetdeck_deck::http::palette_routes::palette_for_display;
use fleetdeck_deck::store::displays::{DisplayChange, DisplayLogin};
use fleetdeck_deck::store::stations::NewStation;
use fleetdeck_proto::StationId;
use fleetdeck_proto::display::DisplayBinding;
use fleetdeck_proto::link::Scope;
use fleetdeck_proto::palette::{PaletteMode, StationPalette};
use serde_json::{Value, json};
use share_support::*;

const NO_PALETTE: &str = "No palette for this station.";

fn palette(base: &str, mode: PaletteMode) -> StationPalette {
    let c = |n: &str| format!("#{base}{base}{n}");
    serde_json::from_value(json!({"v": 1, "mode": mode, "colors": {
        "background": c("00"), "foreground": c("01"), "accent": c("02"),
        "muted": c("03"), "dark_background": c("04"), "dark_foreground": c("05"),
        "bright_foreground": c("06"), "selection": c("07"), "red": c("08"),
        "yellow": c("09")}}))
    .unwrap()
}

struct Rig {
    _f: Fixture,
    c: BoardClient,
    h: Headers,
    /// The linked station of the fixture; it has a palette.
    st: String,
    /// A second station; it has a palette.
    other: String,
    /// A third station; it has no palette.
    bare: String,
    display: String,
    cookie: String,
}

async fn pin(c: &BoardClient, name: &str) -> String {
    c.store
        .pin_station(NewStation {
            key: Identity::generate().unwrap().public_key(),
            name: name.to_owned(),
            scopes: BTreeSet::from([Scope::Read]),
            dial_url: None,
        })
        .await
        .unwrap()
        .station_id
        .as_str()
        .to_owned()
}

/// A display with the fixture station and `other` in its ceiling, bound to the
/// fixture station.
async fn rig() -> Rig {
    let (f, c, h, st) = setup_deck().await;
    let other = pin(&c, "other").await;
    let bare = pin(&c, "bare").await;
    for (id, base, mode) in [
        (&st, "ab", PaletteMode::Dark),
        (&other, "cd", PaletteMode::Light),
    ] {
        c.store
            .put_station_palette(StationId::new(id.clone()), palette(base, mode))
            .await
            .unwrap();
    }
    let (display, cookie) = display_login(&c, "Wall", &[st.as_str(), other.as_str()]).await;
    bind(&c, &display, Some(&st)).await;
    Rig {
        _f: f,
        c,
        h,
        st,
        other,
        bare,
        display,
        cookie,
    }
}

async fn bind(c: &BoardClient, display: &str, station: Option<&str>) {
    c.store
        .display_set_binding(
            display.to_owned(),
            station.map(|s| DisplayBinding {
                station_id: StationId::new(s),
                monitor: "DP-1".to_owned(),
            }),
        )
        .await
        .unwrap();
}

impl Rig {
    async fn read(&self) -> Http {
        get_with(&self.c, "/api/displays/self/palette", &self.cookie).await
    }

    async fn login(&self) -> DisplayLogin {
        let value = self.cookie.strip_prefix("fd_display=").unwrap();
        self.c
            .store
            .display_session(&Secret::new(value.to_owned()))
            .await
            .unwrap()
            .expect("a live display login")
    }
}

fn miss(r: &Http) {
    assert_eq!(
        (r.status, r.code(), r.body["message"].as_str()),
        (404, "no_palette", Some(NO_PALETTE)),
        "{}",
        r.body
    );
}

#[tokio::test]
async fn the_owner_reads_the_palette_of_a_station_by_id() {
    let r = rig().await;
    let got = get(&r.c, &r.h, &format!("/api/stations/{}/palette", r.st)).await;
    assert_eq!(got.status, 200, "{}", got.body);
    assert_eq!(got.body["mode"], "dark");
    assert_eq!(got.body["colors"]["background"], "#abab00");
    assert_eq!(got.body["colors"]["yellow"], "#abab09");
    assert!(got.body["updated_at"].as_i64().is_some());
    assert_eq!(got.body.as_object().unwrap().len(), 3, "{}", got.body);
    let other = get(&r.c, &r.h, &format!("/api/stations/{}/palette", r.other)).await;
    assert_eq!(other.body["mode"], "light");
    assert_eq!(other.body["colors"]["red"], "#cdcd08");
}

#[tokio::test]
async fn the_owner_route_takes_an_id_not_a_name_and_misses_without_a_row() {
    let r = rig().await;
    // The name of the fixture station is `fake`.
    miss(&get(&r.c, &r.h, "/api/stations/fake/palette").await);
    miss(&get(&r.c, &r.h, "/api/stations/other/palette").await);
    miss(&get(&r.c, &r.h, "/api/stations/no-such-station/palette").await);
    // A paired station that never sent a palette.
    miss(&get(&r.c, &r.h, &format!("/api/stations/{}/palette", r.bare)).await);
}

#[tokio::test]
async fn the_owner_route_needs_the_owner_login() {
    let r = rig().await;
    let path = format!("/api/stations/{}/palette", r.st);
    expect(&r.c.get(&path, &[]).await, 401, "login_required");
    // A display never reaches an owner row.
    expect(
        &get_with(&r.c, &path, &r.cookie).await,
        403,
        "display_read_only",
    );
}

#[tokio::test]
async fn the_display_reads_the_palette_of_its_bound_station_with_no_store() {
    let r = rig().await;
    let got = r.read().await;
    assert_eq!(got.status, 200, "{}", got.body);
    assert_eq!(got.header("cache-control"), Some("no-store"));
    assert_eq!(got.body["mode"], "dark");
    assert_eq!(got.body["colors"]["accent"], "#abab02");
    // The owner route answers the same palette.
    let owner = get(&r.c, &r.h, &format!("/api/stations/{}/palette", r.st)).await;
    assert_eq!(owner.body, got.body);
    // A rebinding to the other station in the ceiling gives its palette.
    bind(&r.c, &r.display, Some(&r.other)).await;
    let moved = r.read().await;
    assert_eq!(moved.status, 200, "{}", moved.body);
    assert_eq!(moved.body["mode"], "light");
}

#[tokio::test]
async fn the_display_route_refuses_an_owner_cookie_and_any_other_principal() {
    let r = rig().await;
    let path = "/api/displays/self/palette";
    // The owner cookie alone is not a display login.
    expect(&get(&r.c, &r.h, path).await, 401, "login_required");
    // No login at all.
    expect(&r.c.get(path, &[]).await, 401, "login_required");
    // A garbage display cookie.
    expect(
        &r.c.get(path, &[("Cookie", "fd_display=nope")]).await,
        401,
        "login_required",
    );
    // With both cookies the display principal answers, never the owner.
    let both = format!("{}; {}", r.h[0].1, r.cookie);
    let got = r.c.get(path, &[("Cookie", &both)]).await;
    assert_eq!(got.status, 200, "{}", got.body);
    // A member of a shared board is not a display either.
    let (member, _, _) = join(
        &r.c,
        &r.h,
        json!({ "kind": "board", "role": "approver" }),
        "Ann",
    )
    .await;
    expect(&get(&r.c, &member, path).await, 401, "login_required");
}

#[tokio::test]
async fn every_refusal_of_the_display_route_is_the_same_404() {
    let r = rig().await;
    // An unbound display.
    bind(&r.c, &r.display, None).await;
    let unbound = r.read().await;
    miss(&unbound);
    // Bound to a station outside the ceiling.
    let outside = pin(&r.c, "outside").await;
    r.c.store
        .put_station_palette(
            StationId::new(outside.clone()),
            palette("ef", PaletteMode::Dark),
        )
        .await
        .unwrap();
    bind(&r.c, &r.display, Some(&outside)).await;
    let beyond = r.read().await;
    miss(&beyond);
    // Bound to a station in the ceiling that has no palette.
    r.c.store
        .display_update(
            r.display.clone(),
            DisplayChange {
                ceiling: Some(vec![
                    StationId::new(r.st.clone()),
                    StationId::new(r.bare.clone()),
                ]),
                ..DisplayChange::default()
            },
        )
        .await
        .unwrap();
    bind(&r.c, &r.display, Some(&r.bare)).await;
    let bare = r.read().await;
    miss(&bare);
    // The three answers are the same, byte for byte.
    assert_eq!(unbound.body, beyond.body);
    assert_eq!(unbound.body, bare.body);
    // A ceiling change that drops the bound station.
    bind(&r.c, &r.display, Some(&r.st)).await;
    assert_eq!(r.read().await.status, 200);
    r.c.store
        .display_update(
            r.display.clone(),
            DisplayChange {
                ceiling: Some(vec![StationId::new(r.bare.clone())]),
                ..DisplayChange::default()
            },
        )
        .await
        .unwrap();
    miss(&r.read().await);
}

#[tokio::test]
async fn an_ended_display_session_and_a_revoked_display_get_401() {
    let r = rig().await;
    assert_eq!(r.read().await.status, 200);
    let login = r.login().await;
    assert!(
        r.c.store
            .end_display_session(login.session_hash)
            .await
            .unwrap()
    );
    expect(&r.read().await, 401, "login_required");

    let q = rig().await;
    assert_eq!(q.read().await.status, 200);
    assert!(q.c.store.display_revoke(q.display.clone()).await.unwrap());
    expect(&q.read().await, 401, "login_required");
}

/// The steps 3 to 4: a change between the read and the second read of the
/// binding and the scope gives a miss (`None`) or a `401`.
#[tokio::test]
async fn a_rebinding_during_the_read_gives_no_palette() {
    let r = rig().await;
    let login = r.login().await;
    let state = &r.c.state;
    // Control: nothing changes, the palette answers.
    let kept = palette_for_display(state, &login, async {}).await.unwrap();
    assert_eq!(kept.unwrap().mode, PaletteMode::Dark);
    // The display is bound to another station of its ceiling mid-read: the
    // palette that was read belongs to the old station.
    let moved = palette_for_display(state, &login, async {
        bind(&r.c, &r.display, Some(&r.other)).await;
    })
    .await
    .unwrap();
    assert_eq!(moved, None);
    // An unbinding mid-read.
    bind(&r.c, &r.display, Some(&r.st)).await;
    let unbound = palette_for_display(state, &login, async {
        bind(&r.c, &r.display, None).await;
    })
    .await
    .unwrap();
    assert_eq!(unbound, None);
}

#[tokio::test]
async fn a_ceiling_change_during_the_read_gives_no_palette() {
    let r = rig().await;
    let login = r.login().await;
    let dropped = palette_for_display(&r.c.state, &login, async {
        r.c.store
            .display_update(
                r.display.clone(),
                DisplayChange {
                    ceiling: Some(vec![StationId::new(r.other.clone())]),
                    ..DisplayChange::default()
                },
            )
            .await
            .unwrap();
    })
    .await
    .unwrap();
    assert_eq!(dropped, None);
}

#[tokio::test]
async fn a_revoke_or_a_logout_during_the_read_gives_401() {
    let r = rig().await;
    let login = r.login().await;
    let ended = palette_for_display(&r.c.state, &login, async {
        r.c.store
            .end_display_session(login.session_hash.clone())
            .await
            .unwrap();
    })
    .await
    .unwrap_err();
    assert_eq!((ended.status.as_u16(), ended.code), (401, "login_required"));

    let q = rig().await;
    let login = q.login().await;
    let revoked = palette_for_display(&q.c.state, &login, async {
        q.c.store.display_revoke(q.display.clone()).await.unwrap();
    })
    .await
    .unwrap_err();
    assert_eq!(
        (revoked.status.as_u16(), revoked.code),
        (401, "login_required")
    );
}

#[tokio::test]
async fn the_palette_of_a_removed_station_is_gone_for_the_owner_and_the_display() {
    let r = rig().await;
    let st = StationId::new(r.st.clone());
    let rec = r.c.store.station(st.clone()).await.unwrap().unwrap();
    r.c.store.revoke_station(rec.key).await.unwrap();
    miss(&get(&r.c, &r.h, &format!("/api/stations/{}/palette", r.st)).await);
    let body: Value = r.read().await.body;
    assert_eq!(body["error"], "no_palette");
}
