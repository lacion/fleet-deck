//! The helpers of the desktop relay tests (R13 P3b relay): a fake station
//! that answers `desktop.act` and `desktop.info`, the owner requests and the
//! authority of a request.

#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::time::Duration;

use crate::common::board::{BoardClient, Http};
use crate::common::*;
use crate::share_support::*;
use fleetdeck_deck::desktop_relay::RelayCtx;
use fleetdeck_deck::store::displays::NewDisplay;
use fleetdeck_link::LiveLink;
use fleetdeck_link::control::InboundRequest;
use fleetdeck_proto::desktop::{
    DesktopAllowed, DesktopInfo, DesktopKind, DesktopTools, MonitorInfo,
};
use fleetdeck_proto::desktop_authority::{AUTHORITY_KEY, DesktopAuthority};
use fleetdeck_proto::link::{Outcome, WireError};
use serde_json::{Value, json};

pub fn info() -> DesktopInfo {
    DesktopInfo {
        kind: DesktopKind::Omarchy,
        omarchy_version: Some("3.0".to_owned()),
        monitors: vec![MonitorInfo {
            name: "DP-1".to_owned(),
            description: "wall".to_owned(),
            width: 3440,
            height: 1440,
            scale_pct: 100,
            refresh_hz: 60,
            focused: true,
            dpms_on: Some(true),
        }],
        locked: Some(false),
        dnd: None,
        idle_inhibited: None,
        walls: vec![],
        allowed: DesktopAllowed {
            desktop: true,
            capture: false,
            input: false,
        },
        tools: DesktopTools {
            wtype: false,
            pointer: false,
            recorder: false,
            grim: false,
            chromium: true,
            notify_send: true,
        },
        utc_offset_minutes: Some(120),
        offset_sampled_at: Some(1),
    }
}

/// The station answer of a successful action.
pub fn ok(address: Option<&str>) -> Outcome {
    Outcome::Ok(json!({ "address": address, "info": info() }))
}

pub fn err(code: &str, message: &str) -> Outcome {
    Outcome::Err(WireError::new(code, message))
}

/// Turns the deck setting `desktop_control` on or off.
pub fn enable(c: &BoardClient, on: bool) {
    let mut live = (*c.state.live.get()).clone();
    live.desktop_control = on;
    c.state.live.publish(live);
}

pub fn act_path(st: &str) -> String {
    format!("/api/stations/{st}/desktop/act")
}

pub fn info_path(st: &str) -> String {
    format!("/api/stations/{st}/desktop")
}

/// The tailnet name of the test board: an allowed origin that is not
/// loopback, so a wall launch can use it.
pub fn tailnet_origin(c: &BoardClient) -> String {
    format!("https://deck.tail1234.ts.net:{}", c.addr.port())
}

/// `POST .../desktop/act` as the owner, with the tailnet `Origin` of the
/// board.
pub async fn call(c: &BoardClient, h: &Headers, st: &str, body: Value) -> Http {
    call_from(c, h, st, body, &tailnet_origin(c)).await
}

/// `POST .../desktop/act` as the owner, with `origin`.
pub async fn call_from(c: &BoardClient, h: &Headers, st: &str, body: Value, origin: &str) -> Http {
    let mut headers = refs(h);
    headers.push(("Origin", origin));
    c.post(&act_path(st), &headers, body).await
}

/// One request of the owner and the answer of the fake station.
pub async fn exchange(
    c: &BoardClient,
    h: &Headers,
    station: &mut LiveLink,
    st: &str,
    body: Value,
    reply: Outcome,
) -> (Http, InboundRequest) {
    let answer = async {
        let req = next_request(station).await;
        station.control.respond(req.id, reply).await.unwrap();
        req
    };
    tokio::join!(call(c, h, st, body), answer)
}

/// The authority that the deck put into a `desktop.act` request.
pub fn authority(req: &InboundRequest) -> DesktopAuthority {
    assert_eq!(req.method, "desktop.act");
    serde_json::from_value(req.params[AUTHORITY_KEY].clone()).unwrap()
}

pub async fn quiet(station: &mut LiveLink) {
    no_request(station, Duration::from_millis(300)).await;
}

pub fn relay_ctx(f: &Fixture, c: &BoardClient) -> RelayCtx {
    RelayCtx {
        store: f.store.clone(),
        link: Arc::clone(&f.ctx),
        live: c.state.live.clone(),
    }
}

/// A display with the fake station in its ceiling.
pub async fn display(f: &Fixture, c: &BoardClient, name: &str) -> String {
    c.store
        .create_display(NewDisplay {
            name: name.to_owned(),
            ceiling: vec![f.station.clone()],
        })
        .await
        .unwrap()
        .display_id
}

/// Every audit row as one text, to look for a sentinel.
pub async fn audit_text(c: &BoardClient) -> String {
    c.store
        .audit_log(200)
        .await
        .unwrap()
        .into_iter()
        .map(|e| {
            format!(
                "{}|{}|{}|{}",
                e.actor,
                e.action,
                e.target.unwrap_or_default(),
                e.detail.unwrap_or_default()
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}
