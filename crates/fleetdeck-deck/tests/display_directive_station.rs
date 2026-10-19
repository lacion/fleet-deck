//! `display.directive` from a fake station (R13 P4 task E, design
//! `omarchy-p4.md` §3.6): the deck authorizes by the binding of the display
//! to the calling station, the deck setting `desktop_control` and a rate
//! limit, and writes an audit row. The store logic (orders, ceiling, other
//! stations) is tested in `store/display_directives_station_tests.rs`.
//!
//! Each test uses its own station: the limit is 2 calls per second for each
//! station.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::*;
use fleetdeck_deck::live::{DeckLive, LiveCell};
use fleetdeck_deck::store::displays::NewDisplay;
use fleetdeck_proto::SessionSummary;
use fleetdeck_proto::display::DisplayBinding;
use fleetdeck_proto::display_directive::{DirectiveBody, DirectiveView, SessionRef};
use fleetdeck_proto::event::{RequestKind, ResponseChannel};
use fleetdeck_proto::link::Outcome;
use fleetdeck_proto::session::OpenRequest;
use serde_json::json;

fn live(desktop_control: bool) -> LiveCell {
    LiveCell::new(DeckLive {
        name: "deck".to_owned(),
        conflict_mail: false,
        fleet_control: false,
        desktop_control,
        tailscale_cert: false,
        tailscale_bin: None,
        knobs: fleetdeck_deck::knobs::DeckKnobs::default(),
    })
}

fn refused(o: Outcome) -> (String, String) {
    match o {
        Outcome::Err(e) => (e.code, e.message),
        Outcome::Ok(v) => panic!("expected an error, got {v}"),
    }
}

/// A display bound to the fake station, with one session that has an open
/// request.
async fn wall(f: &Fixture) -> String {
    let mut card = SessionSummary::new("s1".into());
    card.open_requests = vec![OpenRequest {
        request_id: "q1".into(),
        node_id: None,
        request_kind: RequestKind::Command,
        title: "a title".to_owned(),
        details: "details".to_owned(),
        input: None,
        options: vec![],
        response: ResponseChannel::RpcReply,
        requested_at: 100,
    }];
    f.store
        .apply_snapshot(f.station.clone(), f.inc.clone(), 0, vec![card])
        .await
        .unwrap();
    let id = f
        .store
        .create_display(NewDisplay {
            name: "Wall".to_owned(),
            ceiling: vec![f.station.clone()],
        })
        .await
        .unwrap()
        .display_id;
    f.store
        .display_set_binding(
            id.clone(),
            Some(DisplayBinding {
                station_id: f.station.clone(),
                monitor: "DP-1".to_owned(),
            }),
        )
        .await
        .unwrap();
    id
}

async fn call(station: &fleetdeck_link::LiveLink, params: serde_json::Value) -> Outcome {
    tokio::time::timeout(
        T,
        station.control.request("display.directive", None, params),
    )
    .await
    .expect("answer")
    .unwrap()
}

fn params(display: &str, intent: &str) -> serde_json::Value {
    json!({"display_id": display, "intent": intent})
}

#[tokio::test]
async fn a_bound_station_gets_the_stored_directive_and_an_audit_row() {
    let mut f = fixture().await;
    assert!(f.ctx.desktop_live.set(live(true)).is_ok());
    let station = f.connect().await;
    let d = wall(&f).await;
    let Outcome::Ok(value) = call(&station, params(&d, "next_needs_you")).await else {
        panic!("the deck refused");
    };
    let view: DirectiveView = serde_json::from_value(value).unwrap();
    assert_eq!(view.generation, 1);
    assert_eq!(
        view.directive,
        DirectiveBody::Focus {
            session: SessionRef {
                station_id: f.station.clone(),
                session_id: "s1".into(),
            },
            pin: false,
            filter: None,
        }
    );
    // The wall reads the same directive.
    let read = f.store.display_directive(d.clone()).await.unwrap().unwrap();
    assert_eq!(read, view);
    // One audit row: the station, the display, identifiers only.
    let rows = f.store.audit_log(50).await.unwrap();
    let rows: Vec<_> = rows
        .into_iter()
        .filter(|r| r.action == "display.directive")
        .collect();
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].actor, format!("station:{}", f.station));
    assert_eq!(rows[0].target.as_deref(), Some(d.as_str()));
    assert_eq!(
        rows[0].detail.as_deref(),
        Some("intent=next_needs_you mode=focus")
    );
}

#[tokio::test]
async fn a_next_mode_directive_answers_the_stored_view() {
    let mut f = fixture().await;
    assert!(f.ctx.desktop_live.set(live(true)).is_ok());
    let station = f.connect().await;
    let d = wall(&f).await;
    let Outcome::Ok(value) = call(&station, params(&d, "next_mode")).await else {
        panic!("the deck refused");
    };
    let view: DirectiveView = serde_json::from_value(value).unwrap();
    assert!(matches!(view.directive, DirectiveBody::Focus { .. }));
}

#[tokio::test]
async fn an_unbound_station_and_an_unknown_display_get_the_same_directive_answer() {
    let mut f = fixture().await;
    assert!(f.ctx.desktop_live.set(live(true)).is_ok());
    let station = f.connect().await;
    // A display that is bound to nobody.
    let free = f
        .store
        .create_display(NewDisplay {
            name: "Free".to_owned(),
            ceiling: vec![f.station.clone()],
        })
        .await
        .unwrap()
        .display_id;
    let a = refused(call(&station, params(&free, "next_mode")).await);
    let b = refused(call(&station, params("no-such-display", "next_mode")).await);
    assert_eq!(
        a,
        (
            "not_bound".to_owned(),
            "This station has no wall display bound to it.".to_owned()
        )
    );
    assert_eq!(a, b);
    // No directive and no audit row came of it.
    let rows = f.store.audit_log(50).await.unwrap();
    assert!(rows.iter().all(|r| r.action != "display.directive"));
}

#[tokio::test]
async fn directive_requests_need_desktop_control_read_live() {
    let mut f = fixture().await;
    // An empty cell counts as off.
    let station = f.connect().await;
    let d = wall(&f).await;
    let off = (
        "desktop_control_off".to_owned(),
        "Desktop control is off on the deck.".to_owned(),
    );
    assert_eq!(refused(call(&station, params(&d, "next_mode")).await), off);
    // A cell that holds `false` is off, and a publish turns it on at once.
    let cell = live(false);
    assert!(f.ctx.desktop_live.set(cell.clone()).is_ok());
    assert_eq!(refused(call(&station, params(&d, "next_mode")).await), off);
    assert!(
        f.store
            .audit_log(50)
            .await
            .unwrap()
            .iter()
            .all(|r| r.action != "display.directive")
    );
    // The limit allows 2 calls in a second: wait for the window.
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    cell.publish(DeckLive {
        desktop_control: true,
        ..(*cell.get()).clone()
    });
    assert!(matches!(
        call(&station, params(&d, "next_mode")).await,
        Outcome::Ok(_)
    ));
}

#[tokio::test]
async fn the_directive_rate_limit_refuses_the_third_call_in_a_second() {
    let mut f = fixture().await;
    assert!(f.ctx.desktop_live.set(live(true)).is_ok());
    let station = f.connect().await;
    let d = wall(&f).await;
    // The two calls and the third go out at once.
    let first = call(&station, params(&d, "next_mode")).await;
    let second = call(&station, params(&d, "next_mode")).await;
    let third = call(&station, params(&d, "next_mode")).await;
    assert!(matches!(first, Outcome::Ok(_)));
    assert!(matches!(second, Outcome::Ok(_)));
    // Only a slow machine could make the window pass here.
    let (c, text) = refused(third);
    assert_eq!(c, "too_many_requests");
    assert_eq!(
        text,
        "Too many wall requests from this station. Wait one second."
    );
    // Only the two accepted calls have an audit row.
    let rows = f.store.audit_log(50).await.unwrap();
    let n = rows
        .iter()
        .filter(|r| r.action == "display.directive")
        .count();
    assert_eq!(n, 2);
}

#[tokio::test]
async fn a_directive_request_with_another_field_is_invalid() {
    let mut f = fixture().await;
    assert!(f.ctx.desktop_live.set(live(true)).is_ok());
    let station = f.connect().await;
    let d = wall(&f).await;
    for bad in [
        json!({"display_id": d, "intent": "next_mode", "body": {"mode": "mission"}}),
        json!({"display_id": d, "intent": "mission"}),
    ] {
        let (c, text) = refused(call(&station, bad).await);
        assert_eq!(c, "invalid_params");
        assert_eq!(text, "The wall request is not valid.");
    }
    assert!(
        f.store
            .audit_log(50)
            .await
            .unwrap()
            .iter()
            .all(|r| r.action != "display.directive")
    );
}
