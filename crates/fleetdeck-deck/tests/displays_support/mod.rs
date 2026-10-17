//! The helpers of the display tests: the deck with a linked station and the
//! owner, the pairing steps and a display login made in the store.

#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::time::Duration;

use crate::common::board::{BoardClient, Http};
use crate::common::*;
use crate::share_support::*;
use fleetdeck_core::Secret;
use fleetdeck_deck::store::displays::NewDisplay;
use fleetdeck_proto::StationId;
use serde_json::{Value, json};

pub const PASSWORD: &str = "a long owner password 123";
pub const SECRET: &str = "SENTINEL-SECRET-4711";

/// The deck with a linked station, the owner (with a password) and the
/// station id.
pub async fn setup_deck() -> (Fixture, BoardClient, Headers, String) {
    let (f, station, c, _) = setup().await;
    // The link stays up for the whole test.
    std::mem::forget(station);
    c.store
        .set_password(Secret::new(PASSWORD.to_owned()))
        .await
        .unwrap();
    // `set_password` ends every login: log in again.
    let h = owner(&c).await;
    let st = f.station.0.clone();
    (f, c, h, st)
}

pub async fn elevate(c: &BoardClient, h: &Headers) -> String {
    let r = post(c, h, "/api/auth/elevate", json!({ "password": PASSWORD })).await;
    assert_eq!(r.status, 200, "{}", r.body);
    r.body["elevation"].as_str().unwrap().to_owned()
}

pub fn set_cookies(r: &Http) -> Vec<String> {
    r.headers
        .iter()
        .filter(|(k, _)| k.eq_ignore_ascii_case("set-cookie"))
        .map(|(_, v)| v.clone())
        .collect()
}

/// `name=value` of the Set-Cookie of `name`.
pub fn cookie_pair(r: &Http, name: &str) -> Option<String> {
    set_cookies(r)
        .iter()
        .find(|c| c.starts_with(&format!("{name}=")))
        .map(|c| c.split(';').next().unwrap().to_owned())
}

pub async fn get_with(c: &BoardClient, path: &str, cookie: &str) -> Http {
    c.get(path, &[("Cookie", cookie)]).await
}

/// A display and its session cookie, made directly in the store.
pub async fn display_login(c: &BoardClient, name: &str, ceiling: &[&str]) -> (String, String) {
    let d = c
        .store
        .create_display(NewDisplay {
            name: name.to_owned(),
            ceiling: ceiling.iter().map(|s| StationId::new(*s)).collect(),
        })
        .await
        .unwrap();
    let (cookie, _) = c
        .store
        .create_display_session(d.display_id.clone())
        .await
        .unwrap();
    (d.display_id, format!("fd_display={}", cookie.expose()))
}

pub struct Pairing {
    pub id: String,
    pub code: String,
    pub pending: String,
}

pub async fn start_pairing(c: &BoardClient) -> Pairing {
    let r = c.post("/api/displays/pair", &[], json!({})).await;
    assert_eq!(r.status, 200, "{}", r.body);
    let pending = cookie_pair(&r, "fd_display_pending").expect("the pending cookie");
    let set = set_cookies(&r).join("|");
    assert!(
        set.contains("HttpOnly") && set.contains("Secure") && set.contains("SameSite=Strict"),
        "{set}"
    );
    assert!(set.contains("Max-Age=420"), "{set}");
    assert!(
        pending.starts_with("fd_display_pending=fdpend1_"),
        "{pending}"
    );
    let code = r.body["code"].as_str().unwrap().to_owned();
    assert!(
        code.len() == 6 && code.bytes().all(|b| b.is_ascii_digit()),
        "{code}"
    );
    Pairing {
        id: r.body["pairing_id"].as_str().unwrap().to_owned(),
        code,
        pending,
    }
}

/// A poll, one second after the last one (the poll limit reads the deck
/// clock).
pub async fn poll(f: &Fixture, c: &BoardClient, p: &Pairing, cookie: Option<&str>) -> Http {
    f.det.advance(Duration::from_secs(2));
    let path = format!("/api/displays/pair/{}", p.id);
    match cookie {
        Some(v) => c.get(&path, &[("Cookie", v)]).await,
        None => c.get(&path, &[]).await,
    }
}

pub fn approve_body(p: &Pairing, ceiling: &[&str], elevation: Option<&str>) -> Value {
    let mut b = json!({ "code": p.code, "name": "Wall", "ceiling": ceiling });
    if let Some(e) = elevation {
        b["elevation"] = json!(e);
    }
    b
}

pub async fn approve(
    c: &BoardClient,
    h: &Headers,
    p: &Pairing,
    ceiling: &[&str],
    elevation: Option<&str>,
) -> Http {
    post(
        c,
        h,
        &format!("/api/displays/pair/{}/approve", p.id),
        approve_body(p, ceiling, elevation),
    )
    .await
}
